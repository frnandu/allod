use allod_core::{SetupError, SetupProgress, SetupRequest};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

use crate::auth::{expected_urls, nip98, reauthenticate, unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum SetupBody {
    Create,
    Restore {
        recovery_phrase: String,
        birthday_height: Option<u32>,
    },
    Standalone {
        ldk_server_address: String,
        ldk_server_tls_cert_pem: String,
        ldk_server_macaroon_hex: String,
        recovery_phrase: Option<String>,
    },
}

#[derive(Serialize)]
pub struct SetupResponse {
    token: String,
    expires_at: i64,
    pubkey: String,
    /// Present only when Allod generated the phrase; shown once for backup.
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_phrase: Option<String>,
}

fn map_error(e: SetupError) -> ApiError {
    match e {
        SetupError::AlreadyComplete => ApiError::conflict(e.to_string()),
        SetupError::LdkAlreadyInitialized => {
            ApiError::new(StatusCode::CONFLICT, "LDK_SERVER_INITIALIZED", e.to_string())
        }
        SetupError::InvalidRequest(_) | SetupError::WrongNetwork { .. } => {
            ApiError::bad_request(e.to_string())
        }
        SetupError::MissingPermissions(_) => {
            ApiError::new(StatusCode::BAD_REQUEST, "MISSING_PERMISSIONS", e.to_string())
        }
        SetupError::Backend(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "LDK_SERVER", e.to_string())
        }
        SetupError::Internal(m) => {
            tracing::error!(error = %m, "setup failed");
            ApiError::internal("setup failed")
        }
    }
}

/// `POST /api/setup` with `Authorization: Nostr <event>`. The signer becomes the
/// first owner (SPEC §3.2). Only possible before setup completes.
pub async fn run(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<SetupResponse>), ApiError> {
    if state.setup.check_complete().await {
        return Err(ApiError::conflict("setup is already complete"));
    }
    let now = unix_now();
    let urls = expected_urls(state.public_url.as_deref(), &headers, "/api/setup");
    let authorization = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let pubkey = nip98::verify(
        authorization,
        &nip98::Expect { urls: &urls, method: "POST", body: &body, now },
    )
    .map_err(|e| ApiError::unauthorized(e.to_string()))?;
    let parsed: SetupBody = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid setup request: {e}")))?;
    let req = match parsed {
        SetupBody::Create => SetupRequest::Create,
        SetupBody::Restore { recovery_phrase, birthday_height } => {
            SetupRequest::Restore { phrase: Zeroizing::new(recovery_phrase), birthday_height }
        }
        SetupBody::Standalone {
            ldk_server_address,
            ldk_server_tls_cert_pem,
            ldk_server_macaroon_hex,
            recovery_phrase,
        } => SetupRequest::Standalone {
            address: ldk_server_address,
            tls_cert_pem: ldk_server_tls_cert_pem,
            macaroon_hex: Zeroizing::new(ldk_server_macaroon_hex),
            phrase: recovery_phrase.map(Zeroizing::new),
        },
    };
    let outcome = state.setup.run(req, &pubkey, now).await.map_err(map_error)?;
    let (token, expires_at) = state.auth.issue_session(&pubkey, now)?;
    Ok((
        StatusCode::CREATED,
        Json(SetupResponse {
            token,
            expires_at,
            pubkey,
            recovery_phrase: outcome.generated_phrase.map(|p| p.to_string()),
        }),
    ))
}

/// `GET /api/setup/progress`
pub async fn progress(State(state): State<AppState>, _auth: Auth) -> Json<SetupProgress> {
    Json(state.setup.progress().await)
}

/// `POST /api/recovery-phrase` (re-authentication required): reveals the phrase.
pub async fn reveal(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    reauthenticate(&state, &auth, &headers, "POST", "/api/recovery-phrase", &body)?;
    let phrase = state
        .setup
        .recovery_phrase()
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("no recovery phrase stored"))?;
    audit(&state, "recovery_phrase_revealed", json!({"by": auth.require_owner()?}), unix_now())
        .await;
    Ok(Json(json!({"recovery_phrase": phrase.as_str()})))
}
