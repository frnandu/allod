//! Lightning addresses on third-party services (SPEC §12.3–§12.6): BIP-353 names on
//! twelve.cash and clank.money, and LNURL addresses on ZEUS Pay and Alby. Addresses on
//! the owner's own domains are in [`crate::domains`].

pub mod clank;
pub mod twelve;
pub mod zeus;

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use allod_db::Db;
use allod_keys::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, OnceCell};

use crate::wallet::{OfferRequest, OfferRow};
use crate::{AppService, Wallet};
pub use clank::{Clank, ClankError};
pub use twelve::{TwelveCash, TwelveError};
pub use zeus::{ZeusError, ZeusPay};

/// `config` key holding [`Stored`] as JSON.
pub const CONFIG_KEY: &str = "public_name";
/// Secret file prefix: clank.money management token, per name.
const CLANK_TOKEN: &str = "clank_token_";
/// Secret file prefix: L402 macaroon of a clank.money registration being paid.
const CLANK_L402: &str = "clank_l402_";

/// Background reconcile interval.
const EVERY: Duration = Duration::from_secs(300);
/// Minimum time between two reconciles.
const MIN_GAP: Duration = Duration::from_secs(20);
/// Seconds after a twelve.cash or clank.money name is added at which its DNS record is
/// checked; later, the 5-minute reconcile checks it.
const NAME_CHECKS: [i64; 4] = [10, 30, 120, 300];

/// Persisted state (`config.public_name`). Secrets are in files, never here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stored {
    /// twelve.cash addresses: `main` or the account id → entry (SPEC §12.3).
    #[serde(default)]
    pub twelve: BTreeMap<String, TwelveEntry>,
    /// clank.money names: `main` or the account id → entry (SPEC §12.4).
    #[serde(default)]
    pub clank: BTreeMap<String, ClankEntry>,
    /// Addresses on an LNURL service reached over NWC, set up by the owner on the
    /// service's website (Alby; SPEC §12.6): one per Alby account, each paying into Main
    /// or an account.
    #[serde(default)]
    pub albys: Vec<AlbyEntry>,
    /// The single Alby address of older versions (Main's): moved into `albys` on load.
    #[serde(default, skip_serializing)]
    pub alby: Option<AlbyEntry>,
    /// The owner's answers in the "add an address" questions (SPEC §13).
    #[serde(default)]
    pub prefs: Prefs,
    /// The ZEUS Pay address of Main (one per node, SPEC §12.5).
    #[serde(default)]
    pub zeus: Option<ZeusEntry>,
    /// Durable target reservations created before paid provider calls.
    #[serde(default)]
    purchase_reservations: BTreeMap<String, PurchaseReservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PurchaseReservation {
    provider: String,
    user_name: String,
    created_at: i64,
}

/// Answers to the "add an address" questions, so later additions skip them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Prefs {
    /// `any` (every wallet must pay), `modern` (BIP-353 wallets are enough), `both`.
    #[serde(default)]
    pub wallets: Option<String>,
    /// `yes`, `buy` (willing to buy one), `no`.
    #[serde(default)]
    pub domain: Option<String>,
    /// Where the node runs: `home` or `server`.
    #[serde(default)]
    pub site: Option<String>,
}

