//! The background reconcile (SPEC §12.1, §12.7): offers for new addresses, records at
//! DNS providers, record and reachability checks, gateway tunnels.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    njalla, now, provider_secret, store, Address, Domain, DomainService, Gateway, ManagedRecord,
    QUICK_CHECKS, REACH_GRACE_SECS,
};
use crate::public_name::{bip353_subname, name_description, twelve};

/// Background reconcile interval.
const EVERY: i64 = 300;
/// Retry interval after a failed DNS provider sync.
const RETRY: i64 = 60;
/// How often a gateway being bought is checked.
const LNVPS_POLL: Duration = Duration::from_secs(15);
/// Minimum time between two reconciles.
const MIN_GAP: Duration = Duration::from_secs(10);

/// What a domain should have at its provider: key → (zone, type, relative name, content).
type Want = BTreeMap<String, (String, String, String, String)>;

impl DomainService {
    /// Runs forever: reconciles every 5 minutes, at the quick checks after a change, and
    /// soon after [`Self::trigger`].
    pub fn start(self: &Arc<Self>) {
        self.spawn_prism();
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.reconcile().await;
                this.lnvps_upkeep().await;
                this.desec_upkeep().await;
                // Until the next full run, a gateway being bought or waiting for its first
                // handshake gets a light pass every few seconds (tunnel and LNVPS only, no
                // DNS). A step or state change triggers a full run.
                let full_at = Instant::now() + this.next_wait();
                loop {
                    let left = full_at.saturating_duration_since(Instant::now());
                    let wait = if this.busy() { left.min(LNVPS_POLL) } else { left };
                    let woken = tokio::time::timeout(wait, this.wake.notified()).await.is_ok();
                    if woken || Instant::now() >= full_at {
                        break;
                    }
                    let gateways = store::list_gateways(&this.pool).await.unwrap_or_default();
                    this.gateway_health(&gateways, &mut Vec::new()).await;
                    this.lightning_files(&gateways, &mut Vec::new()).await;
                    this.lnvps_upkeep().await;
                    this.desec_upkeep().await;
                }
                let gap = this
                    .last_run
                    .lock()
                    .unwrap()
                    .map(|t| MIN_GAP.saturating_sub(t.elapsed()))
                    .unwrap_or_default();
                tokio::time::sleep(gap).await;
            }
        });
    }

    /// A gateway is being bought, or has not connected yet; a deSEC account waits for
    /// its activation mail.
    fn busy(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        self.lnvps_active.load(Relaxed)
            || self.gateway_waiting.load(Relaxed)
            || self.onion_waiting.load(Relaxed)
            || self.desec_waiting.load(Relaxed)
    }

    async fn desec_upkeep(&self) {
        self.desec_signup_upkeep().await;
        self.njalla_signup_upkeep().await;
        self.njalla_buy_upkeep().await;
        let waiting = self.desec_signup_active().await
            || self.njalla_signup_active().await
            || self.njalla_purchase_active().await;
        self.desec_waiting.store(waiting, std::sync::atomic::Ordering::Relaxed);
    }

    fn next_wait(&self) -> Duration {
        let t = now();
        let base = if self.status.lock().unwrap().errors.is_empty() { EVERY } else { RETRY };
        let quick = self.changed_since.lock().unwrap().and_then(|c| {
            QUICK_CHECKS.iter().map(|o| c + o).find(|due| *due > t).map(|due| due - t)
        });
        Duration::from_secs(quick.map_or(base, |q| q.min(base)).max(1) as u64)
    }

    /// One pass over everything.
    pub async fn reconcile(&self) {
        *self.last_run.lock().unwrap() = Some(Instant::now());
        let _g = self.run_lock.lock().await;
        let mut errors = Vec::new();
        if let Err(e) = self.drop_closed_accounts().await {
            errors.push(format!("accounts: {e}"));
        }
        let offers = self.ensure_offers(&mut errors).await;
        let gateways = store::list_gateways(&self.pool).await.unwrap_or_default();
        self.gateway_health(&gateways, &mut errors).await;
        self.lightning_files(&gateways, &mut errors).await;
        self.lightning_check(&gateways).await;
        let domains = store::list_domains(&self.pool).await.unwrap_or_default();
        let addresses = store::addresses(&self.pool, None).await.unwrap_or_default();
        let gw: HashMap<i64, &Gateway> = gateways.iter().map(|g| (g.id, g)).collect();
        let mut a_ok = HashMap::new();
        let mut health = HashMap::new();
        let mut ds = HashMap::new();
        let mut provider_errors: HashMap<i64, Option<String>> = HashMap::new();
        for d in &domains {
            let mine: Vec<&Address> = addresses.iter().filter(|a| a.domain_id == d.id).collect();
            let ipv4 = d.gateway_id.and_then(|g| gw.get(&g)).and_then(|g| g.ipv4.clone());
            if let Some(p) = d.dns_provider_id {
                let err = self.dns_sync(d, &mine, &offers, ipv4.as_deref()).await;
                if let Some(e) = &err {
                    errors.push(format!("{}: {e}", d.name));
                }
                let entry = provider_errors.entry(p).or_insert(None);
                if entry.is_none() {
                    *entry = err;
                }
            }
            for a in mine.iter().filter(|a| a.bip353) {
                let txt = a.offer_id.as_ref().and_then(|o| offers.get(o));
                let Some(txt) = txt.map(|o| format!("bitcoin:?lno={o}")) else { continue };
                let Some(values) = twelve::lookup(&a.username, &d.name).await else { continue };
                let ok = values.iter().any(|v| v.eq_ignore_ascii_case(&txt));
                if ok != a.verified {
                    let _ = store::set_verified(&self.pool, a.id, ok).await;
                }
            }
            if let Some(ip) = &ipv4 {
                a_ok.insert(d.id, resolves_to(&d.name, ip).await);
            }
            let (h, d_ds) = self.domain_health(d).await;
            health.insert(d.id, h);
            if !d_ds.is_empty() {
                ds.insert(d.id, d_ds);
            }
            self.check_reach(d).await;
        }
        for (p, e) in provider_errors {
            let _ = store::set_provider_status(&self.pool, p, e.as_deref()).await;
        }
        // Done with the quick checks once everything is live.
        let done = store::addresses(&self.pool, None)
            .await
            .map(|l| l.iter().all(|a| a.verified))
            .unwrap_or(false);
        if done && a_ok.values().all(|ok| *ok) {
            let mut c = self.changed_since.lock().unwrap();
            if c.is_some_and(|c| now() - c > QUICK_CHECKS[QUICK_CHECKS.len() - 1]) {
                *c = None;
            }
        }
        for e in &errors {
            tracing::warn!(error = %e, "domains");
        }
        let mut st = self.status.lock().unwrap();
        st.a_ok = a_ok;
        st.health = health;
        st.ds = ds;
        st.errors = errors;
        st.synced_at = Some(now());
    }

    /// Addresses of closed accounts go away with their offers.
    async fn drop_closed_accounts(&self) -> Result<(), super::DomainError> {
        let closed: Vec<(i64,)> = sqlx::query_as("SELECT id FROM apps WHERE closed_at IS NOT NULL")
            .fetch_all(&self.pool)
            .await?;
        if closed.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = closed.into_iter().map(|(i,)| i).collect();
        for a in store::addresses(&self.pool, None).await? {
            if a.app_id.is_some_and(|i| ids.contains(&i)) {
                self.disable_offer(&a).await;
                sqlx::query("DELETE FROM ln_addresses WHERE id = $1")
                    .bind(a.id)
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    /// Every address gets its own active offer; returns offer id → offer string.
    async fn ensure_offers(&self, errors: &mut Vec<String>) -> HashMap<String, String> {
        let Some(wallet) = self.wallet.get() else { return HashMap::new() };
        let Ok(all) = wallet.offers(None).await else { return HashMap::new() };
        let active: HashMap<String, String> = all
            .iter()
            .filter(|o| o.state == "active")
            .map(|o| (o.offer_id.clone(), o.offer.clone()))
            .collect();
        let mut out = active.clone();
        let (Ok(addresses), Ok(domains)) =
            (store::addresses(&self.pool, None).await, store::list_domains(&self.pool).await)
        else {
            return out;
        };
        let names: HashMap<i64, String> = domains.into_iter().map(|d| (d.id, d.name)).collect();
        for a in addresses {
            if a.offer_id.as_ref().is_some_and(|o| active.contains_key(o)) {
                continue;
            }
            let Some(domain) = names.get(&a.domain_id) else { continue };
            let address = format!("{}@{domain}", a.username);
            match crate::public_name::name_offer(
                wallet,
                a.app_id,
                &address,
                name_description(&address),
            )
            .await
            {
                Ok(o) => {
                    let _ = store::set_offer(&self.pool, a.id, &o.offer_id).await;
                    out.insert(o.offer_id, o.offer);
                }
                Err(e) => errors.push(format!("no BOLT12 offer for {address}: {e}")),
            }
        }
        out
    }

    /// Brings one domain's records at its provider in line; returns the error, if any.
    async fn dns_sync(
        &self,
        d: &Domain,
        addresses: &[&Address],
        offers: &HashMap<String, String>,
        ipv4: Option<&str>,
    ) -> Option<String> {
        let p = d.dns_provider_id?;
        let token = match self.secrets.read(&provider_secret(p)) {
            Ok(Some(t)) => t,
            _ => return Some("the API token is missing; enter it again".into()),
        };
        let kind = self.provider_kind(p).await;
        let (zone, sub) = match self.dns_zone(&kind, &token, &d.name).await {
            Ok(z) => z,
            Err(e) => {
                let _ = store::set_sync(&self.pool, d.id, &d.managed, Some(&e)).await;
                return Some(e);
            }
        };
        let mut want: Want = BTreeMap::new();
        for a in addresses.iter().filter(|a| a.bip353) {
            let Some(offer) = a.offer_id.as_ref().and_then(|o| offers.get(o)) else { continue };
            let name = njalla::relative(&bip353_subname(&a.username), &sub);
            want.insert(
                format!("TXT {name}@{zone}"),
                (zone.clone(), "TXT".into(), name, format!("bitcoin:?lno={offer}")),
            );
        }
        if let Some(ip) = ipv4 {
            want.insert(
                format!("A {sub}@{zone}"),
                (zone.clone(), "A".into(), sub.clone(), ip.to_string()),
            );
        }
        // Moved to a gateway that has no IP yet (an LNVPS server being bought): keep the
        // A record there is until the new one can replace it.
        if ipv4.is_none() && d.gateway_id.is_some() {
            let key = format!("A {sub}@{zone}");
            if let Some(m) = d.managed.get(&key) {
                want.insert(key, (m.zone.clone(), "A".into(), m.name.clone(), m.content.clone()));
            }
        }
        let mut managed = d.managed.clone();
        let mut errors = Vec::new();
        // Remove what is no longer wanted, or changed.
        let stale: Vec<(String, ManagedRecord)> = managed
            .iter()
            .filter(|(k, m)| want.get(*k).is_none_or(|w| w.3 != m.content))
            .map(|(k, m)| (k.clone(), m.clone()))
            .collect();
        for (k, m) in stale {
            match self.dns_remove(&kind, &token, &m.zone, &m.id).await {
                Ok(()) => {
                    managed.remove(&k);
                }
                Err(e) => errors.push(format!("removing {} {}: {e}", m.kind, m.name)),
            }
        }
        // Add what is missing (adopting identical records already there).
        let missing: Vec<(String, (String, String, String, String))> =
            want.into_iter().filter(|(k, _)| !managed.contains_key(k)).collect();
        let mut existing: Option<Vec<njalla::DnsRecord>> = None;
        for (key, (zone, rtype, name, content)) in missing {
            if existing.is_none() {
                match self.dns_list(&kind, &token, &zone).await {
                    Ok(l) => existing = Some(l),
                    Err(e) => {
                        errors.push(format!("{zone}: {e}"));
                        break;
                    }
                }
            }
            let list = existing.clone().unwrap_or_default();
            if let Some(same) =
                list.iter().find(|r| r.kind == rtype && r.name == name && r.content == content)
            {
                managed.insert(
                    key,
                    ManagedRecord { id: same.id.clone(), zone, kind: rtype, name, content },
                );
                continue;
            }
            // Allod's A record replaces other A records on that name.
            if rtype == "A" {
                for old in list.iter().filter(|r| r.kind == "A" && r.name == name) {
                    if let Err(e) = self.dns_remove(&kind, &token, &zone, &old.id).await {
                        errors.push(format!("replacing A {name}: {e}"));
                    }
                }
            }
            match self.dns_add(&kind, &token, &zone, &rtype, &name, &content, 3600).await {
                Ok(id) => {
                    managed.insert(key, ManagedRecord { id, zone, kind: rtype, name, content });
                }
                Err(e) => errors.push(format!("adding {rtype} {name}: {e}")),
            }
        }
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        let _ = store::set_sync(&self.pool, d.id, &managed, error.as_deref()).await;
        error
    }

    /// The domain itself, whatever addresses it has: its zone answers and is signed
    /// with DNSSEC, and its DNS provider (if any) accepts the token for it.
    /// Also the DS values the provider gives for the registrar (deSEC).
    async fn domain_health(&self, d: &Domain) -> (super::Health, Vec<String>) {
        let zone = crate::wallet::bip353::zone_health(&d.name, self.cfg.dns_resolver).await;
        let mut ds = Vec::new();
        let provider = match d.dns_provider_id {
            None => None,
            Some(p) => Some(match self.secrets.read(&provider_secret(p)) {
                Ok(Some(token)) => {
                    let kind = self.provider_kind(p).await;
                    match self.dns_zone(&kind, &token, &d.name).await {
                        Ok((z, _)) => {
                            let ok = self.dns_list(&kind, &token, &z).await.is_ok();
                            if ok {
                                ds = self.dns_ds(&kind, &token, &z).await;
                            }
                            ok
                        }
                        Err(_) => false,
                    }
                }
                _ => false,
            }),
        };
        (super::Health { dns: zone.dns, dnssec: zone.dnssec, provider }, ds)
    }

    /// For served domains: does `https://<domain>/.well-known/allod-reach` answer? Caddy
    /// answers it itself on address domains, which pass only LNURL on to Allod.
    async fn check_reach(&self, d: &Domain) {
        if !d.served() {
            if d.reach_state.is_some() {
                let _ = store::set_reach(&self.pool, d.id, None, None).await;
            }
            return;
        }
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
        if let Some(addr) = &self.cfg.tls_check_addr {
            if let Ok(Some(a)) = tokio::net::lookup_host(addr.as_str()).await.map(|mut a| a.next())
            {
                builder = builder.resolve(&d.name, a);
            }
        }
        let result = match builder.build() {
            Ok(c) => c
                .get(format!("https://{}/.well-known/allod-reach", d.name))
                .send()
                .await
                .map(|r| r.status()),
            Err(e) => Err(e),
        };
        let (state, detail) = match result {
            Ok(s) if s.is_success() => ("reachable", None),
            other => {
                let detail = match other {
                    Ok(s) => format!("answered {s}"),
                    Err(e) => crate::public_name::error_chain(&e),
                };
                let state = if now() - d.changed_at < REACH_GRACE_SECS {
                    "waiting_for_certificate"
                } else {
                    "not_reachable"
                };
                (state, Some(detail))
            }
        };
        let _ = store::set_reach(&self.pool, d.id, Some(state), detail.as_deref()).await;
    }
}

/// Whether `name` points at `ipv4`, asked from its zone's own servers (a recursive
/// resolver may still remember an older answer).
async fn resolves_to(name: &str, ipv4: &str) -> bool {
    crate::wallet::bip353::authoritative_a(name)
        .await
        .is_some_and(|a| a.iter().any(|ip| ip == ipv4))
}
