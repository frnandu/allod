//! The payer's side of zaps: building and checking the NIP-57 zap request (kind 9734) of
//! an LNURL zap, and the NIP-B1 zap intent (kind 9737) and zap (kind 9736) of a BOLT12
//! zap (nostr-protocol/nips#2421, draft).

use nostr::prelude::*;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// NIP-B1 zap: the public proof that a BOLT12 payment was made.
pub const BOLT12_ZAP_KIND: u16 = 9736;
/// NIP-B1 zap intent: signed before paying, embedded in the zap.
pub const BOLT12_INTENT_KIND: u16 = 9737;
/// NIP-B1 offer announcement of the recipient.
pub const BOLT12_OFFERS_KIND: u16 = 10058;
/// What the BOLT12 payer note starts with; the intent's event id follows.
pub const BOLT12_NOTE_PREFIX: &str = "nostr:nipB1:";
/// Longest zap comment accepted from the owner.
pub const COMMENT_MAX: usize = 280;
/// Largest zap intent accepted; it embeds the recipient's offer announcement.
pub const MAX_INTENT_BYTES: usize = 32 * 1024;

/// An event still to be signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Template {
    pub kind: u16,
    pub content: String,
    pub tags: Vec<Vec<String>>,
    pub created_at: u64,
}

impl Template {
    pub fn sign(&self, keys: &Keys) -> Result<Event, String> {
        let tags: Vec<Tag> = self
            .tags
            .iter()
            .map(|t| Tag::parse(t.clone()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        EventBuilder::new(Kind::from(self.kind), self.content.clone())
            .tags(tags)
            .custom_created_at(Timestamp::from(self.created_at))
            .finalize(keys)
            .map_err(|e| e.to_string())
    }
}

fn tag(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

fn values<'a>(ev: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    ev.tags.iter().filter_map(move |t| {
        let s = t.as_slice();
        (s.first().map(String::as_str) == Some(name)).then(|| s.get(1).map(String::as_str))?
    })
}

fn only<'a>(ev: &'a Event, name: &'a str) -> Result<&'a str, String> {
    let mut found = values(ev, name);
    match (found.next(), found.next()) {
        (Some(v), None) => Ok(v),
        _ => Err(format!("the zap needs exactly one {name} tag")),
    }
}

/// The NIP-57 zap request of `amount_msat` to `recipient`.
pub fn request_template(
    recipient: &str,
    amount_msat: u64,
    comment: &str,
    relays: &[String],
    anonymous: bool,
    created_at: u64,
) -> Template {
    let mut relay_tag = tag(&["relays"]);
    relay_tag.extend(relays.iter().take(crate::server::MAX_ZAP_RELAYS).cloned());
    let mut tags =
        vec![relay_tag, tag(&["amount", &amount_msat.to_string()]), tag(&["p", recipient])];
    if anonymous {
        tags.push(tag(&["anon"]));
    }
    Template { kind: 9734, content: comment.to_string(), tags, created_at }
}

/// A BOLT12 offer in the canonical form of NIP-B1: no `+` separators or whitespace.
pub fn canonical_offer(offer: &str) -> String {
    offer.chars().filter(|c| *c != '+' && !c.is_whitespace()).collect::<String>().to_lowercase()
}

/// The offers a kind 10058 announcement lists.
pub fn announced_offers(announcement: &Event) -> Vec<String> {
    values(announcement, "offer").map(canonical_offer).filter(|o| o.starts_with("lno1")).collect()
}

/// The NIP-B1 zap intent. `zap_id` is at least 128 random bits in lowercase hex.
pub fn intent_template(
    recipient: &str,
    amount_msat: u64,
    comment: &str,
    announcement: &str,
    zap_id: &str,
    created_at: u64,
) -> Template {
    Template {
        kind: BOLT12_INTENT_KIND,
        content: comment.to_string(),
        tags: vec![
            tag(&["p", recipient]),
            tag(&["amount", &amount_msat.to_string()]),
            tag(&["offer_event", announcement]),
            tag(&["zap_id", zap_id]),
        ],
        created_at,
    }
}

/// A checked zap intent.
#[derive(Debug, Clone)]
pub struct Intent {
    pub event: Event,
    pub zap_id: String,
    /// Canonical offers of the embedded announcement.
    pub offers: Vec<String>,
}

/// The NIP-B1 rules for an intent, including its embedded offer announcement.
pub fn check_intent(raw: &str, amount_msat: u64) -> Result<Intent, String> {
    if raw.len() > MAX_INTENT_BYTES {
        return Err("the zap intent is too large".into());
    }
    let event = Event::from_json(raw).map_err(|_| "the zap intent is not a Nostr event")?;
    if event.kind.as_u16() != BOLT12_INTENT_KIND {
        return Err("the zap intent is not kind 9737".into());
    }
    event.verify().map_err(|_| "the zap intent's signature is not valid")?;
    let recipient = only(&event, "p")?;
    if only(&event, "amount")?.parse::<u64>().ok() != Some(amount_msat) || amount_msat == 0 {
        return Err("the zap intent's amount does not match".into());
    }
    if values(&event, "e").count() + values(&event, "a").count() > 1 {
        return Err("the zap intent has more than one target".into());
    }
    let zap_id = only(&event, "zap_id")?;
    if zap_id.len() < 32 || !zap_id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("the zap intent's zap_id is not valid".into());
    }
    let announcement = Event::from_json(only(&event, "offer_event")?)
        .map_err(|_| "the offer announcement is not a Nostr event")?;
    announcement.verify().map_err(|_| "the offer announcement's signature is not valid")?;
    if announcement.kind.as_u16() != BOLT12_OFFERS_KIND
        || announcement.pubkey.to_hex() != recipient
        || announcement.created_at > event.created_at
    {
        return Err("the offer announcement does not belong to this zap".into());
    }
    let offers = announced_offers(&announcement);
    if offers.is_empty() {
        return Err("the offer announcement lists no offer".into());
    }
    let zap_id = zap_id.to_string();
    Ok(Intent { event, zap_id, offers })
}

