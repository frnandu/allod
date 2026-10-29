//! Nostr contacts: the owner's follow list (NIP-02, kind 3) and the profiles of the people
//! followed, so Send can pay them by name. Pay targets, BOLT12 offers first (faster and
//! more private than LNURL):
//! - BOLT12 offers: kind 10058 `["offer", "lno1…"]` tags (NIP-B1 draft, nostr-protocol/nips
//!   #2421); `lno=` in a kind 10133 `["payto", "bip321", "bitcoin:?lno=…"]` tag (NIP-A3,
//!   bip321 type proposed in #2522); legacy kind 0 keys `lno`, `bolt12`, `offer`.
//! - Lightning addresses: kind 0 `lud16`; kind 10133 `["payto", "lightning", …]` (NIP-A3).
//! - LNURL: kind 0 `lud06`.
//!
//! Also resolves names for the pubkeys that payments come from (zaps). Cached 10 minutes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nostr::prelude::*;
use nostr_sdk::client::{Client, ReqTarget};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

const TTL: Duration = Duration::from_secs(600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CONTACTS: usize = 2000;

/// Relays that index profiles well; used with the node's own relays.
pub const DEFAULT_PROFILE_RELAYS: &[&str] =
    &["wss://purplepag.es", "wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"];

/// One way to pay a contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PayTarget {
    /// bolt12 | lightning_address | lnurl
    pub kind: &'static str,
    pub value: String,
    /// Where it was found: nip-b1 (kind 10058) | payto (kind 10133) | profile (kind 0)
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub pubkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// NIP-01 `name` (the handle), when a display name is shown as `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nip05: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lud16: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lud06: Option<String>,
    /// First BOLT12 offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<String>,
    /// Every way to pay this contact, BOLT12 offers first.
    pub targets: Vec<PayTarget>,
    /// The preferred target (the first of `targets`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pay_target: Option<String>,
}

impl Profile {
    fn add(&mut self, kind: &'static str, value: String, source: &'static str) {
        if !self.targets.iter().any(|t| t.value.eq_ignore_ascii_case(&value)) {
            self.targets.push(PayTarget { kind, value, source });
        }
    }

    /// Orders targets (offers, then addresses, then LNURL; stable within a kind) and
    /// fills the convenience fields.
    pub fn finish(mut self) -> Self {
        let rank = |k: &str| match k {
            "bolt12" => 0,
            "lightning_address" => 1,
            _ => 2,
        };
        self.targets.sort_by_key(|t| rank(t.kind));
        self.offer = self.targets.iter().find(|t| t.kind == "bolt12").map(|t| t.value.clone());
        if self.lud16.is_none() {
            self.lud16 = self
                .targets
                .iter()
                .find(|t| t.kind == "lightning_address")
                .map(|t| t.value.clone());
        }
        self.pay_target = self.targets.first().map(|t| t.value.clone());
        self
    }
}

fn is_offer(v: &str) -> bool {
    v.to_ascii_lowercase().starts_with("lno1") && v.len() <= 4000
}

/// BOLT12 offers (`lno=`) in a BIP-321 URI.
fn bip321_offers(uri: &str) -> Vec<String> {
    let Some((_, q)) = uri.split_once('?') else { return Vec::new() };
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, v)| k.eq_ignore_ascii_case("lno") && is_offer(v))
        .map(|(_, v)| v.to_string())
        .collect()
}

