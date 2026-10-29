//! Nostr contacts and profiles for Send and the activity list.

use std::sync::Arc;

use allod_nwc::contacts::Contacts;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::auth::{Auth, Subject};
use crate::error::ApiError;
use crate::AppState;

fn contacts(state: &AppState) -> Result<Arc<Contacts>, ApiError> {
    state.contacts.get().cloned().ok_or_else(|| {
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "NOT_READY", "contacts are not available")
    })
}

#[derive(Deserialize)]
pub struct ContactsQuery {
    #[serde(default)]
    refresh: bool,
}

/// `GET /api/contacts` → `{contacts, discovering}`: the signed-in owner's follow list
/// with profiles and pay targets.
/// API tokens use the first owner.
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ContactsQuery>,
) -> Result<Json<Value>, ApiError> {
    let owner = match &auth.subject {
        Subject::Owner { pubkey, .. } => pubkey.clone(),
        Subject::Token { .. } => state
            .db
            .owners()
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .first()
            .map(|o| o.pubkey.clone())
            .ok_or_else(|| ApiError::not_found("no owner"))?,
    };
    let svc = contacts(&state)?;
    let list = svc
        .contacts(&owner, q.refresh)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "RELAYS_UNAVAILABLE", e))?;
    // `discovering`: contacts' own relays (NIP-65) are still being asked; the list grows.
    Ok(Json(serde_json::json!({"contacts": list, "discovering": svc.discovering()})))
}

#[derive(Deserialize)]
pub struct ProfilesQuery {
    pubkeys: String,
}

/// `GET /api/profiles?pubkeys=a,b`: names and pictures for up to 100 pubkeys.
pub async fn profiles(
    State(state): State<AppState>,
    _auth: Auth,
    Query(q): Query<ProfilesQuery>,
) -> Result<Json<Value>, ApiError> {
    let keys: Vec<String> = q
        .pubkeys
        .split(',')
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()))
        .take(100)
        .collect();
    let list = contacts(&state)?.profiles(&keys, false).await;
    Ok(Json(serde_json::to_value(list).unwrap_or_default()))
}
