//! Public name for Lightning addresses (SPEC §12.1): a free `*.dedyn.io` name from
//! deSEC (signed up from inside Allod) or the owner's own domain, BIP-353 records for
//! every account with a username, and, with exposure `public`, address records that
//! follow this machine's public IP.

pub mod desec;
pub mod twelve;

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use allod_db::Db;
use allod_keys::SecretStore;
use allod_types::Platform;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, OnceCell};
use zeroize::Zeroizing;

use crate::wallet::{OfferRequest, OfferRow};
use crate::{AppService, Wallet};
pub use desec::{Captcha, Desec, DesecError, Endpoints};
pub use twelve::{TwelveCash, TwelveError};

/// `config` key holding [`Stored`] as JSON.
pub const CONFIG_KEY: &str = "public_name";
/// Secret file: the long-lived, domain-restricted deSEC token.
pub const DESEC_TOKEN: &str = "desec_token";
/// Secret file: `{email, password, domain}` of the deSEC account Allod signed up.
pub const DESEC_ACCOUNT: &str = "desec_account";
/// Suffix of the free names.
pub const DEDYN: &str = "dedyn.io";

/// deSEC's activation link is valid this long.
const SIGNUP_VALID_SECS: i64 = 12 * 3600;
/// After a change, how long the certificate may take before "not reachable".
const REACH_GRACE_SECS: i64 = 600;
/// Background reconcile interval.
const EVERY: Duration = Duration::from_secs(300);
/// Minimum time between two reconciles, and between two activation attempts.
const MIN_GAP: Duration = Duration::from_secs(20);
const LOGIN_GAP: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    None,
    /// The owner's domain; the owner manages its DNS.
    Own,
    /// A `*.dedyn.io` name at deSEC; Allod manages its records.
    Desec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    /// No address records: BIP-353 only, the IP stays private.
    #[default]
    Private,
    /// Address records point at this machine; Caddy serves the name.
    Public,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub email: String,
    pub started_at: i64,
}

/// Persisted state (`config.public_name`). Secrets are in files, never here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub exposure: Exposure,
    /// deSEC signup waiting for the owner to click the activation link.
    #[serde(default)]
    pub pending: Option<Pending>,
    /// BIP-353 TXT RRsets Allod wrote at deSEC: subname → value.
    #[serde(default)]
    pub published: BTreeMap<String, String>,
    /// The long-lived offer behind each account's BIP-353 record: app id → offer id.
    #[serde(default)]
    pub offers: BTreeMap<i64, String>,
    #[serde(default)]
    pub ipv4: Option<String>,
    #[serde(default)]
    pub ipv6: Option<String>,
    #[serde(default)]
    pub ip_updated_at: Option<i64>,
    /// Apex A/AAAA deleted at deSEC (exposure `private`).
    #[serde(default)]
    pub addresses_cleared: bool,
    /// Apex AAAA deleted at deSEC (exposure `public` without IPv6).
    #[serde(default)]
    pub aaaa_cleared: bool,
    /// Last change of mode, name or exposure.
    #[serde(default)]
    pub changed_at: i64,
    /// twelve.cash addresses: `main` or the account id → entry (SPEC §12.3).
    #[serde(default)]
    pub twelve: BTreeMap<String, TwelveEntry>,
}

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
}

fn twelve_key(target: Option<i64>) -> String {
    target.map(|id| id.to_string()).unwrap_or_else(|| "main".into())
}

