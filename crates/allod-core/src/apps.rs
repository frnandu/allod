//! NWC connections ("apps"): scopes, budgets, visibility and wallet keys (SPEC §6.2,
//! §8.6, §9). Accounts (`kind = 'account'`, SPEC §10) share the table: they are isolated
//! apps with fixed scopes that the owner creates, funds and closes from the UI.

use std::sync::Arc;

use allod_keys::{KeyPair, NostrKeys};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::any::AnyRow;
use sqlx::{AnyPool, Row};
use tokio::sync::watch;

use crate::wallet::budget::{self, Budget, Renewal};
use crate::wallet::outbox;

/// Stored permission scopes (SPEC §9.1).
pub const SCOPES: &[&str] = &[
    "pay_invoice",
    "get_balance",
    "get_info",
    "make_invoice",
    "lookup_invoice",
    "list_transactions",
    "sign_message",
    "notifications",
    "superuser",
];

/// NWC methods Allod answers today. Methods of later milestones stay out of info events
/// and `get_info` until they are implemented.
pub const IMPLEMENTED_METHODS: &[&str] = &[
    "get_info",
    "get_budget",
    "get_balance",
    "pay_invoice",
    "multi_pay_invoice",
    "pay",
    "make_invoice",
    "make_hold_invoice",
    "settle_hold_invoice",
    "cancel_hold_invoice",
    "receive",
    "make_offer",
    "lookup_invoice",
    "lookup_payment",
    "list_transactions",
    "sign_message",
    "create_connection",
    "register_push",
    "unregister_push",
];

/// Hard caps for durable NWC request/response history. Counts include both tables;
/// bytes are conservative stored-payload estimates, not database page sizes.
pub const NWC_REQUEST_ROWS_PER_APP: i64 = 10_000;
pub const NWC_REQUEST_BYTES_PER_APP: i64 = 4 * 1024 * 1024;
pub const NWC_REQUEST_ROWS_GLOBAL: i64 = 50_000;
pub const NWC_REQUEST_BYTES_GLOBAL: i64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestAdmission {
    Inserted(i64),
    Duplicate,
    Full,
}

/// NWC extensions advertised for an app's effective methods (SPEC §8.1).
pub fn extensions(methods: &[String], notifications: bool) -> Vec<&'static str> {
    let has = |m: &str| methods.iter().any(|x| x == m);
    let mut out = Vec::new();
    if notifications {
        out.push("02");
    }
    if has("make_hold_invoice") {
        out.push("03");
    }
    if has("list_transactions") {
        out.push("05");
    }
    out.push("06");
    // Client-initiated connections (NWC-08) are open to every app.
    out.push("08");
    if has("lookup_payment") {
        out.push("09");
    }
    if has("make_offer") && has("lookup_payment") {
        out.push("12");
    }
    if has("pay") || has("receive") {
        out.push("321");
    }
    out
}

/// Scopes of every account (SPEC §10).
pub const ACCOUNT_SCOPES: &[&str] = &[
    "pay_invoice",
    "get_balance",
    "get_info",
    "make_invoice",
    "lookup_invoice",
    "list_transactions",
    "notifications",
];

/// Lightning address usernames that cannot be taken (SPEC §12).
const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "root",
    "allod",
    "api",
    "www",
    "mail",
    "support",
    "help",
    "info",
    "owner",
    "wallet",
    "main",
    "lnurlp",
    "well-known",
    "postmaster",
    "hostmaster",
    "abuse",
    "security",
    "noreply",
    "_",
];

/// Validates a Lightning address username (SPEC §12): `^[a-z0-9._-]{1,64}$`, not reserved.
pub fn validate_username(u: &str) -> Result<String, AppError> {
    let u = u.trim().to_ascii_lowercase();
    let ok = !u.is_empty()
        && u.len() <= 64
        && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !ok {
        return Err(AppError::BadRequest(
            "username must be 1 to 64 characters of a-z, 0-9, '.', '_' or '-'".into(),
        ));
    }
    if RESERVED_USERNAMES.contains(&u.as_str()) {
        return Err(AppError::BadRequest(format!("the username {u} is reserved")));
    }
    Ok(u)
}

/// "the username sats is already used by Sara" (`owner` is an account name or "Main").
pub fn taken_message(username: &str, owner: &str) -> String {
    format!("the username {username} is already used by {owner}")
}

/// Methods granted by a scope (SPEC §9.1).
pub fn scope_methods(scope: &str) -> &'static [&'static str] {
    match scope {
        "pay_invoice" => &["pay_invoice", "multi_pay_invoice", "pay"],
        "get_balance" => &["get_balance"],
        "make_invoice" => &[
            "make_invoice",
            "make_hold_invoice",
            "settle_hold_invoice",
            "cancel_hold_invoice",
            "receive",
            "make_offer",
        ],
        "lookup_invoice" => &["lookup_invoice", "lookup_payment"],
        "list_transactions" => &["list_transactions"],
        "sign_message" => &["sign_message"],
        "superuser" => &["create_connection"],
        // Push endpoints carry the connection's notifications (NWC-XX, SPEC §8.9).
        "notifications" => &["register_push", "unregister_push"],
        _ => &[],
    }
}

/// The scope a method needs, `None` for always-granted methods.
pub fn method_scope(method: &str) -> Option<&'static str> {
    if matches!(method, "get_info" | "get_budget") {
        return None;
    }
    SCOPES.iter().copied().find(|s| scope_methods(s).contains(&method))
}

