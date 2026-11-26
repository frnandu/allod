//! Settings → Lightning → Connectivity (SPEC §12.7.2): how other nodes reach this one —
//! privately not at all, over Tor, through a gateway's IP, at the host's own IP, or over
//! Tor and one clearnet path together.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use allod_core::domains::lightning::{Clearnet, Connectivity};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

/// The settings plus what the node itself reports (alias, announced addresses).
async fn view(state: &AppState) -> Result<Json<Value>, ApiError> {
    let Some(svc) = state.domains.as_ref() else {
        return Ok(Json(json!({ "available": false, "gateways": [] })));
    };
    let mut v = svc.lightning_view().await?;
    let info = match state.ldk.backend().await {
        Ok(n) => n.node_info().await.ok(),
        Err(_) => None,
    };
    // Whether the node announces exactly what was chosen (it restarts to pick a change up).
    let announced = match (v["addresses"].as_array(), &info) {
        (Some(want), Some(i)) => {
            let mut want: Vec<&str> = want.iter().filter_map(|a| a.as_str()).collect();
            let mut have: Vec<&str> = i.announcement_addresses.iter().map(|a| a.as_str()).collect();
            want.sort_unstable();
            have.sort_unstable();
            Some(want == have)
        }
        _ => None,
    };
    v["available"] = json!(true);
    v["announced"] = json!(announced);
    v["node"] = json!(info.map(|i| json!({
        "pubkey": i.summary.pubkey,
        "alias": i.alias,
        "announcement_addresses": i.announcement_addresses,
    })));
    Ok(Json(v))
}

/// `GET /api/settings/lightning`
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    view(&state).await
}

#[derive(Deserialize)]
pub struct ConnectivityRequest {
    /// `none`, `gateway` or `host`. Left out together with `tor`: the earlier form,
    /// `{gateway_id}` (a gateway, or `null` for private).
    clearnet: Option<Clearnet>,
    /// Also (or only) reachable at an onion address.
    tor: Option<bool>,
    gateway_id: Option<i64>,
    /// Public port on the gateway (default 9735).
    port: Option<u16>,
    /// Public IP or DNS name of the host.
    host: Option<String>,
    /// Public port on the host (default: the port Compose publishes).
    host_port: Option<u16>,
}

/// `PUT /api/settings/lightning {clearnet, tor, gateway_id?, port?, host?, host_port?}`:
/// ldk-server restarts to announce and listen accordingly.
pub async fn set(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ConnectivityRequest>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = state
        .domains
        .as_ref()
        .ok_or_else(|| ApiError::conflict("this setting is not available on this installation"))?;
    let c = match (req.clearnet, req.tor) {
        (None, None) => Connectivity {
            clearnet: if req.gateway_id.is_some() { Clearnet::Gateway } else { Clearnet::None },
            gateway_id: req.gateway_id,
            gateway_port: req.port,
            ..Default::default()
        },
        (clearnet, tor) => Connectivity {
            tor: tor.unwrap_or(false),
            clearnet: clearnet.unwrap_or_default(),
            gateway_id: req.gateway_id,
            gateway_port: req.port,
            host: req.host.clone(),
            host_port: req.host_port,
        },
    };
    svc.set_connectivity(c.clone()).await?;
    audit(
        &state,
        "lightning_connectivity_changed",
        json!({
            "tor": c.tor,
            "clearnet": c.clearnet,
            "gateway_id": c.gateway_id,
            "port": c.gateway_port,
            "host": (c.clearnet == Clearnet::Host).then_some(c.host.clone()).flatten(),
            "host_port": c.host_port,
        }),
        unix_now(),
    )
    .await;
    // The clearnet check answers within seconds; the onion one can take most of a minute,
    // so it runs on its own and the page follows it.
    svc.lightning_recheck_clearnet().await;
    let bg = svc.clone();
    tokio::spawn(async move { bg.lightning_check_tor().await });
    view(&state).await
}

/// `POST /api/settings/lightning/check`: checks every chosen path again now.
pub async fn check(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    if let Some(svc) = state.domains.as_ref() {
        svc.lightning_recheck().await;
    }
    view(&state).await
}

/// `POST /api/settings/lightning/detect-ip`: the public IP outgoing connections use, as an
/// echo service sees it (`{ip, source}`).
pub async fn detect_ip(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = state
        .domains
        .as_ref()
        .ok_or_else(|| ApiError::conflict("this setting is not available on this installation"))?;
    Ok(Json(svc.detect_public_ip().await?))
}