/// The long-lived variable-amount offer of an account (or Main): the known one, any
/// active one without amount, expiry or single use, or a new one.
async fn long_lived_offer(
    wallet: &Wallet,
    app_id: Option<i64>,
    name: &str,
    known: Option<&str>,
) -> Result<OfferRow, String> {
    let offers = wallet.offers(Some(app_id)).await.map_err(|e| e.to_string())?;
    let usable = |o: &&OfferRow| {
        o.state == "active" && o.amount_msat.is_none() && !o.single_use && o.expires_at.is_none()
    };
    if let Some(o) = offers
        .iter()
        .filter(usable)
        .find(|o| Some(o.offer_id.as_str()) == known)
        .or_else(|| offers.iter().find(usable))
    {
        return Ok(o.clone());
    }
    let description = match app_id {
        Some(_) => format!("Payment to {name}"),
        None => "Payment".to_string(),
    };
    wallet
        .create_offer(OfferRequest {
            app_id,
            amount_msat: None,
            description,
            expires_at: None,
            metadata: None,
        })
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Account {
    email: String,
    password: String,
    domain: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReachState {
    WaitingForCertificate,
    Reachable,
    NotReachable,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reach {
    pub state: ReachState,
    pub checked_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One account's Lightning address and its BIP-353 record.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub account_id: i64,
    pub account_name: String,
    pub username: String,
    pub address: String,
    /// Fully qualified record name, with the trailing dot.
    pub name: String,
    pub txt: String,
    /// Written at deSEC by Allod (always false with an own domain).
    pub published: bool,
}

#[derive(Debug, Default)]
struct Status {
    errors: Vec<String>,
    reach: Option<Reach>,
    records: Vec<Record>,
    /// twelve.cash entries whose offer is no longer active.
    twelve_inactive: Vec<String>,
    synced_at: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum PublicNameError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Desec(#[from] DesecError),
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
    /// `ALLOD_PUBLIC_URL`: when set, it is the name and the settings are read-only.
    pub env_public_url: Option<String>,
    pub platform: Option<Platform>,
    /// `ALLOD_PUBLIC_IPV6`: publish an AAAA record with exposure `public`.
    pub public_ipv6: bool,
    /// `ALLOD_TLS_CHECK_ADDR` (`caddy:443`): where to test the certificate from inside.
    pub tls_check_addr: Option<String>,
    pub endpoints: Endpoints,
    /// Tests: skip the DNS lookup that checks a `dedyn.io` name is free.
    pub skip_label_dns_check: bool,
    /// twelve.cash base URL; `None`: `https://twelve.cash`.
    pub twelve_cash_url: Option<String>,
}

pub struct PublicNameService {
    db: Db,
    secrets: SecretStore,
    desec: Desec,
    twelve: TwelveCash,
    cfg: PublicNameConfig,
    wallet: Arc<OnceCell<Arc<Wallet>>>,
    apps: Arc<OnceCell<Arc<AppService>>>,
    stored: Mutex<Stored>,
    status: Mutex<Status>,
    run_lock: tokio::sync::Mutex<()>,
    wake: Notify,
    last_run: Mutex<Option<Instant>>,
    last_login: Mutex<Option<Instant>>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A `dedyn.io` label: `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`.
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

/// RRset changes that turn `published` into `desired` (new, changed and deleted names).
pub fn txt_changes(
    published: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> Vec<Value> {
    let mut out: Vec<Value> = desired
        .iter()
        .filter(|(k, v)| published.get(*k) != Some(*v))
        .map(|(k, v)| desec::txt_rrset(k, Some(v)))
        .collect();
    out.extend(
        published.keys().filter(|k| !desired.contains_key(*k)).map(|k| desec::txt_rrset(k, None)),
    );
    out
}

/// Whether a `dedyn.io` label is free: NXDOMAIN means no zone of that name exists.
/// `None` when DNS could not tell.
pub async fn label_available(label: &str) -> Option<bool> {
    use hickory_resolver::proto::rr::RecordType;
    let resolver = hickory_resolver::Resolver::builder_tokio().ok()?.build().ok()?;
    match resolver.lookup(format!("{label}.{DEDYN}."), RecordType::SOA).await {
        Ok(_) => Some(false),
        Err(e) if e.is_nx_domain() => Some(true),
        Err(e) if e.is_no_records_found() => Some(false),
        Err(_) => None,
    }
}

impl PublicNameService {
    pub async fn load(
        db: Db,
        secrets: SecretStore,
        cfg: PublicNameConfig,
        wallet: Arc<OnceCell<Arc<Wallet>>>,
        apps: Arc<OnceCell<Arc<AppService>>>,
    ) -> Result<Arc<Self>, PublicNameError> {
        let stored = match db.config_get(CONFIG_KEY).await? {
            Some(e) => serde_json::from_str(&e.value).unwrap_or_default(),
            None => Stored::default(),
        };
        Ok(Arc::new(PublicNameService {
            db,
            secrets,
            desec: Desec::new(cfg.endpoints.clone()),
            twelve: TwelveCash::new(
                cfg.twelve_cash_url.as_deref().unwrap_or("https://twelve.cash"),
            ),
            cfg,
            wallet,
            apps,
            stored: Mutex::new(stored),
            status: Mutex::new(Status::default()),
            run_lock: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            last_run: Mutex::new(None),
            last_login: Mutex::new(None),
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

    fn token(&self) -> Option<Zeroizing<String>> {
        self.secrets.read(DESEC_TOKEN).ok().flatten().filter(|t| !t.is_empty())
    }

    fn account(&self) -> Option<Account> {
        let text = self.secrets.read(DESEC_ACCOUNT).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    fn env_locked(&self) -> Result<(), PublicNameError> {
        if self.cfg.env_public_url.is_some() {
            return Err(PublicNameError::Conflict(
                "the public name is set by ALLOD_PUBLIC_URL on this installation".into(),
            ));
        }
        Ok(())
    }

    /// Whether exposure `public` is offered with `mode`. Umbrel and StartOS publish
    /// services through their own proxies (SPEC §12.1).
    pub fn public_allowed(&self, mode: Mode) -> bool {
        mode == Mode::Own
            || !matches!(self.cfg.platform, Some(Platform::Umbrel | Platform::Startos))
    }

    /// twelve.cash serves mainnet only; a test stand-in (`twelve_cash_url`) any network.
    pub fn twelve_any_network(&self) -> bool {
        self.cfg.twelve_cash_url.is_some()
    }

    /// The domain of Lightning addresses and BIP-353 records, if any.
    pub fn lightning_domain(&self) -> Option<String> {
        if let Some(url) = &self.cfg.env_public_url {
            return url_domain(url);
        }
        let s = self.stored.lock().unwrap();
        match s.mode {
            Mode::None => None,
            _ => s.name.clone(),
        }
    }

    /// Caddy's on-demand TLS `ask`: a certificate only for the current public name.
    pub fn allows_certificate(&self, domain: &str) -> bool {
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        let s = self.stored.lock().unwrap();
        s.mode != Mode::None && s.exposure == Exposure::Public && s.name.as_deref() == Some(&domain)
    }

    /// Remembers the offer created for an account's BIP-353 record and republishes.
    pub async fn note_offer(&self, app_id: i64, offer_id: &str) {
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.offers.insert(app_id, offer_id.to_string());
        if let Err(e) = self.save(s).await {
            tracing::warn!(error = %e, "public name: cannot store the BIP-353 offer");
        }
        drop(_g);
        self.trigger();
    }

    /// Asks the background task to reconcile soon (accounts or settings changed).
    pub fn trigger(&self) {
        self.wake.notify_one();
    }

    /// Runs forever: reconciles every 5 minutes, or soon after [`Self::trigger`].
    pub fn start(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.reconcile().await;
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

    // ------------------------------------------------------------- owner actions

    /// A captcha for the signup form.
    pub async fn captcha(&self) -> Result<Captcha, PublicNameError> {
        self.env_locked()?;
        Ok(self.desec.captcha().await?)
    }

    /// Signs the owner up at deSEC with `<label>.dedyn.io`. The account is usable once
    /// the owner clicks the link deSEC emails; [`Self::poll_activation`] picks it up.
    pub async fn signup(
        &self,
        label: &str,
        email: &str,
        captcha: (&str, &str),
    ) -> Result<(), PublicNameError> {
        self.env_locked()?;
        let label = label.trim().to_ascii_lowercase();
        if !valid_label(&label) {
            return Err(PublicNameError::BadRequest(
                "use 1–63 lowercase letters, digits or hyphens (not at the start or end)".into(),
            ));
        }
        let email = email.trim();
        if !email.contains('@') || email.len() > 254 || email.contains(char::is_whitespace) {
            return Err(PublicNameError::BadRequest("enter a valid email address".into()));
        }
        if !self.cfg.skip_label_dns_check && label_available(&label).await == Some(false) {
            return Err(PublicNameError::Conflict(format!("{label}.{DEDYN} is already taken")));
        }
        let domain = format!("{label}.{DEDYN}");
        let password = hex::encode(rand::random::<[u8; 32]>());
        let account = Account { email: email.into(), password, domain: domain.clone() };
        let json = serde_json::to_string(&account)
            .map_err(|e| PublicNameError::Internal(e.to_string()))?;
        // Stored first, so a restart during signup still finds the account.
        self.secrets.write(DESEC_ACCOUNT, &json)?;
        self.desec.register(email, &account.password, captcha, &domain).await?;
        let _g = self.run_lock.lock().await;
        self.secrets.remove(DESEC_TOKEN)?;
        let mut s = self.stored();
        self.switch_name(&mut s, Mode::Desec, Some(domain));
        s.pending = Some(Pending { email: email.into(), started_at: now() });
        self.save(s).await?;
        self.status.lock().unwrap().errors.clear();
        Ok(())
    }

    /// Uses an existing deSEC account through a token the owner pasted.
    pub async fn use_token(&self, name: &str, token: &str) -> Result<(), PublicNameError> {
        self.env_locked()?;
        let name = normalize_domain(name)?;
        let token = token.trim();
        if token.is_empty() || token.contains(char::is_whitespace) {
            return Err(PublicNameError::BadRequest("paste the deSEC token".into()));
        }
        match self.desec.domain_exists(token, &name).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(PublicNameError::BadRequest(format!(
                    "{name} is not a domain of this deSEC account"
                )))
            }
            Err(DesecError::Unauthorized) => {
                return Err(PublicNameError::BadRequest("deSEC does not accept this token".into()))
            }
            Err(e) => return Err(e.into()),
        }
        let _g = self.run_lock.lock().await;
        self.secrets.write(DESEC_TOKEN, token)?;
        if self.account().is_some_and(|a| a.domain != name) {
            self.secrets.remove(DESEC_ACCOUNT)?;
        }
        let mut s = self.stored();
        self.switch_name(&mut s, Mode::Desec, Some(name));
        s.pending = None;
        self.save(s).await?;
        drop(_g);
        self.trigger();
        Ok(())
    }

    /// Uses the deSEC account Allod signed up earlier again (after turning it off).
    pub async fn resume_desec(&self) -> Result<(), PublicNameError> {
        self.env_locked()?;
        let account = self
            .account()
            .ok_or_else(|| PublicNameError::Conflict("no deSEC account to use again".into()))?;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        self.switch_name(&mut s, Mode::Desec, Some(account.domain));
        s.pending = Some(Pending { email: account.email, started_at: now() });
        self.save(s).await?;
        drop(_g);
        self.poll_activation().await;
        Ok(())
    }

    /// The owner's own domain.
    pub async fn use_own(&self, name: &str, exposure: Exposure) -> Result<(), PublicNameError> {
        self.env_locked()?;
        let name = normalize_domain(name)?;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        self.leave_desec(&mut s).await;
        self.switch_name(&mut s, Mode::Own, Some(name));
        s.exposure = exposure;
        self.save(s).await?;
        drop(_g);
        self.trigger();
        Ok(())
    }

    pub async fn set_exposure(&self, exposure: Exposure) -> Result<(), PublicNameError> {
        self.env_locked()?;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if s.mode == Mode::None {
            return Err(PublicNameError::Conflict("choose a name first".into()));
        }
        if exposure == Exposure::Public && !self.public_allowed(s.mode) {
            return Err(PublicNameError::BadRequest(
                "this platform publishes Allod through its own proxy; use your own domain there"
                    .into(),
            ));
        }
        if s.exposure != exposure {
            s.exposure = exposure;
            s.addresses_cleared = false;
            s.aaaa_cleared = false;
            s.ipv4 = None;
            s.ipv6 = None;
            s.changed_at = now();
            self.status.lock().unwrap().reach = None;
        }
        self.save(s).await?;
        drop(_g);
        self.trigger();
        Ok(())
    }

    /// Turns the public name off. With deSEC, the BIP-353 records Allod wrote are
    /// deleted and the token is removed; the address record stays (SPEC §12.1).
    pub async fn disable(&self) -> Result<(), PublicNameError> {
        self.env_locked()?;
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        self.leave_desec(&mut s).await;
        self.switch_name(&mut s, Mode::None, None);
        self.save(s).await?;
        let mut st = self.status.lock().unwrap();
        *st = Status::default();
        Ok(())
    }

    /// The deSEC login Allod created, for the owner to sign in at desec.io.
    pub fn desec_credentials(&self) -> Option<(String, Zeroizing<String>)> {
        self.account().map(|a| (a.email, Zeroizing::new(a.password)))
    }

    fn switch_name(&self, s: &mut Stored, mode: Mode, name: Option<String>) {
        if s.mode != mode || s.name != name {
            s.mode = mode;
            s.name = name;
            s.pending = None;
            s.published.clear();
            s.addresses_cleared = false;
            s.aaaa_cleared = false;
            s.ipv4 = None;
            s.ipv6 = None;
            s.ip_updated_at = None;
            s.changed_at = now();
            if !self.public_allowed(mode) {
                s.exposure = Exposure::Private;
            }
            let mut st = self.status.lock().unwrap();
            st.reach = None;
            st.errors.clear();
        }
    }

    /// Best effort: delete the BIP-353 records Allod wrote, then forget the token.
    async fn leave_desec(&self, s: &mut Stored) {
        if s.mode != Mode::Desec {
            return;
        }
        if let (Some(token), Some(name)) = (self.token(), s.name.clone()) {
            let changes = txt_changes(&s.published, &BTreeMap::new());
            if let Err(e) = self.desec.put_rrsets(&token, &name, &changes).await {
                tracing::warn!(error = %e, "public name: could not delete the BIP-353 records");
            }
        }
        if let Err(e) = self.secrets.remove(DESEC_TOKEN) {
            tracing::warn!(error = %e, "public name: could not remove the deSEC token");
        }
    }

    /// While a signup waits for its email link: tries to log in (at most every 15 s),
    /// and on success creates the restricted token. Called by the status route so the
    /// page continues by itself, and by the background task.
    pub async fn poll_activation(&self) {
        {
            let mut last = self.last_login.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < LOGIN_GAP) {
                return;
            }
            *last = Some(Instant::now());
        }
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        if self.activate(&mut s).await {
            if let Err(e) = self.save(s).await {
                tracing::warn!(error = %e, "public name: cannot store state");
            }
            drop(_g);
            self.trigger();
        }
    }

    /// Returns true when the stored state changed.
    async fn activate(&self, s: &mut Stored) -> bool {
        if s.mode != Mode::Desec || self.token().is_some() {
            return false;
        }
        let Some(pending) = s.pending.clone() else { return false };
        if now() - pending.started_at > SIGNUP_VALID_SECS {
            s.pending = None;
            self.status.lock().unwrap().errors =
                vec!["The deSEC email link has expired. Start the signup again.".into()];
            return true;
        }
        let Some(account) = self.account() else { return false };
        let login = match self.desec.login(&account.email, &account.password).await {
            Ok(Some(t)) => Zeroizing::new(t),
            Ok(None) => return false,
            Err(e) => {
                tracing::info!(error = %e, "public name: deSEC login failed");
                return false;
            }
        };
        let result = self.desec.create_restricted_token(&login, &account.domain).await;
        if let Err(e) = self.desec.logout(&login).await {
            tracing::debug!(error = %e, "public name: deSEC logout failed");
        }
        match result {
            Ok(token) => match self.secrets.write(DESEC_TOKEN, &token) {
                Ok(()) => {
                    tracing::info!(domain = %account.domain, "public name: deSEC account activated");
                    s.pending = None;
                    true
                }
                Err(e) => {
                    self.status.lock().unwrap().errors =
                        vec![format!("cannot store the token: {e}")];
                    false
                }
            },
            Err(e) => {
                self.status.lock().unwrap().errors =
                    vec![format!("cannot create a deSEC token: {e}")];
                false
            }
        }
    }

    // ------------------------------------------------------------- reconcile

    /// Brings DNS in line with the settings: activation, address records, BIP-353
    /// records, and the reachability check.
    pub async fn reconcile(&self) {
        *self.last_run.lock().unwrap() = Some(Instant::now());
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        let before = serde_json::to_string(&s).unwrap_or_default();
        let mut errors = Vec::new();
        self.activate(&mut s).await;

        let domain = if self.cfg.env_public_url.is_some() {
            self.lightning_domain()
        } else if s.mode == Mode::None {
            None
        } else {
            s.name.clone()
        };
        let desired = match &domain {
            Some(d) => self.bip353_records(&mut s, d, &mut errors).await,
            None => Vec::new(),
        };

        let token = (s.mode == Mode::Desec && self.cfg.env_public_url.is_none())
            .then(|| self.token())
            .flatten();
        if let (Some(token), Some(name)) = (&token, s.name.clone()) {
            if let Err(e) = self.address_records(&mut s, token, &name).await {
                errors.push(format!("address records: {e}"));
            }
            let want: BTreeMap<String, String> =
                desired.iter().map(|r| (bip353_subname(&r.username), r.txt.clone())).collect();
            let changes = txt_changes(&s.published, &want);
            match self.desec.put_rrsets(token, &name, &changes).await {
                Ok(()) => s.published = want,
                Err(e) => errors.push(format!("BIP-353 records: {e}")),
            }
        }
        let records = desired
            .into_iter()
            .map(|mut r| {
                r.published =
                    token.is_some() && s.published.contains_key(&bip353_subname(&r.username));
                r
            })
            .collect();

        let twelve_inactive = self.twelve_check(&mut s).await;

        let public = s.mode != Mode::None && s.exposure == Exposure::Public
            || self.cfg.env_public_url.is_some();
        let reach = match (&domain, public) {
            (Some(d), true) => Some(self.check_reach(d, s.changed_at).await),
            _ => None,
        };

        if serde_json::to_string(&s).unwrap_or_default() != before {
            if let Err(e) = self.save(s).await {
                errors.push(format!("cannot store state: {e}"));
            }
        }
        let mut st = self.status.lock().unwrap();
        // Keep an activation error until the owner acts.
        if errors.is_empty() && st.errors.iter().any(|e| e.contains("deSEC email link")) {
            errors = st.errors.clone();
        }
        for e in &errors {
            tracing::warn!(error = %e, "public name");
        }
        st.errors = errors;
        st.records = records;
        st.twelve_inactive = twelve_inactive;
        st.reach = reach;
        st.synced_at = Some(now());
    }

    /// One BIP-353 record per open account with a username, each backed by a
    /// long-lived variable-amount offer owned by the account (SPEC §12).
    async fn bip353_records(
        &self,
        s: &mut Stored,
        domain: &str,
        errors: &mut Vec<String>,
    ) -> Vec<Record> {
        let (Some(apps), Some(wallet)) = (self.apps.get(), self.wallet.get()) else {
            return Vec::new();
        };
        let accounts = match apps.list_accounts().await {
            Ok(a) => a,
            Err(e) => {
                errors.push(format!("accounts: {e}"));
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        let mut keep = BTreeMap::new();
        for app in accounts.into_iter().filter(|a| a.closed_at.is_none()) {
            let Some(username) = app.lud16_username.clone() else { continue };
            let known = s.offers.get(&app.id).cloned();
            let offer =
                match long_lived_offer(wallet, Some(app.id), &app.name, known.as_deref()).await {
                    Ok(o) => o,
                    Err(e) => {
                        errors.push(format!("no BOLT12 offer for {username}: {e}"));
                        continue;
                    }
                };
            keep.insert(app.id, offer.offer_id.clone());
            out.push(Record {
                account_id: app.id,
                account_name: app.name.clone(),
                address: format!("{username}@{domain}"),
                name: format!("{}.{domain}.", bip353_subname(&username)),
                txt: format!("bitcoin:?lno={}", offer.offer),
                username,
                published: false,
            });
        }
        s.offers = keep;
        out
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
        if s.twelve.contains_key(&key) {
            return Err(PublicNameError::Conflict(
                "this account already has a twelve.cash address".into(),
            ));
        }
        let offer = long_lived_offer(&wallet, target, &name, None)
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
        };
        s.twelve.insert(key, entry.clone());
        self.save(s).await?;
        drop(_g);
        self.trigger();
        Ok(entry)
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
        let (wallet, name) = self.twelve_target(target).await?;
        let offer = long_lived_offer(&wallet, target, &name, None)
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
        };
        let _g = self.run_lock.lock().await;
        let mut s = self.stored();
        s.twelve.insert(twelve_key(target), entry.clone());
        self.save(s).await?;
        Ok(entry)
    }

    /// The offer a custom twelve.cash name must carry (created if needed).
    pub async fn twelve_offer(&self, target: Option<i64>) -> Result<String, PublicNameError> {
        let (wallet, name) = self.twelve_target(target).await?;
        long_lived_offer(&wallet, target, &name, None)
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

    /// Checks by DNS that new addresses resolve, and which ones still have an active
    /// offer (closed accounts disable theirs).
    async fn twelve_check(&self, s: &mut Stored) -> Vec<String> {
        let Some(wallet) = self.wallet.get() else { return Vec::new() };
        let active: std::collections::HashSet<String> = match wallet.offers(None).await {
            Ok(o) => o.into_iter().filter(|o| o.state == "active").map(|o| o.offer_id).collect(),
            Err(_) => return Vec::new(),
        };
        let mut inactive = Vec::new();
        for (key, e) in s.twelve.iter_mut() {
            if !active.contains(&e.offer_id) {
                inactive.push(key.clone());
                continue;
            }
            if !e.verified {
                if let Some(values) = twelve::lookup(&e.user_name, &e.domain).await {
                    e.verified = values.iter().any(|v| v.contains("lno="));
                }
            }
        }
        inactive
    }

    async fn address_records(
        &self,
        s: &mut Stored,
        token: &str,
        name: &str,
    ) -> Result<(), DesecError> {
        match s.exposure {
            Exposure::Private => {
                if !s.addresses_cleared {
                    self.desec
                        .put_rrsets(
                            token,
                            name,
                            &[desec::delete_rrset("A"), desec::delete_rrset("AAAA")],
                        )
                        .await?;
                    s.addresses_cleared = true;
                    s.ipv4 = None;
                    s.ipv6 = None;
                }
            }
            Exposure::Public => {
                let ipv4 = self.desec.public_ipv4().await?.to_string();
                let ipv6 = if self.cfg.public_ipv6 {
                    self.desec.public_ipv6().await.ok().map(|ip| ip.to_string())
                } else {
                    None
                };
                if s.ipv4.as_deref() != Some(&ipv4) || s.ipv6 != ipv6 {
                    self.desec
                        .update_ip(token, name, &ipv4, ipv6.as_deref().unwrap_or("preserve"))
                        .await?;
                    tracing::info!(%name, %ipv4, "public name: address updated");
                    s.ipv4 = Some(ipv4);
                    s.ipv6 = ipv6.clone();
                    s.ip_updated_at = Some(now());
                    s.addresses_cleared = false;
                }
                // A stale or unroutable AAAA breaks clients that prefer IPv6.
                if ipv6.is_none() && !s.aaaa_cleared {
                    self.desec.put_rrsets(token, name, &[desec::delete_rrset("AAAA")]).await?;
                    s.aaaa_cleared = true;
                }
            }
        }
        Ok(())
    }

    /// Fetches `https://{domain}/api/info` through Caddy (or public DNS): a valid
    /// certificate means Let's Encrypt reached this machine on port 443.
    async fn check_reach(&self, domain: &str, changed_at: i64) -> Reach {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
        if let Some(addr) = &self.cfg.tls_check_addr {
            if let Ok(Some(a)) = tokio::net::lookup_host(addr.as_str()).await.map(|mut a| a.next())
            {
                builder = builder.resolve(domain, a);
            }
        }
        let result = match builder.build() {
            Ok(c) => c.get(format!("https://{domain}/api/info")).send().await.map(|r| r.status()),
            Err(e) => Err(e),
        };
        let t = now();
        match result {
            Ok(status) if status.is_success() => {
                Reach { state: ReachState::Reachable, checked_at: t, detail: None }
            }
            other => {
                let detail = match other {
                    Ok(status) => format!("answered {status}"),
                    Err(e) => error_chain(&e),
                };
                let state = if t - changed_at < REACH_GRACE_SECS {
                    ReachState::WaitingForCertificate
                } else {
                    ReachState::NotReachable
                };
                Reach { state, checked_at: t, detail: Some(detail) }
            }
        }
    }

    // ------------------------------------------------------------- view

    pub fn view(&self) -> Value {
        let s = self.stored();
        let st = self.status.lock().unwrap();
        let account = self.account();
        let env = self.cfg.env_public_url.clone();
        let (mode, name, exposure) = match &env {
            Some(url) => (Mode::Own, url_domain(url), Exposure::Public),
            None => (s.mode, s.name.clone(), s.exposure),
        };
        serde_json::json!({
            "mode": mode,
            "name": name,
            "exposure": exposure,
            "env_public_url": env,
            "public_allowed": {
                "own": self.public_allowed(Mode::Own),
                "desec": self.public_allowed(Mode::Desec),
            },
            "lightning_domain": self.lightning_domain(),
            "desec": {
                "account_email": account.as_ref().map(|a| a.email.clone()),
                "account_domain": account.as_ref().map(|a| a.domain.clone()),
                "has_token": self.token().is_some(),
                "pending": s.pending.as_ref().map(|p| serde_json::json!({
                    "email": p.email,
                    "started_at": p.started_at,
                    "expires_at": p.started_at + SIGNUP_VALID_SECS,
                })),
            },
            "status": {
                "ipv4": s.ipv4,
                "ipv6": s.ipv6,
                "ip_updated_at": s.ip_updated_at,
                "errors": st.errors,
                "reach": st.reach,
                "synced_at": st.synced_at,
                "tls_check": self.cfg.tls_check_addr.is_some(),
            },
            "records": st.records,
            "twelve_cash": s.twelve.iter().map(|(k, e)| serde_json::json!({
                "target": k,
                "address": format!("{}@{}", e.user_name, e.domain),
                "offer_id": e.offer_id,
                "custom": e.custom,
                "verified": e.verified,
                "active": !st.twelve_inactive.contains(k),
                "created_at": e.created_at,
            })).collect::<Vec<_>>(),
        })
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
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
