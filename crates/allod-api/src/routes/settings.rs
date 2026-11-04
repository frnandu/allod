use allod_core::RateSource;
use allod_db::{config_keys, ConfigEntry};
use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

#[derive(Serialize)]
pub struct Settings {
    /// ISO 4217 code, or "none" for sats only.
    fiat_currency: String,
    fiat_source: RateSource,
    /// Host the rates come from (shown to the user).
    fiat_source_host: &'static str,
}

/// Display currency until the owner picks another in Settings.
pub const DEFAULT_CURRENCY: &str = "USD";

async fn load(state: &AppState) -> Result<(String, RateSource), ApiError> {
    let currency = state
        .db
        .config_get(config_keys::FIAT_CURRENCY)
        .await?
        .map(|e| e.value)
        .unwrap_or_else(|| DEFAULT_CURRENCY.into());
    let source = state
        .db
        .config_get(config_keys::FIAT_SOURCE)
        .await?
        .and_then(|e| RateSource::parse(&e.value))
        .unwrap_or(RateSource::Yadio);
    Ok((currency, source))
}

fn to_dto(currency: String, source: RateSource) -> Settings {
    Settings { fiat_currency: currency, fiat_source: source, fiat_source_host: source.host() }
}

/// `GET /api/settings`
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Result<Json<Settings>, ApiError> {
    let (c, s) = load(&state).await?;
    Ok(Json(to_dto(c, s)))
}

#[derive(Deserialize)]
pub struct Patch {
    fiat_currency: Option<String>,
    fiat_source: Option<String>,
}

/// `PATCH /api/settings`
pub async fn patch(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Patch>,
) -> Result<Json<Settings>, ApiError> {
    auth.require_full()?;
    let (mut currency, mut source) = load(&state).await?;
    if let Some(s) = req.fiat_source {
        source = RateSource::parse(&s).ok_or_else(|| {
            ApiError::bad_request("fiat_source must be yadio, mempool or coingecko")
        })?;
    }
    if let Some(c) = req.fiat_currency {
        let c = c.trim().to_ascii_uppercase();
        currency = if c == "NONE" { "none".into() } else { c };
    }
    if currency != "none" {
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(ApiError::bad_request("fiat_currency must be an ISO 4217 code or none"));
        }
        let offered = state.fiat.currencies(source).await.map_err(|e| {
            ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "RATE_SOURCE", e.to_string())
        })?;
        if !offered.contains(&currency) {
            return Err(ApiError::bad_request(format!(
                "{currency} is not offered by {}",
                source.host()
            )));
        }
    }
    let entry = |k: &str, v: &str| ConfigEntry { key: k.into(), value: v.into(), encrypted: false };
    state
        .db
        .config_set_many(&[
            entry(config_keys::FIAT_CURRENCY, &currency),
            entry(config_keys::FIAT_SOURCE, source.as_str()),
        ])
        .await?;
    audit(
        &state,
        "settings_changed",
        json!({"fiat_currency": currency, "fiat_source": source.as_str()}),
        unix_now(),
    )
    .await;
    Ok(Json(to_dto(currency, source)))
}

#[derive(Deserialize)]
pub struct CurrenciesQuery {
    source: Option<String>,
}

/// `GET /api/fiat/currencies?source=yadio`
pub async fn currencies(
    State(state): State<AppState>,
    _auth: Auth,
    Query(q): Query<CurrenciesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let source = match q.source {
        Some(s) => RateSource::parse(&s).ok_or_else(|| ApiError::bad_request("unknown source"))?,
        None => load(&state).await?.1,
    };
    let list = state.fiat.currencies(source).await.map_err(|e| {
        ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "RATE_SOURCE", e.to_string())
    })?;
    Ok(Json(json!({"source": source, "host": source.host(), "currencies": list})))
}