/// Effective methods of an app (SPEC §9.2), limited to what Allod implements.
pub fn effective_methods(scopes: &[String], bolt12: bool) -> Vec<String> {
    let mut out: Vec<String> = vec!["get_info".into(), "get_budget".into()];
    for s in scopes {
        for m in scope_methods(s) {
            if !bolt12 && *m == "make_offer" {
                continue;
            }
            if IMPLEMENTED_METHODS.contains(m) && !out.iter().any(|x| x == m) {
                out.push((*m).into());
            }
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<crate::WalletError> for AppError {
    fn from(e: crate::WalletError) -> Self {
        match e {
            crate::WalletError::NotFound => AppError::NotFound,
            other => AppError::Internal(other.to_string()),
        }
    }
}

/// A connection or account.
#[derive(Debug, Clone, Serialize)]
pub struct App {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub app_pubkey: String,
    pub wallet_pubkey: Option<String>,
    pub isolated: bool,
    pub max_amount_msat: i64,
    pub budget_renewal: String,
    pub expires_at: Option<i64>,
    pub created_by_app_id: Option<i64>,
    pub lud16_username: Option<String>,
    pub metadata: Value,
    pub relay_urls: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: Option<i64>,
    /// Set when an account was closed (SPEC §10); closed accounts answer no requests.
    pub closed_at: Option<i64>,
    /// NWC-08: the client's correlation value, repeated in the connection's info event.
    pub auth_state: Option<String>,
    /// NWC-08: the client's relays that still have to receive the info event.
    #[serde(skip)]
    pub auth_relays: Option<Vec<String>>,
    /// The methods the client asked for (NWC-08). `None`: all its permissions allow.
    pub methods: Option<Vec<String>>,
    /// The notification types the client asked for (NWC-08). `None`: all.
    pub notification_types: Option<Vec<String>>,
    pub scopes: Vec<String>,
}

impl App {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }

    pub fn is_account(&self) -> bool {
        self.kind == "account"
    }

    /// Whether NWC shows this app only the transactions it made itself: isolated
    /// connections, and the receive-only connections Allod makes for Lightning address
    /// services (Alby, ZEUS Pay), which credit Main but must not read its history.
    pub fn own_rows_only(&self) -> bool {
        self.isolated
            || matches!(
                self.metadata.get("allod_kind").and_then(Value::as_str),
                Some("alby" | "zeus_pay")
            )
    }

    /// The account a Lightning address service's connection makes invoices for (SPEC
    /// §12.6); `None`: its own (Main for non-isolated connections).
    pub fn credit_account(&self) -> Option<i64> {
        if self.created_by_app_id.is_some() {
            return None;
        }
        self.metadata.get("account").and_then(Value::as_i64)
    }

    /// Whether NWC requests from this app are answered at all.
    pub fn is_live(&self, now: i64) -> bool {
        self.closed_at.is_none() && !self.is_expired(now)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// Whether the method is inside what the client asked for. A permission covers
    /// several methods (`pay_invoice` also pays offers), and NWC-08 forbids granting a
    /// method that was not requested. `get_info` and `get_budget` are always answered.
    pub fn permits_method(&self, method: &str) -> bool {
        matches!(method, "get_info" | "get_budget")
            || self.methods.as_ref().is_none_or(|m| m.iter().any(|x| x == method))
    }

    /// The methods this connection may call (SPEC §9.2).
    pub fn effective_methods(&self, bolt12: bool) -> Vec<String> {
        effective_methods(&self.scopes, bolt12)
            .into_iter()
            .filter(|m| self.permits_method(m))
            .collect()
    }

    /// The notification types this connection receives; empty without the permission.
    pub fn effective_notification_types(&self) -> Vec<&'static str> {
        if !self.has_scope("notifications") {
            return Vec::new();
        }
        outbox::TYPES
            .iter()
            .copied()
            .filter(|t| self.notification_types.as_ref().is_none_or(|n| n.iter().any(|x| x == t)))
            .collect()
    }
}

fn split_list(s: Option<String>) -> Option<Vec<String>> {
    s.map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect())
}

const APP_COLUMNS: &str = "id, name, description, kind, app_pubkey, wallet_pubkey, \
    CAST(isolated AS INTEGER) AS isolated, max_amount_msat, budget_renewal, expires_at, \
    created_by_app_id, lud16_username, metadata, relay_urls, created_at, updated_at, \
    last_used_at, closed_at, auth_state, auth_relays, methods, notification_types";

fn app_from_row(r: &AnyRow) -> Result<App, sqlx::Error> {
    let metadata: String = r.try_get("metadata")?;
    let relays: Option<String> = r.try_get("relay_urls")?;
    Ok(App {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        kind: r.try_get("kind")?,
        app_pubkey: r.try_get("app_pubkey")?,
        wallet_pubkey: r.try_get("wallet_pubkey")?,
        isolated: r.try_get::<i64, _>("isolated")? != 0,
        max_amount_msat: r.try_get("max_amount_msat")?,
        budget_renewal: r.try_get("budget_renewal")?,
        expires_at: r.try_get("expires_at")?,
        created_by_app_id: r.try_get("created_by_app_id")?,
        lud16_username: r.try_get("lud16_username")?,
        metadata: serde_json::from_str(&metadata).unwrap_or(Value::Object(Default::default())),
        relay_urls: relays.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        last_used_at: r.try_get("last_used_at")?,
        closed_at: r.try_get("closed_at")?,
        auth_state: r.try_get("auth_state")?,
        auth_relays: split_list(r.try_get("auth_relays")?).filter(|r| !r.is_empty()),
        methods: split_list(r.try_get("methods")?),
        notification_types: split_list(r.try_get("notification_types")?),
        scopes: Vec::new(),
    })
}

/// Request to create a connection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewApp {
    pub name: String,
    /// `connection` (default) or `account` (SPEC §10: isolated, fixed scopes).
    pub kind: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Client pubkey (hex). `None`: Allod generates the client key pair and returns
    /// the pairing URI once.
    pub pubkey: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub max_amount_msat: Option<i64>,
    pub budget_renewal: Option<String>,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub isolated: bool,
    pub metadata: Option<Value>,
    pub relay_urls: Option<Vec<String>>,
    /// Lightning address username (SPEC §12).
    pub lud16_username: Option<String>,
    /// NWC-08 fields, set by [`authorization`] only (never read from a request body).
    #[serde(skip)]
    pub auth_state: Option<String>,
    #[serde(skip)]
    pub auth_relays: Option<Vec<String>>,
    #[serde(skip)]
    pub methods: Option<Vec<String>>,
    #[serde(skip)]
    pub notification_types: Option<Vec<String>>,
}

/// A client's request for a connection to its own key (NWC-08), with the owner's choice
/// of optional methods.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizeRequest {
    pub pubkey: String,
    pub state: String,
    pub name: Option<String>,
    pub expires_at: Option<i64>,
    /// Millisatoshis per `renewal_period`.
    pub max_amount: Option<i64>,
    pub renewal_period: Option<String>,
    #[serde(default)]
    pub request_methods: Vec<String>,
    #[serde(default)]
    pub optional_request_methods: Vec<String>,
    /// The optional methods the owner left ticked. `None`: all that can be granted.
    pub granted_optional_methods: Option<Vec<String>>,
    #[serde(default)]
    pub notification_types: Vec<String>,
    #[serde(default)]
    pub isolated: bool,
    pub metadata: Option<Value>,
    /// Nostr flow: the client's relays, which receive the info event.
    #[serde(default)]
    pub relays: Vec<String>,
}

