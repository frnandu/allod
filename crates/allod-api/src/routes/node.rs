//! Node, channels, peers, on-chain and balances (SPEC §11.3, M2).

use std::sync::Arc;

use allod_ln::types::*;
use allod_ln::LightningBackend;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

async fn node(state: &AppState) -> Result<Arc<dyn LightningBackend>, ApiError> {
    state
        .ldk
        .backend()
        .await
        .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "NODE_NOT_READY", e))
}

fn is_pubkey(s: &str) -> bool {
    s.len() == 66
        && s.chars().all(|c| c.is_ascii_hexdigit())
        && (s.starts_with("02") || s.starts_with("03"))
}

/// `GET /api/node`
pub async fn info(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let n = node(&state).await?;
    let i = n.node_info().await?;
    Ok(Json(json!({
        "pubkey": i.summary.pubkey,
        "alias": i.alias,
        "network": i.network,
        "block_height": i.summary.block_height,
        "block_hash": i.summary.block_hash,
        "version": i.version,
        "listening_addresses": i.listening_addresses,
        "announcement_addresses": i.announcement_addresses,
        "node_uris": i.node_uris,
        "capabilities": n.capabilities(),
    })))
}

/// `GET /api/node/gossip`: validated graph size plus pruned-block recovery activity.
pub async fn gossip(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<crate::state::GossipStatus>, ApiError> {
    let backend = node(&state).await?;
    Ok(Json(state.gossip.snapshot(backend.as_ref()).await?))
}

/// `GET /api/fees`: bitcoind's fee estimates at the block targets ldk-node uses.
/// `{"source": null}` in light modes, which have no bitcoind of their own.
pub async fn fees(State(state): State<AppState>, _auth: Auth) -> Json<Value> {
    Json(match state.setup.fee_estimates().await {
        Some(mut fees) => {
            fees["source"] = Value::String("bitcoind".into());
            fees
        }
        None => json!({"source": null, "available": false, "targets": [], "missing": []}),
    })
}

/// `GET /api/balances`. Account allocation (Main vs accounts) arrives with the ledger.
pub async fn balances(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let n = node(&state).await?;
    let b = n.balances().await?;
    let channels = n.list_channels().await?;
    let spendable: u64 =
        channels.iter().filter(|c| c.is_usable).map(|c| c.outbound_capacity_msat).sum();
    let receivable: u64 =
        channels.iter().filter(|c| c.is_usable).map(|c| c.inbound_capacity_msat).sum();
    let pending: u64 = b.pending_sweeps.iter().map(|x| x.amount_sat).sum();
    let allocation = match &state.wallet.get() {
        Some(w) => Some(w.allocation().await.map_err(|e| ApiError::internal(e.to_string()))?),
        None => None,
    };
    Ok(Json(json!({
        "lightning": {
            "total_msat": b.lightning_total_sat * 1000,
            "spendable_msat": spendable,
            "receivable_msat": receivable,
            "breakdown": b.lightning,
        },
        "onchain": {
            "total_sat": b.onchain_total_sat,
            "spendable_sat": b.onchain_spendable_sat,
            "reserved_sat": b.anchor_reserve_sat,
            "pending_from_closures_sat": pending,
        },
        "accounts_total_msat": allocation.as_ref().map(|a| a.accounts_total_msat),
        "main_spendable_msat": allocation.as_ref().map(|a| a.main_msat),
        "in_flight_msat": allocation.as_ref().map(|a| a.pending_out_msat),
    })))
}

/// `GET /api/channels`
/// A list entry with the counterparty's alias from the node's own gossip graph.
#[derive(Serialize)]
pub struct Named<T> {
    #[serde(flatten)]
    item: T,
    alias: Option<String>,
}

pub async fn channels(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Vec<Named<Channel>>>, ApiError> {
    let backend = node(&state).await?;
    let channels = backend.list_channels().await?;
    let aliases = state
        .ldk
        .aliases()
        .resolve(
            backend.as_ref(),
            &state.gossip,
            channels.iter().map(|c| c.counterparty_node_id.clone()),
        )
        .await;
    Ok(Json(
        channels
            .into_iter()
            .map(|c| {
                let alias = aliases.get(&c.counterparty_node_id).cloned().flatten();
                Named { item: c, alias }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct OpenChannel {
    /// `pubkey@host:port`, or pubkey with `address`.
    pubkey: String,
    address: Option<String>,
    amount_sat: Option<u64>,
    #[serde(default)]
    all_funds: bool,
    push_msat: Option<u64>,
    #[serde(default = "yes")]
    public: bool,
    fee_base_msat: Option<u32>,
    fee_ppm: Option<u32>,
}

fn yes() -> bool {
    true
}

fn split_uri(pubkey: &str, address: Option<String>) -> Result<(String, String), ApiError> {
    let (pk, addr) = match pubkey.split_once('@') {
        Some((pk, a)) => (pk.to_string(), Some(a.to_string())),
        None => (pubkey.to_string(), address),
    };
    let pk = pk.trim().to_ascii_lowercase();
    if !is_pubkey(&pk) {
        return Err(ApiError::bad_request("pubkey must be a 33-byte hex node id"));
    }
    let addr = addr
        .filter(|a| !a.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("address (host:port) is required"))?;
    Ok((pk, addr.trim().to_string()))
}

/// `POST /api/channels`
pub async fn open(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<OpenChannel>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    auth.require_full()?;
    let (pubkey, address) = split_uri(&req.pubkey, req.address)?;
    let amount_sat = match (req.amount_sat, req.all_funds) {
        (Some(a), false) if a >= 20_000 => Some(a),
        (Some(_), false) => {
            return Err(ApiError::bad_request("channels must be at least 20,000 sats"))
        }
        (None, true) => None,
        _ => return Err(ApiError::bad_request("give amount_sat or all_funds, not both")),
    };
    let ucid = node(&state)
        .await?
        .open_channel(OpenChannelReq {
            node_pubkey: pubkey.clone(),
            address,
            amount_sat,
            push_msat: req.push_msat,
            announce: req.public,
            fee_base_msat: req.fee_base_msat,
            fee_ppm: req.fee_ppm,
        })
        .await?;
    audit(
        &state,
        "channel_opened",
        json!({"peer": pubkey, "amount_sat": amount_sat, "user_channel_id": ucid}),
        unix_now(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(json!({"user_channel_id": ucid}))))
}

#[derive(Deserialize)]
pub struct UpdateFees {
    counterparty: String,
    fee_base_msat: Option<u32>,
    fee_ppm: Option<u32>,
    cltv_expiry_delta: Option<u32>,
}

/// `PATCH /api/channels/{user_channel_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(ucid): Path<String>,
    Json(req): Json<UpdateFees>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    if let Some(d) = req.cltv_expiry_delta {
        if !(18..=2016).contains(&d) {
            return Err(ApiError::bad_request("cltv_expiry_delta must be 18-2016"));
        }
    }
    node(&state)
        .await?
        .update_channel_fees(
            &ucid,
            &req.counterparty,
            ChannelFees {
                fee_base_msat: req.fee_base_msat,
                fee_ppm: req.fee_ppm,
                cltv_expiry_delta: req.cltv_expiry_delta,
            },
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CloseQuery {
    counterparty: String,
    #[serde(default)]
    force: bool,
}

/// `DELETE /api/channels/{user_channel_id}?counterparty=&force=`
pub async fn close(
    State(state): State<AppState>,
    auth: Auth,
    Path(ucid): Path<String>,
    Query(q): Query<CloseQuery>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    let n = node(&state).await?;
    if q.force && !n.capabilities().force_close {
        return Err(ApiError::forbidden("force close is not permitted for this node"));
    }
    let reason = q.force.then(|| "closed by the owner in Allod".to_string());
    n.close_channel(&ucid, &q.counterparty, q.force, reason).await?;
    audit(&state, "channel_closed", json!({"user_channel_id": ucid, "force": q.force}), unix_now())
        .await;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub struct Splice {
    counterparty: String,
    amount_sat: u64,
    address: Option<String>,
}

/// `POST /api/channels/{user_channel_id}/splice-in`
pub async fn splice_in(
    State(state): State<AppState>,
    auth: Auth,
    Path(ucid): Path<String>,
    Json(req): Json<Splice>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    node(&state).await?.splice_in(&ucid, &req.counterparty, req.amount_sat).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/channels/{user_channel_id}/splice-out`
pub async fn splice_out(
    State(state): State<AppState>,
    auth: Auth,
    Path(ucid): Path<String>,
    Json(req): Json<Splice>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    node(&state).await?.splice_out(&ucid, &req.counterparty, req.address, req.amount_sat).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/peers`
pub async fn peers(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Vec<Named<Peer>>>, ApiError> {
    let backend = node(&state).await?;
    let peers = backend.list_peers().await?;
    let aliases = state
        .ldk
        .aliases()
        .resolve(backend.as_ref(), &state.gossip, peers.iter().map(|p| p.node_id.clone()))
        .await;
    Ok(Json(
        peers
            .into_iter()
            .map(|p| {
                let alias = aliases.get(&p.node_id).cloned().flatten();
                Named { item: p, alias }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct ConnectPeer {
    pubkey: String,
    address: Option<String>,
    #[serde(default = "yes")]
    persist: bool,
}

/// `POST /api/peers`
pub async fn connect(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ConnectPeer>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    let (pk, addr) = split_uri(&req.pubkey, req.address)?;
    node(&state).await?.connect_peer(&pk, &addr, req.persist).await?;
    Ok(StatusCode::CREATED)
}

/// `DELETE /api/peers/{pubkey}`
pub async fn disconnect(
    State(state): State<AppState>,
    auth: Auth,
    Path(pubkey): Path<String>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    node(&state).await?.disconnect_peer(&pubkey).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/onchain/address`: a new receive address.
pub async fn new_address(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(json!({"address": node(&state).await?.onchain_receive().await?})))
}

#[derive(Deserialize)]
pub struct OnchainSend {
    address: String,
    amount_sat: Option<u64>,
    #[serde(default)]
    send_all: bool,
    fee_rate_sat_vb: Option<u64>,
}

/// `POST /api/onchain/send`
pub async fn onchain_send(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<OnchainSend>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let amount = match (req.amount_sat, req.send_all) {
        (Some(a), false) if a >= 546 => OnchainAmount::Sats(a),
        (Some(_), false) => {
            return Err(ApiError::bad_request("amount is below the dust limit (546 sats)"))
        }
        (None, true) => OnchainAmount::All,
        _ => return Err(ApiError::bad_request("give amount_sat or send_all, not both")),
    };
    if let Some(r) = req.fee_rate_sat_vb {
        if !(1..=1000).contains(&r) {
            return Err(ApiError::bad_request("fee_rate_sat_vb must be 1-1000"));
        }
    }
    let txid =
        node(&state).await?.onchain_send(req.address.trim(), amount, req.fee_rate_sat_vb).await?;
    audit(&state, "onchain_sent", json!({"txid": txid, "send_all": req.send_all}), unix_now())
        .await;
    Ok(Json(json!({"txid": txid})))
}

#[derive(Deserialize)]
pub struct BumpFee {
    payment_id: String,
    fee_rate_sat_vb: Option<u64>,
}

/// `POST /api/onchain/bump-fee`
pub async fn bump_fee(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<BumpFee>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let txid = node(&state).await?.onchain_bump_fee(&req.payment_id, req.fee_rate_sat_vb).await?;
    Ok(Json(json!({"txid": txid})))
}

/// `GET /api/onchain/transactions`
pub async fn onchain_transactions(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Vec<LnPayment>>, ApiError> {
    let n = node(&state).await?;
    let mut out = Vec::new();
    let mut token = None;
    for _ in 0..20 {
        let (page, next) = n.list_payments(token).await?;
        out.extend(page.into_iter().filter(|p| matches!(p.kind, LnPaymentKind::Onchain { .. })));
        match next {
            Some(t) => token = Some(t),
            None => break,
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    Ok(Json(out))
}

/// `GET /api/forwards`
pub async fn forwards(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let (items, next) = node(&state).await?.list_forwards(None).await?;
    let fees: u64 = items.iter().filter_map(|f| f.fee_earned_msat).sum();
    Ok(Json(json!({"items": items, "next_page_token": next, "fees_earned_msat": fees})))
}

#[derive(Deserialize)]
pub struct SignMessage {
    message: String,
}

/// `POST /api/sign-message`
pub async fn sign_message(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<SignMessage>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let sig = node(&state).await?.sign_message(req.message.as_bytes()).await?;
    Ok(Json(json!({"message": req.message, "signature": sig})))
}

#[derive(Deserialize)]
pub struct VerifyMessage {
    message: String,
    signature: String,
    pubkey: String,
}

/// `POST /api/verify-message`
pub async fn verify_message(
    State(state): State<AppState>,
    _auth: Auth,
    Json(req): Json<VerifyMessage>,
) -> Result<Json<Value>, ApiError> {
    let ok = node(&state)
        .await?
        .verify_signature(req.message.as_bytes(), &req.signature, &req.pubkey)
        .await?;
    Ok(Json(json!({"valid": ok})))
}
