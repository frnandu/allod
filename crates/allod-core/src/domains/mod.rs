//! Domains, DNS providers, gateways and Lightning addresses (SPEC §12.1, §12.7, §12.8).
//!
//! - A **domain** is the owner's DNS name. Its addresses (`username@domain`) answer
//!   BIP-353 through TXT records, and LNURL when the domain is served over HTTPS: through
//!   a gateway (home IP hidden) or directly from this machine.
//! - A **DNS provider** (Njalla or deSEC) lets Allod keep a domain's records itself; without one
//!   the owner adds them by hand.
//! - A **gateway** is a small public server that passes HTTPS through to this node over
//!   WireGuard, without decrypting it.
//!
//! State lives in tables (migration 0013); API tokens and keys live in secret files.

pub mod desec;
mod desec_signup;
mod dns;
pub mod gateway;
pub mod lnurl;
pub mod lnvps;
pub mod mailbox;
pub mod nip05;
pub mod njalla;
mod njalla_buy;
mod njalla_signup;
pub mod prism;
pub mod ssh;
mod store;
mod sync;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use allod_db::Db;
use allod_keys::SecretStore;
use serde_json::{json, Value};
use sqlx::AnyPool;
use tokio::sync::{Notify, OnceCell};

use crate::public_name::{bip353_subname, name_description, normalize_domain, url_domain};
use crate::{AppService, Wallet};

pub use store::{Address, DnsProvider, Domain, Gateway, ManagedRecord};

/// Seconds after a change at which records and reachability are checked again; later,
/// the 5-minute reconcile checks them.
const QUICK_CHECKS: [i64; 4] = [10, 30, 120, 300];
/// After a change, how long the certificate may take before "not reachable".
pub(crate) const REACH_GRACE_SECS: i64 = 600;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
}

impl From<sqlx::Error> for DomainError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                DomainError::Conflict("it already exists".into())
            }
            e => DomainError::Internal(e.to_string()),
        }
    }
}

impl From<std::io::Error> for DomainError {
    fn from(e: std::io::Error) -> Self {
        DomainError::Internal(e.to_string())
    }
}

impl From<allod_db::DbError> for DomainError {
    fn from(e: allod_db::DbError) -> Self {
        DomainError::Internal(e.to_string())
    }
}

/// Process settings.
#[derive(Debug, Clone, Default)]
pub struct DomainConfig {
    /// `ALLOD_PUBLIC_URL`: its domain is a fixed, directly served domain.
    pub env_public_url: Option<String>,
    /// `ALLOD_TLS_CHECK_ADDR` (`caddy:443`): where to test HTTPS from inside the stack.
    pub tls_check_addr: Option<String>,
    /// Njalla API URL; `None`: `https://njal.la/api/1/`.
    pub njalla_url: Option<String>,
    /// deSEC API URL; `None`: `https://desec.io/api/v1/`.
    pub desec_url: Option<String>,
    /// LNURL callbacks over plain http (regtest and tests).
    pub lnurl_http: bool,
    /// The volume shared with the `tunnel` service (`wg0.conf`, `status.json`); `None`:
    /// gateways cannot be reached (no tunnel service).
    pub tunnel_dir: Option<PathBuf>,
    /// LNVPS API base URL; `None`: `https://api.lnvps.net`.
    pub lnvps_url: Option<String>,
    /// The DNSSEC resolver (`ALLOD_DNS_RESOLVER`): asked for DS records, since the system
    /// stub may drop them.
    pub dns_resolver: Option<std::net::SocketAddr>,
}

/// What a domain's own checks found (SPEC §12.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Health {
    /// Its zone's name servers answer.
    pub dns: bool,
    /// The zone is signed (a DS record at its parent).
    pub dnssec: bool,
    /// The DNS provider accepts the token for it; `None`: managed by hand.
    pub provider: Option<bool>,
}

/// Secret file names.
pub(crate) fn provider_secret(id: i64) -> String {
    format!("dns_provider_{id}")
}

/// What `reconcile` learned that is not stored: per domain, whether its A record points
/// where it should; per gateway, the tunnel's last handshake.
#[derive(Debug, Default)]
struct Status {
    a_ok: HashMap<i64, bool>,
    /// Per domain: its own checks, whatever addresses it has.
    health: HashMap<i64, Health>,
    /// Per domain: the DS values its provider gives for the registrar (deSEC).
    ds: HashMap<i64, Vec<String>>,
    errors: Vec<String>,
    synced_at: Option<i64>,
}

