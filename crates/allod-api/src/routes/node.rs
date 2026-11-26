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

pub(crate) async fn node(state: &AppState) -> Result<Arc<dyn LightningBackend>, ApiError> {
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
        // ldk-node and rust-lightning under it, when the image says (managed mode).
        "components": state.setup.node_components(),
        "listening_addresses": i.listening_addresses,
        "announcement_addresses": i.announcement_addresses,
        "node_uris": i.node_uris,
        "lightning_synced_at": i.lightning_synced_at,
        "onchain_synced_at": i.onchain_synced_at,
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
    let fees = if state.watches_node_bitcoind() { state.setup.fee_estimates().await } else { None };
    Json(match fees {
        Some(mut fees) => {
            fees["source"] = Value::String("bitcoind".into());
            fees
        }
        None => json!({"source": null, "available": false, "targets": [], "missing": []}),
    })
}

/// `GET /api/node/bitcoind`: Bitcoin Core's version, tip, peers, mempool, warnings and
/// background validation after a fast sync. `{"source": null}` in light modes.
pub async fn bitcoind(State(state): State<AppState>, _auth: Auth) -> Json<Value> {
    let status =
        if state.watches_node_bitcoind() { state.setup.bitcoind_status().await } else { None };
    let Some(mut v) = status else { return Json(json!({"source": null})) };
    v["source"] = Value::String("bitcoind".into());
    newer_tip_from_ldk(&state, &mut v).await;
    // bitcoind could not tell the snapshot's base height (its header lookup failed):
    // the fast-sync helper wrote it when it loaded the snapshot.
    let bg = &mut v["background_validation"];
    if bg.is_object() && bg["target_height"].is_null() {
        let base =
            state.fast_sync.as_ref().and_then(|f| f.status()).map(|s| s["base_height"].clone());
        if let Some(h) = base.as_ref().and_then(Value::as_u64).filter(|h| *h > 0) {
            bg["target_height"] = json!(h);
            if let Some(b) = bg["blocks"].as_u64() {
                bg["progress"] = json!((b as f64 / h as f64).min(1.0));
                bg["progress_basis"] = json!("blocks");
            }
        }
    }
    Json(v)
}

/// bitcoind's chain figures are this old before the tip is taken from ldk-server.
const STALE_CHAIN_SECS: i64 = 120;

/// While background validation keeps bitcoind's chain lock, its chain figures can be
/// minutes old. ldk-server follows the same bitcoind and answers at once: when it knows a
/// newer block, that one is shown (`tip_source: "ldk-server"`; its time is unknown).
async fn newer_tip_from_ldk(state: &AppState, v: &mut Value) {
    if !chain_stale(v, crate::auth::unix_now()) {
        return;
    }
    let Ok(backend) = node(state).await else { return };
    let info = tokio::time::timeout(std::time::Duration::from_secs(3), backend.node_info()).await;
    if let Ok(Ok(info)) = info {
        take_newer_tip(v, u64::from(info.summary.block_height), &info.summary.block_hash);
    }
}

/// Old chain figures, or none yet because bitcoind has not answered the first query, and
/// no live tip (bitcoind's log or ZMQ) to vouch for them.
fn chain_stale(v: &Value, now: i64) -> bool {
    v["tip_live"] != true
        && (v["chain_pending"] == true
            || v["updated_at"].as_i64().is_some_and(|at| now - at >= STALE_CHAIN_SECS))
}

/// Shows ldk-server's tip when it is newer than bitcoind's last answer.
fn take_newer_tip(v: &mut Value, height: u64, hash: &str) {
    if v["blocks"].as_u64().is_some_and(|b| b >= height) {
        return;
    }
    v["blocks"] = json!(height);
    v["best_block_hash"] = json!(hash);
    v["best_block_time"] = Value::Null;
    v["tip_source"] = json!("ldk-server");
    if v["headers"].as_u64().is_some_and(|h| h < height) {
        v["headers"] = json!(height);
    }
}

#[cfg(test)]
mod tip_tests {
    use super::*;

    #[test]
    fn a_splice_says_what_it_did() {
        let f = |txid: &str, kind: &str, direction: Option<&str>, cap: Option<u64>| {
            allod_core::ChannelFunding {
                txid: txid.into(),
                kind: kind.into(),
                seen_at: 0,
                direction: direction.map(Into::into),
                capacity_sat: cap,
            }
        };
        let list = funding_list(&[
            f("a", "open", None, Some(12_000)),
            // Started through Allod: its direction is known, the change from capacities.
            f("b", "splice", Some("out"), Some(9_000)),
            // Started by the peer: its direction from the change.
            f("c", "splice", None, Some(20_000)),
            // Never seen while current: nothing to say.
            f("d", "splice", None, None),
        ]);
        let got: Vec<(Option<&str>, Option<i64>)> =
            list.iter().map(|x| (x.direction.as_deref(), x.change_sat)).collect();
        assert_eq!(
            got,
            vec![
                (None, None),
                (Some("out"), Some(-3_000)),
                (Some("in"), Some(11_000)),
                (None, None)
            ]
        );
    }