/// `GET /api/fiat/rate`: BTC price in the chosen currency.
pub async fn rate(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (currency, source) = load(&state).await?;
    if currency == "none" {
        return Ok(Json(json!({"currency": "none"})));
    }
    let price = state.fiat.price(source, &currency).await.map_err(|e| {
        ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "RATE_SOURCE", e.to_string())
    })?;
    Ok(Json(
        json!({"currency": currency, "source": source, "btc_price": price, "fetched_at": unix_now()}),
    ))
}

fn chain_source_dto(state: &AppState) -> serde_json::Value {
    use allod_core::chain_source::presets;
    let network = state.setup.network().unwrap_or(allod_types::Network::Bitcoin);
    let store = state.chain_source.as_deref();
    json!({
        // The stack's default from ALLOD_CHAIN; `active.mode` is what the node runs with.
        "default_mode": state.deployment.chain_mode,
        "network": network,
        // Allod can only change the chain source of an ldk-server it manages.
        "editable": store.is_some(),
        "active": store.and_then(|s| s.active()),
        "requested": store.and_then(|s| s.requested()).map(|r| r.view()),
        "restart_pending": store.is_some_and(|s| s.restart_pending()),
        "presets": presets(network),
        "standby": standby_dto(state),
    })
}

/// Whether Allod suggests the standby bitcoind (SPEC §2.3.2): the node runs on the
/// stack's external bitcoind, and that node has not answered for a few minutes.
pub fn standby_suggested(state: &AppState, now: i64) -> bool {
    let (Some(standby), Some(store)) = (state.setup.standby(), state.chain_source.as_deref())
    else {
        return false;
    };
    state.deployment.chain_mode == Some(allod_types::ChainMode::ExternalBitcoind)
        && store.default_in_use()
        && standby.outage_long_enough(now)
}

fn standby_dto(state: &AppState) -> serde_json::Value {
    let Some(standby) = state.setup.standby() else { return serde_json::Value::Null };
    let now = unix_now();
    let outage = standby.outage();
    json!({
        "enabled": standby.enabled(),
        "in_use": state.chain_source.as_deref().is_some_and(|s| s.wants_standby()),
        "outage_since": outage.as_ref().map(|o| o.0),
        "outage_error": outage.map(|o| o.1),
        "suggested": standby_suggested(state, now),
    })
}

/// `POST /api/settings/chain-source/standby`: starts the stack's standby bitcoind and
/// points ldk-server at it. The node waits for its initial sync before it starts.
pub async fn use_standby_bitcoind(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth.require_full()?;
    let not_supported = || {
        ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "NOT_SUPPORTED",
            "this installation has no built-in Bitcoin node to start",
        )
    };
    let standby = state.setup.standby().ok_or_else(not_supported)?;
    let store = state.chain_source.as_deref().ok_or_else(not_supported)?;
    let now = unix_now();
    standby.enable(now).map_err(|e| ApiError::internal(e.to_string()))?;
    let source = store.request_standby(standby).map_err(|e| ApiError::internal(e.to_string()))?;
    standby.reset_probe();
    audit(&state, "chain_source_changed", json!(source.view()), now).await;
    Ok(Json(chain_source_dto(&state)))
}

/// `GET /api/settings/chain-source`
pub async fn chain_source(State(state): State<AppState>, _auth: Auth) -> Json<serde_json::Value> {
    Json(chain_source_dto(&state))
}

/// `PUT /api/settings/chain-source`: picks the chain source; ldk-server restarts with it.
pub async fn set_chain_source(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<allod_core::chain_source::SourceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use allod_core::chain_source::ChainSourceError;
    auth.require_full()?;
    let store = state.chain_source.as_deref().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "NOT_SUPPORTED",
            "the chain source can only be changed when Allod manages ldk-server",
        )
    })?;
    let source = store.request(req).map_err(|e| match e {
        ChainSourceError::BadRequest(m) => ApiError::bad_request(m),
        ChainSourceError::Io(e) => ApiError::internal(e.to_string()),
    })?;
    audit(&state, "chain_source_changed", json!(source.view()), unix_now()).await;
    Ok(Json(chain_source_dto(&state)))
}