pub struct DomainService {
    pool: AnyPool,
    db: Db,
    secrets: SecretStore,
    njalla: njalla::Njalla,
    desec: desec::Desec,
    lnvps: lnvps::Lnvps,
    cfg: DomainConfig,
    wallet: Arc<OnceCell<Arc<Wallet>>>,
    apps: Arc<OnceCell<Arc<AppService>>>,
    status: Mutex<Status>,
    run_lock: tokio::sync::Mutex<()>,
    wake: Notify,
    /// The prism worker should look at the queue now (a retry was asked for).
    prism_wake: Notify,
    /// Since when something changed that the quick checks should follow.
    changed_since: Mutex<Option<i64>>,
    last_run: Mutex<Option<Instant>>,
    /// Held while an LNVPS gateway is bought or renewed.
    buying: tokio::sync::Mutex<()>,
    /// An LNVPS gateway is being bought (or renewed): checked every few seconds.
    lnvps_active: std::sync::atomic::AtomicBool,
    /// A gateway with an address has not connected yet: its tunnel is checked every few
    /// seconds, so it shows as active soon after its first handshake.
    gateway_waiting: std::sync::atomic::AtomicBool,
    /// A deSEC account waits for its activation mail.
    desec_waiting: std::sync::atomic::AtomicBool,
    /// Tests: the mailbox's messages, instead of Nostr relays.
    #[cfg(test)]
    test_mail: Mutex<Option<Vec<String>>>,
    /// Exchange rates, for caps in a fiat currency.
    fiat: OnceCell<Arc<crate::FiatService>>,
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `main` or the account id.
fn target_of(app_id: Option<i64>) -> String {
    app_id.map(|i| i.to_string()).unwrap_or_else(|| "main".into())
}

/// Parses `main` or an account id.
pub fn parse_target(t: &str) -> Result<Option<i64>, DomainError> {
    match t.trim() {
        "" | "main" => Ok(None),
        id => id
            .parse()
            .map(Some)
            .map_err(|_| DomainError::BadRequest("target is main or an account id".into())),
    }
}

impl DomainService {
    /// Loads the service; adds the `ALLOD_PUBLIC_URL` domain and moves the single public
    /// name of older versions into the tables (SPEC §12.1).
    #[allow(clippy::too_many_arguments)]
    pub async fn load(
        db: Db,
        pool: AnyPool,
        secrets: SecretStore,
        cfg: DomainConfig,
        wallet: Arc<OnceCell<Arc<Wallet>>>,
        apps: Arc<OnceCell<Arc<AppService>>>,
    ) -> Result<Arc<Self>, DomainError> {
        let svc = Arc::new(DomainService {
            njalla: njalla::Njalla::new(cfg.njalla_url.as_deref().unwrap_or(njalla::DEFAULT_URL)),
            desec: desec::Desec::new(cfg.desec_url.as_deref().unwrap_or(desec::DEFAULT_URL)),
            lnvps: lnvps::Lnvps::new(cfg.lnvps_url.as_deref().unwrap_or(lnvps::DEFAULT_URL)),
            pool,
            db,
            secrets,
            cfg,
            wallet,
            apps,
            status: Mutex::new(Status::default()),
            run_lock: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            prism_wake: Notify::new(),
            changed_since: Mutex::new(None),
            last_run: Mutex::new(None),
            buying: tokio::sync::Mutex::new(()),
            lnvps_active: std::sync::atomic::AtomicBool::new(false),
            gateway_waiting: std::sync::atomic::AtomicBool::new(false),
            desec_waiting: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            test_mail: Mutex::new(None),
            fiat: OnceCell::new(),
        });
        svc.migrate_legacy().await?;
        svc.ensure_env_domain().await?;
        Ok(svc)
    }

    /// Exchange rates for fiat caps (LNVPS).
    pub fn set_fiat(&self, fiat: Arc<crate::FiatService>) {
        let _ = self.fiat.set(fiat);
    }

    /// Asks the background task to reconcile soon, with the quick checks after it.
    pub fn trigger(&self) {
        *self.changed_since.lock().unwrap() = Some(now());
        self.wake.notify_one();
    }

