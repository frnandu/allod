//! The wallet (SPEC §6–7): payments, invoices, offers, hold invoices, self payments
//! and internal transfers on a double-entry ledger, kept in step with the Lightning
//! backend by an event handler and a reconciler.
//!
//! Every money movement is one DB transaction that changes a `transactions` row with
//! `UPDATE … WHERE state = expected` and posts a uniquely keyed journal entry, so a
//! replayed event can never post twice. All wallet writes go through `write_lock`
//! (SQLite has one writer); network calls happen outside it.

pub mod bip321;
pub mod budget;
pub mod counterparty;
pub mod ledger;
pub mod outbox;
pub mod store;

use std::sync::Arc;
use std::time::Duration;

use allod_ln::types::{Direction, InvoiceDescription, LnEvent, LnPayment, LnPaymentKind, LnStatus};
use allod_ln::{BackendError, LightningBackend};
use allod_types::Network;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{AnyConnection, AnyPool, Row};
use tokio::sync::{broadcast, Mutex};

use bip321::Instruction;
pub use store::{NewTx, OfferRow, Tx};

/// Wallet errors.
#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("this payment would exceed the connection's budget")]
    QuotaExceeded,
    #[error("this invoice has already been paid")]
    AlreadyPaid,
    #[error("a payment for this invoice is already in progress")]
    InProgress,
    #[error("not found")]
    NotFound,
    #[error("expired")]
    Expired,
    #[error("the payment instruction is for a different network than this node ({0})")]
    WrongNetwork(String),
    #[error("payment failed: {0}")]
    PaymentFailed(String),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<sqlx::Error> for WalletError {
    fn from(e: sqlx::Error) -> Self {
        WalletError::Internal(e.to_string())
    }
}

/// Change notifications for the UI (SSE) and later NWC notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WalletEvent {
    TransactionUpdated { public_id: String, state: String },
    BalanceChanged,
}

/// Tunables.
#[derive(Debug, Clone)]
pub struct WalletConfig {
    /// Blocks subtracted from ldk-server's claim deadline (SPEC §6.6.2).
    pub settle_safety_blocks: u32,
    /// How long `pay` waits for a terminal state before answering "pending".
    pub sync_timeout: Duration,
    /// How often the reconciler runs.
    pub reconcile_interval: Duration,
    /// Poll interval while waiting for a payment.
    pub poll_interval: Duration,
}

impl Default for WalletConfig {
    fn default() -> Self {
        WalletConfig {
            settle_safety_blocks: 3,
            sync_timeout: Duration::from_secs(55),
            reconcile_interval: Duration::from_secs(60),
            poll_interval: Duration::from_millis(500),
        }
    }
}

/// Request to pay something.
#[derive(Debug, Clone, Default)]
pub struct PayRequest {
    pub app_id: Option<i64>,
    pub payment: String,
    pub amount_msat: Option<u64>,
    pub max_fee_msat: Option<u64>,
    pub payer_note: Option<String>,
    pub metadata: Option<String>,
}

/// Request for a BOLT11 invoice.
#[derive(Debug, Clone, Default)]
pub struct InvoiceRequest {
    pub app_id: Option<i64>,
    pub amount_msat: Option<u64>,
    pub description: String,
    pub description_hash: Option<String>,
    pub expiry_secs: Option<u32>,
    pub metadata: Option<String>,
}

/// Request for a BOLT12 offer.
#[derive(Debug, Clone, Default)]
pub struct OfferRequest {
    pub app_id: Option<i64>,
    pub amount_msat: Option<u64>,
    pub description: String,
    pub expires_at: Option<i64>,
    pub metadata: Option<String>,
}

/// NWC `list_transactions` parameters (NWC-05).
#[derive(Debug, Clone)]
pub struct AppTxQuery {
    pub from: i64,
    pub until: i64,
    pub limit: i64,
    pub offset: i64,
    pub unpaid: bool,
    /// `incoming` | `outgoing`
    pub direction: Option<String>,
}

/// Filter for listing transactions.
#[derive(Debug, Clone, Default)]
pub struct TxFilter {
    /// `Some(None)` = Main only; `Some(Some(id))` = one app; `None` = all.
    pub app_id: Option<Option<i64>>,
    pub direction: Option<String>,
    pub state: Option<String>,
    pub payment_type: Option<String>,
    /// Text search in description / payment hash.
    pub query: Option<String>,
    /// Include PENDING incoming invoices that were never paid.
    pub include_unpaid: bool,
    pub limit: i64,
    pub offset: i64,
}

/// How node funds are allocated (SPEC §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Allocation {
    pub main_msat: i64,
    pub accounts_total_msat: i64,
    pub pending_out_msat: i64,
}