/// The payer note that binds the payment to the intent.
pub fn payer_note(intent: &Event) -> String {
    format!("{BOLT12_NOTE_PREFIX}{}", intent.id.to_hex())
}

/// The zap (kind 9736) proving `intent` was paid. A public zap names its payer in `P`.
pub fn zap_template(intent: &Intent, proof: &str, public: bool, paid_at: u64) -> Template {
    let ev = &intent.event;
    let mut tags = vec![tag(&["description", &ev.as_json()])];
    for name in ["p", "amount", "offer_event", "e", "a", "k"] {
        if let Some(v) = values(ev, name).next() {
            tags.push(tag(&[name, v]));
        }
    }
    tags.push(tag(&["proof", proof]));
    if public {
        tags.push(tag(&["P", &ev.pubkey.to_hex()]));
    }
    Template {
        kind: BOLT12_ZAP_KIND,
        content: ev.content.clone(),
        tags,
        created_at: paid_at.max(ev.created_at.as_secs()),
    }
}

/// Checks a signed zap against what [`zap_template`] asked to be signed.
pub fn check_signed_zap(raw: &str, intent: &Intent, want: &Template) -> Result<Event, String> {
    let ev = Event::from_json(raw).map_err(|_| "the zap is not a Nostr event")?;
    ev.verify().map_err(|_| "the zap's signature is not valid")?;
    if ev.pubkey != intent.event.pubkey {
        return Err("the zap must be signed with the key that signed its intent".into());
    }
    let tags: Vec<Vec<String>> = ev.tags.iter().map(|t| t.as_slice().to_vec()).collect();
    if ev.kind.as_u16() != want.kind || ev.content != want.content || tags != want.tags {
        return Err("the signed zap is not the one that was asked for".into());
    }
    Ok(ev)
}

/// The throwaway key of one anonymous zap, derived from `secret` and the zap's own random
/// id: nothing is stored, and the zap can still be signed after a restart.
pub fn anonymous_keys(secret: &str, zap_id: &str) -> Result<Keys, String> {
    let mut h = Sha256::new();
    for part in ["allod anonymous zap", secret, zap_id] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    Keys::parse(&hex::encode(h.finalize())).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00112233445566778899aabbccddeeff";

    fn announcement(keys: &Keys) -> String {
        Template {
            kind: BOLT12_OFFERS_KIND,
            content: String::new(),
            tags: vec![tag(&["offer", "LNO1ABC+DEF"])],
            created_at: 10,
        }
        .sign(keys)
        .unwrap()
        .as_json()
    }

    #[test]
    fn intent_and_zap() {
        let (payer, recipient) = (Keys::generate(), Keys::generate());
        let pk = recipient.public_key().to_hex();
        let raw = intent_template(&pk, 21_000, "hi", &announcement(&recipient), ID, 20)
            .sign(&payer)
            .unwrap()
            .as_json();
        let intent = check_intent(&raw, 21_000).unwrap();
        assert_eq!(intent.offers, ["lno1abcdef"]);
        assert!(payer_note(&intent.event).starts_with("nostr:nipB1:"));
        assert!(check_intent(&raw, 22_000).is_err());

        let want = zap_template(&intent, "lnp1proof", true, 30);
        let signed = want.sign(&payer).unwrap().as_json();
        assert!(check_signed_zap(&signed, &intent, &want).is_ok());
        let other = want.sign(&recipient).unwrap().as_json();
        assert!(check_signed_zap(&other, &intent, &want).is_err());
        let anonymous = zap_template(&intent, "lnp1proof", false, 30);
        assert!(anonymous.tags.iter().all(|t| t[0] != "P"));
        assert!(check_signed_zap(&signed, &intent, &anonymous).is_err());
    }

    #[test]
    fn intent_needs_the_recipients_own_announcement() {
        let (payer, recipient) = (Keys::generate(), Keys::generate());
        let pk = recipient.public_key().to_hex();
        for bad in [announcement(&payer), "{}".to_string()] {
            let raw = intent_template(&pk, 1000, "", &bad, ID, 20).sign(&payer).unwrap().as_json();
            assert!(check_intent(&raw, 1000).is_err());
        }
        // Announced after the intent was made.
        let raw = intent_template(&pk, 1000, "", &announcement(&recipient), ID, 5)
            .sign(&payer)
            .unwrap()
            .as_json();
        assert!(check_intent(&raw, 1000).is_err());
    }

    #[test]
    fn request_is_accepted_by_our_own_server_checks() {
        let relays = vec!["wss://relay.example".to_string()];
        let raw =
            request_template(&Keys::generate().public_key().to_hex(), 5000, "gm", &relays, true, 1)
                .sign(&Keys::generate())
                .unwrap()
                .as_json();
        assert!(crate::server::check_zap_request(&raw, 5000).is_ok());
    }

    #[test]
    fn anonymous_keys_are_per_zap_and_repeatable() {
        let a = anonymous_keys("s", ID).unwrap();
        assert_eq!(a.public_key(), anonymous_keys("s", ID).unwrap().public_key());
        assert_ne!(a.public_key(), anonymous_keys("s", "ff").unwrap().public_key());
        assert_ne!(a.public_key(), anonymous_keys("t", ID).unwrap().public_key());
    }
}
