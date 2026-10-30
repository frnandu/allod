use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{expected_urls, nip98, unix_now, Auth, Subject};
use crate::error::ApiError;
use crate::AppState;

pub const LOGIN_PATH: &str = "/api/auth/login";

#[derive(Serialize)]
pub struct ChallengeResponse {
    challenge: String,
    expires_at: i64,
}

/// `GET /api/auth/challenge`
pub async fn challenge(State(state): State<AppState>) -> Result<Json<ChallengeResponse>, ApiError> {
    let now = unix_now();
    let challenge = state.auth.new_challenge(now)?;
    Ok(Json(ChallengeResponse { challenge, expires_at: now + crate::auth::CHALLENGE_SECS }))
}

#[derive(Deserialize)]
struct LoginBody {
    challenge: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    token: String,
    expires_at: i64,
    pubkey: String,
}

/// `POST /api/auth/login` with `Authorization: Nostr <event>` and body `{"challenge"}`.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<LoginResponse>, ApiError> {
    let now = unix_now();
    let urls = expected_urls(state.public_url.as_deref(), &headers, LOGIN_PATH);
    let authorization = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let pubkey = match nip98::verify(
        authorization,
        &nip98::Expect { urls: &urls, method: "POST", body: &body, now },
    ) {
        Ok(pk) => pk,
        Err(e) => {
            // An invalid NIP-98 event has no authenticated identity. Logging every
            // malformed header would let anyone fill the persistent audit table.
            return Err(ApiError::unauthorized(e.to_string()));
        }
    };
    let parsed: LoginBody = serde_json::from_slice(&body)
        .map_err(|_| ApiError::bad_request("body must be {\"challenge\": \"...\"}"))?;
    if !state.auth.take_challenge(&parsed.challenge, now) {
        audit(
            &state,
            "login_failed",
            json!({"pubkey": pubkey, "reason": "unknown challenge"}),
            now,
        )
        .await;
        return Err(ApiError::unauthorized("unknown or expired challenge"));
    }
    if !state.db.is_owner(&pubkey).await? {
        audit(&state, "login_failed", json!({"pubkey": pubkey, "reason": "not an owner"}), now)
            .await;
        return Err(ApiError::forbidden("this Nostr key is not an owner of this node"));
    }
    let (token, expires_at) = state.auth.issue_session(&pubkey, now)?;
    audit(&state, "login", json!({"pubkey": pubkey}), now).await;
    Ok(Json(LoginResponse { token, expires_at, pubkey }))
}

/// `POST /api/auth/logout`
pub async fn logout(State(state): State<AppState>, auth: Auth) -> StatusCode {
    auth.logout(&state.auth);
    StatusCode::NO_CONTENT
}

/// `GET /api/auth/session`: who is calling.
pub async fn session(auth: Auth) -> Json<serde_json::Value> {
    let body = match &auth.subject {
        Subject::Owner { pubkey, expires_at, .. } => json!({
            "kind": "owner", "pubkey": pubkey, "expires_at": expires_at,
            "permission": auth.permission.as_str(),
        }),
        Subject::Token { id, name } => json!({
            "kind": "api_token", "id": id, "name": name, "permission": auth.permission.as_str(),
        }),
    };
    Json(body)
}

/// Best-effort audit log write; failures are logged, never returned to the client.
pub async fn audit(state: &AppState, kind: &str, data: serde_json::Value, now: i64) {
    if let Err(e) = state.db.audit(kind, None, &data.to_string(), now).await {
        tracing::warn!(error = %e, kind, "audit log write failed");
    }
}
