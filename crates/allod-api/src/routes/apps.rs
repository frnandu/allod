//! NWC connections (SPEC §11.3, M4).

use std::sync::Arc;

use allod_core::apps::{AppPatch, AppView, NewApp};
use allod_core::{AppError, AppService};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::routes::wallet::wallet;
use crate::AppState;

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::BadRequest(m) => ApiError::bad_request(m),
            AppError::NotFound => ApiError::not_found("connection not found"),
            AppError::Internal(m) => {
                tracing::error!(error = %m, "apps error");
                ApiError::internal("internal error")
            }
        }
    }
}

fn apps(state: &AppState) -> Result<Arc<AppService>, ApiError> {
    state.apps.get().cloned().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "NODE_NOT_READY",
            "connections are available once the Lightning node is ready",
        )
    })
}

async fn view(state: &AppState, app: allod_core::App) -> Result<AppView, ApiError> {
    Ok(apps(state)?.view(app, &*wallet(state)?).await?)
}

/// `GET /api/apps`
pub async fn list(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let mut out = Vec::new();
    for app in apps(&state)?.list().await? {
        out.push(view(&state, app).await?);
    }
    Ok(Json(serde_json::to_value(out).unwrap_or_default()))
}

/// `GET /api/apps/{id}`: the connection and its latest requests.
pub async fn get(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let svc = apps(&state)?;
    let app = svc.get(id).await?;
    let requests = svc.requests(id, 50).await?;
    let mut v = serde_json::to_value(view(&state, app).await?).unwrap_or_default();
    v["requests"] = serde_json::to_value(requests).unwrap_or_default();
    Ok(Json(v))
}

/// `POST /api/apps` → `{app, pairing_uri?}`
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewApp>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    auth.require_full()?;
    let now = unix_now();
    let (app, uri) = apps(&state)?.create(req, None, now).await?;
    audit(
        &state,
        "app_created",
        json!({"app_id": app.id, "name": app.name, "scopes": app.scopes}),
        now,
    )
    .await;
    let app = view(&state, app).await?;
    Ok((StatusCode::CREATED, Json(json!({"app": app, "pairing_uri": uri}))))
}

/// `PATCH /api/apps/{id}`
pub async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(patch): Json<AppPatch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let now = unix_now();
    let app = apps(&state)?.update(id, patch, now).await?;
    audit(&state, "app_updated", json!({"app_id": id, "scopes": app.scopes}), now).await;
    Ok(Json(serde_json::to_value(view(&state, app).await?).unwrap_or_default()))
}

/// `DELETE /api/apps/{id}`
pub async fn delete(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    apps(&state)?.delete(id, &*wallet(&state)?).await?;
    audit(&state, "app_deleted", json!({"app_id": id}), unix_now()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/apps/{id}/regenerate` → new pairing URI; the old client stops working.
pub async fn regenerate(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let now = unix_now();
    let (app, uri) = apps(&state)?.regenerate(id, now).await?;
    audit(&state, "app_regenerated", json!({"app_id": id}), now).await;
    let app = view(&state, app).await?;
    Ok(Json(json!({"app": app, "pairing_uri": uri})))
}