/// Whether a connection made on a client's request can get this method.
pub fn grantable_method(method: &str, isolated: bool, bolt12: bool) -> bool {
    IMPLEMENTED_METHODS.contains(&method)
        // Delegating access is the owner's own decision, made in Allod.
        && method != "create_connection"
        && (bolt12 || method != "make_offer")
        && !(isolated && method == "sign_message")
}

/// Turns an NWC-08 request into the connection to create. Every required method is
/// granted or the request is refused; nothing the client did not ask for is granted.
pub fn authorization(req: &AuthorizeRequest, bolt12: bool) -> Result<NewApp, AppError> {
    let bad = |m: String| AppError::BadRequest(m);
    if req
        .metadata
        .as_ref()
        .is_some_and(|m| m.get("account").is_some() || m.get("allod_kind").is_some())
    {
        return Err(bad("metadata contains a reserved key".into()));
    }
    let pubkey = validate_hex_pubkey(&req.pubkey)?;
    let state = req.state.as_str();
    if !(32..=128).contains(&state.len())
        || !state.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad("state must be 32 to 128 lowercase hex characters".into()));
    }
    let mut methods: Vec<String> = Vec::new();
    for m in &req.request_methods {
        if !grantable_method(m, req.isolated, bolt12) {
            return Err(bad(format!("{m} cannot be granted, so this request cannot be approved")));
        }
        if !methods.contains(m) {
            methods.push(m.clone());
        }
    }
    if let Some(granted) = &req.granted_optional_methods {
        if let Some(m) = granted.iter().find(|m| !req.optional_request_methods.contains(m)) {
            return Err(bad(format!("{m} is not among the optional methods of this request")));
        }
    }
    let optional = req.granted_optional_methods.as_ref().unwrap_or(&req.optional_request_methods);
    for m in optional {
        if grantable_method(m, req.isolated, bolt12) && !methods.contains(m) {
            methods.push(m.clone());
        }
    }
    let mut scopes: Vec<String> = Vec::new();
    for m in &methods {
        let scope = if m == "get_info" { Some("get_info") } else { method_scope(m) };
        if let Some(s) = scope {
            if !scopes.iter().any(|x| x == s) {
                scopes.push(s.into());
            }
        }
    }
    // Fewer types than requested is allowed: unknown ones are left out.
    let mut types: Vec<String> = Vec::new();
    for t in &req.notification_types {
        if outbox::TYPES.contains(&t.as_str()) && !types.contains(t) {
            types.push(t.clone());
        }
    }
    if !types.is_empty() && !scopes.iter().any(|s| s == "notifications") {
        scopes.push("notifications".into());
    }
    if req.max_amount.is_some_and(|m| m <= 0) {
        return Err(bad("max_amount must be a positive number of millisatoshis".into()));
    }
    if req.renewal_period.is_some() && req.max_amount.is_none() {
        return Err(bad("renewal_period needs max_amount".into()));
    }
    let renewal = req.renewal_period.as_deref().unwrap_or("never");
    if Renewal::parse(renewal).is_none() {
        return Err(bad("renewal_period must be never, daily, weekly, monthly or yearly".into()));
    }
    if req.relays.len() > 10 {
        return Err(bad("at most 10 relays".into()));
    }
    if !req.relays.is_empty() {
        validate_relays(&req.relays)?;
        if req.relays.iter().any(|r| r.contains(',') || r.len() > 300) {
            return Err(bad("invalid relay URL".into()));
        }
    }
    let name: String =
        req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("App").into();
    Ok(NewApp {
        name: name.chars().take(100).collect(),
        pubkey: Some(pubkey),
        scopes,
        max_amount_msat: req.max_amount,
        budget_renewal: Some(renewal.into()),
        expires_at: req.expires_at,
        isolated: req.isolated,
        metadata: req.metadata.clone().filter(|m| !m.is_null()),
        auth_state: Some(state.into()),
        auth_relays: Some(req.relays.clone()).filter(|r| !r.is_empty()),
        methods: Some(methods),
        notification_types: Some(types),
        ..Default::default()
    })
}

/// Changes to a connection. Absent fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub max_amount_msat: Option<i64>,
    pub budget_renewal: Option<String>,
    /// `Some(None)` clears the expiry.
    #[serde(default, with = "double_option")]
    pub expires_at: Option<Option<i64>>,
    pub metadata: Option<Value>,
    #[serde(default, with = "double_option")]
    pub relay_urls: Option<Option<Vec<String>>>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};
    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// A connection with its budget usage and (for isolated ones) balance.
#[derive(Debug, Clone, Serialize)]
pub struct AppView {
    #[serde(flatten)]
    pub app: App,
    pub budget: Option<Budget>,
    pub balance_msat: Option<i64>,
}

/// One logged NWC request (method and outcome only, SPEC §6.2).
#[derive(Debug, Clone, Serialize)]
pub struct RequestLog {
    pub id: i64,
    pub method: Option<String>,
    pub state: String,
    pub created_at: i64,
}

fn validate_hex_pubkey(pk: &str) -> Result<String, AppError> {
    let pk = pk.trim().to_ascii_lowercase();
    if pk.len() == 64 && pk.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(pk)
    } else {
        Err(AppError::BadRequest("pubkey must be a 32-byte hex Nostr public key".into()))
    }
}

fn validate_scopes(scopes: &[String], isolated: bool) -> Result<Vec<String>, AppError> {
    let mut out: Vec<String> = Vec::new();
    for s in scopes {
        if !SCOPES.contains(&s.as_str()) {
            return Err(AppError::BadRequest(format!("unknown scope: {s}")));
        }
        if isolated && s == "sign_message" {
            return Err(AppError::BadRequest(
                "sign_message cannot be granted to an isolated connection".into(),
            ));
        }
        if !out.contains(s) {
            out.push(s.clone());
        }
    }
    Ok(out)
}

fn validate_relays(relays: &[String]) -> Result<(), AppError> {
    if relays.is_empty() {
        return Err(AppError::BadRequest("relay_urls must not be empty".into()));
    }
    for r in relays {
        if !(r.starts_with("wss://") || r.starts_with("ws://")) {
            return Err(AppError::BadRequest(format!("invalid relay URL: {r}")));
        }
    }
    Ok(())
}

fn metadata_text(m: &Value) -> Result<String, AppError> {
    let s = m.to_string();
    if !m.is_object() {
        return Err(AppError::BadRequest("metadata must be a JSON object".into()));
    }
    if s.len() > 4096 {
        return Err(AppError::BadRequest("metadata is limited to 4096 bytes".into()));
    }
    Ok(s)
}