/// Preview of a payment string for the Send screen.
#[derive(Debug, Clone, Serialize)]
pub struct Decoded {
    /// bolt11 | bolt12 | onchain
    pub kind: &'static str,
    pub amount_msat: Option<u64>,
    pub description: Option<String>,
    pub payee: Option<String>,
    pub expires_at: Option<i64>,
    pub is_expired: bool,
    pub network_ok: bool,
    /// The invoice or offer was created by this node.
    pub self_payment: bool,
    pub address: Option<String>,
    /// LNURL-pay limits and LUD-12 comment length (Lightning addresses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_msat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_msat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_allowed: Option<u64>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn random_preimage() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// SPEC §6.8: the caller's max fee, or max(1%, 10 sat).
pub fn fee_reserve_msat(amount_msat: u64, max_fee_msat: Option<u64>) -> u64 {
    max_fee_msat.unwrap_or_else(|| (amount_msat.div_ceil(100)).max(10_000))
}

fn i(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Resolves the amount to pay (SPEC §7.2.1): conflicting amounts are rejected.
pub fn resolve_amount(
    instruction_msat: Option<u64>,
    param_msat: Option<u64>,
    uri_msat: Option<u64>,
) -> Result<u64, WalletError> {
    for a in [param_msat, uri_msat].into_iter().flatten() {
        if a == 0 {
            return Err(WalletError::BadRequest("amount must be greater than 0".into()));
        }
    }
    match instruction_msat {
        Some(fixed) if fixed > 0 => {
            if param_msat.is_some_and(|a| a != fixed) || uri_msat.is_some_and(|a| a != fixed) {
                return Err(WalletError::BadRequest(
                    "amount conflicts with the amount of the payment instruction".into(),
                ));
            }
            Ok(fixed)
        }
        _ => match (param_msat, uri_msat) {
            (Some(a), Some(b)) if a != b => {
                Err(WalletError::BadRequest("amount conflicts with the URI amount".into()))
            }
            (Some(a), _) | (None, Some(a)) => Ok(a),
            (None, None) => Err(WalletError::BadRequest(
                "amount is required for this payment instruction".into(),
            )),
        },
    }
}

fn network_matches(node: Network, instruction: &str) -> bool {
    let i = instruction.to_ascii_lowercase();
    match node {
        Network::Bitcoin => i == "bitcoin" || i == "mainnet" || i == "bc",
        Network::Testnet => i.starts_with("testnet") && i != "testnet4",
        Network::Testnet4 => i == "testnet4" || i == "testnet",
        Network::Signet => i == "signet",
        Network::Regtest => i == "regtest",
    }
}

/// The wallet service.
pub struct Wallet {
    pool: AnyPool,
    node: Arc<dyn LightningBackend>,
    write_lock: Mutex<()>,
    events: broadcast::Sender<WalletEvent>,
    cfg: WalletConfig,
    network: Mutex<Option<Network>>,
    /// Reasons of `PaymentFailed` events that arrived before their row knew the payment
    /// id (keyed by ln payment id), so polling can still record the real reason.
    early_failures: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// LNURL-pay client (https only unless `allow_http_lnurl` is called).
    lnurl: std::sync::RwLock<allod_lnurl::LnurlClient>,
}

impl Wallet {
    pub fn new(pool: AnyPool, node: Arc<dyn LightningBackend>, cfg: WalletConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(512);
        Arc::new(Wallet {
            pool,
            node,
            write_lock: Mutex::new(()),
            events,
            cfg,
            network: Mutex::new(None),
            early_failures: Default::default(),
            lnurl: std::sync::RwLock::new(allod_lnurl::LnurlClient::new(false)),
        })
    }

    /// Starts a write transaction. On SQLite it takes the write lock up front
    /// (`BEGIN IMMEDIATE`): a deferred transaction that reads first cannot upgrade while
    /// another connection (API audit log, settings) writes, and fails with "database is
    /// locked" instead of waiting for the busy timeout.
    async fn begin(&self) -> Result<sqlx::Transaction<'static, sqlx::Any>, sqlx::Error> {
        if self.pool.connect_options().database_url.scheme() == "sqlite" {
            self.pool.begin_with("BEGIN IMMEDIATE").await
        } else {
            self.pool.begin().await
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WalletEvent> {
        self.events.subscribe()
    }

    pub fn node(&self) -> &Arc<dyn LightningBackend> {
        &self.node
    }

    fn notify(&self, tx: &Tx) {
        let _ = self.events.send(WalletEvent::TransactionUpdated {
            public_id: tx.public_id.clone(),
            state: tx.state.clone(),
        });
        let _ = self.events.send(WalletEvent::BalanceChanged);
    }

    async fn network(&self) -> Result<Network, WalletError> {
        let mut cached = self.network.lock().await;
        if let Some(n) = *cached {
            return Ok(n);
        }
        let n = self.node.node_info().await?.network.unwrap_or(Network::Bitcoin);
        *cached = Some(n);
        Ok(n)
    }

    /// Starts the event handler and the periodic reconciler.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        let mut rx = self.node.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if let Err(e) = me.handle_event(ev).await {
                            tracing::warn!(error = %e, "wallet event handling failed");
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            missed = n,
                            "wallet lagged behind backend events; reconciling"
                        );
                        let _ = me.reconcile().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = me.reconcile().await {
                    tracing::warn!(error = %e, "reconcile failed");
                }
                tokio::time::sleep(me.cfg.reconcile_interval).await;
            }
        });
    }

    async fn conn(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Any>, WalletError> {
        Ok(self.pool.acquire().await?)
    }

    /// The budget of an app (NWC `get_budget`), `None` when it has no limit.
    pub async fn budget(&self, app_id: i64) -> Result<Option<budget::Budget>, WalletError> {
        let mut c = self.conn().await?;
        budget::get(&mut c, app_id, now()).await
    }

    /// Spendable balance of an app: its own account if isolated, else Main.
    pub async fn app_balance(&self, app_id: Option<i64>) -> Result<i64, WalletError> {
        let mut c = self.conn().await?;
        let code = Self::wallet_code(&mut c, app_id).await?;
        Ok(ledger::balance(&mut c, &code).await?.max(0))
    }

    /// Ledger account that holds the funds of `app_id` (SPEC §6.3).
    async fn wallet_code(
        conn: &mut AnyConnection,
        app_id: Option<i64>,
    ) -> Result<String, WalletError> {
        let Some(id) = app_id else { return Ok(ledger::MAIN.into()) };
        let isolated: Option<i64> =
            sqlx::query("SELECT CAST(isolated AS INTEGER) FROM apps WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?
                .map(|r| r.get::<i64, _>(0));
        Ok(if isolated == Some(1) { ledger::app_wallet(id) } else { ledger::MAIN.into() })
    }

    // ------------------------------------------------------------------ queries

    pub async fn transaction(&self, public_id: &str) -> Result<Tx, WalletError> {
        let mut c = self.conn().await?;
        store::by_public_id(&mut c, public_id).await?.ok_or(WalletError::NotFound)
    }

    /// Lets LNURL use plain `http://` for localhost (tests and regtest only).
    pub fn allow_http_lnurl(&self) {
        *self.lnurl.write().unwrap_or_else(|e| e.into_inner()) =
            allod_lnurl::LnurlClient::new(true);
    }

    fn lnurl(&self) -> allod_lnurl::LnurlClient {
        self.lnurl.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Pays a Lightning address or LNURL: resolve, fetch an invoice for the amount (with
    /// the note as LUD-12 comment), check it, and pay it. The recipient's identifier and
    /// the comment are kept in the payment's metadata (NWC-06 `recipient_data`, `comment`).
    async fn pay_lnurl(&self, req: PayRequest, target: &str) -> Result<Tx, WalletError> {
        let amount = req.amount_msat.filter(|a| *a > 0).ok_or_else(|| {
            WalletError::BadRequest("enter an amount to pay a Lightning address".into())
        })?;
        let client = self.lnurl();
        let p = client.resolve(target).await.map_err(lnurl_error)?;
        let comment = req.payer_note.clone().filter(|c| !c.is_empty());
        let invoice = client.invoice(&p, amount, comment.as_deref()).await.map_err(lnurl_error)?;
        let d = self.node.decode_invoice(&invoice).await.map_err(bad_input)?;
        p.check_invoice(amount, d.amount_msat, d.description_hash.as_deref())
            .map_err(lnurl_error)?;
        let mut meta: serde_json::Map<String, serde_json::Value> =
            req.metadata.as_deref().and_then(|m| serde_json::from_str(m).ok()).unwrap_or_default();
        let identifier = p.identifier.clone().unwrap_or_else(|| p.target.clone());
        meta.entry("recipient_data")
            .or_insert_with(|| serde_json::json!({"identifier": identifier}));
        if let Some(c) = &comment {
            meta.entry("comment").or_insert_with(|| serde_json::json!(c));
        }
        let inner = PayRequest {
            payment: invoice.clone(),
            amount_msat: None,
            payer_note: None,
            metadata: Some(serde_json::Value::Object(meta).to_string()),
            ..req
        };
        self.pay_bolt11(&inner, &invoice, None).await
    }

    /// What the Lightning backend supports.
    pub fn capabilities(&self) -> allod_ln::types::Capabilities {
        self.node.capabilities()
    }

    /// A transaction by its internal id.
    pub async fn transaction_by_id(&self, id: i64) -> Result<Tx, WalletError> {
        let mut c = self.conn().await?;
        store::by_id(&mut c, id).await?.ok_or(WalletError::NotFound)
    }

    /// Notifications ready to publish (SPEC §8.5).
    pub async fn outbox_due(&self, limit: i64) -> Result<Vec<outbox::Due>, WalletError> {
        let mut c = self.conn().await?;
        outbox::due(&mut c, now(), limit).await
    }

    pub async fn outbox_done(&self, d: &outbox::Due, sent: bool) -> Result<(), WalletError> {
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        if sent {
            outbox::mark_sent(&mut c, d.id).await
        } else {
            outbox::mark_failed(&mut c, d, now()).await
        }
    }

    /// NWC `list_transactions` (SPEC §8.4 #8, NWC-05): rows visible to the app (§9.3),
    /// newest first, with the total count before paging.
    pub async fn list_for_app(
        &self,
        app_id: i64,
        isolated: bool,
        q: &AppTxQuery,
    ) -> Result<(Vec<Tx>, i64), WalletError> {
        let mut conds = vec![crate::apps::visibility_clause(isolated, 1)];
        conds.push("created_at >= $2 AND created_at <= $3".into());
        if let Some(d) = &q.direction {
            if d != "incoming" && d != "outgoing" {
                return Err(WalletError::BadRequest("type must be incoming or outgoing".into()));
            }
            conds.push(format!("direction = '{d}'"));
        }
        if !q.unpaid {
            conds.push(
                "NOT (direction = 'incoming' AND state IN ('PENDING', 'EXPIRED', 'CANCELED'))"
                    .into(),
            );
        }
        let wh = conds.join(" AND ");
        let mut c = self.conn().await?;
        let total: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM transactions WHERE {wh}"))
                .bind(app_id)
                .bind(q.from)
                .bind(q.until)
                .fetch_one(&mut *c)
                .await?;
        let rows = sqlx::query_as::<_, Tx>(&format!(
            "SELECT {} FROM transactions WHERE {wh} ORDER BY created_at DESC, id DESC LIMIT $4 OFFSET $5",
            store::TX_COLUMNS
        ))
        .bind(app_id)
        .bind(q.from)
        .bind(q.until)
        .bind(q.limit)
        .bind(q.offset)
        .fetch_all(&mut *c)
        .await?;
        Ok((rows, total))
    }

    /// Rows visible to the app that match every `(column, value)` pair (at most 2, enough
    /// to detect `MULTIPLE_MATCHES` in NWC `lookup_payment`). Columns are fixed by callers.
    pub async fn find_for_app(
        &self,
        app_id: i64,
        isolated: bool,
        conds: &[(&str, String)],
    ) -> Result<Vec<Tx>, WalletError> {
        const ALLOWED: &[&str] =
            &["public_id", "payment_hash", "payment_request", "offer_id", "payment_type"];
        let mut wh = vec![crate::apps::visibility_clause(isolated, 1)];
        for (i, (col, _)) in conds.iter().enumerate() {
            if !ALLOWED.contains(col) {
                return Err(WalletError::Internal(format!("unexpected lookup column {col}")));
            }
            wh.push(format!("{col} = ${}", i + 2));
        }
        let sql = format!(
            "SELECT {} FROM transactions WHERE {} ORDER BY id DESC LIMIT 2",
            store::TX_COLUMNS,
            wh.join(" AND ")
        );
        let mut query = sqlx::query_as::<_, Tx>(&sql).bind(app_id);
        for (_, v) in conds {
            query = query.bind(v.clone());
        }
        let mut c = self.conn().await?;
        Ok(query.fetch_all(&mut *c).await?)
    }

    /// NWC `lookup_invoice` (SPEC §8.4 #7): the row with this payment hash or invoice
    /// that the app may see (§9.3). With several, prefer the app's own row, then the
    /// incoming one.
    pub async fn lookup_for_app(
        &self,
        app_id: i64,
        isolated: bool,
        payment_hash: Option<&str>,
        invoice: Option<&str>,
    ) -> Result<Tx, WalletError> {
        let (col, val) = match (payment_hash, invoice) {
            (Some(h), _) => ("payment_hash", h.to_ascii_lowercase()),
            (None, Some(i)) => ("payment_request", i.to_string()),
            (None, None) => {
                return Err(WalletError::BadRequest("payment_hash or invoice is required".into()))
            }
        };
        let mut c = self.conn().await?;
        let rows = sqlx::query_as::<_, Tx>(&format!(
            "SELECT {} FROM transactions WHERE {col} = $1 AND {} ORDER BY id DESC LIMIT 20",
            store::TX_COLUMNS,
            crate::apps::visibility_clause(isolated, 2)
        ))
        .bind(&val)
        .bind(app_id)
        .fetch_all(&mut *c)
        .await?;
        rows.iter()
            .find(|t| t.app_id == Some(app_id))
            .or_else(|| rows.iter().find(|t| t.direction == "incoming"))
            .or_else(|| rows.first())
            .cloned()
            .ok_or(WalletError::NotFound)
    }

    pub async fn transactions(&self, f: &TxFilter) -> Result<(Vec<Tx>, i64), WalletError> {
        enum B {
            I(i64),
            S(String),
        }
        let mut conds: Vec<String> = Vec::new();
        let mut binds: Vec<B> = Vec::new();
        match &f.app_id {
            Some(None) => conds.push("app_id IS NULL".into()),
            Some(Some(id)) => {
                binds.push(B::I(*id));
                conds.push(format!("app_id = ${}", binds.len()));
            }
            None => {}
        }
        for (col, v) in
            [("direction", &f.direction), ("state", &f.state), ("payment_type", &f.payment_type)]
        {
            if let Some(v) = v {
                let v = if col == "state" { v.to_ascii_uppercase() } else { v.clone() };
                binds.push(B::S(v));
                conds.push(format!("{col} = ${}", binds.len()));
            }
        }
        if let Some(q) = f.query.as_ref().map(|q| q.trim()).filter(|q| !q.is_empty()) {
            binds.push(B::S(format!("%{}%", q.to_lowercase())));
            let like = binds.len();
            binds.push(B::S(q.to_lowercase()));
            conds.push(format!(
                "(LOWER(description) LIKE ${like} OR payment_hash = ${})",
                binds.len()
            ));
        }
        if !f.include_unpaid {
            conds.push("NOT (direction = 'incoming' AND state IN ('PENDING', 'EXPIRED', 'CANCELED') AND hold = FALSE)".into());
        }
        let wher =
            if conds.is_empty() { String::new() } else { format!("WHERE {}", conds.join(" AND ")) };
        let count_sql = format!("SELECT COUNT(*) FROM transactions {wher}");
        let list_sql = format!(
            "SELECT {} FROM transactions {wher} ORDER BY created_at DESC, id DESC LIMIT {} OFFSET {}",
            store::TX_COLUMNS,
            f.limit.clamp(1, 500),
            f.offset.max(0)
        );
        let mut c = self.conn().await?;
        let mut q = sqlx::query(&count_sql);
        for b in &binds {
            q = match b {
                B::I(v) => q.bind(*v),
                B::S(v) => q.bind(v.clone()),
            };
        }
        let total: i64 = q.fetch_one(&mut *c).await?.get(0);
        let mut q = sqlx::query_as::<_, Tx>(&list_sql);
        for b in &binds {
            q = match b {
                B::I(v) => q.bind(*v),
                B::S(v) => q.bind(v.clone()),
            };
        }
        Ok((q.fetch_all(&mut *c).await?, total))
    }

    pub async fn offers(&self, app_id: Option<Option<i64>>) -> Result<Vec<OfferRow>, WalletError> {
        let mut c = self.conn().await?;
        let rows = match app_id {
            None => {
                sqlx::query_as::<_, OfferRow>(&format!(
                    "SELECT {} FROM offers ORDER BY id DESC",
                    store::OFFER_COLUMNS
                ))
                .fetch_all(&mut *c)
                .await?
            }
            Some(None) => {
                sqlx::query_as::<_, OfferRow>(&format!(
                    "SELECT {} FROM offers WHERE app_id IS NULL ORDER BY id DESC",
                    store::OFFER_COLUMNS
                ))
                .fetch_all(&mut *c)
                .await?
            }
            Some(Some(id)) => {
                sqlx::query_as::<_, OfferRow>(&format!(
                    "SELECT {} FROM offers WHERE app_id = $1 ORDER BY id DESC",
                    store::OFFER_COLUMNS
                ))
                .bind(id)
                .fetch_all(&mut *c)
                .await?
            }
        };
        Ok(rows)
    }

    pub async fn set_offer_state(
        &self,
        offer_id: &str,
        state: &str,
    ) -> Result<OfferRow, WalletError> {
        if !["active", "disabled"].contains(&state) {
            return Err(WalletError::BadRequest("state must be active or disabled".into()));
        }
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        sqlx::query("UPDATE offers SET state = $1 WHERE offer_id = $2")
            .bind(state)
            .bind(offer_id)
            .execute(&mut *c)
            .await?;
        store::offer_by_id(&mut c, offer_id).await?.ok_or(WalletError::NotFound)
    }

    pub async fn allocation(&self) -> Result<Allocation, WalletError> {
        let mut c = self.conn().await?;
        Ok(Allocation {
            main_msat: ledger::balance(&mut c, ledger::MAIN).await?,
            accounts_total_msat: ledger::kind_total(&mut c, "wallet", Some(ledger::MAIN)).await?,
            pending_out_msat: ledger::balance(&mut c, ledger::PENDING_OUT).await?,
        })
    }

    pub async fn balance(&self, app_id: Option<i64>) -> Result<i64, WalletError> {
        let mut c = self.conn().await?;
        let code = Self::wallet_code(&mut c, app_id).await?;
        ledger::balance(&mut c, &code).await
    }

    /// Ledger invariant problems (empty = healthy).
    pub async fn ledger_check(&self) -> Result<Vec<String>, WalletError> {
        let mut c = self.conn().await?;
        ledger::check(&mut c).await
    }

    pub async fn update_labels(
        &self,
        public_id: &str,
        labels: &[String],
    ) -> Result<Tx, WalletError> {
        let json =
            serde_json::to_string(labels).map_err(|e| WalletError::Internal(e.to_string()))?;
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        let n = sqlx::query(
            "UPDATE transactions SET labels = $1, updated_at = $2 WHERE public_id = $3",
        )
        .bind(json)
        .bind(now())
        .bind(public_id)
        .execute(&mut *c)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(WalletError::NotFound);
        }
        store::by_public_id(&mut c, public_id).await?.ok_or(WalletError::NotFound)
    }

    /// Preview for the Send screen.
    pub async fn decode(&self, payment: &str) -> Result<Decoded, WalletError> {
        let parsed = bip321::parse(payment)?;
        let network = self.network().await?;
        let mut c = self.conn().await?;
        Ok(match parsed.instruction {
            Instruction::Bolt11(inv) => {
                let d = self.node.decode_invoice(&inv).await.map_err(bad_input)?;
                let own: Option<i64> = sqlx::query("SELECT id FROM transactions WHERE direction = 'incoming' AND payment_request = $1")
                    .bind(&inv).fetch_optional(&mut *c).await?.map(|r| r.get(0));
                Decoded {
                    kind: "bolt11",
                    amount_msat: d.amount_msat.or(parsed.uri_amount_msat),
                    description: d.description,
                    payee: Some(d.destination),
                    expires_at: Some(i(d.timestamp + d.expiry_secs)),
                    is_expired: d.is_expired,
                    network_ok: network_matches(network, &d.network),
                    self_payment: own.is_some(),
                    address: None,
                    min_msat: None,
                    max_msat: None,
                    comment_allowed: None,
                }
            }
            Instruction::Bolt12(offer) => {
                let d = self.node.decode_offer(&offer).await.map_err(bad_input)?;
                let own = store::offer_by_id(&mut c, &d.offer_id).await?.is_some();
                Decoded {
                    kind: "bolt12",
                    amount_msat: d.amount_msat.or(parsed.uri_amount_msat),
                    description: d.description,
                    payee: d.issuer,
                    expires_at: d.absolute_expiry.map(i),
                    is_expired: d.is_expired,
                    network_ok: d.chains.is_empty()
                        || d.chains.iter().any(|ch| network_matches(network, ch)),
                    self_payment: own,
                    address: None,
                    min_msat: None,
                    max_msat: None,
                    comment_allowed: None,
                }
            }
            Instruction::Onchain { address } => Decoded {
                kind: "onchain",
                amount_msat: parsed.uri_amount_msat,
                description: None,
                payee: None,
                expires_at: None,
                is_expired: false,
                network_ok: true,
                self_payment: false,
                address: Some(address),
                min_msat: None,
                max_msat: None,
                comment_allowed: None,
            },
            Instruction::Lnurl(target) => {
                drop(c);
                let p = self.lnurl().resolve(&target).await.map_err(lnurl_error)?;
                Decoded {
                    kind: "lnurl",
                    amount_msat: (p.min_sendable_msat == p.max_sendable_msat)
                        .then_some(p.min_sendable_msat),
                    description: p.description,
                    payee: p.identifier,
                    expires_at: None,
                    is_expired: false,
                    network_ok: true,
                    self_payment: false,
                    address: None,
                    min_msat: Some(p.min_sendable_msat),
                    max_msat: Some(p.max_sendable_msat),
                    comment_allowed: Some(p.comment_allowed),
                }
            }
        })
    }

    // ------------------------------------------------------------------ receive

    /// Creates a BOLT11 invoice (SPEC §7.4).
    pub async fn create_invoice(&self, req: InvoiceRequest) -> Result<Tx, WalletError> {
        if req.amount_msat == Some(0) {
            return Err(WalletError::BadRequest("amount must be greater than 0".into()));
        }
        let expiry = req.expiry_secs.unwrap_or(86_400).clamp(60, 31_536_000);
        let desc = match &req.description_hash {
            Some(h) => InvoiceDescription::Hash(h.clone()),
            None => InvoiceDescription::Direct(req.description.clone()),
        };
        let created = self.node.bolt11_receive(req.amount_msat, desc, expiry).await?;
        // ldk-node keeps the preimage of invoices it created (needed for self payments).
        let preimage = self
            .node
            .payment(&created.payment_hash)
            .await
            .ok()
            .flatten()
            .and_then(|p| p.kind.preimage().map(str::to_string));
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        let tx = store::insert(
            &mut c,
            &NewTx {
                app_id: req.app_id,
                direction: "incoming",
                payment_type: "bolt11",
                state: "PENDING",
                amount_msat: i(req.amount_msat.unwrap_or(0)),
                ln_payment_id: Some(created.payment_hash.clone()),
                payment_request: Some(created.invoice.to_ascii_lowercase()),
                payment_hash: Some(created.payment_hash),
                preimage,
                description: req.description,
                description_hash: req.description_hash,
                metadata: req.metadata,
                expires_at: Some(t + i64::from(expiry)),
                ..Default::default()
            },
            t,
        )
        .await?;
        drop(c);
        self.notify(&tx);
        Ok(tx)
    }

    /// Creates a hold invoice for a caller-chosen payment hash (SPEC §7.5).
    pub async fn create_hold_invoice(
        &self,
        req: InvoiceRequest,
        payment_hash: &str,
    ) -> Result<Tx, WalletError> {
        let hash = payment_hash.trim().to_ascii_lowercase();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(WalletError::BadRequest("payment_hash must be 32 bytes of hex".into()));
        }
        let amount = req
            .amount_msat
            .filter(|a| *a > 0)
            .ok_or_else(|| WalletError::BadRequest("hold invoices need an amount".into()))?;
        let expiry = req.expiry_secs.unwrap_or(86_400).clamp(60, 31_536_000);
        let desc = match &req.description_hash {
            Some(h) => InvoiceDescription::Hash(h.clone()),
            None => InvoiceDescription::Direct(req.description.clone()),
        };
        let invoice = self.node.bolt11_receive_for_hash(Some(amount), desc, expiry, &hash).await?;
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        let tx = store::insert(
            &mut c,
            &NewTx {
                app_id: req.app_id,
                direction: "incoming",
                payment_type: "bolt11",
                state: "PENDING",
                amount_msat: i(amount),
                ln_payment_id: Some(hash.clone()),
                payment_request: Some(invoice.to_ascii_lowercase()),
                payment_hash: Some(hash),
                description: req.description,
                description_hash: req.description_hash,
                metadata: req.metadata,
                hold: true,
                expires_at: Some(t + i64::from(expiry)),
                ..Default::default()
            },
            t,
        )
        .await?;
        drop(c);
        self.notify(&tx);
        Ok(tx)
    }

    /// Creates a BOLT12 offer owned by `app_id` (SPEC §6.6.4).
    pub async fn create_offer(&self, mut req: OfferRequest) -> Result<OfferRow, WalletError> {
        if req.amount_msat == Some(0) {
            return Err(WalletError::BadRequest("amount must be greater than 0".into()));
        }
        // BOLT12 requires a description when the offer has an amount. The description is
        // optional for the user, so fall back to the node alias, or "Payment".
        if req.amount_msat.is_some() && req.description.trim().is_empty() {
            let alias = self.node.node_info().await.ok().and_then(|n| n.alias);
            req.description = alias
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| "Payment".into());
        }
        let t = now();
        let expiry_secs = match req.expires_at {
            Some(e) if e <= t => {
                return Err(WalletError::BadRequest("expires_at must be in the future".into()))
            }
            Some(e) => Some(u32::try_from(e - t).unwrap_or(u32::MAX)),
            None => None,
        };
        let created =
            self.node.bolt12_receive(&req.description, req.amount_msat, expiry_secs).await?;
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        sqlx::query(
            "INSERT INTO offers (offer_id, offer, app_id, amount_msat, description, single_use, metadata, \
             state, expires_at, created_at) VALUES ($1, $2, $3, $4, $5, FALSE, $6, 'active', $7, $8)",
        )
        .bind(&created.offer_id).bind(&created.offer).bind(req.app_id).bind(req.amount_msat.map(i))
        .bind(&req.description).bind(&req.metadata).bind(req.expires_at).bind(t)
        .execute(&mut *c).await?;
        store::offer_by_id(&mut c, &created.offer_id).await?.ok_or(WalletError::NotFound)
    }

    /// BIP-321 receive (SPEC §8.4.15): with an amount, a BOLT11 invoice and a fixed
    /// offer; without, a variable-amount offer.
    pub async fn receive(
        &self,
        app_id: Option<i64>,
        amount_msat: Option<u64>,
        description: String,
        metadata: Option<String>,
    ) -> Result<(String, Option<Tx>), WalletError> {
        let offer = self
            .create_offer(OfferRequest {
                app_id,
                amount_msat,
                description: description.clone(),
                metadata: metadata.clone(),
                ..Default::default()
            })
            .await?;
        match amount_msat {
            Some(a) => {
                let tx = self
                    .create_invoice(InvoiceRequest {
                        app_id,
                        amount_msat: Some(a),
                        description,
                        metadata,
                        ..Default::default()
                    })
                    .await?;
                let inv = tx.payment_request.clone().unwrap_or_default();
                Ok((format!("bitcoin:?lightning={inv}&lno={}", offer.offer), Some(tx)))
            }
            None => Ok((format!("bitcoin:?lno={}", offer.offer), None)),
        }
    }

    /// Settles an accepted hold invoice owned by `app_id` (SPEC §7.5, §6.6.1).
    pub async fn settle_hold(
        &self,
        app_id: Option<i64>,
        preimage: &str,
    ) -> Result<Tx, WalletError> {
        let pre = preimage.trim().to_ascii_lowercase();
        let bytes = hex::decode(&pre)
            .ok()
            .filter(|b| b.len() == 32)
            .ok_or_else(|| WalletError::BadRequest("preimage must be 32 bytes of hex".into()))?;
        let hash = sha256_hex(&bytes);
        let tx = self.owned_hold(app_id, &hash).await?;
        if tx.state != "ACCEPTED" {
            return Err(WalletError::BadRequest(format!(
                "hold invoice is {}, not ACCEPTED",
                tx.state
            )));
        }
        if tx.counterpart_tx_id.is_some() {
            return self.settle_internal_hold(&tx, &pre).await;
        }
        let ln_id = tx.ln_payment_id.clone().unwrap_or(hash);
        let amount = tx.claimable_amount_msat.unwrap_or(tx.amount_msat);
        self.node.bolt11_claim_for_id(&ln_id, u64::try_from(amount).unwrap_or(0), &pre).await?;
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        if store::transition(&mut dbtx, tx.id, &["ACCEPTED"], "SETTLED", t).await? {
            sqlx::query("UPDATE transactions SET preimage = $1, amount_msat = $2 WHERE id = $3")
                .bind(&pre)
                .bind(amount)
                .bind(tx.id)
                .execute(&mut *dbtx)
                .await?;
            let code = Self::wallet_code(&mut dbtx, tx.app_id).await?;
            ledger::post(
                &mut dbtx,
                "receive_settle",
                Some(tx.id),
                "hold invoice settled",
                &[(ledger::EXTERNAL, -amount), (&code, amount)],
                t,
            )
            .await?;
        }
        let out = store::by_id(&mut dbtx, tx.id).await?.ok_or(WalletError::NotFound)?;
        dbtx.commit().await?;
        self.notify(&out);
        Ok(out)
    }

    /// Cancels a hold invoice owned by `app_id`.
    pub async fn cancel_hold(
        &self,
        app_id: Option<i64>,
        payment_hash: &str,
    ) -> Result<Tx, WalletError> {
        let hash = payment_hash.trim().to_ascii_lowercase();
        let tx = self.owned_hold(app_id, &hash).await?;
        if !["PENDING", "ACCEPTED"].contains(&tx.state.as_str()) {
            return Err(WalletError::BadRequest(format!("hold invoice is {}", tx.state)));
        }
        if tx.counterpart_tx_id.is_none() {
            if let Some(id) = &tx.ln_payment_id {
                if let Err(e) = self.node.bolt11_fail_for_id(id).await {
                    if tx.state == "ACCEPTED" {
                        return Err(e.into());
                    }
                }
            }
        }
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        if store::transition(&mut dbtx, tx.id, &["PENDING", "ACCEPTED"], "CANCELED", t).await? {
            if let Some(out_id) = tx.counterpart_tx_id {
                if let Some(out) = store::by_id(&mut dbtx, out_id).await? {
                    Self::release(&mut dbtx, &out, "canceled by recipient", t).await?;
                }
            }
        }
        let out = store::by_id(&mut dbtx, tx.id).await?.ok_or(WalletError::NotFound)?;
        dbtx.commit().await?;
        self.notify(&out);
        Ok(out)
    }

    async fn owned_hold(&self, app_id: Option<i64>, hash: &str) -> Result<Tx, WalletError> {
        let mut c = self.conn().await?;
        let tx = sqlx::query_as::<_, Tx>(&format!(
            "SELECT {} FROM transactions WHERE direction = 'incoming' AND hold = TRUE AND payment_hash = $1 \
             ORDER BY id DESC LIMIT 1",
            store::TX_COLUMNS
        ))
        .bind(hash)
        .fetch_optional(&mut *c)
        .await?;
        // Ownership check: another app's hold invoice looks like no invoice at all.
        tx.filter(|t| t.app_id == app_id).ok_or(WalletError::NotFound)
    }

    // ------------------------------------------------------------------ pay

    /// Pays a BOLT11 invoice, BOLT12 offer or BIP-321 URI (SPEC §7.2–7.3).
    pub async fn pay(&self, req: PayRequest) -> Result<Tx, WalletError> {
        let parsed = bip321::parse(&req.payment)?;
        match parsed.instruction {
            Instruction::Bolt11(inv) => self.pay_bolt11(&req, &inv, parsed.uri_amount_msat).await,
            // A node without BOLT12 pays the URI's BOLT11 invoice, if it has one.
            Instruction::Bolt12(_) if !self.node.capabilities().bolt12 => {
                match parsed.bolt11_fallback {
                    Some(inv) => self.pay_bolt11(&req, &inv, parsed.uri_amount_msat).await,
                    None => {
                        Err(WalletError::Unsupported("this node cannot pay BOLT12 offers".into()))
                    }
                }
            }
            Instruction::Bolt12(offer) => {
                self.pay_bolt12(&req, &offer, parsed.uri_amount_msat).await
            }
            Instruction::Lnurl(target) => self.pay_lnurl(req, &target).await,
            Instruction::Onchain { .. } => Err(WalletError::BadRequest(
                "this is an on-chain address; use on-chain send".into(),
            )),
        }
    }

    async fn pay_bolt11(
        &self,
        req: &PayRequest,
        inv: &str,
        uri_amount: Option<u64>,
    ) -> Result<Tx, WalletError> {
        if req.payer_note.as_deref().is_some_and(|n| !n.is_empty()) {
            return Err(WalletError::BadRequest("payer notes need a BOLT12 offer".into()));
        }
        let d = self.node.decode_invoice(inv).await.map_err(bad_input)?;
        let network = self.network().await?;
        if !network_matches(network, &d.network) {
            return Err(WalletError::WrongNetwork(network.to_string()));
        }
        if d.is_expired {
            return Err(WalletError::Expired);
        }
        let amount = resolve_amount(d.amount_msat, req.amount_msat, uri_amount)?;

        // Self payment: the invoice was created by this node (SPEC §6.7).
        {
            let mut c = self.conn().await?;
            let own = sqlx::query_as::<_, Tx>(&format!(
                "SELECT {} FROM transactions WHERE direction = 'incoming' AND payment_request = $1 ORDER BY id DESC LIMIT 1",
                store::TX_COLUMNS
            ))
            .bind(inv)
            .fetch_optional(&mut *c)
            .await?;
            if let Some(incoming) = own {
                drop(c);
                return self.self_pay_bolt11(req, incoming, amount).await;
            }
        }

        let reserve = fee_reserve_msat(amount, req.max_fee_msat);
        let tx = self
            .reserve(
                req,
                NewTx {
                    app_id: req.app_id,
                    direction: "outgoing",
                    payment_type: "bolt11",
                    state: "PENDING",
                    amount_msat: i(amount),
                    fee_reserve_msat: i(reserve),
                    max_fee_msat: req.max_fee_msat.map(i),
                    ln_payment_id: Some(d.payment_hash.clone()),
                    payment_request: Some(inv.to_string()),
                    payment_hash: Some(d.payment_hash.clone()),
                    description: d.description.clone().unwrap_or_default(),
                    description_hash: d.description_hash.clone(),
                    metadata: req.metadata.clone(),
                    expires_at: Some(i(d.timestamp + d.expiry_secs)),
                    ..Default::default()
                },
                Some(inv),
            )
            .await?;

        let send_amount = if d.amount_msat.unwrap_or(0) == 0 { Some(amount) } else { None };
        match self.node.bolt11_send(inv, send_amount, req.max_fee_msat).await {
            Ok(_) => {}
            Err(
                e @ (BackendError::InvalidRequest(_)
                | BackendError::Lightning(_)
                | BackendError::Permission(_)),
            ) => {
                self.fail_outgoing(tx.id, &e.to_string()).await?;
                return Err(WalletError::PaymentFailed(e.to_string()));
            }
            // Outcome unknown: keep PENDING for the reconciler (payment id = hash).
            Err(e) => {
                tracing::warn!(error = %e, "bolt11 send outcome unknown; reconciler will resolve it")
            }
        }
        self.wait(tx.id, &d.payment_hash).await
    }

    async fn pay_bolt12(
        &self,
        req: &PayRequest,
        offer: &str,
        uri_amount: Option<u64>,
    ) -> Result<Tx, WalletError> {
        let d = self.node.decode_offer(offer).await.map_err(bad_input)?;
        if d.is_expired {
            return Err(WalletError::Expired);
        }
        if d.currency.is_some() {
            return Err(WalletError::Unsupported("offers priced in fiat are not supported".into()));
        }
        let network = self.network().await?;
        if !d.chains.is_empty() && !d.chains.iter().any(|c| network_matches(network, c)) {
            return Err(WalletError::WrongNetwork(network.to_string()));
        }
        let amount = resolve_amount(d.amount_msat, req.amount_msat, uri_amount)?;
        {
            let mut c = self.conn().await?;
            if let Some(own) = store::offer_by_id(&mut c, &d.offer_id).await? {
                drop(c);
                return self.self_pay_bolt12(req, own, amount).await;
            }
        }
        let reserve = fee_reserve_msat(amount, req.max_fee_msat);
        let tx = self
            .reserve(
                req,
                NewTx {
                    app_id: req.app_id,
                    direction: "outgoing",
                    payment_type: "bolt12",
                    state: "PENDING",
                    amount_msat: i(amount),
                    fee_reserve_msat: i(reserve),
                    max_fee_msat: req.max_fee_msat.map(i),
                    offer_id: Some(d.offer_id.clone()),
                    offer: Some(offer.to_string()),
                    payer_note: req.payer_note.clone().filter(|n| !n.is_empty()),
                    description: d.description.clone().unwrap_or_default(),
                    metadata: req.metadata.clone(),
                    ..Default::default()
                },
                None,
            )
            .await?;
        let send_amount = if d.amount_msat.is_none() { Some(amount) } else { None };
        let payment_id = match self
            .node
            .bolt12_send(offer, send_amount, req.payer_note.clone(), req.max_fee_msat)
            .await
        {
            Ok(id) => id,
            Err(
                e @ (BackendError::InvalidRequest(_)
                | BackendError::Lightning(_)
                | BackendError::Permission(_)),
            ) => {
                self.fail_outgoing(tx.id, &e.to_string()).await?;
                return Err(WalletError::PaymentFailed(e.to_string()));
            }
            Err(e) => {
                tracing::warn!(error = %e, "bolt12 send outcome unknown; reconciler will resolve it");
                return self.transaction(&tx.public_id).await;
            }
        };
        {
            let _g = self.write_lock.lock().await;
            let mut c = self.conn().await?;
            sqlx::query("UPDATE transactions SET ln_payment_id = $1 WHERE id = $2")
                .bind(&payment_id)
                .bind(tx.id)
                .execute(&mut *c)
                .await?;
        }
        self.wait(tx.id, &payment_id).await
    }

    /// Validation and the PENDING insert under the write lock (SPEC §7.1).
    async fn reserve(
        &self,
        req: &PayRequest,
        new: NewTx,
        invoice: Option<&str>,
    ) -> Result<Tx, WalletError> {
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        if let Some(inv) = invoice {
            let live: Option<String> = sqlx::query(
                "SELECT state FROM transactions WHERE direction = 'outgoing' AND payment_type = 'bolt11' \
                 AND payment_request = $1 AND state IN ('PENDING', 'SETTLED') LIMIT 1",
            )
            .bind(inv)
            .fetch_optional(&mut *dbtx)
            .await?
            .map(|r| r.get(0));
            match live.as_deref() {
                Some("SETTLED") => return Err(WalletError::AlreadyPaid),
                Some(_) => return Err(WalletError::InProgress),
                None => {}
            }
        }
        let code = Self::wallet_code(&mut dbtx, req.app_id).await?;
        let total = new.amount_msat + new.fee_reserve_msat;
        budget::check(&mut dbtx, req.app_id, total, now()).await?;
        if ledger::balance(&mut dbtx, &code).await? < total {
            return Err(WalletError::InsufficientBalance);
        }
        let tx = store::insert(&mut dbtx, &new, t).await?;
        ledger::post(
            &mut dbtx,
            "payment_reserve",
            Some(tx.id),
            "payment started",
            &[(&code, -total), (ledger::PENDING_OUT, total)],
            t,
        )
        .await?;
        dbtx.commit().await?;
        self.notify(&tx);
        Ok(tx)
    }

    /// Waits for a terminal state, polling the backend; events may settle it first.
    async fn wait(&self, tx_id: i64, ln_id: &str) -> Result<Tx, WalletError> {
        let deadline = tokio::time::Instant::now() + self.cfg.sync_timeout;
        // When polling sees a failure first, the `PaymentFailed` event (which carries the
        // reason, e.g. `RouteNotFound`) gets a short grace period to record it.
        let mut failed_since: Option<tokio::time::Instant> = None;
        loop {
            let current = {
                let mut c = self.conn().await?;
                store::by_id(&mut c, tx_id).await?
            }
            .ok_or(WalletError::NotFound)?;
            if current.state != "PENDING" {
                return Ok(current);
            }
            if let Ok(Some(p)) = self.node.payment(ln_id).await {
                match p.status {
                    LnStatus::Succeeded => {
                        self.apply_outgoing_success(&p, None).await?;
                    }
                    LnStatus::Failed => {
                        let since = *failed_since.get_or_insert_with(tokio::time::Instant::now);
                        let early = self
                            .early_failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(ln_id);
                        if let Some(reason) = early {
                            self.fail_outgoing(tx_id, &reason).await?;
                        } else if since.elapsed() >= Duration::from_secs(1) {
                            self.fail_outgoing(tx_id, "payment failed").await?;
                        }
                    }
                    LnStatus::Pending => {}
                }
            }
            if tokio::time::Instant::now() >= deadline {
                if failed_since.is_some() {
                    self.fail_outgoing(tx_id, "payment failed").await?;
                }
                return {
                    let mut c = self.conn().await?;
                    store::by_id(&mut c, tx_id).await?
                }
                .ok_or(WalletError::NotFound);
            }
            tokio::time::sleep(self.cfg.poll_interval).await;
        }
    }

    /// Marks an outgoing payment failed and releases its reserve (idempotent).
    async fn fail_outgoing(&self, tx_id: i64, reason: &str) -> Result<(), WalletError> {
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let Some(tx) = store::by_id(&mut dbtx, tx_id).await? else { return Ok(()) };
        Self::release(&mut dbtx, &tx, reason, t).await?;
        let out = store::by_id(&mut dbtx, tx_id).await?;
        dbtx.commit().await?;
        if let Some(out) = out {
            self.notify(&out);
        }
        Ok(())
    }

    async fn release(
        conn: &mut AnyConnection,
        tx: &Tx,
        reason: &str,
        t: i64,
    ) -> Result<(), WalletError> {
        if store::transition(conn, tx.id, &["PENDING"], "FAILED", t).await? {
            sqlx::query(
                "UPDATE transactions SET failure_reason = $1, fee_reserve_msat = 0 WHERE id = $2",
            )
            .bind(reason)
            .bind(tx.id)
            .execute(&mut *conn)
            .await?;
            let code = Self::wallet_code(conn, tx.app_id).await?;
            let total = tx.amount_msat + tx.fee_reserve_msat;
            ledger::post(
                conn,
                "payment_release",
                Some(tx.id),
                reason,
                &[(ledger::PENDING_OUT, -total), (&code, total)],
                t,
            )
            .await?;
        }
        Ok(())
    }

    /// Settles an outgoing payment from a backend record (idempotent).
    async fn apply_outgoing_success(
        &self,
        p: &LnPayment,
        bolt12_invoice: Option<String>,
    ) -> Result<(), WalletError> {
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let Some(tx) = store::by_ln(&mut dbtx, "outgoing", &p.id, p.kind.payment_hash()).await?
        else {
            return Ok(());
        };
        if store::transition(&mut dbtx, tx.id, &["PENDING"], "SETTLED", t).await? {
            let fee = i(p.fee_msat.unwrap_or(0));
            sqlx::query(
                "UPDATE transactions SET fee_msat = $1, fee_reserve_msat = 0, preimage = COALESCE($2, preimage), \
                 payment_hash = COALESCE(payment_hash, $3), payment_request = COALESCE(payment_request, $4) WHERE id = $5",
            )
            .bind(fee)
            .bind(p.kind.preimage())
            .bind(p.kind.payment_hash())
            .bind(&bolt12_invoice)
            .bind(tx.id)
            .execute(&mut *dbtx)
            .await?;
            let code = Self::wallet_code(&mut dbtx, tx.app_id).await?;
            let a = tx.amount_msat;
            let r = tx.fee_reserve_msat;
            ledger::post(
                &mut dbtx,
                "payment_settle",
                Some(tx.id),
                "payment settled",
                &[
                    (ledger::PENDING_OUT, -(a + r)),
                    (ledger::EXTERNAL, a),
                    (ledger::FEES, fee),
                    (&code, r - fee),
                ],
                t,
            )
            .await?;
            sqlx::query("UPDATE apps SET last_settled_tx_at = $1 WHERE id = $2")
                .bind(t)
                .bind(tx.app_id)
                .execute(&mut *dbtx)
                .await?;
        }
        let out = store::by_id(&mut dbtx, tx.id).await?;
        dbtx.commit().await?;
        // Release the lock before the payer-proof call, which takes it again.
        drop(_g);
        if let Some(out) = out {
            // Payer proof (BOLT12): best effort, never fails the payment.
            if out.payment_type == "bolt12"
                && out.payer_proof.is_none()
                && self.node.capabilities().payer_proof
            {
                if let (Some(pre), Some(inv)) = (&out.preimage, &out.payment_request) {
                    if let Ok(proof) = self.node.bolt12_payer_proof(&p.id, pre, inv).await {
                        let _g2 = self.write_lock.lock().await;
                        let mut c = self.conn().await?;
                        sqlx::query("UPDATE transactions SET payer_proof = $1 WHERE id = $2")
                            .bind(proof)
                            .bind(out.id)
                            .execute(&mut *c)
                            .await?;
                    }
                }
            }
            self.notify(&out);
        }
        Ok(())
    }

    // ------------------------------------------------------------------ self payments

    async fn self_pay_bolt11(
        &self,
        req: &PayRequest,
        incoming: Tx,
        amount: u64,
    ) -> Result<Tx, WalletError> {
        if incoming.state != "PENDING" {
            return Err(WalletError::BadRequest(format!(
                "this invoice is {}",
                incoming.state.to_lowercase()
            )));
        }
        // ldk-server does not return the preimage of invoices it creates (O-13), so a
        // self payment of a normal invoice may settle without one: Allod books both sides
        // and needs no proof of payment from itself.
        let t = now();
        let a = i(amount);
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let payer = Self::wallet_code(&mut dbtx, req.app_id).await?;
        let receiver = Self::wallet_code(&mut dbtx, incoming.app_id).await?;
        budget::check(&mut dbtx, req.app_id, a, t).await?;
        if ledger::balance(&mut dbtx, &payer).await? < a {
            return Err(WalletError::InsufficientBalance);
        }
        let out = store::insert(
            &mut dbtx,
            &NewTx {
                app_id: req.app_id,
                direction: "outgoing",
                payment_type: "bolt11",
                state: if incoming.hold { "PENDING" } else { "SETTLED" },
                amount_msat: a,
                payment_request: incoming.payment_request.clone(),
                payment_hash: incoming.payment_hash.clone(),
                preimage: if incoming.hold { None } else { incoming.preimage.clone() },
                description: incoming.description.clone(),
                metadata: req.metadata.clone(),
                self_payment: true,
                settled_at: (!incoming.hold).then_some(t),
                ..Default::default()
            },
            t,
        )
        .await?;
        sqlx::query("UPDATE transactions SET counterpart_tx_id = $1 WHERE id = $2")
            .bind(incoming.id)
            .bind(out.id)
            .execute(&mut *dbtx)
            .await?;
        sqlx::query("UPDATE transactions SET counterpart_tx_id = $1, self_payment = TRUE, amount_msat = $2 WHERE id = $3")
            .bind(out.id).bind(a).bind(incoming.id).execute(&mut *dbtx).await?;
        if incoming.hold {
            // Internal hold: reserve on the payer; the receiver accepts (SPEC §6.7).
            store::transition(&mut dbtx, incoming.id, &["PENDING"], "ACCEPTED", t).await?;
            ledger::post(
                &mut dbtx,
                "payment_reserve",
                Some(out.id),
                "internal hold payment",
                &[(&payer, -a), (ledger::PENDING_OUT, a)],
                t,
            )
            .await?;
        } else {
            store::transition(&mut dbtx, incoming.id, &["PENDING"], "SETTLED", t).await?;
            ledger::post(
                &mut dbtx,
                "transfer",
                Some(out.id),
                "self payment",
                &[(&payer, -a), (&receiver, a)],
                t,
            )
            .await?;
        }
        let result = store::by_id(&mut dbtx, out.id).await?.ok_or(WalletError::NotFound)?;
        let incoming = store::by_id(&mut dbtx, incoming.id).await?;
        dbtx.commit().await?;
        self.notify(&result);
        if let Some(inc) = incoming {
            self.notify(&inc);
        }
        Ok(result)
    }

    /// An accepted hold invoice whose HTLC the node failed back: nothing was credited, so
    /// only the state changes.
    async fn hold_failed_by_node(&self, tx_id: i64) -> Result<(), WalletError> {
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        if store::transition(&mut c, tx_id, &["ACCEPTED"], "CANCELED", t).await? {
            sqlx::query(
                "UPDATE transactions SET failure_reason = 'canceled by the node' WHERE id = $1",
            )
            .bind(tx_id)
            .execute(&mut *c)
            .await?;
            if let Some(tx) = store::by_id(&mut c, tx_id).await? {
                drop(c);
                self.notify(&tx);
            }
        }
        Ok(())
    }

    async fn settle_internal_hold(&self, incoming: &Tx, preimage: &str) -> Result<Tx, WalletError> {
        let t = now();
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let out_id = incoming.counterpart_tx_id.ok_or(WalletError::NotFound)?;
        if store::transition(&mut dbtx, incoming.id, &["ACCEPTED"], "SETTLED", t).await? {
            store::transition(&mut dbtx, out_id, &["PENDING"], "SETTLED", t).await?;
            sqlx::query("UPDATE transactions SET preimage = $1 WHERE id IN ($2, $3)")
                .bind(preimage)
                .bind(incoming.id)
                .bind(out_id)
                .execute(&mut *dbtx)
                .await?;
            let receiver = Self::wallet_code(&mut dbtx, incoming.app_id).await?;
            let a = incoming.amount_msat;
            ledger::post(
                &mut dbtx,
                "payment_settle",
                Some(out_id),
                "internal hold settled",
                &[(ledger::PENDING_OUT, -a), (&receiver, a)],
                t,
            )
            .await?;
        }
        let result = store::by_id(&mut dbtx, incoming.id).await?.ok_or(WalletError::NotFound)?;
        let out = store::by_id(&mut dbtx, out_id).await?;
        dbtx.commit().await?;
        self.notify(&result);
        if let Some(o) = out {
            self.notify(&o);
        }
        Ok(result)
    }

    async fn self_pay_bolt12(
        &self,
        req: &PayRequest,
        offer: OfferRow,
        amount: u64,
    ) -> Result<Tx, WalletError> {
        if offer.state != "active" && offer.state != "disabled" {
            return Err(WalletError::BadRequest(format!("this offer is {}", offer.state)));
        }
        if offer.expires_at.is_some_and(|e| e <= now()) {
            return Err(WalletError::Expired);
        }
        let preimage = random_preimage();
        let hash = sha256_hex(&hex::decode(&preimage).unwrap_or_default());
        let t = now();
        let a = i(amount);
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let payer = Self::wallet_code(&mut dbtx, req.app_id).await?;
        let receiver = Self::wallet_code(&mut dbtx, offer.app_id).await?;
        budget::check(&mut dbtx, req.app_id, a, t).await?;
        if ledger::balance(&mut dbtx, &payer).await? < a {
            return Err(WalletError::InsufficientBalance);
        }
        let base = NewTx {
            payment_type: "bolt12",
            state: "SETTLED",
            amount_msat: a,
            payment_hash: Some(hash),
            preimage: Some(preimage),
            offer_id: Some(offer.offer_id.clone()),
            offer: Some(offer.offer.clone()),
            payer_note: req.payer_note.clone(),
            description: offer.description.clone().unwrap_or_default(),
            self_payment: true,
            settled_at: Some(t),
            ..Default::default()
        };
        let out = store::insert(
            &mut dbtx,
            &NewTx {
                app_id: req.app_id,
                direction: "outgoing",
                metadata: req.metadata.clone(),
                ..base.clone()
            },
            t,
        )
        .await?;
        let inc = store::insert(
            &mut dbtx,
            &NewTx {
                app_id: offer.app_id,
                direction: "incoming",
                metadata: offer.metadata.clone(),
                ..base
            },
            t,
        )
        .await?;
        sqlx::query("UPDATE transactions SET counterpart_tx_id = $1 WHERE id = $2")
            .bind(inc.id)
            .bind(out.id)
            .execute(&mut *dbtx)
            .await?;
        sqlx::query("UPDATE transactions SET counterpart_tx_id = $1 WHERE id = $2")
            .bind(out.id)
            .bind(inc.id)
            .execute(&mut *dbtx)
            .await?;
        ledger::post(
            &mut dbtx,
            "transfer",
            Some(out.id),
            "self payment (offer)",
            &[(&payer, -a), (&receiver, a)],
            t,
        )
        .await?;
        let result = store::by_id(&mut dbtx, out.id).await?.ok_or(WalletError::NotFound)?;
        dbtx.commit().await?;
        self.notify(&result);
        self.notify(&inc);
        Ok(result)
    }

    /// Moves funds between Main and accounts without touching the network (SPEC §6.7).
    pub async fn transfer(
        &self,
        from: Option<i64>,
        to: Option<i64>,
        amount_msat: u64,
        description: String,
    ) -> Result<Tx, WalletError> {
        if amount_msat == 0 || from == to {
            return Err(WalletError::BadRequest(
                "transfer needs a positive amount and two different accounts".into(),
            ));
        }
        let t = now();
        let a = i(amount_msat);
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let payer = Self::wallet_code(&mut dbtx, from).await?;
        let receiver = Self::wallet_code(&mut dbtx, to).await?;
        if payer == receiver {
            return Err(WalletError::BadRequest("both sides use the same balance".into()));
        }
        if ledger::balance(&mut dbtx, &payer).await? < a {
            return Err(WalletError::InsufficientBalance);
        }
        let base = NewTx {
            payment_type: "internal",
            state: "SETTLED",
            amount_msat: a,
            description,
            self_payment: true,
            settled_at: Some(t),
            ..Default::default()
        };
        let out = store::insert(
            &mut dbtx,
            &NewTx { app_id: from, direction: "outgoing", ..base.clone() },
            t,
        )
        .await?;
        let inc = store::insert(&mut dbtx, &NewTx { app_id: to, direction: "incoming", ..base }, t)
            .await?;
        sqlx::query("UPDATE transactions SET counterpart_tx_id = $1 WHERE id = $2")
            .bind(inc.id)
            .bind(out.id)
            .execute(&mut *dbtx)
            .await?;
        sqlx::query("UPDATE transactions SET counterpart_tx_id = $1 WHERE id = $2")
            .bind(out.id)
            .bind(inc.id)
            .execute(&mut *dbtx)
            .await?;
        ledger::post(
            &mut dbtx,
            "transfer",
            Some(out.id),
            "transfer",
            &[(&payer, -a), (&receiver, a)],
            t,
        )
        .await?;
        let result = store::by_id(&mut dbtx, out.id).await?.ok_or(WalletError::NotFound)?;
        dbtx.commit().await?;
        self.notify(&result);
        self.notify(&inc);
        Ok(result)
    }

    // ------------------------------------------------------------------ events

    async fn handle_event(&self, ev: LnEvent) -> Result<(), WalletError> {
        match ev {
            LnEvent::PaymentSuccessful { payment, bolt12_invoice, .. }
                if payment.direction == allod_ln::types::Direction::Outbound =>
            {
                self.apply_outgoing_success(&payment, bolt12_invoice).await
            }
            LnEvent::PaymentFailed { payment, reason }
                if payment.direction == allod_ln::types::Direction::Outbound =>
            {
                let tx = {
                    let mut c = self.conn().await?;
                    store::by_ln(&mut c, "outgoing", &payment.id, payment.kind.payment_hash())
                        .await?
                };
                match tx {
                    Some(tx) => {
                        self.fail_outgoing(tx.id, reason.as_deref().unwrap_or("payment failed"))
                            .await?
                    }
                    None => {
                        if let Some(r) = reason {
                            let mut m =
                                self.early_failures.lock().unwrap_or_else(|e| e.into_inner());
                            if m.len() > 1000 {
                                m.clear();
                            }
                            m.insert(payment.id.clone(), r);
                        }
                    }
                }
                Ok(())
            }
            LnEvent::PaymentFailed { payment, .. }
                if payment.direction == allod_ln::types::Direction::Inbound =>
            {
                let tx = {
                    let mut c = self.conn().await?;
                    store::by_ln(&mut c, "incoming", &payment.id, payment.kind.payment_hash())
                        .await?
                };
                if let Some(tx) = tx.filter(|t| t.hold && t.state == "ACCEPTED") {
                    self.hold_failed_by_node(tx.id).await?;
                }
                Ok(())
            }
            LnEvent::PaymentReceived { payment } => self.apply_inbound(&payment).await,
            LnEvent::PaymentClaimable { payment, claim_deadline, claimable_amount_msat } => {
                self.apply_claimable(&payment, claim_deadline, claimable_amount_msat).await
            }
            LnEvent::StreamConnected => self.reconcile().await,
            LnEvent::ChannelStateChanged { .. } => {
                let _ = self.events.send(WalletEvent::BalanceChanged);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A received payment (SPEC §7.4). Idempotent.
    async fn apply_inbound(&self, p: &LnPayment) -> Result<(), WalletError> {
        if p.status != LnStatus::Succeeded {
            return Ok(());
        }
        let t = now();
        let amount = i(p.amount_msat.unwrap_or(0));
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let touched: Option<i64> = match &p.kind {
            LnPaymentKind::Bolt11 { payment_hash, preimage } => {
                match store::by_ln(&mut dbtx, "incoming", &p.id, Some(payment_hash)).await? {
                    Some(tx) => {
                        if store::transition(
                            &mut dbtx,
                            tx.id,
                            &["PENDING", "ACCEPTED", "EXPIRED"],
                            "SETTLED",
                            t,
                        )
                        .await?
                        {
                            let a = amount.max(tx.amount_msat);
                            sqlx::query("UPDATE transactions SET amount_msat = $1, preimage = COALESCE(preimage, $2) WHERE id = $3")
                                .bind(a).bind(preimage).bind(tx.id).execute(&mut *dbtx).await?;
                            let code = Self::wallet_code(&mut dbtx, tx.app_id).await?;
                            ledger::post(
                                &mut dbtx,
                                "receive_settle",
                                Some(tx.id),
                                "payment received",
                                &[(ledger::EXTERNAL, -a), (&code, a)],
                                t,
                            )
                            .await?;
                            Some(tx.id)
                        } else {
                            None
                        }
                    }
                    None => {
                        // Invoice created outside Allod: credit Main.
                        let tx = store::insert(
                            &mut dbtx,
                            &NewTx {
                                direction: "incoming",
                                payment_type: "bolt11",
                                state: "SETTLED",
                                amount_msat: amount,
                                ln_payment_id: Some(p.id.clone()),
                                payment_hash: Some(payment_hash.clone()),
                                preimage: preimage.clone(),
                                settled_at: Some(t),
                                ..Default::default()
                            },
                            t,
                        )
                        .await?;
                        ledger::post(
                            &mut dbtx,
                            "receive_settle",
                            Some(tx.id),
                            "payment received",
                            &[(ledger::EXTERNAL, -amount), (ledger::MAIN, amount)],
                            t,
                        )
                        .await?;
                        Some(tx.id)
                    }
                }
            }
            LnPaymentKind::Bolt12Offer {
                payment_hash,
                preimage,
                offer_id,
                payer_note,
                quantity,
            } => {
                let exists: Option<i64> = sqlx::query("SELECT id FROM transactions WHERE direction = 'incoming' AND ln_payment_id = $1")
                    .bind(&p.id).fetch_optional(&mut *dbtx).await?.map(|r| r.get(0));
                if exists.is_some() {
                    None
                } else {
                    let offer = store::offer_by_id(&mut dbtx, offer_id).await?;
                    let app_id = offer.as_ref().and_then(|o| o.app_id);
                    let tx = store::insert(
                        &mut dbtx,
                        &NewTx {
                            app_id,
                            direction: "incoming",
                            payment_type: "bolt12",
                            state: "SETTLED",
                            amount_msat: amount,
                            ln_payment_id: Some(p.id.clone()),
                            payment_hash: payment_hash.clone(),
                            preimage: preimage.clone(),
                            offer_id: Some(offer_id.clone()),
                            offer: offer.as_ref().map(|o| o.offer.clone()),
                            payer_note: payer_note.clone(),
                            quantity: quantity.map(i),
                            description: offer
                                .as_ref()
                                .and_then(|o| o.description.clone())
                                .unwrap_or_default(),
                            metadata: offer.as_ref().and_then(|o| o.metadata.clone()),
                            settled_at: Some(t),
                            ..Default::default()
                        },
                        t,
                    )
                    .await?;
                    let code = Self::wallet_code(&mut dbtx, app_id).await?;
                    ledger::post(
                        &mut dbtx,
                        "receive_settle",
                        Some(tx.id),
                        "offer payment received",
                        &[(ledger::EXTERNAL, -amount), (&code, amount)],
                        t,
                    )
                    .await?;
                    if let Some(o) = offer.filter(|o| o.single_use && o.state == "active") {
                        sqlx::query("UPDATE offers SET state = 'used' WHERE id = $1")
                            .bind(o.id)
                            .execute(&mut *dbtx)
                            .await?;
                    }
                    Some(tx.id)
                }
            }
            // Keysend and refunds are out of scope (SPEC §1.2); reconciliation books
            // such funds to Main.
            _ => None,
        };
        let out = match touched {
            Some(id) => store::by_id(&mut dbtx, id).await?,
            None => None,
        };
        dbtx.commit().await?;
        if let Some(out) = out {
            self.notify(&out);
        }
        Ok(())
    }

    /// An HTLC is held for one of our hold invoices (SPEC §7.5, §6.6.2–3).
    async fn apply_claimable(
        &self,
        p: &LnPayment,
        claim_deadline: Option<u32>,
        claimable: u64,
    ) -> Result<(), WalletError> {
        let tx = {
            let mut c = self.conn().await?;
            store::by_ln(&mut c, "incoming", &p.id, p.kind.payment_hash()).await?
        };
        let Some(tx) = tx.filter(|t| t.hold) else { return Ok(()) };
        let t = now();
        if i(claimable) < tx.amount_msat {
            self.node.bolt11_fail_for_id(&p.id).await?;
            let _g = self.write_lock.lock().await;
            let mut dbtx = self.begin().await?;
            if store::transition(&mut dbtx, tx.id, &["PENDING"], "CANCELED", t).await? {
                sqlx::query("UPDATE transactions SET failure_reason = 'underpaid' WHERE id = $1")
                    .bind(tx.id)
                    .execute(&mut *dbtx)
                    .await?;
            }
            dbtx.commit().await?;
            return Ok(());
        }
        let deadline =
            claim_deadline.map(|d| i64::from(d.saturating_sub(self.cfg.settle_safety_blocks)));
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        if store::transition(&mut dbtx, tx.id, &["PENDING"], "ACCEPTED", t).await? {
            sqlx::query("UPDATE transactions SET settle_deadline = $1, claimable_amount_msat = $2, ln_payment_id = $3 WHERE id = $4")
                .bind(deadline).bind(i(claimable)).bind(&p.id).bind(tx.id).execute(&mut *dbtx).await?;
        }
        let out = store::by_id(&mut dbtx, tx.id).await?;
        dbtx.commit().await?;
        if let Some(out) = out {
            self.notify(&out);
        }
        Ok(())
    }

    // ------------------------------------------------------------------ reconcile

    /// Brings the ledger in line with the backend (SPEC §7.6).
    pub async fn reconcile(&self) -> Result<(), WalletError> {
        let t = now();
        // 1–2. Outgoing payments still pending.
        let pending: Vec<Tx> = {
            let mut c = self.conn().await?;
            sqlx::query_as::<_, Tx>(&format!("SELECT {} FROM transactions WHERE direction = 'outgoing' AND state = 'PENDING' AND self_payment = FALSE", store::TX_COLUMNS))
                .fetch_all(&mut *c).await?
        };
        // An ID-less BOLT12 send may have reached the node before its RPC reply
        // was lost. Finish a complete backend scan before releasing any reserve.
        let bolt12_without_id: Vec<&Tx> = pending
            .iter()
            .filter(|tx| tx.payment_type == "bolt12" && tx.ln_payment_id.is_none())
            .collect();
        let mut outbound = Vec::new();
        if !bolt12_without_id.is_empty() {
            let mut token = None;
            let mut seen_tokens = std::collections::HashSet::new();
            loop {
                let (page, next) = self.node.list_payments(token).await?;
                outbound.extend(page.into_iter().filter(|p| p.direction == Direction::Outbound));
                match next {
                    Some(next) if seen_tokens.insert(next.clone()) => token = Some(next),
                    Some(_) => {
                        return Err(WalletError::Internal(
                            "backend payment pagination repeated a token".into(),
                        ));
                    }
                    None => break,
                }
            }
        }
        for tx in &pending {
            match &tx.ln_payment_id {
                Some(id) => match self.node.payment(id).await {
                    Ok(Some(p)) if p.status == LnStatus::Succeeded => {
                        self.apply_outgoing_success(&p, None).await?
                    }
                    Ok(Some(p)) if p.status == LnStatus::Failed => {
                        self.fail_outgoing(tx.id, "payment failed").await?
                    }
                    Ok(None) if t - tx.created_at > 600 => {
                        self.fail_outgoing(tx.id, "unknown to the node").await?
                    }
                    _ => {}
                },
                None if tx.payment_type == "bolt12" => {
                    let matches: Vec<&LnPayment> = outbound
                        .iter()
                        .filter(|p| {
                            matches!(
                                &p.kind,
                                LnPaymentKind::Bolt12Offer { offer_id, .. }
                                    if Some(offer_id) == tx.offer_id.as_ref()
                            ) && p.amount_msat == u64::try_from(tx.amount_msat).ok()
                                && p.updated_at >= tx.created_at as u64
                        })
                        .collect();
                    let matching_rows = matches.first().map_or(0, |p| {
                        bolt12_without_id
                            .iter()
                            .filter(|other| {
                                other.offer_id == tx.offer_id
                                    && other.amount_msat == tx.amount_msat
                                    && p.updated_at >= other.created_at as u64
                            })
                            .count()
                    });
                    match matches.as_slice() {
                        [p] if matching_rows == 1 => {
                            let _g = self.write_lock.lock().await;
                            let mut c = self.conn().await?;
                            sqlx::query("UPDATE transactions SET ln_payment_id = $1 WHERE id = $2 AND state = 'PENDING' AND ln_payment_id IS NULL")
                                .bind(&p.id).bind(tx.id).execute(&mut *c).await?;
                            drop(c);
                            drop(_g);
                            match p.status {
                                LnStatus::Succeeded => self.apply_outgoing_success(p, None).await?,
                                LnStatus::Failed => {
                                    self.fail_outgoing(tx.id, "payment failed").await?
                                }
                                LnStatus::Pending => {}
                            }
                        }
                        [] if t - tx.created_at > 600 => {
                            self.fail_outgoing(tx.id, "never submitted").await?
                        }
                        [] => {}
                        _ => {
                            tracing::error!(public_id = %tx.public_id, matches = matches.len(), "ambiguous BOLT12 payment recovery");
                            let data = serde_json::json!({
                                "severity": "critical",
                                "public_id": tx.public_id,
                                "candidate_payment_ids": matches.iter().map(|p| &p.id).collect::<Vec<_>>()
                            }).to_string();
                            let mut c = self.conn().await?;
                            sqlx::query("INSERT INTO audit_log (kind, app_id, data, created_at) SELECT 'bolt12_reconcile_ambiguous', $1, $2, $3 WHERE NOT EXISTS (SELECT 1 FROM audit_log WHERE kind = 'bolt12_reconcile_ambiguous' AND data = $2)")
                                .bind(tx.app_id).bind(data).bind(t).execute(&mut *c).await?;
                        }
                    }
                }
                None => {}
            }
        }
        // 3–4. Received payments we may have missed.
        let mut token = None;
        for _ in 0..50 {
            let (page, next) = self.node.list_payments(token).await?;
            for p in page.iter().filter(|p| {
                p.direction == allod_ln::types::Direction::Inbound
                    && p.status == LnStatus::Succeeded
            }) {
                self.apply_inbound(p).await?;
            }
            match next {
                Some(n) => token = Some(n),
                None => break,
            }
        }
        // 5. Expiry.
        {
            let _g = self.write_lock.lock().await;
            let mut c = self.conn().await?;
            sqlx::query("UPDATE transactions SET state = 'EXPIRED', updated_at = $1 WHERE direction = 'incoming' \
                         AND state = 'PENDING' AND expires_at IS NOT NULL AND expires_at < $1")
                .bind(t).execute(&mut *c).await?;
            sqlx::query("UPDATE offers SET state = 'expired' WHERE state = 'active' AND expires_at IS NOT NULL AND expires_at < $1")
                .bind(t).execute(&mut *c).await?;
        }
        // 6. Accepted holds past their deadline.
        let height = i64::from(self.node.node_info().await?.summary.block_height);
        let late: Vec<Tx> = {
            let mut c = self.conn().await?;
            sqlx::query_as::<_, Tx>(&format!(
                "SELECT {} FROM transactions WHERE hold = TRUE AND state = 'ACCEPTED' \
                AND settle_deadline IS NOT NULL AND settle_deadline <= $1",
                store::TX_COLUMNS
            ))
            .bind(height)
            .fetch_all(&mut *c)
            .await?
        };
        for tx in late {
            if let Some(h) = &tx.payment_hash {
                let _ = self.cancel_hold(tx.app_id, h).await;
            }
        }
        // 6b. Accepted holds the node failed on its own (or someone failed outside Allod):
        //     report them as canceled, never as still settleable (Hub fund-loss note 3).
        let accepted: Vec<Tx> = {
            let mut c = self.conn().await?;
            sqlx::query_as::<_, Tx>(&format!(
                "SELECT {} FROM transactions WHERE hold = TRUE AND state = 'ACCEPTED' \
                 AND self_payment = FALSE AND ln_payment_id IS NOT NULL",
                store::TX_COLUMNS
            ))
            .fetch_all(&mut *c)
            .await?
        };
        for tx in accepted {
            if let Ok(Some(p)) =
                self.node.payment(tx.ln_payment_id.as_deref().unwrap_or_default()).await
            {
                if p.status == LnStatus::Failed {
                    self.hold_failed_by_node(tx.id).await?;
                }
            }
        }
        // 7. Book differences the ledger did not see (channel opens/closes, forwarding,
        //    keysend) to Main, only while nothing is in flight.
        let busy: i64 = {
            let mut c = self.conn().await?;
            sqlx::query("SELECT COUNT(*) FROM transactions WHERE (direction = 'outgoing' AND state = 'PENDING') OR state = 'ACCEPTED'")
                .fetch_one(&mut *c).await?.get(0)
        };
        if busy == 0 {
            let node_msat = i(self.node.balances().await?.lightning_total_sat.saturating_mul(1000));
            let _g = self.write_lock.lock().await;
            let mut dbtx = self.begin().await?;
            let wallets = ledger::kind_total(&mut dbtx, "wallet", None).await?;
            let pending = ledger::balance(&mut dbtx, ledger::PENDING_OUT).await?;
            let diff = node_msat - wallets - pending;
            if diff.abs() >= 1000 {
                ledger::post(
                    &mut dbtx,
                    "reconcile",
                    None,
                    "node balance reconciliation",
                    &[(ledger::MAIN, diff), (ledger::EQUITY, -diff)],
                    t,
                )
                .await?;
                tracing::info!(diff_msat = diff, "reconciled Main with the node balance");
            }
            dbtx.commit().await?;
        }
        // 8. Invariants.
        let problems = self.ledger_check().await?;
        if !problems.is_empty() {
            tracing::error!(?problems, "ledger invariant check failed");
        }
        let _ = self.events.send(WalletEvent::BalanceChanged);
        Ok(())
    }
}

fn lnurl_error(e: allod_lnurl::LnurlError) -> WalletError {
    match e {
        allod_lnurl::LnurlError::Service(m) => WalletError::PaymentFailed(format!("LNURL: {m}")),
        other => WalletError::BadRequest(other.to_string()),
    }
}

fn bad_input(e: BackendError) -> WalletError {
    match e {
        BackendError::InvalidRequest(m) => WalletError::BadRequest(m),
        other => WalletError::Backend(other),
    }
}

#[cfg(test)]
mod tests;
