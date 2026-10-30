//! Payments, invoices, offers, transactions and live events (SPEC §11.3, M3).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use allod_core::wallet::{InvoiceRequest, OfferRequest, PayRequest, TxFilter};
use allod_core::{Wallet, WalletError};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

impl From<WalletError> for ApiError {
    fn from(e: WalletError) -> Self {
        match e {
            WalletError::BadRequest(m) => ApiError::bad_request(m),
            WalletError::Unsupported(m) => {
                ApiError::new(StatusCode::NOT_IMPLEMENTED, "NOT_SUPPORTED", m)
            }
            WalletError::InsufficientBalance => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INSUFFICIENT_BALANCE",
                e.to_string(),
            ),
            WalletError::QuotaExceeded => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "QUOTA_EXCEEDED", e.to_string())
            }
            WalletError::AlreadyPaid | WalletError::InProgress => ApiError::conflict(e.to_string()),
            WalletError::NotFound => ApiError::not_found("not found"),
            WalletError::Expired => ApiError::new(StatusCode::GONE, "EXPIRED", "expired"),
            WalletError::WrongNetwork(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "UNSUPPORTED_NETWORK", e.to_string())
            }
            WalletError::PaymentFailed(m) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "PAYMENT_FAILED", m)
            }
            WalletError::Backend(b) => b.into(),
            WalletError::Internal(m) => {
                tracing::error!(error = %m, "wallet error");
                ApiError::internal("internal error")
            }
        }
    }
}

pub fn wallet(state: &AppState) -> Result<Arc<Wallet>, ApiError> {
    state.wallet.get().cloned().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "NODE_NOT_READY",
            "the Lightning node is not ready yet",
        )
    })
}