/// `GET /api/settings/lightning/liquidity`: whether Receive may ask LSPS2 LSPs for a
/// just-in-time channel, which ones, 0-conf, the ones to pick from and what the node runs
/// with.
pub async fn liquidity(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Value>, ApiError> {
    let Some(svc) = state.domains.as_ref() else {
        return Ok(Json(json!({ "available": false, "sources": [] })));
    };
    let mut v = svc.lsps2_view().await?;
    describe_lsps(&state, &mut v).await;
    Ok(Json(v))
}

/// What the node's own network graph says about each LSP (its alias, how many channels it
/// announced) and whether the node is connected to it now. Nothing is asked of the LSP
/// itself: its LSPS2 fee menu is only reachable through ldk-server, which keeps it to
/// itself (it asks while making a just-in-time invoice).
async fn describe_lsps(state: &AppState, v: &mut Value) {
    let Ok(backend) = state.ldk.backend().await else { return };
    let Some(sources) = v["sources"].as_array_mut() else { return };
    let ids: Vec<String> = sources
        .iter()
        .filter_map(|s| s["lsp"].as_str().and_then(|l| l.split('@').next()).map(str::to_string))
        .collect();
    let aliases = state.ldk.aliases().resolve(backend.as_ref(), &state.gossip, ids.clone()).await;
    let connected: Vec<String> = backend
        .list_peers()
        .await
        .map(|p| p.into_iter().filter(|p| p.is_connected).map(|p| p.node_id).collect())
        .unwrap_or_default();
    for (s, id) in sources.iter_mut().zip(ids) {
        s["alias"] = json!(aliases.get(&id).cloned().flatten());
        s["channels"] = json!(backend.node_channel_count(&id).await.ok().flatten());
        s["connected"] = json!(connected.contains(&id));
    }
}

/// `PUT /api/settings/lightning/liquidity {enabled, lsps, trust_0conf, custom: [{name, lsp,
/// token?}]}`: ldk-server restarts to use it (the page asks the owner first).
pub async fn set_liquidity(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<allod_core::domains::lsps2::Lsps2Request>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = state
        .domains
        .as_ref()
        .ok_or_else(|| ApiError::conflict("this setting is not available on this installation"))?;
    let (enabled, lsps, trust_0conf, custom) =
        (req.enabled, req.lsps.clone(), req.trust_0conf, req.custom.len());
    let mut v = svc.set_lsps2(req).await?;
    audit(
        &state,
        "lightning_lsps2_changed",
        json!({ "enabled": enabled, "lsps": lsps, "trust_0conf": trust_0conf, "custom": custom }),
        unix_now(),
    )
    .await;
    describe_lsps(&state, &mut v).await;
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct JitQuoteQuery {
    amount_msat: u64,
}

/// `GET /api/invoices/jit-quote?amount_msat=N`: what a just-in-time channel would cost for
/// this payment. Allod asks each LSP the node uses for its LSPS2 fee menu itself (a
/// Lightning connection of its own, nothing opened or bought) and picks the cheapest offer
/// that fits, as ldk-node will. `cheapest` is null when none fits; `lsps` says why per LSP.
pub async fn jit_quote(
    State(state): State<AppState>,
    _auth: Auth,
    Query(q): Query<JitQuoteQuery>,
) -> Result<Json<Value>, ApiError> {
    let Some(svc) = state.domains.as_ref() else {
        return Ok(Json(json!({ "amount_msat": q.amount_msat, "cheapest": null, "lsps": [] })));
    };
    if q.amount_msat == 0 {
        return Err(ApiError::bad_request("amount_msat must be greater than 0"));
    }
    let mut v = svc.lsps2_quote(q.amount_msat).await?;
    // The default LSP goes by the alias it announced, as on the Liquidity page.
    if let Ok(backend) = state.ldk.backend().await {
        let ids: Vec<String> = v["lsps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["lsp"].as_str()?.split('@').next().map(str::to_string))
            .collect();
        let aliases = state.ldk.aliases().resolve(backend.as_ref(), &state.gossip, ids).await;
        let named = |e: &mut Value| {
            let id = e["lsp"].as_str().and_then(|l| l.split('@').next()).unwrap_or("").to_string();
            // The default LSP, and one added without a name ("034066…53b0").
            let unnamed = e["name"] == "Default LSP"
                || e["name"]
                    .as_str()
                    .is_some_and(|n| n.contains('…') && id.starts_with(&n[..6.min(n.len())]));
            if unnamed {
                if let Some(Some(a)) = aliases.get(&id) {
                    e["name"] = json!(a);
                }
            }
        };
        if let Some(list) = v["lsps"].as_array_mut() {
            list.iter_mut().for_each(named);
        }
        if v["cheapest"].is_object() {
            named(&mut v["cheapest"]);
        }
    }
    Ok(Json(v))
}
