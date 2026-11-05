//! Device notifications over Web Push (SPEC §8.9).

use std::sync::Arc;

use allod_core::push::{NewSubscription, SubscriptionPatch, SubscriptionView};
use allod_core::{PushError, PushService};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

impl From<PushError> for ApiError {
    fn from(e: PushError) -> Self {
        match e {
            PushError::BadRequest(m) => ApiError::bad_request(m),
            PushError::QuotaExceeded(m) => ApiError::bad_request(m),
            PushError::NotFound => ApiError::not_found("subscription not found"),
            PushError::Db(e) => {
                tracing::error!(error = %e, "push error");
                ApiError::internal("internal error")
            }
        }
    }
}

fn push(state: &AppState) -> Result<Arc<PushService>, ApiError> {
    state.push.get().cloned().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "NODE_NOT_READY",
            "notifications are available once the Lightning node is ready",
        )
    })
}

/// `GET /api/push`
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let svc = push(&state)?;
    let subs: Vec<SubscriptionView> = svc.list().await?.iter().map(|s| s.view()).collect();
    Ok(Json(json!({
        "vapid_public_key": svc.public_key(),
        "events": allod_core::wallet::outbox::TYPES,
        "subscriptions": subs,
    })))
}

#[derive(Deserialize)]
pub struct Keys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
pub struct Subscribe {
    endpoint: String,
    keys: Keys,
    label: Option<String>,
    events: Option<Vec<String>>,
    hide_amounts: Option<bool>,
}

/// `POST /api/push/subscriptions` (owner session): registers this browser, or refreshes
/// it when the endpoint is known.
pub async fn subscribe(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Subscribe>,
) -> Result<Json<SubscriptionView>, ApiError> {
    let owner = auth.require_owner()?.to_string();
    let svc = push(&state)?;
    let known = svc.list().await?.iter().any(|s| s.endpoint == req.endpoint);
    let sub = svc
        .upsert(
            &owner,
            NewSubscription {
                endpoint: req.endpoint,
                p256dh: req.keys.p256dh,
                auth: req.keys.auth,
                label: req.label,
                events: req.events,
                hide_amounts: req.hide_amounts,
            },
        )
        .await?;
    if !known {
        let data = json!({"id": sub.id, "label": sub.label, "by": owner});
        audit(&state, "push_subscription_created", data, unix_now()).await;
    }
    Ok(Json(sub.view()))
}

#[derive(Deserialize)]
pub struct Patch {
    label: Option<String>,
    events: Option<Vec<String>>,
    hide_amounts: Option<bool>,
}

/// `PATCH /api/push/subscriptions/{id}`
pub async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<Patch>,
) -> Result<Json<SubscriptionView>, ApiError> {
    auth.require_full()?;
    let sub = push(&state)?
        .patch(
            id,
            SubscriptionPatch {
                label: req.label,
                events: req.events,
                hide_amounts: req.hide_amounts,
            },
        )
        .await?;
    let data = json!({"id": id, "events": sub.events, "hide_amounts": sub.hide_amounts});
    audit(&state, "push_subscription_updated", data, unix_now()).await;
    Ok(Json(sub.view()))
}

/// `DELETE /api/push/subscriptions/{id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    push(&state)?.delete(id).await?;
    audit(&state, "push_subscription_deleted", json!({"id": id}), unix_now()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/push/subscriptions/{id}/test`: sends a test notification at once.
pub async fn test(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let result = push(&state)?.test(id).await?;
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}