#[derive(Deserialize)]
pub struct ListQuery {
    direction: Option<String>,
    state: Option<String>,
    payment_type: Option<String>,
    q: Option<String>,
    /// "main" or an app id.
    account: Option<String>,
    #[serde(default)]
    include_unpaid: bool,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// `GET /api/transactions`
pub async fn list(
    State(state): State<AppState>,
    _auth: Auth,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let app_id = match q.account.as_deref() {
        None | Some("") | Some("all") => None,
        Some("main") => Some(None),
        Some(id) => Some(Some(
            id.parse().map_err(|_| ApiError::bad_request("account must be main, all or an id"))?,
        )),
    };
    let (items, total) = wallet(&state)?
        .transactions(&TxFilter {
            app_id,
            direction: q.direction,
            state: q.state,
            payment_type: q.payment_type,
            query: q.q,
            include_unpaid: q.include_unpaid,
            limit: q.limit.unwrap_or(50),
            offset: q.offset.unwrap_or(0),
        })
        .await?;
    Ok(Json(json!({"items": items, "total": total})))
}

/// `GET /api/transactions/{public_id}`
pub async fn get(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(wallet(&state)?.transaction(&id).await?).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct Labels {
    labels: Vec<String>,
}

/// `PATCH /api/transactions/{public_id}/labels`
pub async fn labels(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<Labels>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    if req.labels.len() > 20 || req.labels.iter().any(|l| l.len() > 40) {
        return Err(ApiError::bad_request("at most 20 labels of 40 characters"));
    }
    Ok(Json(
        serde_json::to_value(wallet(&state)?.update_labels(&id, &req.labels).await?)
            .unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
pub struct Pay {
    payment: String,
    amount_msat: Option<u64>,
    max_fee_msat: Option<u64>,
    payer_note: Option<String>,
    from_app_id: Option<i64>,
    /// Who is paid, recorded in the payment's metadata (e.g. a Nostr contact:
    /// `{pubkey, name}`); shown in the activity list.
    recipient: Option<Value>,
}

/// `POST /api/payments`
pub async fn pay(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Pay>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    if req.payer_note.as_ref().is_some_and(|n| n.len() > 512) {
        return Err(ApiError::bad_request("payer_note is limited to 512 characters"));
    }
    let tx = wallet(&state)?
        .pay(PayRequest {
            app_id: req.from_app_id,
            payment: req.payment,
            amount_msat: req.amount_msat,
            max_fee_msat: req.max_fee_msat,
            payer_note: req.payer_note,
            metadata: req
                .recipient
                .filter(|r| r.is_object())
                .map(|r| serde_json::json!({"recipient": r}).to_string())
                .filter(|m| m.len() <= 4096),
        })
        .await?;
    audit(
        &state,
        "payment_sent",
        json!({"transaction": tx.public_id, "state": tx.state, "amount_msat": tx.amount_msat}),
        unix_now(),
    )
    .await;
    Ok(Json(serde_json::to_value(tx).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct Decode {
    payment: String,
}

/// `POST /api/payments/decode`
pub async fn decode(
    State(state): State<AppState>,
    _auth: Auth,
    Json(req): Json<Decode>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(wallet(&state)?.decode(&req.payment).await?).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct NewInvoice {
    amount_msat: Option<u64>,
    #[serde(default)]
    description: String,
    expiry_secs: Option<u32>,
    app_id: Option<i64>,
    /// Receive through an LSPS2 just-in-time channel (needs a configured LSP).
    #[serde(default)]
    jit: bool,
    /// Cap on the total LSP fee for a just-in-time channel (msat).
    max_lsp_fee_msat: Option<u64>,
}

fn invoice_request(req: NewInvoice) -> InvoiceRequest {
    InvoiceRequest {
        app_id: req.app_id,
        amount_msat: req.amount_msat,
        description: req.description,
        expiry_secs: req.expiry_secs,
        jit: req.jit,
        max_lsp_fee_msat: req.max_lsp_fee_msat,
        ..Default::default()
    }
}

/// `POST /api/invoices`
pub async fn invoice(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewInvoice>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    auth.require_full()?;
    if req.description.len() > 640 {
        return Err(ApiError::bad_request("description is limited to 640 characters"));
    }
    let tx = wallet(&state)?.create_invoice(invoice_request(req)).await?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(tx).unwrap_or_default())))
}

#[derive(Deserialize)]
pub struct NewOffer {
    amount_msat: Option<u64>,
    #[serde(default)]
    description: String,
    expires_at: Option<i64>,
    app_id: Option<i64>,
}

/// `POST /api/offers`
pub async fn new_offer(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewOffer>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    auth.require_full()?;
    let offer = wallet(&state)?
        .create_offer(OfferRequest {
            app_id: req.app_id,
            amount_msat: req.amount_msat,
            description: req.description,
            expires_at: req.expires_at,
            metadata: None,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(offer).unwrap_or_default())))
}

/// `GET /api/offers`
pub async fn offers(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(wallet(&state)?.offers(None).await?).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct OfferState {
    state: String,
}

/// `PATCH /api/offers/{offer_id}`
pub async fn offer_state(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<OfferState>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(
        serde_json::to_value(wallet(&state)?.set_offer_state(&id, &req.state).await?)
            .unwrap_or_default(),
    ))
}

/// `POST /api/receive`: BIP-321 with BOLT11 and/or BOLT12.
pub async fn receive(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewInvoice>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let (bip321, tx) = wallet(&state)?.receive(invoice_request(req)).await?;
    Ok(Json(json!({"bip321": bip321, "transaction": tx})))
}

#[derive(Deserialize)]
pub struct HoldInvoice {
    amount_msat: u64,
    payment_hash: String,
    #[serde(default)]
    description: String,
    expiry_secs: Option<u32>,
}

/// `POST /api/hold-invoices`
pub async fn hold(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<HoldInvoice>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    auth.require_full()?;
    let tx = wallet(&state)?
        .create_hold_invoice(
            InvoiceRequest {
                amount_msat: Some(req.amount_msat),
                description: req.description,
                expiry_secs: req.expiry_secs,
                ..Default::default()
            },
            &req.payment_hash,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(tx).unwrap_or_default())))
}

#[derive(Deserialize)]
pub struct Settle {
    preimage: String,
}

/// `POST /api/hold-invoices/settle`
pub async fn hold_settle(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Settle>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(
        serde_json::to_value(wallet(&state)?.settle_hold(None, &req.preimage).await?)
            .unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
pub struct Cancel {
    payment_hash: String,
}

/// `POST /api/hold-invoices/cancel`
pub async fn hold_cancel(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Cancel>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(
        serde_json::to_value(wallet(&state)?.cancel_hold(None, &req.payment_hash).await?)
            .unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
pub struct Transfer {
    from_app_id: Option<i64>,
    to_app_id: Option<i64>,
    amount_msat: u64,
    #[serde(default)]
    description: String,
}

/// `POST /api/transfers`
pub async fn transfer(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Transfer>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let tx = wallet(&state)?
        .transfer(req.from_app_id, req.to_app_id, req.amount_msat, req.description)
        .await?;
    Ok(Json(serde_json::to_value(tx).unwrap_or_default()))
}

/// `GET /api/ledger/check`: invariant problems (empty = healthy).
pub async fn ledger_check(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Value>, ApiError> {
    let problems = wallet(&state)?.ledger_check().await?;
    Ok(Json(json!({"ok": problems.is_empty(), "problems": problems})))
}

/// `POST /api/events/ticket`: a 60-second ticket for `GET /api/events` (EventSource
/// cannot send an Authorization header).
pub async fn ticket(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    let subject = match &auth.subject {
        crate::auth::Subject::Owner { pubkey, .. } => pubkey.clone(),
        crate::auth::Subject::Token { id, .. } => format!("token:{id}"),
    };
    Ok(Json(json!({"ticket": state.auth.new_ticket(&subject, unix_now())?})))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    ticket: String,
}

/// `GET /api/events?ticket=` (SSE): transaction and balance updates, heartbeat 15 s.
pub async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    if !state.auth.take_ticket(&q.ticket, unix_now()) {
        return Err(ApiError::unauthorized("invalid or expired event ticket"));
    }
    let rx = wallet(&state)?.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let name = match &ev {
                        allod_core::WalletEvent::TransactionUpdated { .. } => "transaction.updated",
                        allod_core::WalletEvent::BalanceChanged => "balance.changed",
                    };
                    let data = serde_json::to_string(&ev).unwrap_or_default();
                    return Some((Ok(Event::default().event(name).data(data)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
