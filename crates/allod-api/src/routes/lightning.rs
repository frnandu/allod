//! Settings → Lightning (SPEC §12.7.2): Lightning connections through a gateway, so peers
//! reach this node at the gateway's IP instead of the home IP.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

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
    let announced = match (v["address"].as_str(), &info) {
        (Some(a), Some(i)) => Some(i.announcement_addresses.iter().any(|x| x == a)),
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
pub struct LightningRequest {
    /// The gateway Lightning connections come through; `null` turns it off.
    gateway_id: Option<i64>,
    /// Public port on the gateway (default 9735).
    port: Option<u16>,
}

/// `PUT /api/settings/lightning {gateway_id: id | null, port?}`: ldk-server restarts to
/// announce the new address.
pub async fn set(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<LightningRequest>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = state.domains.as_ref().ok_or_else(|| {
        ApiError::conflict("gateways are not available here, so neither is this setting")
    })?;
    svc.set_lightning_gateway(req.gateway_id, req.port).await?;
    audit(
        &state,
        "lightning_gateway_changed",
        json!({"gateway_id": req.gateway_id, "port": req.port}),
        unix_now(),
    )
    .await;
    svc.lightning_recheck().await;
    view(&state).await
}

/// `POST /api/settings/lightning/check`: connects through the gateway again now.
pub async fn check(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    if let Some(svc) = state.domains.as_ref() {
        svc.lightning_recheck().await;
    }
    view(&state).await
}
