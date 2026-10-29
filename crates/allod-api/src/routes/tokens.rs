use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{hash_token, new_api_token, unix_now, Auth, Permission};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

#[derive(Serialize)]
pub struct TokenDto {
    id: i64,
    name: String,
    permission: String,
    expires_at: Option<i64>,
    last_used_at: Option<i64>,
    created_at: i64,
}

impl From<allod_db::ApiToken> for TokenDto {
    fn from(t: allod_db::ApiToken) -> Self {
        TokenDto {
            id: t.id,
            name: t.name,
            permission: t.permission,
            expires_at: t.expires_at,
            last_used_at: t.last_used_at,
            created_at: t.created_at,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateToken {
    name: String,
    permission: String,
    expires_in_days: Option<u32>,
}

#[derive(Serialize)]
pub struct CreatedToken {
    /// Shown once; only its hash is stored.
    token: String,
    #[serde(flatten)]
    info: TokenDto,
}

/// `POST /api/tokens` (owner session only)
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateToken>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    let owner = auth.require_owner()?.to_string();
    let permission = Permission::parse(&req.permission)
        .ok_or_else(|| ApiError::bad_request("permission must be full or readonly"))?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::bad_request("name must be 1-64 characters"));
    }
    let now = unix_now();
    let expires_at = match req.expires_in_days {
        Some(0) => return Err(ApiError::bad_request("expires_in_days must be positive")),
        Some(d) => Some(now + i64::from(d.min(3650)) * 86_400),
        None => None,
    };
    let token = new_api_token();
    let id = state
        .db
        .create_api_token(name, permission.as_str(), &hash_token(&token), expires_at, now)
        .await?;
    audit(&state, "api_token_created", json!({"id": id, "name": name, "by": owner}), now).await;
    let info = TokenDto {
        id,
        name: name.to_string(),
        permission: permission.as_str().into(),
        expires_at,
        last_used_at: None,
        created_at: now,
    };
    Ok((StatusCode::CREATED, Json(CreatedToken { token, info })))
}

/// `GET /api/tokens` (owner session only)
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Vec<TokenDto>>, ApiError> {
    auth.require_owner()?;
    Ok(Json(state.db.api_tokens().await?.into_iter().map(TokenDto::from).collect()))
}

/// `DELETE /api/tokens/:id` (owner session only)
pub async fn delete(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let owner = auth.require_owner()?.to_string();
    if !state.db.delete_api_token(id).await? {
        return Err(ApiError::not_found("no such token"));
    }
    audit(&state, "api_token_deleted", json!({"id": id, "by": owner}), unix_now()).await;
    Ok(StatusCode::NO_CONTENT)
}
