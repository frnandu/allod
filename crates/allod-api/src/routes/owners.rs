use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{reauthenticate, unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

#[derive(Serialize)]
pub struct OwnerDto {
    pubkey: String,
    npub: String,
    label: String,
    created_at: i64,
}

fn to_dto(o: allod_db::Owner) -> OwnerDto {
    let npub = nostr::PublicKey::from_hex(&o.pubkey)
        .ok()
        .and_then(|pk| nostr::ToBech32::to_bech32(&pk).ok())
        .unwrap_or_default();
    OwnerDto { pubkey: o.pubkey, npub, label: o.label, created_at: o.created_at }
}

/// Parses hex or npub into lowercase x-only hex.
pub fn normalize_pubkey(input: &str) -> Result<String, ApiError> {
    nostr::PublicKey::parse(input.trim())
        .map(|pk| pk.to_hex())
        .map_err(|_| ApiError::bad_request("pubkey must be 64 hex characters or an npub"))
}

/// `GET /api/owners`
pub async fn list(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Vec<OwnerDto>>, ApiError> {
    Ok(Json(state.db.owners().await?.into_iter().map(to_dto).collect()))
}

#[derive(Deserialize)]
struct AddOwner {
    pubkey: String,
    #[serde(default)]
    label: String,
}

/// `POST /api/owners {pubkey, label}` (re-authentication required)
pub async fn add(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<OwnerDto>), ApiError> {
    reauthenticate(&state, &auth, &headers, "POST", "/api/owners", &body)?;
    let req: AddOwner =
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("invalid body"))?;
    let pubkey = normalize_pubkey(&req.pubkey)?;
    let label: String = req.label.chars().take(64).collect();
    let now = unix_now();
    if !state.db.add_owner(&pubkey, &label, now).await? {
        return Err(ApiError::conflict("already an owner"));
    }
    audit(&state, "owner_added", json!({"pubkey": pubkey, "by": auth.require_owner()?}), now).await;
    let owner = allod_db::Owner { pubkey, label, created_at: now };
    Ok((StatusCode::CREATED, Json(to_dto(owner))))
}

/// `DELETE /api/owners/:pubkey` (re-authentication required, never the last owner)
pub async fn remove(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
) -> Result<StatusCode, ApiError> {
    reauthenticate(&state, &auth, &headers, "DELETE", &format!("/api/owners/{pubkey}"), b"")?;
    let pubkey = normalize_pubkey(&pubkey)?;
    if !state.db.remove_owner(&pubkey).await? {
        return Err(ApiError::not_found("no such owner"));
    }
    audit(
        &state,
        "owner_removed",
        json!({"pubkey": pubkey, "by": auth.require_owner()?}),
        unix_now(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