    /// Asks the prism worker to look at its queue now (after "Retry now").
    pub fn wake_prism(&self) {
        self.prism_wake.notify_one();
    }

    // ------------------------------------------------------------- legacy, env

    async fn ensure_env_domain(&self) -> Result<(), DomainError> {
        let Some(name) = self.cfg.env_public_url.as_deref().and_then(url_domain) else {
            return Ok(());
        };
        if let Some(d) = store::domain_by_name(&self.pool, &name).await? {
            if !d.env {
                sqlx::query("UPDATE domains SET env = TRUE, direct = TRUE WHERE id = $1")
                    .bind(d.id)
                    .execute(&self.pool)
                    .await?;
            }
            return Ok(());
        }
        store::insert_domain(&self.pool, &name, None, None, true, true).await?;
        Ok(())
    }

    /// Moves `config.public_name`'s own domain (mode, name, exposure, Njalla token, A
    /// record, usernames) into the tables, once.
    async fn migrate_legacy(&self) -> Result<(), DomainError> {
        let Some(entry) = self.db.config_get(crate::public_name::CONFIG_KEY).await? else {
            return Ok(());
        };
        let Ok(mut cfg) = serde_json::from_str::<Value>(&entry.value) else { return Ok(()) };
        let legacy = ["mode", "name", "exposure", "offers", "main_username", "main_offer", "dns"];
        if !legacy.iter().any(|k| cfg.get(k).is_some()) {
            return Ok(());
        }
        let own = cfg.get("mode").and_then(Value::as_str) == Some("own");
        let name = cfg.get("name").and_then(Value::as_str).map(str::to_string);
        if let (true, Some(name), true) =
            (own, name, store::list_domains(&self.pool).await?.is_empty())
        {
            let direct = cfg.get("exposure").and_then(Value::as_str) == Some("public");
            let dns = cfg.get("dns").cloned().unwrap_or(Value::Null);
            // The Njalla token becomes provider 1.
            let mut provider = None;
            if dns.get("provider").and_then(Value::as_str) == Some("njalla") {
                if let Ok(Some(token)) = self.secrets.read("dns_token") {
                    let id = store::insert_provider(&self.pool, "njalla", "Njalla").await?;
                    self.secrets.write(&provider_secret(id), &token)?;
                    let _ = self.secrets.remove("dns_token");
                    provider = Some(id);
                }
            }
            // An A record kept for a relay becomes a manual gateway (shared: the relay
            // routed by name).
            let mut gateway = None;
            if let Some(ip) = dns.get("a_record").and_then(Value::as_str) {
                let id =
                    store::insert_gateway(&self.pool, "Gateway", "manual", Some(ip), "shared", 1)
                        .await?;
                gateway = Some(id);
            }
            let domain =
                store::insert_domain(&self.pool, &name, provider, gateway, direct, false).await?;
            if let Some(m) = dns.get("managed") {
                sqlx::query("UPDATE domains SET managed = $1 WHERE id = $2")
                    .bind(m.to_string())
                    .bind(domain)
                    .execute(&self.pool)
                    .await?;
            }
            if let Some(u) = cfg.get("main_username").and_then(Value::as_str) {
                let _ = store::insert_address(&self.pool, domain, u, None).await;
            }
            let rows = sqlx::query_as::<_, (i64, String)>(
                "SELECT id, lud16_username FROM apps WHERE kind = 'account' \
                 AND closed_at IS NULL AND lud16_username IS NOT NULL",
            )
            .fetch_all(&self.pool)
            .await?;
            for (app, u) in rows {
                let _ = store::insert_address(&self.pool, domain, &u, Some(app)).await;
            }
            tracing::info!(domain = %name, "domains: moved the public name into the domain list");
        }
        sqlx::query("UPDATE apps SET lud16_username = NULL WHERE lud16_username IS NOT NULL")
            .execute(&self.pool)
            .await?;
        if let Some(o) = cfg.as_object_mut() {
            for k in legacy.iter().chain(["changed_at"].iter()) {
                o.remove(*k);
            }
        }
        self.db.config_set(crate::public_name::CONFIG_KEY, &cfg.to_string(), false).await?;
        Ok(())
    }

    // ------------------------------------------------------------- DNS providers