/// Connections, their permissions and wallet keys.
/// One Nostr relay of the NWC wallet service, as that service last saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayState {
    pub url: String,
    pub connected: bool,
    /// nostr-sdk's status in lower case: `connected`, `connecting`, `disconnected`, ….
    pub status: String,
    /// Unix seconds of the current connection, while connected.
    pub connected_at: Option<u64>,
}

/// The NWC service's relays and when it last looked at them.
#[derive(Debug, Clone, Serialize)]
pub struct RelayStates {
    pub updated_at: u64,
    pub relays: Vec<RelayState>,
}

pub struct AppService {
    pool: AnyPool,
    keys: Arc<NostrKeys>,
    relays: Vec<String>,
    /// Relays as clients reach them, written into connection links.
    client_relays: Vec<String>,
    /// Bumped on every change, so the NWC service resubscribes and republishes.
    changed: watch::Sender<u64>,
    /// Written by the NWC service (`allod-nwc`), read by the API for the node page.
    relay_states: std::sync::Mutex<Option<RelayStates>>,
}

impl AppService {
    /// `client_relays`: the same relays as clients reach them (empty: same as `relays`).
    pub fn new(
        pool: AnyPool,
        keys: Arc<NostrKeys>,
        relays: Vec<String>,
        client_relays: Vec<String>,
    ) -> Arc<Self> {
        let (changed, _) = watch::channel(0);
        let client_relays = if client_relays.is_empty() { relays.clone() } else { client_relays };
        Arc::new(AppService {
            pool,
            keys,
            relays,
            client_relays,
            changed,
            relay_states: std::sync::Mutex::new(None),
        })
    }

    /// Records the NWC service's relay connections (it calls this every few seconds).
    pub fn set_relay_states(&self, relays: Vec<RelayState>, now: u64) {
        *self.relay_states.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(RelayStates { updated_at: now, relays });
    }