    #[test]
    fn a_stale_chain_takes_the_newer_tip_from_ldk_server() {
        let mut v = json!({"blocks": 977_150, "headers": 977_150, "best_block_time": 1, "updated_at": 1000});
        assert!(!chain_stale(&v, 1000 + STALE_CHAIN_SECS - 1));
        assert!(chain_stale(&v, 1000 + STALE_CHAIN_SECS));
        take_newer_tip(&mut v, 977_183, "00ab");
        assert_eq!(v["blocks"], 977_183);
        assert_eq!(v["headers"], 977_183);
        assert_eq!(v["tip_source"], "ldk-server");
        assert!(v["best_block_time"].is_null());
        // Not an older or equal one.
        let mut v = json!({"blocks": 977_183, "best_block_time": 1});
        take_newer_tip(&mut v, 977_183, "00ab");
        assert!(v.get("tip_source").is_none() && v["best_block_time"] == 1);
        // Without a chain answer there is nothing to correct.
        assert!(!chain_stale(&json!({"error": "connection refused"}), 5000));
        // Before bitcoind's first chain answer the tip comes from ldk-server alone.
        let mut v = json!({"chain_pending": true});
        assert!(chain_stale(&v, 5000));
        take_newer_tip(&mut v, 977_183, "00ab");
        assert_eq!(
            (v["blocks"].clone(), v["tip_source"].clone()),
            (json!(977_183), json!("ldk-server"))
        );
        assert!(v["headers"].is_null());
        // A live tip (log or ZMQ) needs no correction.
        assert!(!chain_stale(&json!({"chain_pending": true, "tip_live": true}), 5000));
        assert!(!chain_stale(&json!({"updated_at": 1000, "tip_live": true}), 5000));
    }
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
    // Reserves: ours can never be sent while a channel is open (it comes back when it
    // closes); the peer's never comes our way. Neither counts in send / receive.
    let ready = || channels.iter().filter(|c| c.is_channel_ready);
    let reserve: u64 = ready().map(|c| c.reserve_sat * 1000).sum();
    let counterparty_reserve: u64 = ready().map(|c| c.counterparty_reserve_sat * 1000).sum();
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
            "reserve_msat": reserve,
            "counterparty_reserve_msat": counterparty_reserve,
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
) -> Result<Json<Vec<ChannelOut>>, ApiError> {
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
    // Each channel's funding transactions (the opening one, then each splice's): the
    // current one is recorded here too, for channels opened before Allod kept them.
    let mut fundings = Default::default();
    if let Some(w) = state.wallet.get() {
        for c in &channels {
            if let Some(txid) = &c.funding_txid {
                let cap = Some(c.capacity_sat);
                if let Err(e) = w.record_funding(&c.user_channel_id, txid, false, cap).await {
                    tracing::warn!(error = %e, "could not record a channel's funding");
                }
            }
        }
        fundings = w.channel_fundings().await.unwrap_or_default();
    }
    Ok(Json(
        channels
            .into_iter()
            .map(|c| {
                let alias = aliases.get(&c.counterparty_node_id).cloned().flatten();
                let fundings =
                    fundings.get(&c.user_channel_id).map(|f| funding_list(f)).unwrap_or_default();
                ChannelOut { item: c, alias, fundings }
            })
            .collect(),
    ))
}

/// A channel with its counterparty's alias and its funding transactions.
#[derive(Serialize)]
pub struct ChannelOut {
    #[serde(flatten)]
    item: Channel,
    alias: Option<String>,
    fundings: Vec<Funding>,
}

/// One funding transaction of a channel: `open` or `splice`, with what a splice did: its
/// `direction` (`in` / `out`) and `change_sat` (capacity against the funding before),
/// when known.
#[derive(Serialize)]
pub struct Funding {
    txid: String,
    kind: String,
    seen_at: i64,
    direction: Option<String>,
    capacity_sat: Option<u64>,
    change_sat: Option<i64>,
}

fn funding_list(f: &[allod_core::ChannelFunding]) -> Vec<Funding> {
    let mut out = Vec::with_capacity(f.len());
    let mut before: Option<u64> = None;
    for x in f {
        let change = match (x.kind.as_str(), x.capacity_sat, before) {
            ("splice", Some(now), Some(prev)) => Some(now as i64 - prev as i64),
            _ => None,
        };
        // A splice the peer started: its direction from the change, when seen.
        let direction = x.direction.clone().or_else(|| {
            change
                .filter(|c| *c != 0)
                .map(|c| if c > 0 { "in".to_string() } else { "out".to_string() })
        });
        out.push(Funding {
            txid: x.txid.clone(),
            kind: x.kind.clone(),
            seen_at: x.seen_at,
            direction,
            capacity_sat: x.capacity_sat,
            change_sat: change,
        });
        if x.capacity_sat.is_some() {
            before = x.capacity_sat;
        }
    }
    out
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
    /// Required for splice-out; for splice-in, left out with `all` to add everything the
    /// node can spare (after fees and its anchor reserve).
    amount_sat: Option<u64>,
    #[serde(default)]
    all: bool,
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
    if let Some(w) = state.wallet.get() {
        w.note_splice(&ucid, "in");
    }
    let amount = match (req.amount_sat, req.all) {
        (_, true) => None,
        (Some(a), false) if a > 0 => Some(a),
        _ => return Err(ApiError::bad_request("give amount_sat, or all: true")),
    };
    node(&state).await?.splice_in(&ucid, &req.counterparty, amount).await?;
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
    if let Some(w) = state.wallet.get() {
        w.note_splice(&ucid, "out");
    }
    let amount = req
        .amount_sat
        .filter(|a| *a > 0)
        .ok_or_else(|| ApiError::bad_request("amount_sat must be greater than 0"))?;
    node(&state).await?.splice_out(&ucid, &req.counterparty, req.address, amount).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub struct BumpSplice {
    counterparty: String,
}

/// `POST /api/channels/{user_channel_id}/splice-bump`: a higher fee for the splice still
/// waiting to confirm (the node picks the rate).
pub async fn splice_bump(
    State(state): State<AppState>,
    auth: Auth,
    Path(ucid): Path<String>,
    Json(req): Json<BumpSplice>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    node(&state).await?.bump_splice_fee(&ucid, &req.counterparty).await?;
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