/// An LNURL address on a service that asks this node for invoices over NWC (Alby).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbyEntry {
    /// The address, once the owner entered it.
    pub address: Option<String>,
    /// The account it pays into (`None`: Main).
    #[serde(default)]
    pub target: Option<i64>,
    /// The receive-only NWC connection the service requests invoices through.
    pub connection_id: i64,
    /// A test invoice requested through the address came from this node.
    pub verified: bool,
    pub created_at: i64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZeusEntry {
    pub handle: String,
    pub domain: String,
    /// The account it pays into (`None`: Main). One ZEUS Pay address per node, so one
    /// target; it can be changed.
    #[serde(default)]
    pub target: Option<i64>,
    /// The receive-only NWC connection ZEUS Pay requests invoices through.
    pub connection_id: i64,
    /// Its LNURL endpoint answered.
    pub verified: bool,
    pub created_at: i64,
    /// ZEUS Pay+ (lets the owner choose the name and domain): end of the paid year.
    #[serde(default)]
    pub plus_expires_at: Option<i64>,
    /// Price of one year, as ZEUS Pay last said.
    #[serde(default)]
    pub plus_fee_sats: Option<u64>,
    #[serde(default)]
    pub plus_checked_at: i64,
    /// Renew ZEUS Pay+ automatically before it ends, paying at most `renew_max_sats`.
    #[serde(default)]
    pub auto_renew: bool,
    #[serde(default)]
    pub renew_max_sats: Option<u64>,
    /// Last automatic renewal attempt (one try every 6 hours at most).
    #[serde(default)]
    pub renew_tried_at: i64,
    /// Exact ZEUS Pay+ order/payment being resolved. Presence blocks another order.
    #[serde(default)]
    pub plus_attempt: Option<ZeusPlusAttempt>,
    /// Last ZEUS Pay+ problem (failed renewal, pending payment).
    #[serde(default)]
    pub plus_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZeusPlusAttempt {
    pub order_id: String,
    pub invoice: String,
    pub invoice_hash: String,
    #[serde(default)]
    pub payment_id: Option<String>,
    pub expected_expires_at: i64,
}

/// What the Alby connection may do: make and look up invoices and list them (Alby shows
/// received payments); never pay.
const ALBY_SCOPES: [&str; 4] = ["make_invoice", "lookup_invoice", "list_transactions", "get_info"];

/// Renew ZEUS Pay+ this long before it ends.
const PLUS_RENEW_BEFORE_SECS: i64 = 3 * 86_400;
/// Refresh the ZEUS Pay+ status this often.
const PLUS_CHECK_SECS: i64 = 12 * 3600;
#[cfg(not(test))]
const PLUS_CONFIRM_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
const PLUS_CONFIRM_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwelveEntry {
    pub user_name: String,
    pub domain: String,
    pub offer_id: String,
    /// Bought on the website and linked, rather than registered by Allod.
    pub custom: bool,
    /// Seen in DNS.
    pub verified: bool,
    pub created_at: i64,
    /// A chosen name Allod is buying: set until twelve.cash publishes it.
    #[serde(default)]
    pub purchase: Option<TwelvePurchase>,
}

/// A twelve.cash name being bought through its MDK checkout (SPEC §12.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwelvePurchase {
    pub pay_code_id: String,
    pub checkout_id: String,
    /// The wallet payment of the checkout's invoice.
    pub payment_id: Option<String>,
    pub price_sats: u64,
    /// Last problem finishing it.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClankState {
    /// The invoice is being paid (or its result is not known yet).
    Paying,
    /// Paid; the registration with the payment proof has not gone through yet.
    Paid,
    /// Registered; the management token is in the secrets.
    Registered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClankEntry {
    pub user_name: String,
    pub offer_id: String,
    pub state: ClankState,
    /// The L402 invoice and the wallet payment for it.
    pub invoice: String,
    pub payment_id: Option<String>,
    pub price_sats: u64,
    /// Seen in DNS with this offer.
    pub verified: bool,
    pub created_at: i64,
    /// Last registration error, while not registered.
    #[serde(default)]
    pub error: Option<String>,
}

/// The offer description for a BIP-353 name: the name itself, as wallets show it.
pub fn name_description(address: &str) -> String {
    format!("₿{address}")
}

/// The offer dedicated to `address` (description `₿address`, metadata
/// `{"bip353": address}`): the active one, or a new one.
pub(crate) async fn name_offer(
    wallet: &Wallet,
    app_id: Option<i64>,
    address: &str,
    description: String,
) -> Result<OfferRow, String> {
    let offers = wallet.offers(Some(app_id)).await.map_err(|e| e.to_string())?;
    let mine = |o: &&OfferRow| {
        o.state == "active"
            && o.amount_msat.is_none()
            && o.metadata
                .as_deref()
                .and_then(|m| serde_json::from_str::<Value>(m).ok())
                .is_some_and(|m| m.get("bip353").and_then(Value::as_str) == Some(address))
    };
    if let Some(o) = offers.iter().find(mine) {
        return Ok(o.clone());
    }
    wallet
        .create_offer(OfferRequest {
            app_id,
            amount_msat: None,
            description,
            expires_at: None,
            metadata: Some(serde_json::json!({"bip353": address}).to_string()),
        })
        .await
        .map_err(|e| e.to_string())
}

fn twelve_key(target: Option<i64>) -> String {
    target.map(|id| id.to_string()).unwrap_or_else(|| "main".into())
}

#[derive(Debug, Default)]
struct Status {
    errors: Vec<String>,
    /// twelve.cash entries whose offer is no longer active.
    twelve_inactive: Vec<String>,
    /// clank.money entries whose offer is no longer active.
    clank_inactive: Vec<String>,
    synced_at: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum PublicNameError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    /// The node cannot pay this invoice itself; the owner can pay it from another
    /// wallet and Allod finishes the purchase.
    #[error("{message}")]
    PaymentRequired { message: String, invoice: String },
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
}

impl From<std::io::Error> for PublicNameError {
    fn from(e: std::io::Error) -> Self {
        PublicNameError::Internal(e.to_string())
    }
}

impl From<allod_db::DbError> for PublicNameError {
    fn from(e: allod_db::DbError) -> Self {
        PublicNameError::Internal(e.to_string())
    }
}

/// Process settings for the service.
#[derive(Debug, Clone, Default)]
pub struct PublicNameConfig {
    /// twelve.cash base URL; `None`: `https://twelve.cash`.
    pub twelve_cash_url: Option<String>,
    /// clank.money base URL; `None`: `https://clank.money`.
    pub clank_url: Option<String>,
    /// ZEUS Pay base URL; `None`: `https://zeuspay.com`.
    pub zeus_url: Option<String>,
    /// Tests: LNURL services over plain http (Alby check).
    pub lnurl_http: bool,
}

pub struct PublicNameService {
    db: Db,
    secrets: SecretStore,
    twelve: TwelveCash,
    clank: Clank,
    zeus: ZeusPay,
    cfg: PublicNameConfig,
    wallet: Arc<OnceCell<Arc<Wallet>>>,
    apps: Arc<OnceCell<Arc<AppService>>>,
    stored: Mutex<Stored>,
    status: Mutex<Status>,
    run_lock: tokio::sync::Mutex<()>,
    wake: Notify,
    /// Wakes the name checker after a name was added.
    names_wake: Notify,
    last_name_check: Mutex<Option<i64>>,
    last_run: Mutex<Option<Instant>>,
    /// Held while a ZEUS Pay+ payment is made, so two cannot overlap.
    zeus_buying: tokio::sync::Mutex<()>,
    /// Paid-name operations serialize per provider and target, without blocking other accounts.
    purchase_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A DNS label: `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`.
pub fn valid_label(label: &str) -> bool {
    let b = label.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// Normalizes a domain the owner typed (`https://Pay.Example.com/` → `pay.example.com`).
pub fn normalize_domain(input: &str) -> Result<String, PublicNameError> {
    let mut d = input.trim().to_ascii_lowercase();
    if let Some(rest) = d.split("://").nth(1) {
        d = rest.to_string();
    }
    let d = d.split(['/', ':', '?', '#']).next().unwrap_or_default().trim_end_matches('.');
    let bad = || PublicNameError::BadRequest("enter a domain name like pay.example.com".into());
    if d.len() > 253 || !d.contains('.') || d.parse::<IpAddr>().is_ok() {
        return Err(bad());
    }
    if !d.split('.').all(valid_label) || d.rsplit('.').next().is_some_and(|tld| tld.len() < 2) {
        return Err(bad());
    }
    Ok(d.to_string())
}

/// Host of a URL, when it is a domain name (not localhost or an IP address).
pub fn url_domain(url: &str) -> Option<String> {
    let host = url.split("://").nth(1)?.split(['/', ':']).next()?.to_ascii_lowercase();
    if host.is_empty() || host == "localhost" || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    Some(host)
}

/// BIP-353 subname for a username.
pub fn bip353_subname(username: &str) -> String {
    format!("{username}.user._bitcoin-payment")
}

impl PublicNameService {
    pub async fn load(
        db: Db,
        secrets: SecretStore,
        cfg: PublicNameConfig,
        wallet: Arc<OnceCell<Arc<Wallet>>>,
        apps: Arc<OnceCell<Arc<AppService>>>,
    ) -> Result<Arc<Self>, PublicNameError> {
        let mut stored: Stored = match db.config_get(CONFIG_KEY).await? {
            Some(e) => serde_json::from_str(&e.value).unwrap_or_default(),
            None => Stored::default(),
        };
        if let Some(old) = stored.alby.take() {
            stored.albys.push(old);
        }
        Ok(Arc::new(PublicNameService {
            db,
            secrets,
            twelve: TwelveCash::new(
                cfg.twelve_cash_url.as_deref().unwrap_or("https://twelve.cash"),
            ),
            clank: Clank::new(cfg.clank_url.as_deref().unwrap_or("https://clank.money")),
            zeus: ZeusPay::new(cfg.zeus_url.as_deref().unwrap_or(zeus::DEFAULT_URL)),
            cfg,
            wallet,
            apps,
            stored: Mutex::new(stored),
            status: Mutex::new(Status::default()),
            run_lock: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            names_wake: Notify::new(),
            last_name_check: Mutex::new(None),
            last_run: Mutex::new(None),
            zeus_buying: tokio::sync::Mutex::new(()),
            purchase_locks: Mutex::new(HashMap::new()),
        }))
    }

    fn stored(&self) -> Stored {
        self.stored.lock().unwrap().clone()
    }

    async fn save(&self, s: Stored) -> Result<(), PublicNameError> {
        let json =
            serde_json::to_string(&s).map_err(|e| PublicNameError::Internal(e.to_string()))?;
        self.db.config_set(CONFIG_KEY, &json, false).await?;
        *self.stored.lock().unwrap() = s;
        Ok(())
    }

    fn purchase_key(provider: &str, target_key: &str) -> String {
        format!("{provider}:{target_key}")
    }

    fn purchase_lock(&self, provider: &str, target_key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let key = Self::purchase_key(provider, target_key);
        self.purchase_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn reserve_purchase(
        &self,
        provider: &str,
        target_key: &str,
        user_name: &str,
    ) -> Result<(), PublicNameError> {
        let reservation_key = Self::purchase_key(provider, target_key);
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        let occupied = match provider {
            "twelve" => s.twelve.contains_key(target_key),
            "clank" => s.clank.contains_key(target_key),
            _ => true,
        };
        if occupied || s.purchase_reservations.contains_key(&reservation_key) {
            return Err(PublicNameError::Conflict(format!(
                "this account already has a {provider} purchase"
            )));
        }
        s.purchase_reservations.insert(
            reservation_key,
            PurchaseReservation {
                provider: provider.to_string(),
                user_name: user_name.to_string(),
                created_at: now(),
            },
        );
        self.save(s).await
    }

    async fn clear_purchase_reservation(
        &self,
        provider: &str,
        target_key: &str,
    ) -> Result<(), PublicNameError> {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.purchase_reservations.remove(&Self::purchase_key(provider, target_key));
        self.save(s).await
    }

    async fn abandon_reserved_offer(
        &self,
        provider: &str,
        target_key: &str,
        wallet: &Wallet,
        offer_id: &str,
        error: PublicNameError,
    ) -> PublicNameError {
        let _ = wallet.set_offer_state(offer_id, "disabled").await;
        match self.clear_purchase_reservation(provider, target_key).await {
            Ok(()) => error,
            Err(clear) => clear,
        }
    }

    /// twelve.cash serves mainnet only; a test stand-in (`twelve_cash_url`) any network.
    pub fn twelve_any_network(&self) -> bool {
        self.cfg.twelve_cash_url.is_some()
    }

    /// clank.money names are for mainnet; a test stand-in (`clank_url`) any network.
    pub fn clank_any_network(&self) -> bool {
        self.cfg.clank_url.is_some()
    }

    /// LNURL address services (Alby) are for mainnet; tests (`lnurl_http`) any network.
    pub fn alby_any_network(&self) -> bool {
        self.cfg.lnurl_http
    }

    /// ZEUS Pay is for mainnet; a test stand-in (`zeus_url`) any network.
    pub fn zeus_any_network(&self) -> bool {
        self.cfg.zeus_url.is_some()
    }

    /// Asks the background task to reconcile soon (accounts or settings changed).
    pub fn trigger(&self) {
        self.wake.notify_one();
        self.names_wake.notify_one();
    }

    /// The next quick DNS check due for a new name (twelve.cash, clank.money, ZEUS Pay).
    fn next_name_check(&self) -> Option<i64> {
        let last = self.last_name_check.lock().unwrap().unwrap_or(0);
        let s = self.stored();
        let added = s.twelve.values().filter(|e| !e.verified).map(|e| e.created_at).chain(
            s.clank
                .values()
                .filter(|e| !e.verified && e.state == ClankState::Registered)
                .map(|e| e.created_at),
        );
        let zeus = s.zeus.iter().filter(|z| !z.verified).map(|z| z.created_at);
        added
            .chain(zeus)
            .flat_map(|c| NAME_CHECKS.iter().map(move |o| c + o))
            .filter(|t| *t > last)
            .min()
    }

    /// Looks up the DNS records of names not seen yet, now ("Check now", and the quick
    /// checks after a name is added).
    pub async fn check_names(&self) {
        *self.last_name_check.lock().unwrap() = Some(now());
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if self.verify_names(&mut s).await {
            if let Err(e) = self.save(s).await {
                tracing::warn!(error = %e, "public name: cannot store name checks");
            }
        }
    }

    /// Marks twelve.cash and clank.money names `verified` once their record, asked from
    /// the registry's own name servers, carries their offer. True when anything changed.
    async fn verify_names(&self, s: &mut Stored) -> bool {
        let Some(wallet) = self.wallet.get() else { return false };
        let mut changed = false;
        for a in s.albys.iter_mut().filter(|a| !a.verified && a.address.is_some()) {
            let address = a.address.clone().unwrap_or_default();
            match self.alby_check(&address, a.target, a.connection_id).await {
                Ok(()) => {
                    a.verified = true;
                    a.error = None;
                    changed = true;
                }
                Err(e) => {
                    let e = e.to_string();
                    changed |= a.error.as_deref() != Some(e.as_str());
                    a.error = Some(e);
                }
            }
        }
        if let Some(z) = s.zeus.as_mut().filter(|z| !z.verified) {
            let a = zeus::Address { handle: z.handle.clone(), domain: z.domain.clone() };
            if self.zeus.answers(&a).await {
                z.verified = true;
                changed = true;
            }
        }
        let pending = s.twelve.values().any(|e| !e.verified)
            || s.clank.values().any(|e| !e.verified && e.state == ClankState::Registered);
        if !pending {
            return changed;
        }
        let offers: std::collections::HashMap<String, String> = match wallet.offers(None).await {
            Ok(o) => o.into_iter().map(|o| (o.offer_id, o.offer)).collect(),
            Err(_) => return changed,
        };
        let entries = s
            .twelve
            .values_mut()
            .filter(|e| !e.verified && e.purchase.is_none())
            .map(|e| (&e.user_name, e.domain.as_str(), &e.offer_id, &mut e.verified))
            .chain(
                s.clank
                    .values_mut()
                    .filter(|e| !e.verified && e.state == ClankState::Registered)
                    .map(|e| (&e.user_name, clank::DOMAIN, &e.offer_id, &mut e.verified)),
            );
        for (user, domain, offer_id, verified) in entries {
            let Some(offer) = offers.get(offer_id) else { continue };
            if let Some(values) = twelve::lookup(user, domain).await {
                if twelve::points_at(&values, offer) {
                    *verified = true;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Runs forever: reconciles every 5 minutes, or soon after [`Self::trigger`]; and
    /// checks new names' DNS records at [`NAME_CHECKS`].
    pub fn start(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let t = now();
                match this.next_name_check() {
                    Some(due) if due <= t => this.check_names().await,
                    Some(due) => {
                        let wait = Duration::from_secs((due - t) as u64);
                        let _ = tokio::time::timeout(wait, this.names_wake.notified()).await;
                    }
                    None => this.names_wake.notified().await,
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.reconcile().await;
                // Outside the reconcile: it takes the run lock itself, and may pay.
                this.zeus_upkeep().await;
                let _ = tokio::time::timeout(EVERY, this.wake.notified()).await;
                let wait = this
                    .last_run
                    .lock()
                    .unwrap()
                    .map(|t| MIN_GAP.saturating_sub(t.elapsed()))
                    .unwrap_or_default();
                tokio::time::sleep(wait).await;
            }
        });
    }

    // ------------------------------------------------------------- reconcile

    /// Refreshes the twelve.cash, clank.money, ZEUS Pay and Alby names.
    pub async fn reconcile(&self) {
        *self.last_run.lock().unwrap() = Some(Instant::now());
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        let before = serde_json::to_string(&s).unwrap_or_default();
        let mut errors = Vec::new();

        // Cheap when nothing is missing; runs once the wallet and apps exist.
        self.alby_ensure_scopes().await;
        self.twelve_purchases(&mut s).await;
        let twelve_inactive = self.twelve_check(&s).await;
        let clank_inactive = self.clank_check(&mut s).await;
        self.verify_names(&mut s).await;

        if serde_json::to_string(&s).unwrap_or_default() != before {
            if let Err(e) = self.save(s).await {
                errors.push(format!("cannot store state: {e}"));
            }
        }
        let mut st = self.status.lock().unwrap();
        for e in &errors {
            tracing::warn!(error = %e, "public name");
        }
        st.errors = errors;
        st.twelve_inactive = twelve_inactive;
        st.clank_inactive = clank_inactive;
        st.synced_at = Some(now());
    }

    // ------------------------------------------------------------- twelve.cash

    /// The wallet and the open account (or Main, `None`) an address is for.
    async fn twelve_target(
        &self,
        target: Option<i64>,
    ) -> Result<(Arc<Wallet>, String), PublicNameError> {
        let wallet = self
            .wallet
            .get()
            .cloned()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        let name = match target {
            None => "Main".to_string(),
            Some(id) => {
                let apps = self.apps.get().ok_or_else(|| {
                    PublicNameError::Conflict("the wallet is not running yet".into())
                })?;
                let app =
                    apps.get(id).await.map_err(|e| PublicNameError::BadRequest(e.to_string()))?;
                if !app.is_account() || app.closed_at.is_some() {
                    return Err(PublicNameError::BadRequest("not an open account".into()));
                }
                app.name
            }
        };
        Ok((wallet, name))
    }

    /// Registers a free random `name@twelve.cash` for the account's (or Main's)
    /// long-lived BOLT12 offer (SPEC §12.3).
    pub async fn twelve_register(
        &self,
        target: Option<i64>,
    ) -> Result<TwelveEntry, PublicNameError> {
        let (wallet, name) = self.twelve_target(target).await?;
        let key = twelve_key(target);
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if s.twelve.contains_key(&key)
            || s.purchase_reservations.contains_key(&Self::purchase_key("twelve", &key))
        {
            return Err(PublicNameError::Conflict(
                "this account already has a twelve.cash address".into(),
            ));
        }
        // Its own offer, so payments show which address they came through. twelve.cash
        // picks the random name only after it has the offer, so the description cannot
        // carry it.
        let description = match target {
            Some(_) => format!("Payment to {name} (twelve.cash)"),
            None => "Payment (twelve.cash)".to_string(),
        };
        let offer = name_offer(&wallet, target, twelve::DOMAIN, description)
            .await
            .map_err(|e| PublicNameError::Conflict(format!("no BOLT12 offer: {e}")))?;
        let user = self
            .twelve
            .create_random(&offer.offer)
            .await
            .map_err(|e| PublicNameError::Upstream(e.to_string()))?;
        let entry = TwelveEntry {
            user_name: user,
            domain: twelve::DOMAIN.into(),
            offer_id: offer.offer_id,
            custom: false,
            verified: false,
            created_at: now(),
            purchase: None,
        };
        s.twelve.insert(key, entry.clone());
        self.save(s).await?;
        drop(_g);
        self.trigger();
        Ok(entry)
    }

    /// Buys `user@twelve.cash` for the account (or Main): reserves it, pays the
    /// website's checkout invoice from `pay_from` (at most `max_price_sats`), and asks
    /// twelve.cash to publish it only once the checkout shows the payment received.
    /// twelve.cash's server does not check payment itself when publishing; Allod never
    /// asks before its own payment settled. A purchase that cannot finish now is
    /// finished by the background task (it never pays twice).
    pub async fn twelve_buy(
        &self,
        target: Option<i64>,
        user: &str,
        max_price_sats: u64,
        pay_from: Option<i64>,
    ) -> Result<TwelveEntry, PublicNameError> {
        let user = user.trim().trim_start_matches('₿').to_string();
        let user = user.strip_suffix(&format!("@{}", twelve::DOMAIN)).unwrap_or(&user).to_string();
        if !twelve::valid_custom_name(&user) {
            return Err(PublicNameError::BadRequest(
                "at least 4 of a-z, A-Z, 0-9, '.', '-' or '_'".into(),
            ));
        }
        if twelve::CUSTOM_PRICE_SATS > max_price_sats {
            return Err(PublicNameError::Conflict(format!(
                "a twelve.cash name costs {} sats, more than the {max_price_sats} sats allowed",
                twelve::CUSTOM_PRICE_SATS
            )));
        }
        let (wallet, _) = self.twelve_target(target).await?;
        if pay_from.is_some() && pay_from != target {
            self.twelve_target(pay_from).await?;
        }
        let key = twelve_key(target);
        let purchase_lock = self.purchase_lock("twelve", &key);
        let _purchase_guard = purchase_lock.lock().await;
        self.reserve_purchase("twelve", &key, &user).await?;
        let address = format!("{user}@{}", twelve::DOMAIN);
        let offer = match name_offer(&wallet, target, &address, name_description(&address)).await {
            Ok(offer) => offer,
            Err(e) => {
                self.clear_purchase_reservation("twelve", &key).await?;
                return Err(PublicNameError::Conflict(format!("no BOLT12 offer: {e}")));
            }
        };
        let abandon =
            |error| self.abandon_reserved_offer("twelve", &key, &wallet, &offer.offer_id, error);
        let pay_code_id = match self.twelve.create_pay_code(&user, &offer.offer).await {
            Ok(id) => id,
            Err(e @ twelve::TwelveError::Taken(_)) => {
                return Err(
                    abandon(PublicNameError::Conflict(format!("{address} is taken ({e})"))).await
                )
            }
            Err(e) => return Err(abandon(PublicNameError::Upstream(e.to_string())).await),
        };
        let checkout = match self.twelve.checkout_for(&pay_code_id, &user).await {
            Ok(c) => c,
            Err(e) => return Err(abandon(PublicNameError::Upstream(e.to_string())).await),
        };
        let Some(invoice) = checkout.invoice.clone() else {
            return Err(abandon(PublicNameError::Upstream(format!(
                "the twelve.cash checkout has no invoice (status {})",
                checkout.status
            )))
            .await);
        };
        let d = match wallet.decode(&invoice).await {
            Ok(d) => d,
            Err(e) => {
                return Err(abandon(PublicNameError::Upstream(format!(
                    "twelve.cash sent an unreadable invoice: {e}"
                )))
                .await)
            }
        };
        let price_msat = twelve::CUSTOM_PRICE_SATS * 1000;
        if d.kind != "bolt11" || !d.network_ok || d.is_expired || d.amount_msat != Some(price_msat)
        {
            return Err(abandon(PublicNameError::Upstream(format!(
                "the twelve.cash invoice is not for {} sats",
                twelve::CUSTOM_PRICE_SATS
            )))
            .await);
        }

        // Record the purchase before paying, so a restart can finish it.
        let mut entry = TwelveEntry {
            user_name: user.clone(),
            domain: twelve::DOMAIN.into(),
            offer_id: offer.offer_id.clone(),
            custom: true,
            verified: false,
            created_at: now(),
            purchase: Some(TwelvePurchase {
                pay_code_id,
                checkout_id: checkout.id,
                payment_id: None,
                price_sats: twelve::CUSTOM_PRICE_SATS,
                error: None,
            }),
        };
        self.put_twelve(&key, Some(entry.clone())).await?;
        let give_up = |e: PublicNameError| {
            let wallet = wallet.clone();
            let offer_id = offer.offer_id.clone();
            async move {
                let _ = wallet.set_offer_state(&offer_id, "disabled").await;
                e
            }
        };
        let paid = wallet
            .pay(crate::wallet::PayRequest {
                app_id: pay_from,
                payment: invoice.clone(),
                metadata: Some(
                    serde_json::json!({"recipient_data": {"identifier": "twelve.cash"},
                        "comment": format!("Name {address}")})
                    .to_string(),
                ),
                ..Default::default()
            })
            .await;
        let tx = match paid {
            Ok(tx) => tx,
            Err(crate::wallet::WalletError::InsufficientBalance) => {
                // Keep the purchase: the owner can pay the checkout's invoice from
                // another wallet, and the background check finishes it.
                if let Some(p) = entry.purchase.as_mut() {
                    p.error = Some("twelve.cash has not seen the payment yet".into());
                }
                self.put_twelve(&key, Some(entry)).await?;
                self.trigger();
                return Err(PublicNameError::PaymentRequired {
                    message: "paying for the name failed: insufficient balance".into(),
                    invoice,
                });
            }
            Err(e) => {
                self.put_twelve(&key, None).await?;
                return Err(give_up(PublicNameError::Conflict(format!(
                    "paying for the name failed: {e}"
                )))
                .await);
            }
        };
        if let Some(p) = entry.purchase.as_mut() {
            p.payment_id = Some(tx.public_id.clone());
        }
        self.put_twelve(&key, Some(entry.clone())).await?;
        let tx = wait_final(&wallet, tx).await;
        match tx.state.as_str() {
            "FAILED" => {
                self.put_twelve(&key, None).await?;
                let why = tx.failure_reason.unwrap_or_else(|| "payment failed".into());
                Err(give_up(PublicNameError::Conflict(format!(
                    "paying for the name failed: {why}"
                )))
                .await)
            }
            "SETTLED" => {
                // MDK records the payment shortly after it settles.
                for _ in 0..5 {
                    self.twelve_finish(&mut entry).await;
                    if entry.purchase.is_none() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                self.put_twelve(&key, Some(entry.clone())).await?;
                self.trigger();
                Ok(entry)
            }
            _ => Ok(entry),
        }
    }

    async fn put_twelve(
        &self,
        key: &str,
        entry: Option<TwelveEntry>,
    ) -> Result<(), PublicNameError> {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.purchase_reservations.remove(&Self::purchase_key("twelve", key));
        match entry {
            Some(e) => s.twelve.insert(key.to_string(), e),
            None => s.twelve.remove(key),
        };
        self.save(s).await
    }

    /// After Allod's payment settled: once the checkout shows the payment received,
    /// asks twelve.cash to publish the name. Does not save. Returns true when the
    /// checkout is dead (expired or cancelled), so nothing can be paid any more.
    async fn twelve_finish(&self, entry: &mut TwelveEntry) -> bool {
        let Some(p) = entry.purchase.as_mut() else { return false };
        match self.twelve.get_checkout(&p.checkout_id).await {
            Ok(c) if c.status == "PAYMENT_RECEIVED" => {
                match self.twelve.redeem(&p.pay_code_id).await {
                    Ok(()) => {
                        entry.purchase = None;
                        return false;
                    }
                    Err(e) => p.error = Some(e.to_string()),
                }
            }
            Ok(c) if c.status == "EXPIRED" || c.status == "CANCELLED" => {
                p.error = Some(format!("the checkout expired ({})", c.status));
                return true;
            }
            Ok(c) => {
                p.error = Some(format!("twelve.cash has not seen the payment yet ({})", c.status))
            }
            Err(e) => p.error = Some(e.to_string()),
        }
        tracing::info!(name = %entry.user_name, error = ?entry.purchase.as_ref().and_then(|p| p.error.clone()), "twelve.cash purchase");
        false
    }

    /// Background: finishes purchases whose payment settled, drops failed ones.
    async fn twelve_purchases(&self, s: &mut Stored) {
        let Some(wallet) = self.wallet.get().cloned() else { return };
        let pending: Vec<String> =
            s.twelve.iter().filter(|(_, e)| e.purchase.is_some()).map(|(k, _)| k.clone()).collect();
        for key in pending {
            let Some(mut e) = s.twelve.get(&key).cloned() else { continue };
            let Some(id) = e.purchase.as_ref().and_then(|p| p.payment_id.clone()) else {
                // Not paid by this node: the owner may have paid the checkout's invoice
                // from another wallet. twelve.cash knows; finish it when it does.
                if self.twelve_finish(&mut e).await {
                    let _ = wallet.set_offer_state(&e.offer_id, "disabled").await;
                    s.twelve.remove(&key);
                } else {
                    s.twelve.insert(key, e);
                }
                continue;
            };
            let Ok(tx) = wallet.transaction(&id).await else { continue };
            match tx.state.as_str() {
                "SETTLED" => {
                    let _ = self.twelve_finish(&mut e).await;
                    s.twelve.insert(key, e);
                }
                "FAILED" => {
                    let _ = wallet.set_offer_state(&e.offer_id, "disabled").await;
                    s.twelve.remove(&key);
                }
                _ => {}
            }
        }
    }

    /// Links a name the owner bought on twelve.cash. It must already publish the
    /// account's (or Main's) offer, which Allod checks by DNS.
    pub async fn twelve_link(
        &self,
        target: Option<i64>,
        address: &str,
    ) -> Result<TwelveEntry, PublicNameError> {
        let (user, domain) = twelve::parse_address(address).ok_or_else(|| {
            PublicNameError::BadRequest("enter an address like name@twelve.cash".into())
        })?;
        let (wallet, _) = self.twelve_target(target).await?;
        let address = format!("{user}@{domain}");
        let offer = name_offer(&wallet, target, &address, name_description(&address))
            .await
            .map_err(|e| PublicNameError::Conflict(format!("no BOLT12 offer: {e}")))?;
        let values = twelve::lookup(&user, &domain).await.unwrap_or_default();
        if !twelve::points_at(&values, &offer.offer) {
            return Err(PublicNameError::BadRequest(format!(
                "{user}@{domain} does not point at this account's offer yet; register it with the offer shown"
            )));
        }
        let entry = TwelveEntry {
            user_name: user,
            domain,
            offer_id: offer.offer_id,
            custom: true,
            verified: true,
            created_at: now(),
            purchase: None,
        };
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.twelve.insert(twelve_key(target), entry.clone());
        self.save(s).await?;
        Ok(entry)
    }

    /// The offer a custom twelve.cash name must carry: its own, described with the
    /// name (created if needed).
    pub async fn twelve_offer(
        &self,
        target: Option<i64>,
        address: &str,
    ) -> Result<String, PublicNameError> {
        let (user, domain) = twelve::parse_address(address).ok_or_else(|| {
            PublicNameError::BadRequest("enter an address like name@twelve.cash".into())
        })?;
        let (wallet, _) = self.twelve_target(target).await?;
        let address = format!("{user}@{domain}");
        name_offer(&wallet, target, &address, name_description(&address))
            .await
            .map(|o| o.offer)
            .map_err(|e| PublicNameError::Conflict(format!("no BOLT12 offer: {e}")))
    }

    /// Forgets the address in Allod. twelve.cash has no way to delete it.
    pub async fn twelve_forget(&self, target: Option<i64>) -> Result<(), PublicNameError> {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if s.twelve.remove(&twelve_key(target)).is_none() {
            return Err(PublicNameError::BadRequest(
                "no twelve.cash address for this account".into(),
            ));
        }
        self.save(s).await
    }

    /// twelve.cash addresses whose offer is no longer active (closed accounts disable
    /// theirs). DNS checks are in [`Self::verify_names`].
    async fn twelve_check(&self, s: &Stored) -> Vec<String> {
        let Some(wallet) = self.wallet.get() else { return Vec::new() };
        let active: std::collections::HashSet<String> = match wallet.offers(None).await {
            Ok(o) => o.into_iter().filter(|o| o.state == "active").map(|o| o.offer_id).collect(),
            Err(_) => return Vec::new(),
        };
        s.twelve
            .iter()
            .filter(|(_, e)| !active.contains(&e.offer_id))
            .map(|(k, _)| k.clone())
            .collect()
    }

    // ------------------------------------------------------------- clank.money

    /// Whether `username` looks free at clank.money (`None`: unknown).
    pub async fn clank_available(&self, username: &str) -> Result<Option<bool>, PublicNameError> {
        if !clank::valid_username(username) {
            return Err(PublicNameError::BadRequest(
                "3–32 lowercase letters, digits or hyphens, not starting or ending with a hyphen"
                    .into(),
            ));
        }
        Ok(self.clank.available(username).await)
    }

    /// Buys `username@clank.money` for the account (or Main), pointing at an offer of its
    /// own described `₿username@clank.money` (SPEC §12.4). The price is paid over L402
    /// from `pay_from` (`None`: Main), only up to `max_price_sats`.
    pub async fn clank_register(
        &self,
        target: Option<i64>,
        username: &str,
        max_price_sats: u64,
        pay_from: Option<i64>,
    ) -> Result<ClankEntry, PublicNameError> {
        let username = username.trim().to_ascii_lowercase();
        if !clank::valid_username(&username) {
            return Err(PublicNameError::BadRequest(
                "3–32 lowercase letters, digits or hyphens, not starting or ending with a hyphen"
                    .into(),
            ));
        }
        let (wallet, _) = self.twelve_target(target).await?;
        if pay_from.is_some() && pay_from != target {
            self.twelve_target(pay_from).await?;
        }
        let key = twelve_key(target);
        let purchase_lock = self.purchase_lock("clank", &key);
        let _purchase_guard = purchase_lock.lock().await;
        self.reserve_purchase("clank", &key, &username).await?;
        let address = format!("{username}@{}", clank::DOMAIN);
        let offer = match name_offer(&wallet, target, &address, name_description(&address)).await {
            Ok(offer) => offer,
            Err(e) => {
                self.clear_purchase_reservation("clank", &key).await?;
                return Err(PublicNameError::Conflict(format!("no BOLT12 offer: {e}")));
            }
        };
        let abandon =
            |error| self.abandon_reserved_offer("clank", &key, &wallet, &offer.offer_id, error);
        let uri = format!("bitcoin:?lno={}", offer.offer);
        let challenge = match self.clank.register(&username, &uri, None).await {
            Ok(clank::Step::Pay(c)) => c,
            Ok(clank::Step::Done { token, .. }) => {
                // Free registration: nothing to pay.
                self.secrets.write(&format!("{CLANK_TOKEN}{username}"), &token)?;
                let entry = ClankEntry {
                    user_name: username,
                    offer_id: offer.offer_id,
                    state: ClankState::Registered,
                    invoice: String::new(),
                    payment_id: None,
                    price_sats: 0,
                    verified: false,
                    created_at: now(),
                    error: None,
                };
                self.put_clank(&key, Some(entry.clone())).await?;
                self.trigger();
                return Ok(entry);
            }
            Err(e @ ClankError::Taken(_)) => {
                return Err(abandon(PublicNameError::Conflict(e.to_string())).await)
            }
            Err(e) => return Err(abandon(PublicNameError::Upstream(e.to_string())).await),
        };

        // Check the invoice before paying: amount within the owner's limit and matching
        // what clank.money announced.
        let d = match wallet.decode(&challenge.invoice).await {
            Ok(d) => d,
            Err(e) => {
                return Err(abandon(PublicNameError::Upstream(format!(
                    "clank.money sent an invoice Allod cannot read: {e}"
                )))
                .await)
            }
        };
        let price_msat = d.amount_msat.unwrap_or(0);
        if d.kind != "bolt11" || price_msat == 0 || !d.network_ok || d.is_expired {
            return Err(abandon(PublicNameError::Upstream(
                "clank.money sent an unusable invoice".into(),
            ))
            .await);
        }
        if challenge.amount_sats.is_some_and(|a| a.saturating_mul(1000) != price_msat) {
            return Err(abandon(PublicNameError::Upstream(
                "the invoice amount does not match clank.money's price".into(),
            ))
            .await);
        }
        let price_sats = price_msat.div_ceil(1000);
        if price_sats > max_price_sats {
            return Err(abandon(PublicNameError::Conflict(format!(
                "the name costs {price_sats} sats, more than the {max_price_sats} sats allowed"
            )))
            .await);
        }

        // Record the attempt before paying, so a restart can finish it.
        self.secrets.write(&format!("{CLANK_L402}{key}"), &challenge.macaroon)?;
        let mut entry = ClankEntry {
            user_name: username.clone(),
            offer_id: offer.offer_id.clone(),
            state: ClankState::Paying,
            invoice: challenge.invoice.clone(),
            payment_id: None,
            price_sats,
            verified: false,
            created_at: now(),
            error: None,
        };
        self.put_clank(&key, Some(entry.clone())).await?;
        let give_up = |e: PublicNameError| {
            let wallet = wallet.clone();
            let offer_id = offer.offer_id.clone();
            async move {
                let _ = wallet.set_offer_state(&offer_id, "disabled").await;
                e
            }
        };

        let paid = wallet
            .pay(crate::wallet::PayRequest {
                app_id: pay_from,
                payment: challenge.invoice.clone(),
                metadata: Some(
                    serde_json::json!({"recipient_data": {"identifier": "clank.money"},
                        "comment": format!("Name {address}")})
                    .to_string(),
                ),
                ..Default::default()
            })
            .await;
        let tx = match paid {
            Ok(tx) => tx,
            Err(crate::wallet::WalletError::InsufficientBalance) => {
                // Keep the entry and the L402 macaroon: the owner can pay the invoice
                // from another wallet and submit the payment proof to finish.
                entry.error = Some("clank.money is waiting for the payment proof".into());
                self.put_clank(&key, Some(entry)).await?;
                self.trigger();
                return Err(PublicNameError::PaymentRequired {
                    message: "paying for the name failed: insufficient balance".into(),
                    invoice: challenge.invoice.clone(),
                });
            }
            Err(e) => {
                let _ = self.secrets.remove(&format!("{CLANK_L402}{key}"));
                self.put_clank(&key, None).await?;
                return Err(give_up(PublicNameError::Conflict(format!(
                    "paying for the name failed: {e}"
                )))
                .await);
            }
        };
        entry.payment_id = Some(tx.public_id.clone());
        self.put_clank(&key, Some(entry.clone())).await?;
        let tx = wait_final(&wallet, tx).await;
        match tx.state.as_str() {
            "FAILED" => {
                let _ = self.secrets.remove(&format!("{CLANK_L402}{key}"));
                self.put_clank(&key, None).await?;
                let why = tx.failure_reason.unwrap_or_else(|| "payment failed".into());
                Err(give_up(PublicNameError::Conflict(format!(
                    "paying for the name failed: {why}"
                )))
                .await)
            }
            "SETTLED" => {
                entry.state = ClankState::Paid;
                self.put_clank(&key, Some(entry.clone())).await?;
                let entry = self.clank_finish(&key, entry, tx.preimage.as_deref()).await?;
                self.trigger();
                Ok(entry)
            }
            // Still in flight: the background check finishes it.
            _ => Ok(entry),
        }
    }

    /// Finishes a clank.money registration paid from another wallet: the owner sends
    /// the invoice's preimage, the L402 proof clank.money asks for.
    pub async fn clank_proof(
        &self,
        target: Option<i64>,
        preimage: &str,
    ) -> Result<ClankEntry, PublicNameError> {
        let preimage = preimage.trim().to_ascii_lowercase();
        if preimage.len() != 64 || !preimage.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(PublicNameError::BadRequest(
                "paste the payment preimage (64 hex characters) from the wallet that paid".into(),
            ));
        }
        let key = twelve_key(target);
        let Some(entry) = self.stored().clank.get(&key).cloned() else {
            return Err(PublicNameError::BadRequest("no clank.money name for this account".into()));
        };
        if entry.state == ClankState::Registered {
            return Err(PublicNameError::Conflict("the name is already registered".into()));
        }
        let entry = self.clank_finish(&key, entry, Some(&preimage)).await?;
        self.trigger();
        Ok(entry)
    }

    async fn put_clank(&self, key: &str, entry: Option<ClankEntry>) -> Result<(), PublicNameError> {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.purchase_reservations.remove(&Self::purchase_key("clank", key));
        match entry {
            Some(e) => s.clank.insert(key.to_string(), e),
            None => s.clank.remove(key),
        };
        self.save(s).await
    }

    /// Sends the payment proof to clank.money and saves the result. On failure the
    /// entry stays `Paid` with the error, and the background check tries again.
    async fn clank_finish(
        &self,
        key: &str,
        entry: ClankEntry,
        preimage: Option<&str>,
    ) -> Result<ClankEntry, PublicNameError> {
        let entry = self.clank_finish_unlocked(key, entry, preimage).await?;
        self.put_clank(key, Some(entry.clone())).await?;
        Ok(entry)
    }

    /// Forgets a clank.money name in Allod (and its management token). The name stays
    /// registered at clank.money.
    pub async fn clank_forget(&self, target: Option<i64>) -> Result<(), PublicNameError> {
        let key = twelve_key(target);
        let Some(e) = self.stored().clank.get(&key).cloned() else {
            return Err(PublicNameError::BadRequest("no clank.money name for this account".into()));
        };
        if e.state != ClankState::Registered && e.payment_id.is_some() {
            return Err(PublicNameError::Conflict(
                "the name is still being paid for or registered; wait for it to finish".into(),
            ));
        }
        self.secrets.remove(&format!("{CLANK_TOKEN}{}", e.user_name))?;
        // An unpaid attempt also leaves its L402 challenge behind.
        let _ = self.secrets.remove(&format!("{CLANK_L402}{key}"));
        self.put_clank(&key, None).await
    }

    /// Finishes paid registrations, drops failed payments and reports names whose offer
    /// is no longer active.
    async fn clank_check(&self, s: &mut Stored) -> Vec<String> {
        let Some(wallet) = self.wallet.get().cloned() else { return Vec::new() };
        // Registrations still in progress.
        for (key, e) in s.clank.clone() {
            if e.state == ClankState::Registered {
                continue;
            }
            let Some(id) = e.payment_id.clone() else { continue };
            let Ok(tx) = wallet.transaction(&id).await else { continue };
            match tx.state.as_str() {
                "SETTLED" => {
                    let mut e = e.clone();
                    e.state = ClankState::Paid;
                    if let Ok(done) =
                        self.clank_finish_unlocked(&key, e, tx.preimage.as_deref()).await
                    {
                        s.clank.insert(key.clone(), done);
                    }
                }
                "FAILED" => {
                    let _ = self.secrets.remove(&format!("{CLANK_L402}{key}"));
                    let _ = wallet.set_offer_state(&e.offer_id, "disabled").await;
                    s.clank.remove(&key);
                }
                _ => {}
            }
        }
        let active: std::collections::HashSet<String> = match wallet.offers(None).await {
            Ok(o) => o.into_iter().filter(|o| o.state == "active").map(|o| o.offer_id).collect(),
            Err(_) => return Vec::new(),
        };
        s.clank
            .iter()
            .filter(|(_, e)| !active.contains(&e.offer_id))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Sends the payment proof without saving: the background check holds the run lock
    /// and saves `Stored` itself.
    async fn clank_finish_unlocked(
        &self,
        key: &str,
        mut entry: ClankEntry,
        preimage: Option<&str>,
    ) -> Result<ClankEntry, PublicNameError> {
        let macaroon = self.secrets.read(&format!("{CLANK_L402}{key}"))?;
        let (Some(macaroon), Some(preimage)) = (macaroon, preimage) else {
            entry.error = Some("the payment proof is missing; contact clank.money".into());
            return Ok(entry);
        };
        let offer = self
            .wallet
            .get()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?
            .offers(None)
            .await
            .map_err(|e| PublicNameError::Internal(e.to_string()))?
            .into_iter()
            .find(|o| o.offer_id == entry.offer_id)
            .ok_or_else(|| PublicNameError::Internal("the name's offer is gone".into()))?;
        let uri = format!("bitcoin:?lno={}", offer.offer);
        let cred = format!("{}:{preimage}", macaroon.as_str());
        match self.clank.register(&entry.user_name, &uri, Some(&cred)).await {
            Ok(clank::Step::Done { token, .. }) => {
                self.secrets.write(&format!("{CLANK_TOKEN}{}", entry.user_name), &token)?;
                let _ = self.secrets.remove(&format!("{CLANK_L402}{key}"));
                entry.state = ClankState::Registered;
                entry.error = None;
            }
            Ok(clank::Step::Pay(_)) => {
                entry.error = Some("clank.money did not accept the payment proof yet".into());
            }
            Err(e) => entry.error = Some(e.to_string()),
        }
        if let Some(e) = &entry.error {
            tracing::warn!(name = %entry.user_name, error = %e, "clank.money registration");
        }
        Ok(entry)
    }

    // ------------------------------------------------------------- DNS provider (Njalla)

    // ------------------------------------------------------------- Alby (NWC LNURL)

    /// Saves the owner's answers to the "add an address" questions (merged).
    pub async fn set_prefs(&self, p: Prefs) -> Result<Prefs, PublicNameError> {
        let ok = |v: &Option<String>, allowed: &[&str]| {
            v.as_deref().is_none_or(|v| allowed.contains(&v))
        };
        if !ok(&p.wallets, &["any", "modern", "both"])
            || !ok(&p.domain, &["yes", "buy", "no"])
            || !ok(&p.site, &["home", "server"])
        {
            return Err(PublicNameError::BadRequest("unknown answer".into()));
        }
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.prefs.wallets = p.wallets.or(s.prefs.wallets);
        s.prefs.domain = p.domain.or(s.prefs.domain);
        s.prefs.site = p.site.or(s.prefs.site);
        let out = s.prefs.clone();
        self.save(s).await?;
        Ok(out)
    }

    /// Forgets the answers, so the questions start over.
    pub async fn reset_prefs(&self) -> Result<(), PublicNameError> {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.prefs = Prefs::default();
        self.save(s).await
    }

    /// The connection string to paste on the service's website (Alby): a receive-only
    /// NWC connection whose invoices belong to `target` (Main, or an account). A new one,
    /// or for `connection_id` a new key for that one (the old string stops working: Allod
    /// does not keep connection secrets). Returns its id and the string.
    pub async fn alby_connection(
        &self,
        target: Option<i64>,
        connection_id: Option<i64>,
    ) -> Result<(i64, String), PublicNameError> {
        let apps = self
            .apps
            .get()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        if let Some(id) = connection_id {
            if !self.stored().albys.iter().any(|a| a.connection_id == id) {
                return Err(PublicNameError::BadRequest("no such Alby address".into()));
            }
            self.alby_ensure_scopes().await;
            let (_, uri) = apps
                .regenerate(id, now())
                .await
                .map_err(|e| PublicNameError::Internal(e.to_string()))?;
            return Ok((id, uri));
        }
        let mut name = "Alby Lightning address".to_string();
        if let Some(acct) = target {
            let a = apps
                .get(acct)
                .await
                .map_err(|_| PublicNameError::BadRequest("no such account".into()))?;
            if !a.is_account() || a.closed_at.is_some() {
                return Err(PublicNameError::BadRequest("not an open account".into()));
            }
            name = format!("Alby Lightning address ({})", a.name);
        }
        let mut metadata = serde_json::json!({"allod_kind": "alby"});
        if let Some(acct) = target {
            metadata["account"] = acct.into();
        }
        let (app, uri) = apps
            .create(
                crate::apps::NewApp {
                    name,
                    description:
                        "Receive-only: the address service asks for invoices for payments to it."
                            .into(),
                    scopes: ALBY_SCOPES.iter().map(|s| s.to_string()).collect(),
                    budget_renewal: Some("never".into()),
                    metadata: Some(metadata),
                    ..Default::default()
                },
                None,
                now(),
            )
            .await
            .map_err(|e| PublicNameError::Internal(e.to_string()))?;
        let uri = uri.ok_or_else(|| PublicNameError::Internal("no connection string".into()))?;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.albys.push(AlbyEntry {
            address: None,
            target,
            connection_id: app.id,
            verified: false,
            created_at: now(),
            error: None,
        });
        self.save(s).await?;
        Ok((app.id, uri))
    }

    /// Gives existing Alby connections the methods Alby needs (older ones lacked
    /// `list_transactions`).
    pub async fn alby_ensure_scopes(&self) {
        let Some(apps) = self.apps.get() else { return };
        for a in self.stored().albys {
            let Ok(app) = apps.get(a.connection_id).await else { continue };
            if ALBY_SCOPES.iter().all(|s| app.scopes.iter().any(|x| x == s)) {
                continue;
            }
            let patch = crate::apps::AppPatch {
                scopes: Some(ALBY_SCOPES.iter().map(|s| s.to_string()).collect()),
                ..Default::default()
            };
            match apps.update(app.id, patch, now()).await {
                Ok(_) => tracing::info!("Alby: connection methods updated"),
                Err(e) => {
                    tracing::warn!(error = %e, "Alby: could not update the connection's methods")
                }
            }
        }
    }

    /// Asks `address` for a small test invoice and checks this node made it, i.e. the
    /// service really requests invoices from this node.
    async fn alby_check(
        &self,
        address: &str,
        target: Option<i64>,
        connection_id: i64,
    ) -> Result<(), PublicNameError> {
        let wallet = self
            .wallet
            .get()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        let client = allod_lnurl::LnurlClient::new(self.cfg.lnurl_http);
        let p = client
            .resolve(address)
            .await
            .map_err(|e| PublicNameError::BadRequest(format!("{address} does not answer: {e}")))?;
        let amount = p.min_sendable_msat.max(1_000).min(p.max_sendable_msat);
        let invoice = client
            .invoice(&p, amount, None)
            .await
            .map_err(|e| PublicNameError::Upstream(format!("{address} gave no invoice: {e}")))?;
        let d = wallet
            .decode(&invoice)
            .await
            .map_err(|e| PublicNameError::Upstream(format!("unreadable invoice: {e}")))?;
        if !d.self_payment {
            return Err(PublicNameError::BadRequest(format!(
                "{address} answers with invoices from another wallet, not this node: connect it to the connection string Allod showed"
            )));
        }
        // And from the connection made for it: an account's invoice belongs to the
        // account; Main's to the connection itself (a non-isolated NWC connection makes
        // Main's invoices under its own id). Which relays the string lists does not matter.
        if let Ok(Some(tx)) = wallet.incoming_by_invoice(&invoice).await {
            if tx.app_id != Some(target.unwrap_or(connection_id)) {
                return Err(PublicNameError::BadRequest(format!(
                    "{address} asks this node through another connection: connect it to the connection string Allod showed for this address"
                )));
            }
        }
        Ok(())
    }

    /// Links the address the owner set up on the service's website for connection
    /// `connection_id`, after checking it requests invoices from that connection.
    pub async fn alby_link(
        &self,
        connection_id: i64,
        address: &str,
    ) -> Result<AlbyEntry, PublicNameError> {
        let address = address.trim().trim_start_matches('₿').to_ascii_lowercase();
        if !address.contains('@') || address.contains(char::is_whitespace) {
            return Err(PublicNameError::BadRequest(
                "enter the address, like you@getalby.com".into(),
            ));
        }
        let Some(mut entry) =
            self.stored().albys.into_iter().find(|a| a.connection_id == connection_id)
        else {
            return Err(PublicNameError::Conflict("get the connection string first".into()));
        };
        if self.stored().albys.iter().any(|a| {
            a.connection_id != connection_id && a.address.as_deref() == Some(address.as_str())
        }) {
            return Err(PublicNameError::Conflict(format!("{address} is already linked")));
        }
        self.alby_check(&address, entry.target, connection_id).await?;
        entry.address = Some(address);
        entry.verified = true;
        entry.error = None;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if let Some(e) = s.albys.iter_mut().find(|a| a.connection_id == connection_id) {
            *e = entry.clone();
        }
        self.save(s).await?;
        Ok(entry)
    }

    /// Forgets an Alby address and revokes its connection (the Alby account stays).
    pub async fn alby_remove(&self, connection_id: i64) -> Result<(), PublicNameError> {
        if !self.stored().albys.iter().any(|a| a.connection_id == connection_id) {
            return Err(PublicNameError::BadRequest("no such Alby address".into()));
        }
        if let (Some(apps), Some(wallet)) = (self.apps.get(), self.wallet.get()) {
            if let Err(e) = apps.delete(connection_id, wallet).await {
                tracing::warn!(error = %e, "Alby: could not revoke its connection");
            }
        }
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.albys.retain(|a| a.connection_id != connection_id);
        self.save(s).await
    }

    // ------------------------------------------------------------- ZEUS Pay

    /// Signs a fresh ZEUS Pay challenge with the node key.
    async fn zeus_auth(&self) -> Result<zeus::Auth, PublicNameError> {
        let wallet = self
            .wallet
            .get()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        let node = wallet.node();
        let pubkey = node
            .node_info()
            .await
            .map_err(|e| PublicNameError::Upstream(format!("node: {e}")))?
            .summary
            .pubkey;
        let message = self
            .zeus
            .challenge(&pubkey)
            .await
            .map_err(|e| PublicNameError::Upstream(e.to_string()))?;
        let signature = node
            .sign_message(message.as_bytes())
            .await
            .map_err(|e| PublicNameError::Upstream(format!("node cannot sign: {e}")))?;
        Ok(zeus::Auth { pubkey, message, signature })
    }

    /// Gets Main a ZEUS Pay address (SPEC §12.5): a receive-only NWC connection for
    /// ZEUS Pay to request invoices through, registered with a node-key signature, then
    /// renamed to `handle@domain` when asked. On a failed rename the address stays with
    /// the name ZEUS Pay picked, and the error says so.
    pub async fn zeus_register(
        &self,
        handle: Option<&str>,
        domain: Option<&str>,
        target: Option<i64>,
    ) -> Result<ZeusEntry, PublicNameError> {
        let wanted = match handle.map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()) {
            Some(h) if !zeus::valid_handle(&h) => {
                return Err(PublicNameError::BadRequest(
                    "use lowercase letters, digits, '.', '_' or '-'".into(),
                ))
            }
            other => other,
        };
        let domain = domain.unwrap_or(zeus::DOMAINS[0]);
        if !zeus::DOMAINS.contains(&domain) {
            return Err(PublicNameError::BadRequest(format!(
                "ZEUS Pay offers {}",
                zeus::DOMAINS.join(" and ")
            )));
        }
        if self.stored().zeus.is_some() {
            return Err(PublicNameError::Conflict(
                "this node has a ZEUS Pay address already (one per node)".into(),
            ));
        }
        let apps = self
            .apps
            .get()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        let (name, metadata) = self.zeus_connection_meta(target).await?;
        let auth = self.zeus_auth().await?;
        let (app, uri) = apps
            .create(
                crate::apps::NewApp {
                    name,
                    description:
                        "Receive-only: ZEUS Pay asks for invoices for payments to the address."
                            .into(),
                    scopes: vec!["make_invoice".into(), "lookup_invoice".into(), "get_info".into()],
                    budget_renewal: Some("never".into()),
                    metadata: Some(metadata),
                    ..Default::default()
                },
                None,
                now(),
            )
            .await
            .map_err(|e| PublicNameError::Internal(e.to_string()))?;
        let uri = uri.ok_or_else(|| PublicNameError::Internal("no connection string".into()))?;
        let address = match self.zeus.create_nwc(&auth, &uri).await {
            Ok(a) => a,
            Err(e) => {
                if let Some(wallet) = self.wallet.get() {
                    let _ = apps.delete(app.id, wallet).await;
                }
                return Err(PublicNameError::Upstream(e.to_string()));
            }
        };
        let mut entry = ZeusEntry {
            handle: address.handle,
            domain: address.domain,
            target,
            connection_id: app.id,
            verified: false,
            created_at: now(),
            plus_expires_at: None,
            plus_fee_sats: None,
            plus_checked_at: 0,
            auto_renew: false,
            renew_max_sats: None,
            renew_tried_at: 0,
            plus_attempt: None,
            plus_error: None,
        };
        if let Ok(st) = self.zeus.status(&auth).await {
            entry.plus_expires_at = st.plus_expires_at;
            entry.plus_fee_sats = st.plus_fee_sats;
            entry.plus_checked_at = now();
        }
        let mut rename_error = None;
        if wanted.as_deref().is_some_and(|h| h != entry.handle) || domain != entry.domain {
            let h = wanted.clone().unwrap_or_else(|| entry.handle.clone());
            match self.zeus.rename(&auth, &h, domain).await {
                Ok(a) => {
                    entry.handle = a.handle;
                    entry.domain = a.domain;
                }
                Err(e) => rename_error = Some(e.to_string()),
            }
        }
        {
            let _g = self.run_lock.lock().await;
            let mut s = self.stored();
            s.zeus = Some(entry.clone());
            self.save(s).await?;
        }
        self.trigger();
        match rename_error {
            Some(e) => Err(PublicNameError::Conflict(format!(
                "registered as {}@{}, but the name you chose was refused: {e}",
                entry.handle, entry.domain
            ))),
            None => Ok(entry),
        }
    }

    /// Renames Main's ZEUS Pay address.
    /// The ZEUS Pay connection's name and metadata for `target` (Main, or an open
    /// account whose invoices it then makes).
    async fn zeus_connection_meta(
        &self,
        target: Option<i64>,
    ) -> Result<(String, serde_json::Value), PublicNameError> {
        let mut name = "ZEUS Pay Lightning address".to_string();
        let mut metadata = serde_json::json!({"allod_kind": "zeus_pay"});
        if let Some(acct) = target {
            let apps = self
                .apps
                .get()
                .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
            let a = apps
                .get(acct)
                .await
                .map_err(|_| PublicNameError::BadRequest("no such account".into()))?;
            if !a.is_account() || a.closed_at.is_some() {
                return Err(PublicNameError::BadRequest("not an open account".into()));
            }
            name = format!("ZEUS Pay Lightning address ({})", a.name);
            metadata["account"] = acct.into();
        }
        Ok((name, metadata))
    }

    /// Points the ZEUS Pay address at another account (or Main): its connection makes
    /// that account's invoices from now on. Nothing changes at ZEUS Pay.
    pub async fn zeus_set_target(&self, target: Option<i64>) -> Result<ZeusEntry, PublicNameError> {
        let Some(mut entry) = self.stored().zeus else {
            return Err(PublicNameError::BadRequest("no ZEUS Pay address".into()));
        };
        let apps = self
            .apps
            .get()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        let (name, metadata) = self.zeus_connection_meta(target).await?;
        let patch = crate::apps::AppPatch {
            name: Some(name),
            metadata: Some(metadata),
            ..Default::default()
        };
        apps.update(entry.connection_id, patch, now())
            .await
            .map_err(|e| PublicNameError::Internal(e.to_string()))?;
        entry.target = target;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if let Some(z) = s.zeus.as_mut() {
            z.target = target;
        }
        self.save(s).await?;
        tracing::info!(target = ?target, "ZEUS Pay: address now pays into another account");
        Ok(entry)
    }

    pub async fn zeus_rename(
        &self,
        handle: &str,
        domain: &str,
    ) -> Result<ZeusEntry, PublicNameError> {
        let handle = handle.trim().to_ascii_lowercase();
        if !zeus::valid_handle(&handle) {
            return Err(PublicNameError::BadRequest(
                "use lowercase letters, digits, '.', '_' or '-'".into(),
            ));
        }
        if !zeus::DOMAINS.contains(&domain) {
            return Err(PublicNameError::BadRequest(format!(
                "ZEUS Pay offers {}",
                zeus::DOMAINS.join(" and ")
            )));
        }
        let Some(mut entry) = self.stored().zeus else {
            return Err(PublicNameError::BadRequest("Main has no ZEUS Pay address".into()));
        };
        let auth = self.zeus_auth().await?;
        let a = self
            .zeus
            .rename(&auth, &handle, domain)
            .await
            .map_err(|e| PublicNameError::Upstream(e.to_string()))?;
        entry.handle = a.handle;
        entry.domain = a.domain;
        entry.verified = false;
        entry.created_at = now();
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.zeus = Some(entry.clone());
        self.save(s).await?;
        drop(_g);
        self.trigger();
        Ok(entry)
    }

    async fn put_zeus(&self, entry: ZeusEntry) -> Result<(), PublicNameError> {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.zeus = Some(entry);
        self.save(s).await
    }

    /// Reads the address and ZEUS Pay+ state from ZEUS Pay and stores them.
    pub async fn zeus_refresh(&self) -> Result<ZeusEntry, PublicNameError> {
        let Some(mut entry) = self.stored().zeus else {
            return Err(PublicNameError::BadRequest("Main has no ZEUS Pay address".into()));
        };
        let auth = self.zeus_auth().await?;
        let st =
            self.zeus.status(&auth).await.map_err(|e| PublicNameError::Upstream(e.to_string()))?;
        if let Some(a) = st.address {
            if a.handle != entry.handle || a.domain != entry.domain {
                entry.verified = false;
            }
            entry.handle = a.handle;
            entry.domain = a.domain;
        }
        entry.plus_expires_at = st.plus_expires_at;
        entry.plus_fee_sats = st.plus_fee_sats.or(entry.plus_fee_sats);
        entry.plus_checked_at = now();
        self.put_zeus(entry.clone()).await?;
        Ok(entry)
    }

    /// Buys (or extends by) one year of ZEUS Pay+, paid from Main, at most
    /// `max_price_sats` (SPEC §12.5). The invoice is checked against ZEUS's price
    /// before paying.
    pub async fn zeus_buy_plus(&self, max_price_sats: u64) -> Result<ZeusEntry, PublicNameError> {
        let _buying = self.zeus_buying.try_lock().map_err(|_| {
            PublicNameError::Conflict("a ZEUS Pay+ payment is already in progress".into())
        })?;
        let mut entry = self.zeus_refresh().await?;
        let wallet = self
            .wallet
            .get()
            .cloned()
            .ok_or_else(|| PublicNameError::Conflict("the wallet is not running yet".into()))?;
        if entry.plus_attempt.is_some() {
            return self.resolve_zeus_plus_attempt(entry, &wallet).await;
        }
        let fee = entry.plus_fee_sats.ok_or_else(|| {
            PublicNameError::Upstream("ZEUS Pay did not say what ZEUS Pay+ costs".into())
        })?;
        if fee > max_price_sats {
            return Err(PublicNameError::Conflict(format!(
                "ZEUS Pay+ costs {fee} sats a year, more than the {max_price_sats} sats allowed"
            )));
        }
        let auth = self.zeus_auth().await?;
        let order = self
            .zeus
            .plus_order(&auth)
            .await
            .map_err(|e| PublicNameError::Upstream(e.to_string()))?;
        let d = wallet.decode(&order.bolt11).await.map_err(|e| {
            PublicNameError::Upstream(format!("ZEUS Pay sent an unreadable invoice: {e}"))
        })?;
        let amount = d.amount_msat.unwrap_or(0);
        if d.kind != "bolt11" || !d.network_ok || d.is_expired || amount != fee.saturating_mul(1000)
        {
            return Err(PublicNameError::Upstream(format!(
                "ZEUS Pay sent an invoice that does not match the price of {fee} sats"
            )));
        }
        let invoice_hash = d.payment_hash.ok_or_else(|| {
            PublicNameError::Upstream("ZEUS Pay invoice has no payment hash".into())
        })?;
        let ordered_at = now();
        let expected_expires_at = entry
            .plus_expires_at
            .unwrap_or(ordered_at)
            .max(ordered_at)
            .saturating_add(360 * 86_400);
        let payment_id = ulid::Ulid::new().to_string();
        entry.plus_attempt = Some(ZeusPlusAttempt {
            order_id: order.id,
            invoice: order.bolt11,
            invoice_hash,
            payment_id: Some(payment_id),
            expected_expires_at,
        });
        entry.plus_error = None;
        // Persist provider/invoice identity before wallet reservation or submission.
        self.put_zeus(entry.clone()).await?;
        self.resolve_zeus_plus_attempt(entry, &wallet).await
    }

    async fn resolve_zeus_plus_attempt(
        &self,
        mut entry: ZeusEntry,
        wallet: &Arc<Wallet>,
    ) -> Result<ZeusEntry, PublicNameError> {
        let attempt = entry.plus_attempt.clone().ok_or_else(|| {
            PublicNameError::Internal("ZEUS Pay+ attempt identity is missing".into())
        })?;
        let mut tx = match attempt.payment_id.as_deref() {
            Some(id) => match wallet.transaction(id).await {
                Ok(tx) => Some(tx),
                Err(crate::wallet::WalletError::NotFound) => wallet
                    .outgoing_by_hash(&attempt.invoice_hash)
                    .await
                    .map_err(|e| PublicNameError::Internal(e.to_string()))?,
                Err(e) => return Err(PublicNameError::Internal(e.to_string())),
            },
            None => wallet
                .outgoing_by_hash(&attempt.invoice_hash)
                .await
                .map_err(|e| PublicNameError::Internal(e.to_string()))?,
        };
        if tx.is_none() {
            let result = wallet
                .pay(crate::wallet::PayRequest {
                    app_id: None,
                    transaction_id: attempt.payment_id.clone(),
                    payment: attempt.invoice.clone(),
                    metadata: Some(
                        serde_json::json!({"recipient_data": {"identifier": "ZEUS Pay+"},
                            "comment": format!("ZEUS Pay+ for {}@{}", entry.handle, entry.domain),
                            "order_id": attempt.order_id})
                        .to_string(),
                    ),
                    ..Default::default()
                })
                .await;
            tx = match result {
                Ok(tx) => Some(tx),
                Err(e) => {
                    let found = wallet
                        .outgoing_by_hash(&attempt.invoice_hash)
                        .await
                        .map_err(|lookup| PublicNameError::Internal(lookup.to_string()))?;
                    let Some(found) = found else {
                        entry.plus_attempt = None;
                        entry.plus_error = Some(format!("paying for ZEUS Pay+ failed: {e}"));
                        self.put_zeus(entry).await?;
                        return Err(PublicNameError::Conflict(format!(
                            "paying for ZEUS Pay+ failed: {e}"
                        )));
                    };
                    Some(found)
                }
            };
        }
        let tx = tx.expect("ZEUS Pay+ transaction is present");
        if entry.plus_attempt.as_ref().and_then(|a| a.payment_id.as_deref())
            != Some(tx.public_id.as_str())
        {
            entry.plus_attempt.as_mut().expect("attempt").payment_id = Some(tx.public_id.clone());
            self.put_zeus(entry.clone()).await?;
        }
        if tx.state == "PENDING" {
            entry.auto_renew = false;
            entry.plus_error = Some(format!(
                "ZEUS Pay+ order {} has an unresolved payment; automatic renewal stopped",
                attempt.order_id
            ));
            self.put_zeus(entry.clone()).await?;
            return Ok(entry);
        }
        let tx = wait_final(wallet, tx).await;
        match tx.state.as_str() {
            "FAILED" => {
                let why = tx.failure_reason.unwrap_or_else(|| "payment failed".into());
                entry.plus_attempt = None;
                entry.plus_error = Some(format!("paying for ZEUS Pay+ failed: {why}"));
                self.put_zeus(entry).await?;
                return Err(PublicNameError::Conflict(format!(
                    "paying for ZEUS Pay+ failed: {why}"
                )));
            }
            "SETTLED" => {}
            _ => {
                entry.auto_renew = false;
                entry.plus_error = Some(format!(
                    "ZEUS Pay+ order {} has an unresolved payment; automatic renewal stopped",
                    attempt.order_id
                ));
                self.put_zeus(entry.clone()).await?;
                return Ok(entry);
            }
        }
        // ZEUS Pay records the payment shortly after it settles.
        for _ in 0..5 {
            entry = self.zeus_refresh().await?;
            if entry.plus_expires_at.is_some_and(|end| end >= attempt.expected_expires_at) {
                break;
            }
            tokio::time::sleep(PLUS_CONFIRM_DELAY).await;
        }
        if entry.plus_expires_at.is_some_and(|end| end >= attempt.expected_expires_at) {
            entry.plus_attempt = None;
            entry.plus_error = None;
        } else {
            entry.auto_renew = false;
            entry.plus_error = Some(format!(
                "ZEUS Pay+ order {} was paid, but expected subscription extension is unconfirmed; automatic renewal stopped",
                attempt.order_id
            ));
        }
        self.put_zeus(entry.clone()).await?;
        Ok(entry)
    }

    /// Turns automatic ZEUS Pay+ renewal on (with a price cap) or off.
    pub async fn zeus_set_auto_renew(
        &self,
        enabled: bool,
        max_price_sats: Option<u64>,
    ) -> Result<ZeusEntry, PublicNameError> {
        let Some(mut entry) = self.stored().zeus else {
            return Err(PublicNameError::BadRequest("Main has no ZEUS Pay address".into()));
        };
        if enabled && max_price_sats.unwrap_or(0) == 0 {
            return Err(PublicNameError::BadRequest(
                "set the most Allod may pay for a year of ZEUS Pay+".into(),
            ));
        }
        entry.auto_renew = enabled;
        if max_price_sats.is_some() {
            entry.renew_max_sats = max_price_sats;
        }
        self.put_zeus(entry.clone()).await?;
        Ok(entry)
    }

    /// Background: refresh the ZEUS Pay+ status twice a day, and renew it shortly
    /// before it ends when the owner turned that on.
    async fn zeus_upkeep(&self) {
        let Some(entry) = self.stored().zeus else { return };
        let t = now();
        let due = entry.auto_renew
            && entry.plus_expires_at.is_some_and(|e| e - t < PLUS_RENEW_BEFORE_SECS)
            && t - entry.renew_tried_at > 6 * 3600;
        if due {
            let mut tried = entry.clone();
            tried.renew_tried_at = t;
            if self.put_zeus(tried).await.is_err() {
                return;
            }
            let max = entry.renew_max_sats.unwrap_or(0);
            match self.zeus_buy_plus(max).await {
                Ok(e) if e.plus_attempt.is_none() => {
                    tracing::info!(until = ?e.plus_expires_at, "ZEUS Pay+ renewed")
                }
                Ok(e) => {
                    tracing::warn!(error = ?e.plus_error, "ZEUS Pay+ renewal unresolved")
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ZEUS Pay+ renewal failed");
                    if let Some(mut cur) = self.stored().zeus {
                        cur.plus_error = Some(format!("automatic renewal failed: {e}"));
                        let _ = self.put_zeus(cur).await;
                    }
                }
            }
        } else if t - entry.plus_checked_at > PLUS_CHECK_SECS {
            if let Err(e) = self.zeus_refresh().await {
                tracing::info!(error = %e, "ZEUS Pay status");
            }
        }
    }

    /// Gives the ZEUS Pay address up at ZEUS and revokes its connection. With `force`,
    /// a ZEUS Pay error does not stop the local removal (the connection is revoked, so
    /// the address stops getting invoices either way).
    pub async fn zeus_remove(&self, force: bool) -> Result<(), PublicNameError> {
        let Some(entry) = self.stored().zeus else {
            return Err(PublicNameError::BadRequest("Main has no ZEUS Pay address".into()));
        };
        let remote = match self.zeus_auth().await {
            Ok(auth) => {
                self.zeus.delete(&auth).await.map_err(|e| PublicNameError::Upstream(e.to_string()))
            }
            Err(e) => Err(e),
        };
        if let Err(e) = remote {
            if !force {
                return Err(e);
            }
            tracing::warn!(error = %e, "ZEUS Pay: could not delete the address there");
        }
        if let (Some(apps), Some(wallet)) = (self.apps.get(), self.wallet.get()) {
            if let Err(e) = apps.delete(entry.connection_id, wallet).await {
                tracing::warn!(error = %e, "ZEUS Pay: could not revoke its connection");
            }
        }
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.zeus = None;
        self.save(s).await
    }

    // ------------------------------------------------------------- view

    pub fn view(&self) -> Value {
        let s = self.stored();
        let st = self.status.lock().unwrap();
        serde_json::json!({
            "status": {
                "errors": st.errors,
                "synced_at": st.synced_at,
            },
            "twelve_cash": s.twelve.iter().map(|(k, e)| serde_json::json!({
                "target": k,
                "address": format!("{}@{}", e.user_name, e.domain),
                "offer_id": e.offer_id,
                "custom": e.custom,
                "verified": e.verified,
                "active": !st.twelve_inactive.contains(k),
                "created_at": e.created_at,
                "purchase": e.purchase.as_ref().map(|p| serde_json::json!({
                    "state": if p.payment_id.is_some() { "paid" } else { "paying" },
                    "price_sats": p.price_sats,
                    "error": p.error,
                })),
            })).collect::<Vec<_>>(),
            "prefs": s.prefs,
            "alby": s.albys.iter().map(|a| serde_json::json!({
                "target": a.target.map_or_else(|| "main".to_string(), |t| t.to_string()),
                "address": a.address,
                "connection_id": a.connection_id,
                "verified": a.verified,
                "created_at": a.created_at,
                "error": a.error,
            })).collect::<Vec<_>>(),
            "zeus": s.zeus.as_ref().map(|z| serde_json::json!({
                "target": z.target.map_or_else(|| "main".to_string(), |t| t.to_string()),
                "address": format!("{}@{}", z.handle, z.domain),
                "handle": z.handle,
                "domain": z.domain,
                "connection_id": z.connection_id,
                "verified": z.verified,
                "created_at": z.created_at,
                "plus_active": z.plus_expires_at.is_some_and(|e| e > now()),
                "plus_expires_at": z.plus_expires_at,
                "plus_fee_sats": z.plus_fee_sats,
                "auto_renew": z.auto_renew,
                "renew_max_sats": z.renew_max_sats,
                "plus_error": z.plus_error,
            })),
            "clank": s.clank.iter().map(|(k, e)| serde_json::json!({
                "target": k,
                "address": format!("{}@{}", e.user_name, clank::DOMAIN),
                "offer_id": e.offer_id,
                "state": e.state,
                "price_sats": e.price_sats,
                "verified": e.verified,
                "active": !st.clank_inactive.contains(k),
                "error": e.error,
                "created_at": e.created_at,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Waits up to a minute for a payment to settle or fail.
async fn wait_final(wallet: &Wallet, tx: crate::wallet::Tx) -> crate::wallet::Tx {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut events = wallet.subscribe();
    let mut cur = tx;
    while cur.state == "PENDING" && tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        if let Ok(t) = wallet.transaction(&cur.public_id).await {
            cur = t;
        }
    }
    cur
}

pub(crate) fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

#[cfg(test)]
mod tests;