    pub async fn add_provider(
        &self,
        kind: &str,
        label: Option<&str>,
        token: &str,
    ) -> Result<i64, DomainError> {
        if !dns::KINDS.contains(&kind) {
            return Err(DomainError::BadRequest(
                "the DNS provider kind must be njalla or desec".into(),
            ));
        }
        let token = token.trim();
        if token.is_empty() || token.contains(char::is_whitespace) {
            return Err(DomainError::BadRequest("paste the API token".into()));
        }
        self.dns_check(kind, token).await.map_err(DomainError::BadRequest)?;
        let label = label.map(str::trim).filter(|l| !l.is_empty()).unwrap_or(dns::kind_label(kind));
        let id = store::insert_provider(&self.pool, kind, label).await?;
        self.secrets.write(&provider_secret(id), token)?;
        Ok(id)
    }

    pub async fn update_provider(
        &self,
        id: i64,
        label: Option<&str>,
        token: Option<&str>,
    ) -> Result<(), DomainError> {
        let p = store::provider(&self.pool, id).await?.ok_or_else(|| not_found("DNS provider"))?;
        if let Some(t) = token.map(str::trim).filter(|t| !t.is_empty()) {
            self.dns_check(&p.kind, t).await.map_err(DomainError::BadRequest)?;
            self.secrets.write(&provider_secret(id), t)?;
            sqlx::query("UPDATE dns_providers SET error = NULL WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(l) = label.map(str::trim).filter(|l| !l.is_empty()) {
            sqlx::query("UPDATE dns_providers SET label = $1 WHERE id = $2")
                .bind(l)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        self.trigger();
        Ok(())
    }

    pub async fn remove_provider(&self, id: i64) -> Result<(), DomainError> {
        store::provider(&self.pool, id).await?.ok_or_else(|| not_found("DNS provider"))?;
        let used = store::list_domains(&self.pool)
            .await?
            .into_iter()
            .filter(|d| d.dns_provider_id == Some(id))
            .map(|d| d.name)
            .collect::<Vec<_>>();
        if !used.is_empty() {
            return Err(DomainError::Conflict(format!(
                "domains still use it: {}; move them to another provider or to manual first",
                used.join(", ")
            )));
        }
        sqlx::query("DELETE FROM dns_providers WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.secrets.remove(&provider_secret(id))?;
        self.signups_provider_removed(id).await;
        Ok(())
    }

    // ------------------------------------------------------------- domains

    /// Adds a domain. `gateway_id` and `direct` exclude each other.
    pub async fn add_domain(
        &self,
        name: &str,
        dns_provider_id: Option<i64>,
        gateway_id: Option<i64>,
        direct: bool,
    ) -> Result<i64, DomainError> {
        let name = normalize_domain(name).map_err(|e| DomainError::BadRequest(e.to_string()))?;
        self.check_serving(dns_provider_id, gateway_id, direct).await?;
        if store::domain_by_name(&self.pool, &name).await?.is_some() {
            return Err(DomainError::Conflict(format!("{name} is already in the list")));
        }
        let id =
            store::insert_domain(&self.pool, &name, dns_provider_id, gateway_id, direct, false)
                .await?;
        self.trigger();
        Ok(id)
    }

    async fn check_serving(
        &self,
        dns_provider_id: Option<i64>,
        gateway_id: Option<i64>,
        direct: bool,
    ) -> Result<(), DomainError> {
        if gateway_id.is_some() && direct {
            return Err(DomainError::BadRequest(
                "serve the domain through a gateway or directly, not both".into(),
            ));
        }
        if let Some(p) = dns_provider_id {
            store::provider(&self.pool, p).await?.ok_or_else(|| not_found("DNS provider"))?;
        }
        if let Some(g) = gateway_id {
            store::gateway(&self.pool, g).await?.ok_or_else(|| not_found("gateway"))?;
        }
        Ok(())
    }

    /// Changes a domain's provider or how it is served (`Some(None)` clears).
    pub async fn update_domain(
        &self,
        id: i64,
        dns_provider_id: Option<Option<i64>>,
        gateway_id: Option<Option<i64>>,
        direct: Option<bool>,
    ) -> Result<(), DomainError> {
        let _g = self.run_lock.lock().await;
        let d = store::domain(&self.pool, id).await?.ok_or_else(|| not_found("domain"))?;
        if d.env {
            return Err(DomainError::Conflict("set by ALLOD_PUBLIC_URL; change it there".into()));
        }
        let provider = dns_provider_id.unwrap_or(d.dns_provider_id);
        let mut gateway = gateway_id.unwrap_or(d.gateway_id);
        let mut direct = direct.unwrap_or(d.direct);
        // Choosing one way of serving clears the other.
        if gateway_id.flatten().is_some() {
            direct = false;
        } else if direct && !d.direct {
            gateway = None;
        }
        self.check_serving(provider, gateway, direct).await?;
        // Leaving a provider: remove Allod's records there first.
        if provider != d.dns_provider_id && d.dns_provider_id.is_some() {
            self.remove_managed(&d).await;
        }
        let serving_changed = gateway != d.gateway_id || direct != d.direct;
        sqlx::query(
            "UPDATE domains SET dns_provider_id = $1, gateway_id = $2, direct = $3, \
             changed_at = CASE WHEN $4 THEN $5 ELSE changed_at END, \
             reach_state = CASE WHEN $4 THEN NULL ELSE reach_state END, \
             managed = CASE WHEN $6 THEN '{}' ELSE managed END, error = NULL WHERE id = $7",
        )
        .bind(provider)
        .bind(gateway)
        .bind(direct)
        .bind(serving_changed)
        .bind(now())
        .bind(provider != d.dns_provider_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        drop(_g);
        self.trigger();
        Ok(())
    }

    /// Removes a domain, its addresses (their offers are disabled) and Allod's records.
    pub async fn remove_domain(&self, id: i64) -> Result<(), DomainError> {
        let _g = self.run_lock.lock().await;
        let d = store::domain(&self.pool, id).await?.ok_or_else(|| not_found("domain"))?;
        if d.env {
            return Err(DomainError::Conflict("set by ALLOD_PUBLIC_URL; remove it there".into()));
        }
        self.remove_managed(&d).await;
        for a in store::addresses(&self.pool, Some(id)).await? {
            self.disable_offer(&a).await;
        }
        sqlx::query("DELETE FROM ln_addresses WHERE domain_id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM domains WHERE id = $1").bind(id).execute(&self.pool).await?;
        drop(_g);
        self.trigger();
        Ok(())
    }

    /// Removes the records Allod created at a domain's provider (best effort).
    async fn remove_managed(&self, d: &Domain) {
        let Some(p) = d.dns_provider_id else { return };
        let Ok(Some(token)) = self.secrets.read(&provider_secret(p)) else { return };
        let kind = self.provider_kind(p).await;
        for r in d.managed.values() {
            if let Err(e) = self.dns_remove(&kind, &token, &r.zone, &r.id).await {
                tracing::warn!(error = %e, name = %r.name, "DNS: could not remove a record");
            }
        }
    }

    /// The domain of new account addresses: the first one.
    pub async fn first_domain(&self) -> Option<Domain> {
        store::list_domains(&self.pool).await.ok()?.into_iter().next()
    }

    // ------------------------------------------------------------- addresses

    pub async fn address_by_id(&self, id: i64) -> Result<Address, DomainError> {
        store::address(&self.pool, id).await?.ok_or_else(|| not_found("address"))
    }

    /// Adds `username@domain` for Main (`None`) or an open account, with its offer.
    pub async fn add_address(
        &self,
        domain_id: i64,
        username: &str,
        target: Option<i64>,
    ) -> Result<Address, DomainError> {
        let username = crate::apps::validate_username(username.trim())
            .map_err(|e| DomainError::BadRequest(e.to_string()))?;
        let d = store::domain(&self.pool, domain_id).await?.ok_or_else(|| not_found("domain"))?;
        if let Some(app) = target {
            let apps = self.apps()?;
            let a = apps.get(app).await.map_err(|_| not_found("account"))?;
            if a.kind != "account" || a.closed_at.is_some() {
                return Err(DomainError::BadRequest("not an open account".into()));
            }
        }
        let id =
            store::insert_address(&self.pool, domain_id, &username, target).await.map_err(|e| {
                match e {
                    DomainError::Conflict(_) => {
                        DomainError::Conflict(format!("{username}@{} is taken", d.name))
                    }
                    e => e,
                }
            })?;
        // The offer now, so the record can be shown at once.
        if let Some(wallet) = self.wallet.get() {
            let address = format!("{username}@{}", d.name);
            match crate::public_name::name_offer(
                wallet,
                target,
                &address,
                name_description(&address),
            )
            .await
            {
                Ok(o) => store::set_offer(&self.pool, id, &o.offer_id).await?,
                Err(e) => tracing::warn!(address = %address, error = %e, "domains: no offer yet"),
            }
        }
        self.trigger();
        store::address(&self.pool, id).await?.ok_or_else(|| not_found("address"))
    }

    /// Whether `username` is free on a domain (account creation checks first).
    pub async fn username_free(&self, domain_id: i64, username: &str) -> bool {
        store::addresses(&self.pool, Some(domain_id))
            .await
            .map(|l| l.iter().all(|a| a.username != username))
            .unwrap_or(false)
    }

    /// What an address answers: LNURL (when its domain is served), a BIP-353 record, or
    /// both. At least one stays on.
    pub async fn set_address_kinds(
        &self,
        id: i64,
        lnurl: Option<bool>,
        bip353: Option<bool>,
    ) -> Result<(), DomainError> {
        let a = store::address(&self.pool, id).await?.ok_or_else(|| not_found("address"))?;
        let (lnurl, bip353) = (lnurl.unwrap_or(a.lnurl), bip353.unwrap_or(a.bip353));
        if !lnurl && !bip353 {
            return Err(DomainError::BadRequest("an address needs LNURL, BIP-353 or both".into()));
        }
        sqlx::query("UPDATE ln_addresses SET lnurl = $1, bip353 = $2 WHERE id = $3")
            .bind(lnurl)
            .bind(bip353)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.trigger();
        Ok(())
    }

    /// LNURL on or off (BIP-353 unchanged).
    pub async fn set_address_lnurl(&self, id: i64, lnurl: bool) -> Result<(), DomainError> {
        self.set_address_kinds(id, Some(lnurl), None).await
    }

    /// Change an own-domain address's name. Rotate its offer because its description
    /// contains the address; reconciliation removes the old DNS record.
    pub async fn rename_address(
        &self,
        id: i64,
        domain_id: i64,
        username: &str,
    ) -> Result<(), DomainError> {
        let username = crate::apps::validate_username(username.trim())
            .map_err(|e| DomainError::BadRequest(e.to_string()))?;
        let domain =
            store::domain(&self.pool, domain_id).await?.ok_or_else(|| not_found("domain"))?;
        let _g = self.run_lock.lock().await;
        let old = store::address(&self.pool, id).await?.ok_or_else(|| not_found("address"))?;
        if old.domain_id == domain_id && old.username == username {
            return Ok(());
        }
        let changed = sqlx::query(
            "UPDATE ln_addresses SET domain_id = $1, username = $2, offer_id = NULL, verified = FALSE WHERE id = $3",
        )
        .bind(domain_id)
        .bind(&username)
        .bind(id)
        .execute(&self.pool)
        .await;
        match changed {
            Ok(_) => {}
            Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
                return Err(DomainError::Conflict(format!("{username}@{} is taken", domain.name)));
            }
            Err(e) => return Err(e.into()),
        }
        self.disable_offer(&old).await;
        if let Some(wallet) = self.wallet.get() {
            let address = format!("{username}@{}", domain.name);
            match crate::public_name::name_offer(
                wallet,
                old.app_id,
                &address,
                name_description(&address),
            )
            .await
            {
                Ok(o) => store::set_offer(&self.pool, id, &o.offer_id).await?,
                Err(e) => tracing::warn!(address = %address, error = %e, "domains: no offer yet"),
            }
        }
        drop(_g);
        self.trigger();
        Ok(())
    }

    /// Points an address at another account (or Main, `None`). Its offer belongs to the
    /// account it pays, so the old one is disabled and a new one, owned by the new
    /// account, takes its place in the TXT record.
    pub async fn set_address_target(
        &self,
        id: i64,
        target: Option<i64>,
    ) -> Result<(), DomainError> {
        let a = store::address(&self.pool, id).await?.ok_or_else(|| not_found("address"))?;
        if a.app_id == target {
            return Ok(());
        }
        if let Some(app) = target {
            let apps = self.apps()?;
            let acct = apps.get(app).await.map_err(|_| not_found("account"))?;
            if acct.kind != "account" || acct.closed_at.is_some() {
                return Err(DomainError::BadRequest("not an open account".into()));
            }
        }
        let d = store::domain(&self.pool, a.domain_id).await?.ok_or_else(|| not_found("domain"))?;
        let _g = self.run_lock.lock().await;
        self.disable_offer(&a).await;
        sqlx::query(
            "UPDATE ln_addresses SET app_id = $1, offer_id = NULL, verified = FALSE WHERE id = $2",
        )
        .bind(target)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if let Some(wallet) = self.wallet.get() {
            let address = format!("{}@{}", a.username, d.name);
            match crate::public_name::name_offer(
                wallet,
                target,
                &address,
                name_description(&address),
            )
            .await
            {
                Ok(o) => store::set_offer(&self.pool, id, &o.offer_id).await?,
                Err(e) => tracing::warn!(address = %address, error = %e, "domains: no offer yet"),
            }
        }
        drop(_g);
        self.trigger();
        Ok(())
    }

    pub async fn remove_address(&self, id: i64) -> Result<(), DomainError> {
        let a = store::address(&self.pool, id).await?.ok_or_else(|| not_found("address"))?;
        self.disable_offer(&a).await;
        sqlx::query("DELETE FROM ln_addresses WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.trigger();
        Ok(())
    }

    async fn disable_offer(&self, a: &Address) {
        if let (Some(w), Some(o)) = (self.wallet.get(), a.offer_id.as_deref()) {
            let _ = w.set_offer_state(o, "disabled").await;
        }
    }

    /// The addresses of an account (or Main), e.g. for the account page.
    pub async fn addresses_of(&self, target: Option<i64>) -> Vec<(Address, String)> {
        let (Ok(all), Ok(domains)) =
            (store::addresses(&self.pool, None).await, store::list_domains(&self.pool).await)
        else {
            return Vec::new();
        };
        let names: HashMap<i64, String> = domains.into_iter().map(|d| (d.id, d.name)).collect();
        all.into_iter()
            .filter(|a| a.app_id == target)
            .filter_map(|a| names.get(&a.domain_id).cloned().map(|n| (a, n)))
            .collect()
    }

    fn apps(&self) -> Result<&Arc<AppService>, DomainError> {
        self.apps.get().ok_or_else(|| DomainError::Conflict("the wallet is not running yet".into()))
    }

    // ------------------------------------------------------------- TLS

    /// Caddy's on-demand TLS check: only served domains.
    pub async fn allows_certificate(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        matches!(store::domain_by_name(&self.pool, &name).await, Ok(Some(d)) if d.served())
    }

    // ------------------------------------------------------------- view

    /// Everything the Settings pages show.
    pub async fn view(&self) -> Result<Value, DomainError> {
        let domains = store::list_domains(&self.pool).await?;
        let providers = store::list_providers(&self.pool).await?;
        let gateways = store::list_gateways(&self.pool).await?;
        let addresses = store::addresses(&self.pool, None).await?;
        let offers: HashMap<String, String> = match self.wallet.get() {
            Some(w) => w
                .offers(None)
                .await
                .map(|o| o.into_iter().map(|o| (o.offer_id, o.offer)).collect())
                .unwrap_or_default(),
            None => HashMap::new(),
        };
        let names: HashMap<i64, String> = match self.apps.get() {
            Some(a) => a
                .list_accounts()
                .await
                .map(|l| l.into_iter().map(|a| (a.id, a.name)).collect())
                .unwrap_or_default(),
            None => HashMap::new(),
        };
        // Splits and their numbers, per address (migration 0020).
        let mut prisms: HashMap<i64, serde_json::Value> = HashMap::new();
        for a in &addresses {
            if let Ok(v) = self.prism_view(a.id).await {
                prisms.insert(a.id, v);
            }
        }
        let lnvps = self.lnvps_view().await;
        let desec_signup = self.desec_signup_view().await;
        let njalla_signup = self.njalla_signup_view().await;
        let njalla_purchase = self.njalla_purchase_view().await;
        let st = self.status.lock().unwrap();
        let gw: HashMap<i64, &Gateway> = gateways.iter().map(|g| (g.id, g)).collect();
        let domains_json: Vec<Value> = domains
            .iter()
            .map(|d| {
                let reachable = d.reach_state.as_deref() == Some("reachable");
                let mine: Vec<Value> = addresses
                    .iter()
                    .filter(|a| a.domain_id == d.id)
                    .map(|a| {
                        let address = format!("{}@{}", a.username, d.name);
                        let txt = a
                            .offer_id
                            .as_ref()
                            .and_then(|o| offers.get(o))
                            .map(|o| format!("bitcoin:?lno={o}"));
                        json!({
                            "id": a.id,
                            "domain_id": d.id,
                            "username": a.username,
                            "address": address,
                            "target": target_of(a.app_id),
                            "account_name": a.app_id.map_or_else(
                                || "Main".to_string(),
                                |i| names.get(&i).cloned().unwrap_or_else(|| format!("Account {i}")),
                            ),
                            "record_name": format!("{}.{}.", bip353_subname(&a.username), d.name),
                            "txt": txt,
                            "verified": a.verified,
                            "lnurl_enabled": a.lnurl,
                            "bip353": a.bip353,
                            "lnurl": a.lnurl && d.served() && reachable,
                            "prism": prisms.get(&a.id).cloned().unwrap_or_else(|| json!({
                                "splits": [],
                                "split_bps": 0,
                                "stats": {
                                    "forwarded_msat": 0, "failed_msat": 0, "pending_msat": 0,
                                    "pending_count": 0, "failed_count": 0, "skipped_msat": 0,
                                    "payments": 0,
                                },
                            })),
                        })
                    })
                    .collect();
                let mut records: Vec<Value> = mine
                    .iter()
                    .filter(|a| a["bip353"] == true)
                    .map(|a| {
                        json!({
                            "type": "TXT",
                            "name": a["record_name"],
                            "value": a["txt"],
                            "ok": a["verified"],
                        })
                    })
                    .collect();
                if let Some(ip) = d.gateway_id.and_then(|g| gw.get(&g)).and_then(|g| g.ipv4.clone()) {
                    records.push(json!({
                        "type": "A",
                        "name": format!("{}.", d.name),
                        "value": ip,
                        "ok": st.a_ok.get(&d.id).copied().unwrap_or(false),
                    }));
                }
                // Live by its own checks, with or without addresses: DNS answers and is
                // signed, the provider (if any) works, and, when served, HTTPS reaches
                // this node. Before the first check: by its addresses.
                let checks = st.health.get(&d.id).copied();
                let live = match checks {
                    Some(h) => {
                        h.dns && h.dnssec && h.provider != Some(false) && (!d.served() || reachable)
                    }
                    None => mine.iter().any(|a| a["verified"] == true),
                };
                json!({
                    "id": d.id,
                    "name": d.name,
                    "env": d.env,
                    "dns_provider_id": d.dns_provider_id,
                    "gateway_id": d.gateway_id,
                    "direct": d.direct,
                    "served": d.served(),
                    "live": live,
                    "checks": checks,
                    "ds": st.ds.get(&d.id),
                    "reach": d.reach_state.as_ref().map(|s| json!({
                        "state": s,
                        "checked_at": d.reach_checked_at,
                        "detail": d.reach_detail,
                    })),
                    "synced_at": d.synced_at,
                    "error": d.error,
                    "addresses": mine,
                    "records": records,
                })
            })
            .collect();
        let providers_json: Vec<Value> = providers
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "kind": p.kind,
                    "label": p.label,
                    "checked_at": p.checked_at,
                    "error": p.error,
                    "domains": domains.iter().filter(|d| d.dns_provider_id == Some(p.id))
                        .map(|d| d.name.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        let gateways_json: Vec<Value> = gateways
            .iter()
            .map(|g| gateway::view(g, self.tunnel(g.id).as_ref(), &domains))
            .collect();
        Ok(json!({
            "domains": domains_json,
            "dns_providers": providers_json,
            "gateways": gateways_json,
            "tunnel": self.cfg.tunnel_dir.is_some(),
            "lnvps": lnvps,
            "desec_signup": desec_signup,
            "njalla_signup": njalla_signup,
            "njalla_purchase": njalla_purchase,
            "errors": st.errors,
            "synced_at": st.synced_at,
        }))
    }
}

fn not_found(what: &str) -> DomainError {
    DomainError::NotFound(format!("no such {what}"))
}
