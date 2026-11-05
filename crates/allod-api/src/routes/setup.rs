use allod_core::{SetupError, SetupRequest};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use zeroize::Zeroizing;

use crate::auth::{expected_urls, nip98, reauthenticate, unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

const SETUP_COOKIE: &str = "allod_setup";

#[derive(Deserialize)]
pub struct ClaimBody {
    token: String,
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(header::COOKIE)?.to_str().ok()?.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

/// Exchanges host-only bootstrap capability for short-lived browser claim cookie.
pub async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ClaimBody>,
) -> Result<(StatusCode, HeaderMap), ApiError> {
    if state.setup.check_complete().await {
        return Err(ApiError::conflict("setup is already complete"));
    }
    if !state.setup.verify_bootstrap_token(&body.token).map_err(map_error)? {
        return Err(ApiError::unauthorized("invalid or expired setup link"));
    }
    new_claim(&state, &headers)
}

fn new_claim(state: &AppState, headers: &HeaderMap) -> Result<(StatusCode, HeaderMap), ApiError> {
    let fingerprint = state.setup.bootstrap_fingerprint().map_err(map_error)?;
    let token = state.auth.new_setup_claim(unix_now(), fingerprint);
    let secure = state.public_url.as_deref().is_some_and(|url| url.starts_with("https://"))
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    let mut value = format!(
        "{SETUP_COOKIE}={token}; Path=/api/setup; HttpOnly; SameSite=Strict; Max-Age={}",
        crate::auth::SETUP_CLAIM_SECS
    );
    if secure {
        value.push_str("; Secure");
    }
    let mut out = HeaderMap::new();
    out.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&value)
            .map_err(|_| ApiError::internal("could not create setup claim"))?,
    );
    Ok((StatusCode::NO_CONTENT, out))
}

/// Reports whether this browser still holds valid setup claim.
pub async fn claim_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Result<(StatusCode, HeaderMap), ApiError> {
    let fingerprint = state.setup.bootstrap_fingerprint().map_err(map_error)?;
    if cookie(&headers, SETUP_COOKIE)
        .is_some_and(|token| state.auth.has_setup_claim(&token, &fingerprint, unix_now()))
    {
        return Ok((StatusCode::NO_CONTENT, HeaderMap::new()));
    }
    if let (Some(proxy), Some(Extension(ConnectInfo(source)))) =
        (&state.trusted_setup_proxy, connect)
    {
        if proxy.contains(source.ip()).await {
            return new_claim(&state, &headers);
        }
    }
    Err(ApiError::unauthorized("setup link required"))
}

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
) -> Result<(StatusCode, HeaderMap, Json<SetupResponse>), ApiError> {
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
    let claim = cookie(&headers, SETUP_COOKIE)
        .ok_or_else(|| ApiError::unauthorized("open setup using host-generated setup link"))?;
    let fingerprint = state.setup.bootstrap_fingerprint().map_err(map_error)?;
    if !state.auth.authorize_setup_claim(&claim, &pubkey, &fingerprint, now) {
        return Err(ApiError::unauthorized("setup link expired; generate a new one on host"));
    }
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
    state.auth.consume_setup_claim(&claim);
    let (token, expires_at) = state.auth.issue_session(&pubkey, now)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "allod_setup=; Path=/api/setup; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    Ok((
        StatusCode::CREATED,
        response_headers,
        Json(SetupResponse {
            token,
            expires_at,
            pubkey,
            recovery_phrase: outcome.generated_phrase.map(|p| p.to_string()),
        }),
    ))
}

/// `GET /api/setup/progress`
pub async fn progress(State(state): State<AppState>, _auth: Auth) -> Json<serde_json::Value> {
    let mut value = serde_json::to_value(state.setup.progress().await).unwrap_or_default();
    // Another chain source was picked in Settings: the stack's bitcoind is not the node's.
    if !state.watches_node_bitcoind() {
        value["bitcoind"] = serde_json::Value::Null;
    }
    // The gossip sidecar runs before ldk-server: the setup screen shows its progress.
    if let Some(sidecar) = state.gossip.sidecar().await {
        value["gossip"] = serde_json::to_value(sidecar).unwrap_or_default();
    }
    Json(value)
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

/// `GET /api/backup`: whether the recovery phrase backup was checked (SPEC §13.3).
pub async fn backup_status(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth.require_owner()?;
    let verified = state.setup.backup_verified().await.map_err(map_error)?;
    Ok(Json(json!({"verified": verified})))
}

#[derive(Deserialize)]
pub struct VerifyBackupBody {
    /// 0-based word position → word, as the owner typed it.
    words: std::collections::BTreeMap<usize, String>,
}

/// `POST /api/backup/verify`: checks words from the owner's paper copy and marks the
/// backup verified when they match.
pub async fn verify_backup(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<VerifyBackupBody>,
) -> Result<StatusCode, ApiError> {
    let owner = auth.require_owner()?.to_string();
    if !state.setup.verify_backup(&body.words).await.map_err(map_error)? {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WRONG_WORDS",
            "the words do not match",
        ));
    }
    audit(&state, "backup_verified", json!({"by": owner}), unix_now()).await;
    Ok(StatusCode::NO_CONTENT)
}
