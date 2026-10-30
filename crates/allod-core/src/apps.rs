//! NWC connections ("apps"): scopes, budgets, visibility and wallet keys (SPEC §6.2,
//! §8.6, §9). Accounts (`kind = 'account'`) share the table and arrive in M6.

use std::sync::Arc;

use allod_keys::{KeyPair, NostrKeys};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::any::AnyRow;
use sqlx::{AnyPool, Row};
use tokio::sync::watch;

use crate::wallet::budget::{self, Budget, Renewal};

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
];

/// NWC extensions advertised for an app's effective methods (SPEC §8.1). `08` arrives
/// with NWC-08 (M7).
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
    pub metadata: Value,
    pub relay_urls: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: Option<i64>,
    pub scopes: Vec<String>,
}

impl App {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

const APP_COLUMNS: &str = "id, name, description, kind, app_pubkey, wallet_pubkey, \
    CAST(isolated AS INTEGER) AS isolated, max_amount_msat, budget_renewal, expires_at, \
    created_by_app_id, metadata, relay_urls, created_at, updated_at, last_used_at";

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
        metadata: serde_json::from_str(&metadata).unwrap_or(Value::Object(Default::default())),
        relay_urls: relays.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        last_used_at: r.try_get("last_used_at")?,
        scopes: Vec::new(),
    })
}

/// Request to create a connection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewApp {
    pub name: String,
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
pub struct AppService {
    pool: AnyPool,
    keys: Arc<NostrKeys>,
    relays: Vec<String>,
    /// Relays as clients reach them, written into connection links.
    client_relays: Vec<String>,
    /// Bumped on every change, so the NWC service resubscribes and republishes.
    changed: watch::Sender<u64>,
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
        Arc::new(AppService { pool, keys, relays, client_relays, changed })
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

    /// The wallet key of a connection (`m/1237'/1'/{id}'`).
    pub fn wallet_key(&self, app_id: i64) -> Result<KeyPair, AppError> {
        self.keys.app_wallet_key(app_id as u64).map_err(|e| AppError::Internal(e.to_string()))
    }

    /// Relays an app listens on.
    pub fn app_relays(&self, app: &App) -> Vec<String> {
        app.relay_urls.clone().unwrap_or_else(|| self.relays.clone())
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
            "SELECT {APP_COLUMNS} FROM apps WHERE app_pubkey = $1 AND kind = 'connection'"
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
        let rows = sqlx::query(&format!(
            "SELECT {APP_COLUMNS} FROM apps WHERE kind = 'connection' ORDER BY id DESC"
        ))
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
        let name = req.name.trim().to_string();
        if name.is_empty() || name.len() > 100 {
            return Err(AppError::BadRequest("name must be 1 to 100 characters".into()));
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
             budget_renewal, expires_at, created_by_app_id, metadata, relay_urls, created_at, updated_at) \
             VALUES ($1, $2, 'connection', $3, $4, $5, $6, $7, $8, $9, $10, $11, $11) RETURNING id",
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
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                AppError::BadRequest("a connection with this pubkey already exists".into())
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

    /// Replaces the client key of a connection and returns the new pairing URI. The old
    /// client stops working.
    pub async fn regenerate(&self, id: i64, now: i64) -> Result<(App, String), AppError> {
        let kp = KeyPair::random();
        let n = sqlx::query("UPDATE apps SET app_pubkey = $1, updated_at = $2 WHERE id = $3 AND kind = 'connection'")
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

    /// Records a request event. `Ok(None)` means it was already seen (dedupe, §8.2).
    pub async fn record_request(
        &self,
        nostr_id: &str,
        app_id: Option<i64>,
        now: i64,
    ) -> Result<Option<i64>, AppError> {
        let r = sqlx::query(
            "INSERT INTO request_events (nostr_id, app_id, state, created_at, updated_at) \
             VALUES ($1, $2, 'received', $3, $3) RETURNING id",
        )
        .bind(nostr_id)
        .bind(app_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await;
        match r {
            Ok(row) => Ok(Some(row.get(0))),
            Err(sqlx::Error::Database(d)) if d.is_unique_violation() => Ok(None),
            Err(e) => Err(e.into()),
        }
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
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO response_events (nostr_id, request_id, state, replied_at, created_at) \
             VALUES ($1, $2, $3, $4, $4)",
        )
        .bind(nostr_id)
        .bind(request_id)
        .bind(if sent { "sent" } else { "failed" })
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
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
            let n = sqlx::query(
                "DELETE FROM request_events WHERE id IN (\
                   SELECT id FROM (\
                     SELECT id, created_at, ROW_NUMBER() OVER (PARTITION BY app_id ORDER BY id DESC) AS rn \
                     FROM request_events\
                   ) AS ranked WHERE created_at < $1 AND rn > $2 LIMIT 5000)",
            )
            .bind(before)
            .bind(keep_per_app)
            .execute(&self.pool)
            .await?
            .rows_affected();
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
        format!("app_id = ${n}")
    } else {
        format!(
            "(app_id IS NULL OR app_id = ${n} OR app_id IN (SELECT id FROM apps WHERE isolated = FALSE))"
        )
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
        assert_eq!(extensions(&all, true), vec!["02", "03", "06", "09", "12", "321"]);
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