/// Adds the targets from kind 10058 (NIP-B1 offers) and kind 10133 (NIP-A3 payto) events.
pub fn add_payment_events(p: &mut Profile, events: &[&Event]) {
    for e in events {
        for t in e.tags.iter() {
            let t = t.as_slice();
            match (e.kind.as_u16(), t.first().map(String::as_str)) {
                (10058, Some("offer")) => {
                    if let Some(v) = t.get(1).filter(|v| is_offer(v)) {
                        p.add("bolt12", v.trim().to_string(), "nip-b1");
                    }
                }
                (10133, Some("payto")) => {
                    let ty = t.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
                    let Some(v) = t.get(2) else { continue };
                    match ty.as_str() {
                        "lightning" if v.contains('@') && v.len() <= 200 => {
                            p.add("lightning_address", v.trim().to_string(), "payto")
                        }
                        "bip321" => {
                            for o in bip321_offers(v) {
                                p.add("bolt12", o, "payto");
                            }
                        }
                        "bolt12" if is_offer(v) => p.add("bolt12", v.trim().to_string(), "payto"),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn field(m: &Value, k: &str, max: usize) -> Option<String> {
    let s = m.get(k)?.as_str()?.trim();
    (!s.is_empty() && s.len() <= max).then(|| s.to_string())
}

/// Parses a kind 0 event's content (targets from other kinds are added separately).
pub fn profile_from(pubkey: &str, content: &str) -> Profile {
    let m: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let display = field(&m, "display_name", 100).or_else(|| field(&m, "displayName", 100));
    let username = field(&m, "name", 100);
    let mut p = Profile {
        pubkey: pubkey.to_string(),
        name: display.clone().or_else(|| username.clone()),
        username: if display.is_some() { username } else { None },
        picture: field(&m, "picture", 1000).filter(|p| p.starts_with("https://")),
        nip05: field(&m, "nip05", 200),
        lud16: None,
        lud06: None,
        offer: None,
        targets: Vec::new(),
        pay_target: None,
    };
    for k in ["lno", "bolt12", "offer"] {
        if let Some(v) = field(&m, k, 4000).filter(|v| is_offer(v)) {
            p.add("bolt12", v, "profile");
        }
    }
    if let Some(v) = field(&m, "lud16", 200).filter(|v| v.contains('@')) {
        p.lud16 = Some(v.clone());
        p.add("lightning_address", v, "profile");
    }
    if let Some(v) =
        field(&m, "lud06", 2000).filter(|v| v.to_ascii_lowercase().starts_with("lnurl1"))
    {
        p.lud06 = Some(v.clone());
        p.add("lnurl", v, "profile");
    }
    p
}

fn newest(events: impl IntoIterator<Item = Event>) -> HashMap<(PublicKey, u16), Event> {
    let mut out: HashMap<(PublicKey, u16), Event> = HashMap::new();
    for e in events {
        let key = (e.pubkey, e.kind.as_u16());
        match out.get(&key) {
            Some(old) if old.created_at >= e.created_at => {}
            _ => {
                out.insert(key, e);
            }
        }
    }
    out
}

/// Newest event per (author, kind), fetched in chunks of `chunk` authors, 4 at a time.
async fn fetch_newest(
    client: &Client,
    authors: &[PublicKey],
    kinds: &[Kind],
    chunk: usize,
) -> HashMap<(PublicKey, u16), Event> {
    use futures_util::stream::{self, StreamExt};
    // Owned inputs per request, so the future is Send for axum handlers.
    let jobs: Vec<(Client, Vec<Kind>, Vec<PublicKey>)> =
        authors.chunks(chunk).map(|c| (client.clone(), kinds.to_vec(), c.to_vec())).collect();
    let batches: Vec<Vec<Event>> = stream::iter(jobs)
        .map(|(client, kinds, authors)| async move {
            let limit = authors.len() * kinds.len();
            let filter = Filter::new().kinds(kinds).authors(authors).limit(limit);
            match client.fetch_events(filter).timeout(FETCH_TIMEOUT).await {
                Ok(events) => events.into_iter().collect::<Vec<Event>>(),
                Err(e) => {
                    tracing::warn!(error = %e, "contacts: profile fetch failed");
                    Vec::new()
                }
            }
        })
        .buffer_unordered(4)
        .collect()
        .await;
    newest(batches.into_iter().flatten())
}

/// Outbox model (NIP-65) limits: each account is asked on up to 2 of its write relays,
/// from at most 25 relays per lookup.
const OUTBOX_PER_AUTHOR: usize = 2;
const OUTBOX_MAX_RELAYS: usize = 25;
const OUTBOX_MAX_PER_LIST: usize = 8;

/// The write relays of a NIP-65 relay list (kind 10002): `r` tags without a marker or
/// marked `write`. Only public `wss://` relays (unless `allow_private`, for tests);
/// normalized and deduplicated.
pub fn write_relays(e: &Event, allow_private: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in e.tags.iter() {
        let t = t.as_slice();
        if t.first().map(String::as_str) != Some("r") {
            continue;
        }
        let marker = t.get(2).map(|m| m.to_ascii_lowercase());
        if marker.as_deref().is_some_and(|m| m != "write") {
            continue;
        }
        let Some(url) = t.get(1) else { continue };
        let url = url.trim().trim_end_matches('/').to_ascii_lowercase();
        let host = match (url.strip_prefix("wss://"), url.strip_prefix("ws://")) {
            (Some(h), _) => h,
            (None, Some(h)) if allow_private => h,
            _ => continue,
        };
        let host = host.split(['/', ':']).next().unwrap_or_default();
        let private = host == "localhost"
            || host.ends_with(".onion")
            || host.ends_with(".local")
            || host.starts_with("127.")
            || host.starts_with("10.")
            || host.starts_with("192.168.")
            || !host.contains('.');
        if (private && !allow_private) || out.contains(&url) {
            continue;
        }
        out.push(url);
        if out.len() >= OUTBOX_MAX_PER_LIST {
            break;
        }
    }
    out
}

/// Chooses relays to ask: greedily the relay that covers the most accounts still
/// needing one, until each account has `per_author` relays or `max` relays are chosen.
pub fn plan_outbox(
    lists: &HashMap<PublicKey, Vec<String>>,
    per_author: usize,
    max: usize,
) -> Vec<(String, Vec<PublicKey>)> {
    let mut need: HashMap<PublicKey, usize> = lists
        .iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(pk, r)| (*pk, per_author.min(r.len())))
        .collect();
    let mut plan: Vec<(String, Vec<PublicKey>)> = Vec::new();
    while plan.len() < max {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (pk, relays) in lists {
            if need.get(pk).copied().unwrap_or(0) == 0 {
                continue;
            }
            for r in relays {
                if !plan.iter().any(|(u, _)| u == r) {
                    *counts.entry(r.as_str()).or_default() += 1;
                }
            }
        }
        let Some((best, n)) = counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        else {
            break;
        };
        if n == 0 {
            break;
        }
        let best = best.to_string();
        let mut authors = Vec::new();
        for (pk, relays) in lists {
            if relays.contains(&best) && need.get(pk).copied().unwrap_or(0) > 0 {
                authors.push(*pk);
                *need.get_mut(pk).expect("present") -= 1;
            }
        }
        authors.sort();
        plan.push((best, authors));
    }
    plan
}

/// Asks each planned relay for its accounts' events, through a temporary client so the
/// index relays' pool stays small. Newest event per (author, kind).
async fn fetch_from_relays(
    plan: Vec<(String, Vec<PublicKey>)>,
    kinds: &[Kind],
) -> HashMap<(PublicKey, u16), Event> {
    use futures_util::stream::{self, StreamExt};
    if plan.is_empty() {
        return HashMap::new();
    }
    let client = Client::builder().build();
    for (url, _) in &plan {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().await;
    let _ = client.try_connect().timeout(Duration::from_secs(5)).await;
    let mut jobs: Vec<(Client, String, Vec<Kind>, Vec<PublicKey>)> = Vec::new();
    for (url, authors) in &plan {
        for c in authors.chunks(50) {
            jobs.push((client.clone(), url.clone(), kinds.to_vec(), c.to_vec()));
        }
    }
    let batches: Vec<Vec<Event>> = stream::iter(jobs)
        .map(|(client, url, kinds, authors)| async move {
            let limit = authors.len() * kinds.len();
            let filter = Filter::new().kinds(kinds).authors(authors).limit(limit);
            match client
                .fetch_events(ReqTarget::single(url.as_str(), [filter]))
                .timeout(FETCH_TIMEOUT)
                .await
            {
                Ok(events) => events.into_iter().collect::<Vec<Event>>(),
                Err(_) => Vec::new(),
            }
        })
        .buffer_unordered(6)
        .collect()
        .await;
    client.shutdown().await;
    newest(batches.into_iter().flatten())
}

/// Keeps the newer of two copies of each (author, kind).
fn merge_newest(
    into: &mut HashMap<(PublicKey, u16), Event>,
    from: HashMap<(PublicKey, u16), Event>,
) {
    for (k, e) in from {
        match into.get(&k) {
            Some(old) if old.created_at >= e.created_at => {}
            _ => {
                into.insert(k, e);
            }
        }
    }
}

/// Profile, offer list (NIP-B1) and payto list (NIP-A3).
const PROFILE_KINDS: [Kind; 3] = [Kind::Metadata, Kind::Custom(10058), Kind::Custom(10133)];

fn build_profile(pk: &PublicKey, found: &HashMap<(PublicKey, u16), Event>) -> Profile {
    let hex = pk.to_hex();
    let mut p = found
        .get(&(*pk, 0))
        .map(|e| profile_from(&hex, &e.content))
        .unwrap_or_else(|| profile_from(&hex, "{}"));
    let extra: Vec<&Event> =
        [10058u16, 10133].iter().filter_map(|k| found.get(&(*pk, *k))).collect();
    add_payment_events(&mut p, &extra);
    p.finish()
}

pub struct Contacts {
    relays: Vec<String>,
    /// Lets the outbox lookup use private/plain-ws relays (in-process test relays).
    allow_private: bool,
    client: Mutex<Option<Client>>,
    profiles: Mutex<HashMap<String, (Instant, Profile)>>,
    follows: Mutex<HashMap<String, (Instant, Vec<String>)>>,
    /// A background outbox lookup is running.
    discovering: AtomicBool,
}

impl Contacts {
    pub fn new(relays: Vec<String>) -> Self {
        Contacts {
            relays,
            allow_private: false,
            client: Mutex::new(None),
            profiles: Mutex::new(HashMap::new()),
            follows: Mutex::new(HashMap::new()),
            discovering: AtomicBool::new(false),
        }
    }

    /// For tests with in-process relays on `ws://127.0.0.1`.
    pub fn allow_private_relays(mut self) -> Self {
        self.allow_private = true;
        self
    }

    async fn client(&self) -> Client {
        let mut c = self.client.lock().await;
        if let Some(c) = c.as_ref() {
            return c.clone();
        }
        let client = Client::builder().build();
        for r in &self.relays {
            if let Err(e) = client.add_relay(r.as_str()).await {
                tracing::warn!(relay = %r, error = %e, "contacts: bad relay");
            }
        }
        client.connect().await;
        let _ = client.try_connect().timeout(Duration::from_secs(5)).await;
        *c = Some(client.clone());
        client
    }

    /// The people `owner` follows, with their profiles, payable ones first.
    pub async fn contacts(
        self: &Arc<Self>,
        owner: &str,
        refresh: bool,
    ) -> Result<Vec<Profile>, String> {
        let owner_pk = PublicKey::from_hex(owner).map_err(|e| e.to_string())?;
        let cached = self
            .follows
            .lock()
            .await
            .get(owner)
            .filter(|(t, _)| !refresh && t.elapsed() < TTL)
            .map(|(_, v)| v.clone());
        let follows = match cached {
            Some(f) => f,
            None => {
                let client = self.client().await;
                let events = client
                    .fetch_events(
                        Filter::new()
                            .kinds([Kind::ContactList, Kind::RelayList])
                            .author(owner_pk)
                            .limit(10),
                    )
                    .timeout(FETCH_TIMEOUT)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut found = newest(events);
                // NIP-65: the owner's follow list may live only on their own write relays.
                if let Some(list) = found.get(&(owner_pk, Kind::RelayList.as_u16())) {
                    let relays = write_relays(list, self.allow_private);
                    let plan: Vec<(String, Vec<PublicKey>)> =
                        relays.into_iter().take(4).map(|r| (r, vec![owner_pk])).collect();
                    merge_newest(&mut found, fetch_from_relays(plan, &[Kind::ContactList]).await);
                }
                let list = found
                    .remove(&(owner_pk, Kind::ContactList.as_u16()))
                    .map(|e| {
                        let mut seen = std::collections::HashSet::new();
                        e.tags
                            .public_keys()
                            .map(|p| p.to_hex())
                            .filter(|p| seen.insert(p.clone()))
                            .take(MAX_CONTACTS)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                self.follows.lock().await.insert(owner.to_string(), (Instant::now(), list.clone()));
                list
            }
        };
        let mut out = self.profiles(&follows, refresh).await;
        out.sort_by(|a, b| {
            (b.pay_target.is_some(), a.name.is_none(), a.name.as_deref().map(str::to_lowercase))
                .cmp(&(
                    a.pay_target.is_some(),
                    b.name.is_none(),
                    b.name.as_deref().map(str::to_lowercase),
                ))
        });
        Ok(out)
    }

    /// Profiles for these pubkeys (cached; missing ones come back with only the pubkey).
    /// Answers from the index relays; accounts still without a BOLT12 offer are then
    /// looked up on their own relays (NIP-65) in the background, which updates the cache:
    /// [`Contacts::discovering`] is true meanwhile.
    pub async fn profiles(self: &Arc<Self>, pubkeys: &[String], refresh: bool) -> Vec<Profile> {
        let mut missing: Vec<PublicKey> = Vec::new();
        {
            let cache = self.profiles.lock().await;
            for pk in pubkeys {
                let fresh = cache.get(pk).is_some_and(|(t, _)| !refresh && t.elapsed() < TTL);
                if !fresh {
                    if let Ok(p) = PublicKey::from_hex(pk) {
                        missing.push(p);
                    }
                }
            }
        }
        if !missing.is_empty() {
            let client = self.client().await;
            // Relays cap the events they return per request (often a few hundred), so
            // ask for 50 accounts (3 kinds each) at a time, 4 requests in parallel...
            let mut found = fetch_newest(&client, &missing, &PROFILE_KINDS, 50).await;
            // ...then once more, in small groups, for accounts whose profile did not come back.
            let again: Vec<PublicKey> =
                missing.iter().filter(|pk| !found.contains_key(&(**pk, 0))).copied().collect();
            if !again.is_empty() {
                for (k, e) in fetch_newest(&client, &again, &[Kind::Metadata], 15).await {
                    found.entry(k).or_insert(e);
                }
            }
            let mut unresolved = Vec::new();
            {
                let mut cache = self.profiles.lock().await;
                for pk in &missing {
                    let p = build_profile(pk, &found);
                    if p.offer.is_none() {
                        unresolved.push(*pk);
                    }
                    cache.insert(pk.to_hex(), (Instant::now(), p));
                }
            }
            if !unresolved.is_empty() && !self.discovering.swap(true, Ordering::SeqCst) {
                let me = self.clone();
                let known: HashMap<(PublicKey, u16), Event> =
                    found.into_iter().filter(|((pk, _), _)| unresolved.contains(pk)).collect();
                tokio::spawn(async move {
                    me.discover(client, unresolved, known).await;
                    me.discovering.store(false, Ordering::SeqCst);
                });
            }
        }
        let cache = self.profiles.lock().await;
        pubkeys
            .iter()
            .map(|pk| {
                cache
                    .get(pk)
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| profile_from(pk, "{}").finish())
            })
            .collect()
    }

    /// True while the background lookup on contacts' own relays runs.
    pub fn discovering(&self) -> bool {
        self.discovering.load(Ordering::SeqCst)
    }

    /// Outbox model (NIP-65): asks accounts' own write relays (from their kind 10002
    /// relay lists) when index relays did not find a BOLT12 offer, and updates the cache.
    async fn discover(
        &self,
        client: Client,
        unresolved: Vec<PublicKey>,
        mut found: HashMap<(PublicKey, u16), Event>,
    ) {
        // Relays already asked would only repeat their answer.
        let asked: Vec<String> = self
            .relays
            .iter()
            .map(|r| r.trim().trim_end_matches('/').to_ascii_lowercase())
            .collect();
        let lists: HashMap<PublicKey, Vec<String>> =
            fetch_newest(&client, &unresolved, &[Kind::RelayList], 50)
                .await
                .into_iter()
                .map(|((pk, _), e)| {
                    let own: Vec<String> = write_relays(&e, self.allow_private)
                        .into_iter()
                        .filter(|r| !asked.contains(r))
                        .collect();
                    (pk, own)
                })
                .collect();
        let plan = plan_outbox(&lists, OUTBOX_PER_AUTHOR, OUTBOX_MAX_RELAYS);
        if plan.is_empty() {
            return;
        }
        tracing::debug!(
            accounts = unresolved.len(),
            relays = plan.len(),
            "contacts: outbox lookup"
        );
        merge_newest(&mut found, fetch_from_relays(plan, &PROFILE_KINDS).await);
        let mut cache = self.profiles.lock().await;
        let mut gained = 0;
        for pk in &unresolved {
            let p = build_profile(pk, &found);
            if !p.targets.is_empty() || p.name.is_some() {
                gained += usize::from(!p.targets.is_empty());
                cache.insert(pk.to_hex(), (Instant::now(), p));
            }
        }
        tracing::debug!(gained, "contacts: outbox lookup done");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_fields_and_pay_target() {
        let p = profile_from("pk", r#"{"name":"bob","display_name":"Bob","lud16":"bob@x.org","lno":"lno1qqq","picture":"https://x/p.png"}"#).finish();
        assert_eq!((p.name.as_deref(), p.username.as_deref()), (Some("Bob"), Some("bob")));
        assert_eq!(p.pay_target.as_deref(), Some("lno1qqq"), "offer preferred");
        assert_eq!(
            p.targets.iter().map(|t| t.kind).collect::<Vec<_>>(),
            ["bolt12", "lightning_address"]
        );
        let p = profile_from("pk", r#"{"name":"al","lud16":"al@x.org","bolt12":"not an offer","picture":"http://insecure"}"#).finish();
        assert_eq!(p.pay_target.as_deref(), Some("al@x.org"));
        assert!(p.offer.is_none() && p.picture.is_none() && p.username.is_none());
        let p = profile_from("pk", r#"{"lud06":"LNURL1DP68"}"#).finish();
        assert_eq!(p.pay_target.as_deref(), Some("LNURL1DP68"));
        let p = profile_from("pk", "garbage").finish();
        assert!(p.pay_target.is_none() && p.name.is_none() && p.targets.is_empty());
    }

    #[test]
    fn payment_events_nip_b1_and_payto() {
        use nostr::prelude::*;
        let k = Keys::generate();
        let offers = EventBuilder::new(Kind::Custom(10058), "")
            .tags([
                Tag::parse(["offer", "lno1fromb1"]).unwrap(),
                Tag::parse(["offer", "garbage"]).unwrap(),
            ])
            .finalize(&k)
            .unwrap();
        let payto = EventBuilder::new(Kind::Custom(10133), "")
            .tags([
                Tag::parse(["payto", "lightning", "carol@pay.example"]).unwrap(),
                Tag::parse(["payto", "bip321", "bitcoin:?lightning=lnbc1x&lno=lno1frompayto"])
                    .unwrap(),
                Tag::parse(["payto", "bitcoin", "bc1qxyz"]).unwrap(),
            ])
            .finalize(&k)
            .unwrap();
        let mut p = profile_from(
            "pk",
            r#"{"name":"carol","lud16":"carol@pay.example","lud06":"lnurl1abc"}"#,
        );
        add_payment_events(&mut p, &[&offers, &payto]);
        let p = p.finish();
        let got: Vec<(&str, &str, &str)> =
            p.targets.iter().map(|t| (t.kind, t.value.as_str(), t.source)).collect();
        assert_eq!(
            got,
            [
                ("bolt12", "lno1fromb1", "nip-b1"),
                ("bolt12", "lno1frompayto", "payto"),
                ("lightning_address", "carol@pay.example", "profile"),
                ("lnurl", "lnurl1abc", "profile"),
            ],
            "offers first, duplicates once"
        );
        assert_eq!(p.pay_target.as_deref(), Some("lno1fromb1"));
    }

    #[test]
    fn nip65_write_relays() {
        use nostr::prelude::*;
        let e = EventBuilder::new(Kind::RelayList, "")
            .tags([
                Tag::parse(["r", "wss://write.example/"]).unwrap(),
                Tag::parse(["r", "wss://Both.Example"]).unwrap(),
                Tag::parse(["r", "wss://w2.example", "write"]).unwrap(),
                Tag::parse(["r", "wss://read.example", "read"]).unwrap(),
                Tag::parse(["r", "ws://plain.example"]).unwrap(),
                Tag::parse(["r", "wss://localhost:7777"]).unwrap(),
                Tag::parse(["r", "wss://abc.onion"]).unwrap(),
                Tag::parse(["r", "wss://write.example"]).unwrap(),
            ])
            .finalize(&Keys::generate())
            .unwrap();
        assert_eq!(
            write_relays(&e, false),
            ["wss://write.example", "wss://both.example", "wss://w2.example"]
        );
    }

    #[test]
    fn outbox_plan_covers_with_few_relays() {
        use nostr::prelude::*;
        let pks: Vec<PublicKey> = (0..4).map(|_| Keys::generate().public_key()).collect();
        let mut lists = HashMap::new();
        lists.insert(pks[0], vec!["wss://big".into(), "wss://a".into()]);
        lists.insert(pks[1], vec!["wss://big".into(), "wss://b".into()]);
        lists.insert(pks[2], vec!["wss://big".into()]);
        lists.insert(pks[3], vec![]);
        let plan = plan_outbox(&lists, 2, 25);
        assert_eq!(plan[0].0, "wss://big", "the relay most accounts share comes first");
        assert_eq!(plan[0].1.len(), 3);
        // Each account gets up to two relays; nobody without a list is planned.
        let count = |pk: &PublicKey| plan.iter().filter(|(_, a)| a.contains(pk)).count();
        assert_eq!((count(&pks[0]), count(&pks[1]), count(&pks[2]), count(&pks[3])), (2, 2, 1, 0));
        assert_eq!(plan_outbox(&lists, 2, 1).len(), 1, "relay cap");
    }
}
