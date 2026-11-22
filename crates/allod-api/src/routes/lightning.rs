//! Settings → Lightning → Connectivity (SPEC §12.7.2): how other nodes reach this one —
//! privately not at all, over Tor, through a gateway's IP, at the host's own IP, or over
//! Tor and one clearnet path together.

use axum::extract::State;
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