    /// The NWC service's relay connections; `None` before it first reported them.
    pub fn relay_states(&self) -> Option<RelayStates> {
        self.relay_states.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn relays(&self) -> &[String] {
        &self.relays
    }

    /// Notified when connections change.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    /// Tells the NWC service that apps changed (e.g. an account was closed).
    pub fn notify_changed(&self) {
        self.bump();
    }

    /// The wallet key of a connection (`m/1237'/1'/{id}'`).
    pub fn wallet_key(&self, app_id: i64) -> Result<KeyPair, AppError> {
        self.keys.app_wallet_key(app_id as u64).map_err(|e| AppError::Internal(e.to_string()))
    }

    /// The key that signs NIP-57 zap receipts (`m/1237'/2'/0'`, SPEC §12).
    pub fn zap_key(&self) -> Result<KeyPair, AppError> {
        self.keys.zap_key().map_err(|e| AppError::Internal(e.to_string()))
    }

    /// The key Allod signs in to gateway providers with (LNVPS, SPEC §12.8).
    pub fn gateway_account_key(&self) -> Result<KeyPair, AppError> {
        self.keys.gateway_account_key().map_err(|e| AppError::Internal(e.to_string()))
    }

    /// Relays an app listens on.
    pub fn app_relays(&self, app: &App) -> Vec<String> {
        app.relay_urls.clone().unwrap_or_else(|| self.relays.clone())
    }

    /// The same relays as a client reaches them (written into links and `relay` tags).
    pub fn client_relays(&self, app: &App) -> Vec<String> {
        app.relay_urls.clone().unwrap_or_else(|| self.client_relays.clone())
    }

    /// The client's relays got the info event (NWC-08): nothing left to deliver.
    pub async fn clear_auth_relays(&self, id: i64) -> Result<(), AppError> {
        sqlx::query("UPDATE apps SET auth_relays = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// `nostr+walletconnect://` URI with the client secret (SPEC §8.6).
    pub fn pairing_uri(&self, app: &App, secret_hex: &str) -> String {
        let mut uri =
            format!("nostr+walletconnect://{}?", app.wallet_pubkey.as_deref().unwrap_or_default());
        let relays = app.relay_urls.clone().unwrap_or_else(|| self.client_relays.clone());
        for r in relays {
            uri.push_str("relay=");
            uri.push_str(&url_encode(&r));
            uri.push('&');
        }
        uri.push_str("secret=");
        uri.push_str(secret_hex);
        uri
    }

    async fn load_scopes(&self, app: &mut App) -> Result<(), AppError> {
        app.scopes = sqlx::query("SELECT scope FROM app_permissions WHERE app_id = $1 ORDER BY id")
            .bind(app.id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.get::<String, _>(0))
            .collect();
        Ok(())
    }

    pub async fn get(&self, id: i64) -> Result<App, AppError> {
        let row = sqlx::query(&format!("SELECT {APP_COLUMNS} FROM apps WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(AppError::NotFound)?;
        let mut app = app_from_row(&row)?;
        self.load_scopes(&mut app).await?;
        Ok(app)
    }

    /// The connection a NWC request comes from (by the client pubkey).
    pub async fn by_app_pubkey(&self, pubkey: &str) -> Result<Option<App>, AppError> {
        let Some(row) = sqlx::query(&format!(
            "SELECT {APP_COLUMNS} FROM apps WHERE app_pubkey = $1 AND closed_at IS NULL"
        ))
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let mut app = app_from_row(&row)?;
        self.load_scopes(&mut app).await?;
        Ok(Some(app))
    }

    /// All connections (not accounts), newest first.
    pub async fn list(&self) -> Result<Vec<App>, AppError> {
        self.query_list("kind = 'connection'").await
    }

    /// Every app that answers NWC requests: connections and open accounts.
    pub async fn list_nwc(&self) -> Result<Vec<App>, AppError> {
        self.query_list("closed_at IS NULL").await
    }

    /// Accounts, oldest first. Closed accounts are included only while they still hold
    /// funds (a payment to one of their old invoices can arrive after closing), so the
    /// owner can withdraw them.
    pub async fn list_accounts(&self) -> Result<Vec<App>, AppError> {
        self.query_list(
            "kind = 'account' AND (closed_at IS NULL OR id IN (\
               SELECT a.app_id FROM ledger_accounts a JOIN ledger_balances b ON b.account_id = a.id \
               WHERE a.app_id IS NOT NULL AND b.balance_msat <> 0))",
        )
        .await
        .map(|mut v| {
            v.reverse();
            v
        })
    }

    async fn query_list(&self, filter: &str) -> Result<Vec<App>, AppError> {
        let rows =
            sqlx::query(&format!("SELECT {APP_COLUMNS} FROM apps WHERE {filter} ORDER BY id DESC"))
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut app = app_from_row(r)?;
            self.load_scopes(&mut app).await?;
            out.push(app);
        }
        Ok(out)
    }

    /// A connection with its budget usage and balance.
    pub async fn view(&self, app: App, wallet: &crate::Wallet) -> Result<AppView, AppError> {
        let budget = wallet.budget(app.id).await?;
        let balance_msat =
            if app.isolated { Some(wallet.app_balance(Some(app.id)).await?) } else { None };
        Ok(AppView { app, budget, balance_msat })
    }

    /// Creates a connection. Returns it and, when Allod generated the client key, the
    /// pairing URI (shown once; the client secret is not stored).
    pub async fn create(
        &self,
        req: NewApp,
        created_by: Option<i64>,
        now: i64,
    ) -> Result<(App, Option<String>), AppError> {
        let mut req = req;
        if created_by.is_some()
            && req
                .metadata
                .as_ref()
                .is_some_and(|m| m.get("account").is_some() || m.get("allod_kind").is_some())
        {
            return Err(AppError::BadRequest("metadata contains a reserved key".into()));
        }
        let name = req.name.trim().to_string();
        if name.is_empty() || name.len() > 100 {
            return Err(AppError::BadRequest("name must be 1 to 100 characters".into()));
        }
        let kind = req.kind.clone().unwrap_or_else(|| "connection".into());
        match kind.as_str() {
            "connection" => {}
            "account" => {
                if created_by.is_some() {
                    return Err(AppError::BadRequest(
                        "accounts can only be created by the owner".into(),
                    ));
                }
                // SPEC §10: isolated, fixed scopes, no budget by default.
                req.isolated = true;
                req.scopes = ACCOUNT_SCOPES.iter().map(|s| s.to_string()).collect();
                let mut m = req.metadata.take().unwrap_or(Value::Object(Default::default()));
                if let Some(o) = m.as_object_mut() {
                    o.insert("allod_kind".into(), "account".into());
                }
                req.metadata = Some(m);
                req.budget_renewal.get_or_insert_with(|| "never".into());
            }
            _ => return Err(AppError::BadRequest("kind must be connection or account".into())),
        }
        let username = req.lud16_username.as_deref().map(validate_username).transpose()?;
        if let Some(u) = &username {
            if let Some(owner) = self.username_owner(u, None).await? {
                return Err(AppError::BadRequest(taken_message(u, &owner)));
            }
        }
        let scopes = validate_scopes(&req.scopes, req.isolated)?;
        let renewal = req.budget_renewal.as_deref().unwrap_or("monthly");
        if Renewal::parse(renewal).is_none() {
            return Err(AppError::BadRequest(
                "budget_renewal must be daily, weekly, monthly, yearly or never".into(),
            ));
        }
        let max = req.max_amount_msat.unwrap_or(0);
        if max < 0 {
            return Err(AppError::BadRequest("max_amount_msat must not be negative".into()));
        }
        if req.expires_at.is_some_and(|e| e <= now) {
            return Err(AppError::BadRequest("expires_at must be in the future".into()));
        }
        if let Some(r) = &req.relay_urls {
            validate_relays(r)?;
        }
        let metadata =
            metadata_text(req.metadata.as_ref().unwrap_or(&Value::Object(Default::default())))?;
        let (app_pubkey, secret) = match &req.pubkey {
            Some(pk) => (validate_hex_pubkey(pk)?, None),
            None => {
                let kp = KeyPair::random();
                (kp.public_hex.clone(), Some(kp.expose_secret_hex()))
            }
        };

        let mut tx = self.pool.begin().await?;
        let id: i64 = sqlx::query(
            "INSERT INTO apps (name, description, kind, app_pubkey, isolated, max_amount_msat, \
             budget_renewal, expires_at, created_by_app_id, metadata, relay_urls, created_at, updated_at, \
             lud16_username, auth_state, auth_relays, methods, notification_types) \
             VALUES ($1, $2, $12, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11, $13, $14, $15, $16, $17) \
             RETURNING id",
        )
        .bind(&name)
        .bind(req.description.trim())
        .bind(&app_pubkey)
        .bind(req.isolated)
        .bind(max)
        .bind(renewal)
        .bind(req.expires_at)
        .bind(created_by)
        .bind(&metadata)
        .bind(req.relay_urls.as_ref().map(|r| serde_json::to_string(r).unwrap_or_default()))
        .bind(now)
        .bind(&kind)
        .bind(&username)
        .bind(&req.auth_state)
        .bind(req.auth_relays.as_ref().map(|r| r.join(",")))
        .bind(req.methods.as_ref().map(|m| m.join(",")))
        .bind(req.notification_types.as_ref().map(|t| t.join(",")))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                AppError::BadRequest("a connection with this pubkey or username already exists".into())
            }
            e => e.into(),
        })?
        .get(0);
        let wallet_pubkey = self.wallet_key(id)?.public_hex;
        sqlx::query("UPDATE apps SET wallet_pubkey = $1 WHERE id = $2")
            .bind(&wallet_pubkey)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for s in &scopes {
            sqlx::query(
                "INSERT INTO app_permissions (app_id, scope, created_at) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(s)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let app = self.get(id).await?;
        let uri = secret.map(|s| self.pairing_uri(&app, &s));
        self.bump();
        Ok((app, uri))
    }

    pub async fn update(&self, id: i64, patch: AppPatch, now: i64) -> Result<App, AppError> {
        let app = self.get(id).await?;
        if app.closed_at.is_some() {
            return Err(AppError::BadRequest("this account is closed".into()));
        }
        if app.is_account() && patch.scopes.is_some() {
            return Err(AppError::BadRequest("the permissions of an account are fixed".into()));
        }
        let name = patch.name.map(|n| n.trim().to_string()).unwrap_or(app.name.clone());
        if name.is_empty() || name.len() > 100 {
            return Err(AppError::BadRequest("name must be 1 to 100 characters".into()));
        }
        let renewal = patch.budget_renewal.unwrap_or(app.budget_renewal.clone());
        if Renewal::parse(&renewal).is_none() {
            return Err(AppError::BadRequest("invalid budget_renewal".into()));
        }
        let max = patch.max_amount_msat.unwrap_or(app.max_amount_msat);
        if max < 0 {
            return Err(AppError::BadRequest("max_amount_msat must not be negative".into()));
        }
        let metadata = metadata_text(patch.metadata.as_ref().unwrap_or(&app.metadata))?;
        let relays = patch.relay_urls.unwrap_or(app.relay_urls.clone());
        if let Some(r) = &relays {
            validate_relays(r)?;
        }
        let expires = patch.expires_at.unwrap_or(app.expires_at);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE apps SET name = $1, description = $2, max_amount_msat = $3, budget_renewal = $4, \
             expires_at = $5, metadata = $6, relay_urls = $7, updated_at = $8 WHERE id = $9",
        )
        .bind(&name)
        .bind(patch.description.unwrap_or(app.description.clone()).trim())
        .bind(max)
        .bind(&renewal)
        .bind(expires)
        .bind(&metadata)
        .bind(relays.as_ref().map(|r| serde_json::to_string(r).unwrap_or_default()))
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if let Some(scopes) = patch.scopes {
            let scopes = validate_scopes(&scopes, app.isolated)?;
            // The owner now decides the permissions: the client's own list no longer
            // narrows them.
            sqlx::query("UPDATE apps SET methods = NULL, notification_types = NULL WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM app_permissions WHERE app_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for s in &scopes {
                sqlx::query(
                    "INSERT INTO app_permissions (app_id, scope, created_at) VALUES ($1, $2, $3)",
                )
                .bind(id)
                .bind(s)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        self.bump();
        self.get(id).await
    }

    /// Deletes a connection. An isolated connection must be empty first.
    pub async fn delete(&self, id: i64, wallet: &crate::Wallet) -> Result<(), AppError> {
        let app = self.get(id).await?;
        if app.is_account() {
            return Err(AppError::BadRequest(
                "accounts are closed with DELETE /api/accounts/:id".into(),
            ));
        }
        if app.created_by_app_id.is_some() {
            return Err(AppError::BadRequest(
                "delegated connections cannot be deleted; expire them to revoke access".into(),
            ));
        }
        if app.isolated && wallet.app_balance(Some(id)).await? > 0 {
            return Err(AppError::BadRequest(
                "this connection still holds funds; move them to Main first".into(),
            ));
        }
        sqlx::query("DELETE FROM apps WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.bump();
        Ok(())
    }

    /// The name of the app (other than `except`) that has `username`, closed or not.
    pub async fn username_owner(
        &self,
        username: &str,
        except: Option<i64>,
    ) -> Result<Option<String>, AppError> {
        let row: Option<(String, Option<i64>)> = sqlx::query_as(
            "SELECT name, closed_at FROM apps WHERE lud16_username = $1 AND id <> $2",
        )
        .bind(username)
        .bind(except.unwrap_or(-1))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(name, closed)| match closed {
            Some(_) => format!("{name} (closed)"),
            None => name,
        }))
    }

    /// Sets or clears an open account's Lightning address username (SPEC §12). Usernames
    /// are unique across accounts.
    pub async fn set_username(
        &self,
        id: i64,
        username: Option<&str>,
        now: i64,
    ) -> Result<App, AppError> {
        let app = self.get(id).await?;
        if !app.is_account() || app.closed_at.is_some() {
            return Err(AppError::BadRequest("not an open account".into()));
        }
        let username = username.map(validate_username).transpose()?;
        if let Some(u) = &username {
            if let Some(owner) = self.username_owner(u, Some(id)).await? {
                return Err(AppError::BadRequest(taken_message(u, &owner)));
            }
        }
        sqlx::query("UPDATE apps SET lud16_username = $1, updated_at = $2 WHERE id = $3")
            .bind(&username)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.bump();
        self.get(id).await
    }

    /// Replaces the client key of a connection and returns the new pairing URI. The old
    /// client stops working.
    pub async fn regenerate(&self, id: i64, now: i64) -> Result<(App, String), AppError> {
        let kp = KeyPair::random();
        let n = sqlx::query(
            "UPDATE apps SET app_pubkey = $1, updated_at = $2 WHERE id = $3 AND closed_at IS NULL",
        )
        .bind(&kp.public_hex)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(AppError::NotFound);
        }
        let app = self.get(id).await?;
        let uri = self.pairing_uri(&app, &kp.expose_secret_hex());
        self.bump();
        Ok((app, uri))
    }

    /// Remembers which encryption the app used last (`nip44` | `nip04`, SPEC §8.5).
    pub async fn set_encryption(&self, id: i64, enc: &str) -> Result<(), AppError> {
        sqlx::query("UPDATE apps SET last_encryption = $1 WHERE id = $2 AND (last_encryption IS NULL OR last_encryption <> $1)")
            .bind(enc)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Whether notifications for this app also go out as NIP-04 (kind 23196): when its
    /// latest request used NIP-04, or it never sent one.
    pub async fn wants_nip04(&self, id: i64) -> Result<bool, AppError> {
        let enc: Option<String> = sqlx::query("SELECT last_encryption FROM apps WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|r| r.get(0));
        Ok(enc.as_deref() != Some("nip44"))
    }

    pub async fn touch(&self, id: i64, now: i64) -> Result<(), AppError> {
        sqlx::query("UPDATE apps SET last_used_at = $1 WHERE id = $2")
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Writes an `audit_log` row (e.g. `nwc_permission_denied`).
    pub async fn audit(&self, kind: &str, app_id: Option<i64>, data: Value, now: i64) {
        let r = sqlx::query(
            "INSERT INTO audit_log (kind, app_id, data, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(kind)
        .bind(app_id)
        .bind(data.to_string())
        .bind(now)
        .execute(&self.pool)
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, kind, "audit log write failed");
        }
    }

    // ------------------------------------------------------------ request log

    /// Atomically reserves request-log quota and records the dedupe barrier. Call only
    /// after cheap request validation, but before audits, state changes or dispatch.
    pub async fn record_request(
        &self,
        nostr_id: &str,
        app_id: i64,
        method: &str,
        now: i64,
    ) -> Result<RequestAdmission, AppError> {
        let storage_bytes =
            128_i64.saturating_add(nostr_id.len() as i64).saturating_add(method.len() as i64);
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO request_events \
             (nostr_id, app_id, method, state, created_at, updated_at, storage_bytes) \
             VALUES ($1, $2, $3, 'executing', $4, $4, $5) \
             ON CONFLICT (nostr_id) DO NOTHING RETURNING id",
        )
        .bind(nostr_id)
        .bind(app_id)
        .bind(method)
        .bind(now)
        .bind(storage_bytes)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = inserted else {
            tx.rollback().await?;
            return Ok(RequestAdmission::Duplicate);
        };

        let global = sqlx::query(
            "UPDATE nwc_request_usage_global \
             SET row_count = row_count + 1, byte_count = byte_count + $1 \
             WHERE id = 1 AND row_count < $2 AND byte_count <= $3 - $1",
        )
        .bind(storage_bytes)
        .bind(NWC_REQUEST_ROWS_GLOBAL)
        .bind(NWC_REQUEST_BYTES_GLOBAL)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if global == 0 {
            tx.rollback().await?;
            return Ok(RequestAdmission::Full);
        }

        sqlx::query(
            "INSERT INTO nwc_request_usage (app_id, row_count, byte_count) \
             VALUES ($1, 0, 0) ON CONFLICT (app_id) DO NOTHING",
        )
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
        let per_app = sqlx::query(
            "UPDATE nwc_request_usage \
             SET row_count = row_count + 1, byte_count = byte_count + $1 \
             WHERE app_id = $2 AND row_count < $3 AND byte_count <= $4 - $1",
        )
        .bind(storage_bytes)
        .bind(app_id)
        .bind(NWC_REQUEST_ROWS_PER_APP)
        .bind(NWC_REQUEST_BYTES_PER_APP)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if per_app == 0 {
            tx.rollback().await?;
            return Ok(RequestAdmission::Full);
        }
        tx.commit().await?;
        Ok(RequestAdmission::Inserted(id))
    }

    pub async fn update_request(
        &self,
        id: i64,
        method: Option<&str>,
        state: &str,
        now: i64,
    ) -> Result<(), AppError> {
        sqlx::query(
            "UPDATE request_events SET method = COALESCE($1, method), state = $2, updated_at = $3 WHERE id = $4",
        )
        .bind(method)
        .bind(state)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_response(
        &self,
        request_id: i64,
        nostr_id: &str,
        sent: bool,
        now: i64,
    ) -> Result<bool, AppError> {
        let storage_bytes = 96_i64.saturating_add(nostr_id.len() as i64);
        let mut tx = self.pool.begin().await?;
        let Some(app_id) =
            sqlx::query_scalar::<_, i64>("SELECT app_id FROM request_events WHERE id = $1")
                .bind(request_id)
                .fetch_optional(&mut *tx)
                .await?
        else {
            tx.rollback().await?;
            return Ok(false);
        };
        let global = sqlx::query(
            "UPDATE nwc_request_usage_global \
             SET row_count = row_count + 1, byte_count = byte_count + $1 \
             WHERE id = 1 AND row_count < $2 AND byte_count <= $3 - $1",
        )
        .bind(storage_bytes)
        .bind(NWC_REQUEST_ROWS_GLOBAL)
        .bind(NWC_REQUEST_BYTES_GLOBAL)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let per_app = if global == 1 {
            sqlx::query(
                "UPDATE nwc_request_usage \
                 SET row_count = row_count + 1, byte_count = byte_count + $1 \
                 WHERE app_id = $2 AND row_count < $3 AND byte_count <= $4 - $1",
            )
            .bind(storage_bytes)
            .bind(app_id)
            .bind(NWC_REQUEST_ROWS_PER_APP)
            .bind(NWC_REQUEST_BYTES_PER_APP)
            .execute(&mut *tx)
            .await?
            .rows_affected()
        } else {
            0
        };
        if per_app == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO response_events \
             (nostr_id, request_id, state, replied_at, created_at, storage_bytes) \
             VALUES ($1, $2, $3, $4, $4, $5)",
        )
        .bind(nostr_id)
        .bind(request_id)
        .bind(if sent { "sent" } else { "failed" })
        .bind(now)
        .bind(storage_bytes)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Deletes request events older than `before` (unix seconds), keeping the newest
    /// `keep_per_app` of each connection for its request log. Their response events go
    /// with them (`ON DELETE CASCADE`). Runs in batches so SQLite is not locked for
    /// long. Returns how many request events were deleted.
    ///
    /// The log only has to cover the replay window: requests are deduplicated against
    /// it, and the subscription looks back at most 6 hours (SPEC §8.1–8.2).
    pub async fn prune_requests(&self, before: i64, keep_per_app: i64) -> Result<u64, AppError> {
        let mut total = 0;
        loop {
            let mut tx = self.pool.begin().await?;
            let n = sqlx::query(
                "DELETE FROM request_events WHERE id IN (\
                   SELECT id FROM (\
                     SELECT id, created_at, ROW_NUMBER() OVER (PARTITION BY app_id ORDER BY id DESC) AS rn \
                     FROM request_events\
                   ) AS ranked WHERE created_at < $1 AND rn > $2 LIMIT 5000)",
            )
            .bind(before)
            .bind(keep_per_app)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            // Lock counters in same global-then-app order as admission transactions.
            sqlx::query(
                "UPDATE nwc_request_usage_global SET \
                 row_count = (SELECT COUNT(*) FROM request_events) + (SELECT COUNT(*) FROM response_events), \
                 byte_count = COALESCE((SELECT SUM(storage_bytes) FROM request_events), 0) + \
                   COALESCE((SELECT SUM(storage_bytes) FROM response_events), 0) WHERE id = 1",
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE nwc_request_usage SET \
                 row_count = (SELECT COUNT(*) FROM request_events r WHERE r.app_id = nwc_request_usage.app_id) + \
                   (SELECT COUNT(*) FROM response_events e JOIN request_events r ON r.id = e.request_id WHERE r.app_id = nwc_request_usage.app_id), \
                 byte_count = COALESCE((SELECT SUM(storage_bytes) FROM request_events r WHERE r.app_id = nwc_request_usage.app_id), 0) + \
                   COALESCE((SELECT SUM(e.storage_bytes) FROM response_events e JOIN request_events r ON r.id = e.request_id WHERE r.app_id = nwc_request_usage.app_id), 0)",
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            total += n;
            if n < 5000 {
                return Ok(total);
            }
        }
    }

    /// The latest requests of a connection (connection detail page).
    pub async fn requests(&self, app_id: i64, limit: i64) -> Result<Vec<RequestLog>, AppError> {
        Ok(sqlx::query(
            "SELECT id, method, state, created_at FROM request_events WHERE app_id = $1 ORDER BY id DESC LIMIT $2",
        )
        .bind(app_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| RequestLog {
            id: r.get(0),
            method: r.get(1),
            state: r.get(2),
            created_at: r.get(3),
        })
        .collect())
    }

    /// Budget of a connection, computed by the wallet's rules.
    pub async fn budget(&self, app_id: i64, now: i64) -> Result<Option<Budget>, AppError> {
        let mut c = self.pool.acquire().await?;
        Ok(budget::get(&mut c, app_id, now).await?)
    }
}

/// SQL condition for the transactions an app may see (SPEC §9.3), with the app id
/// bound as `$n`.
pub fn visibility_clause(isolated: bool, n: usize) -> String {
    if isolated {
        // Its own rows, and the invoices it asked for on behalf of an account.
        format!("(app_id = ${n} OR via_app_id = ${n})")
    } else {
        // Main is a shared wallet, but rows made by another delegated app are not
        // public to every other app holding the same wallet.
        format!("(app_id IS NULL OR app_id = ${n} OR via_app_id = ${n})")
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AuthorizeRequest {
        AuthorizeRequest {
            pubkey: "ab".repeat(32),
            state: "0f".repeat(16),
            request_methods: vec!["pay_invoice".into(), "get_info".into()],
            optional_request_methods: vec!["make_invoice".into(), "list_transactions".into()],
            ..Default::default()
        }
    }

    #[test]
    fn nwc08_grants_what_was_asked_and_nothing_else() {
        let new = authorization(&request(), true).unwrap();
        assert_eq!(new.scopes, ["pay_invoice", "get_info", "make_invoice", "list_transactions"]);
        // The permission also covers `pay` and `multi_pay_invoice`: the list keeps them out.
        assert_eq!(
            new.methods.as_deref().unwrap(),
            ["pay_invoice", "get_info", "make_invoice", "list_transactions"]
        );
        assert_eq!(new.budget_renewal.as_deref(), Some("never"));
        assert_eq!(new.max_amount_msat, None);
        assert_eq!(new.auth_state.as_deref(), Some("0f".repeat(16).as_str()));
        assert_eq!(new.name, "App");

        // The owner unticked one optional method.
        let mut r = request();
        r.granted_optional_methods = Some(vec!["make_invoice".into()]);
        let new = authorization(&r, true).unwrap();
        assert_eq!(new.methods.as_deref().unwrap(), ["pay_invoice", "get_info", "make_invoice"]);
        // ...but cannot add one the client never listed.
        r.granted_optional_methods = Some(vec!["sign_message".into()]);
        assert!(authorization(&r, true).is_err());

        // Client metadata cannot route credit to an account or forge a service connection.
        r = request();
        r.metadata = Some(serde_json::json!({"account": 7, "allod_kind": "alby"}));
        assert!(authorization(&r, true).is_err());

        // No methods asked: none granted (`get_info` and `get_budget` always answer).
        let mut r = request();
        r.request_methods.clear();
        r.optional_request_methods.clear();
        let new = authorization(&r, true).unwrap();
        assert!(new.scopes.is_empty() && new.methods.as_deref().unwrap().is_empty());
    }

    #[test]
    fn nwc08_refuses_what_it_cannot_honour() {
        let refused = |change: fn(&mut AuthorizeRequest), bolt12: bool| {
            let mut r = request();
            change(&mut r);
            authorization(&r, bolt12).is_err()
        };
        // A required method Allod cannot grant declines the whole request.
        assert!(refused(|r| r.request_methods.push("pay_keysend".into()), true));
        assert!(refused(|r| r.request_methods.push("create_connection".into()), true));
        assert!(refused(|r| r.request_methods.push("make_offer".into()), false));
        assert!(!refused(|r| r.request_methods.push("make_offer".into()), true));
        assert!(refused(
            |r| {
                r.isolated = true;
                r.request_methods.push("sign_message".into())
            },
            true
        ));
        // The same methods as optional ones are simply left out.
        let mut r = request();
        r.optional_request_methods = vec!["pay_keysend".into(), "create_connection".into()];
        assert_eq!(
            authorization(&r, true).unwrap().methods.as_deref().unwrap(),
            ["pay_invoice", "get_info"]
        );

        assert!(refused(|r| r.state = "short".into(), true));
        assert!(refused(|r| r.state = "0F".repeat(16), true), "lowercase hex only");
        assert!(refused(|r| r.pubkey = "npub1xyz".into(), true));
        assert!(refused(|r| r.max_amount = Some(0), true));
        assert!(refused(|r| r.renewal_period = Some("monthly".into()), true), "needs max_amount");
        assert!(refused(
            |r| {
                r.max_amount = Some(1000);
                r.renewal_period = Some("hourly".into())
            },
            true
        ));
        assert!(refused(|r| r.relays = vec!["https://relay.example".into()], true));

        let mut r = request();
        r.max_amount = Some(21_000);
        r.renewal_period = Some("weekly".into());
        r.notification_types = vec!["payment_received".into(), "made_up".into()];
        r.relays = vec!["wss://relay.example".into()];
        let new = authorization(&r, true).unwrap();
        assert_eq!(new.max_amount_msat, Some(21_000));
        assert_eq!(new.budget_renewal.as_deref(), Some("weekly"));
        assert_eq!(new.notification_types.as_deref().unwrap(), ["payment_received"]);
        assert!(new.scopes.iter().any(|s| s == "notifications"));
        assert_eq!(new.auth_relays.as_deref().unwrap(), ["wss://relay.example"]);
    }

    #[test]
    fn effective_methods_follow_scopes_and_implementation() {
        let m = effective_methods(&["pay_invoice".into(), "lookup_invoice".into()], true);
        assert_eq!(
            m,
            vec![
                "get_info",
                "get_budget",
                "pay_invoice",
                "multi_pay_invoice",
                "pay",
                "lookup_invoice",
                "lookup_payment"
            ]
        );
        assert!(
            !effective_methods(&["make_invoice".into()], false).contains(&"make_offer".to_string())
        );
        let all = effective_methods(&["make_invoice".into(), "lookup_invoice".into()], true);
        assert_eq!(extensions(&all, true), vec!["02", "03", "06", "08", "09", "12", "321"]);
        assert_eq!(method_scope("pay_invoice"), Some("pay_invoice"));
        assert_eq!(method_scope("lookup_payment"), Some("lookup_invoice"));
        assert_eq!(method_scope("get_budget"), None);
        assert_eq!(method_scope("make_hold_invoice"), Some("make_invoice"));
    }

    #[test]
    fn scope_validation() {
        assert!(validate_scopes(&["sign_message".into()], true).is_err());
        assert!(validate_scopes(&["sign_message".into()], false).is_ok());
        assert!(validate_scopes(&["nope".into()], false).is_err());
        assert_eq!(
            validate_scopes(&["get_balance".into(), "get_balance".into()], false).unwrap(),
            vec!["get_balance"]
        );
    }

    #[test]
    fn url_encoding_of_relays() {
        assert_eq!(url_encode("wss://relay.example/v1"), "wss%3A%2F%2Frelay.example%2Fv1");
    }
}
