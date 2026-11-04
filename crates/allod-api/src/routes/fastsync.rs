//! Optional fast sync (AssumeUTXO) for the stack's own bitcoind, chosen in the setup
//! wizard or later while Bitcoin is still syncing: the bundled node, or the standby node
//! once the Lightning node uses it (SPEC §2.3.2).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

/// The fast sync helper, while the node reads the chain from a bitcoind that runs it:
/// the bundled one, or the standby one after the owner switched to it.
fn offered(state: &AppState) -> Option<&allod_core::fastsync::FastSync> {
    use allod_types::ChainMode::{Bundled, BundledRest};
    let fs = state.fast_sync.as_deref()?;
    let bundled = matches!(state.deployment.chain_mode, Some(Bundled | BundledRest));
    let standby = state.setup.standby().is_some_and(|s| s.enabled())
        && state.chain_source.as_deref().is_some_and(|c| c.wants_standby());
    (bundled || standby).then_some(fs)
}

fn fast_sync(state: &AppState) -> Result<&allod_core::fastsync::FastSync, ApiError> {
    offered(state).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "NOT_SUPPORTED",
            "fast sync is only available with the Bitcoin node of this stack",
        )
    })
}

/// `GET /api/sync/fast`
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Json<Value> {
    let network = state.setup.network();
    match offered(&state) {
        None => Json(json!({"available": false})),
        Some(fs) => Json(json!({
            "available": true,
            "requested_url": fs.requested_url(),
            "status": fs.status(),
            "suggestion": fs.suggestion(network),
        })),
    }
}

#[derive(Deserialize)]
pub struct Request {
    url: String,
}

/// `POST /api/sync/fast {url}`: only while Bitcoin is still in initial block download.
pub async fn request(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Request>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let fs = fast_sync(&state)?;
    let progress = state.setup.progress().await;
    let ibd = progress
        .bitcoind
        .as_ref()
        .and_then(|b| b.get("initial_block_download"))
        .and_then(Value::as_bool);
    if ibd == Some(false) {
        return Err(ApiError::conflict("Bitcoin is already synced; fast sync is not needed"));
    }
    fs.request(&req.url).map_err(|e| ApiError::bad_request(e.to_string()))?;
    audit(&state, "fast_sync_requested", json!({"url": req.url}), unix_now()).await;
    Ok(Json(json!({"requested_url": fs.requested_url()})))
}

/// `DELETE /api/sync/fast`: cancel before the snapshot is loaded.
pub async fn cancel(State(state): State<AppState>, auth: Auth) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    fast_sync(&state)?.cancel().map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
