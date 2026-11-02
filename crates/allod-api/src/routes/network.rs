//! Network settings of the appliance host (SPEC §2.3.10): Wi-Fi status, scan, connect,
//! forget and the Wi-Fi country. Only offered on the appliance images, which run the host
//! helper behind `ALLOD_HOST_NETWORK_SOCKET`.

use allod_core::hostnet::{HostNetError, HostNetwork};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

fn host(state: &AppState) -> Result<&HostNetwork, ApiError> {
    state.host_network.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "NOT_SUPPORTED",
            "network settings are only available on the Allod appliance images",
        )
    })
}

fn map_err(e: HostNetError) -> ApiError {
    match e {
        HostNetError::BadRequest(m) => ApiError::bad_request(m),
        HostNetError::Failed(m) => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "NETWORK_FAILED", m)
        }
        e @ HostNetError::Unavailable(_) => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "HOST_UNAVAILABLE", e.to_string())
        }
    }
}

/// `GET /api/system/network`: `{"available": false}` outside the appliance images.
pub async fn status(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let Some(host) = state.host_network.as_deref() else {
        return Ok(Json(json!({ "available": false })));
    };
    let mut status = host.status().await.map_err(map_err)?;
    status["available"] = json!(true);
    if let Some(o) = status.as_object_mut() {
        o.remove("ok");
    }
    Ok(Json(status))
}

/// `POST /api/system/network/scan`: Wi-Fi networks in range.
pub async fn scan(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let mut reply = host(&state)?.scan().await.map_err(map_err)?;
    if let Some(o) = reply.as_object_mut() {
        o.remove("ok");
    }
    Ok(Json(reply))
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    ssid: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    hidden: bool,
}

/// `POST /api/system/network/wifi {ssid, password?, hidden?}`: joins a network and keeps it.
pub async fn connect(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ConnectRequest>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    host(&state)?.connect(&req.ssid, &req.password, req.hidden).await.map_err(map_err)?;
    audit(&state, "wifi_connected", json!({ "ssid": req.ssid }), unix_now()).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ForgetRequest {
    ssid: String,
}

/// `POST /api/system/network/wifi/forget {ssid}`: removes a saved network.
pub async fn forget(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ForgetRequest>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    host(&state)?.forget(&req.ssid).await.map_err(map_err)?;
    audit(&state, "wifi_forgotten", json!({ "ssid": req.ssid }), unix_now()).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct CountryRequest {
    country: String,
}

/// `PUT /api/system/network/country {country}`: the Wi-Fi regulatory country.
pub async fn country(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CountryRequest>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    host(&state)?.set_country(&req.country).await.map_err(map_err)?;
    audit(
        &state,
        "wifi_country_set",
        json!({ "country": req.country.to_ascii_uppercase() }),
        unix_now(),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}
