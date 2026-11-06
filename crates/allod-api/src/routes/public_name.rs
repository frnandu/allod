//! Public name: deSEC signup, own domain, exposure, BIP-353 records (SPEC §12.1).

use std::sync::Arc;

use allod_core::public_name::{DesecError, Exposure, PublicNameError, PublicNameService};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{reauthenticate, unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

impl From<PublicNameError> for ApiError {
    fn from(e: PublicNameError) -> Self {
        match e {
            PublicNameError::BadRequest(m) => ApiError::bad_request(m),
            PublicNameError::Conflict(m) => ApiError::conflict(m),
            PublicNameError::Desec(DesecError::Captcha) => ApiError::new(
                StatusCode::BAD_REQUEST,
                "CAPTCHA",
                "the captcha answer was not accepted",
            ),
            PublicNameError::Desec(DesecError::Denied(m)) => {
                ApiError::new(StatusCode::FORBIDDEN, "REGISTRATION_DENIED", m)
            }
            PublicNameError::Desec(DesecError::Throttled(s)) => {
                ApiError::too_many(format!("deSEC asks to wait {s} s before trying again"))
            }
            PublicNameError::Desec(e) => {
                ApiError::new(StatusCode::BAD_GATEWAY, "DESEC", e.to_string())
            }
            PublicNameError::Internal(m) => ApiError::internal(m),
        }
    }
}

fn service(state: &AppState) -> Result<&Arc<PublicNameService>, ApiError> {
    state.public_name.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::CONFLICT, "NOT_SUPPORTED", "public names are not available here")
    })
}

/// `GET /api/public-name`. While a deSEC signup waits for its email link, each call
/// also checks (at most every 15 s) whether the owner clicked it.
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let svc = service(&state)?;
    svc.poll_activation().await;
    Ok(Json(svc.view()))
}

/// `POST /api/public-name/refresh`: reconcile now (after fixing DNS or a port forward).
pub async fn refresh(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.reconcile().await;
    Ok(Json(svc.view()))
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ModeReq {
    None,
    Own,
    Desec,
}

#[derive(Deserialize)]
pub struct PutBody {
    mode: ModeReq,
    name: Option<String>,
    exposure: Option<Exposure>,
}

/// `PUT /api/public-name {mode: none|own|desec, name?, exposure?}`. `own` sets the
/// owner's domain; `desec` changes the exposure, or uses the deSEC account Allod signed
/// up earlier again; `none` turns the public name off.
pub async fn put(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let current = svc.view();
    match req.mode {
        ModeReq::None => svc.disable().await?,
        ModeReq::Own => {
            let name = req.name.ok_or_else(|| ApiError::bad_request("name is required"))?;
            svc.use_own(&name, req.exposure.unwrap_or_default()).await?;
        }
        ModeReq::Desec => {
            if current["mode"] != "desec" {
                svc.resume_desec().await?;
            }
            if let Some(e) = req.exposure {
                svc.set_exposure(e).await?;
            }
        }
    }
    let view = svc.view();
    audit(
        &state,
        "public_name_changed",
        json!({"mode": view["mode"], "name": view["name"], "exposure": view["exposure"]}),
        unix_now(),
    )
    .await;
    Ok(Json(view))
}

/// `POST /api/public-name/desec/captcha` → `{id, challenge}` (base64 PNG).
pub async fn captcha(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let c = service(&state)?.captcha().await?;
    Ok(Json(json!({"id": c.id, "challenge": c.challenge})))
}

#[derive(Deserialize)]
pub struct CheckQuery {
    label: String,
}

/// `GET /api/public-name/desec/available?label=` → `{available: true|false|null}`.
pub async fn available(
    State(state): State<AppState>,
    _auth: Auth,
    Query(q): Query<CheckQuery>,
) -> Result<Json<Value>, ApiError> {
    service(&state)?;
    let label = q.label.trim().to_ascii_lowercase();
    if !allod_core::public_name::valid_label(&label) {
        return Ok(Json(json!({"label": label, "valid": false, "available": false})));
    }
    let available = allod_core::public_name::label_available(&label).await;
    Ok(Json(json!({"label": label, "valid": true, "available": available})))
}

#[derive(Deserialize)]
pub struct SignupBody {
    label: String,
    email: String,
    captcha_id: String,
    captcha_solution: String,
}

/// `POST /api/public-name/desec/signup`: creates the owner's deSEC account with
/// `<label>.dedyn.io`; deSEC then emails an activation link.
pub async fn signup(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<SignupBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.signup(&req.label, &req.email, (&req.captcha_id, req.captcha_solution.trim())).await?;
    audit(&state, "public_name_signup", json!({"label": req.label}), unix_now()).await;
    Ok(Json(svc.view()))
}

#[derive(Deserialize)]
pub struct TokenBody {
    name: String,
    token: String,
}

/// `POST /api/public-name/desec/token`: an existing deSEC account's domain and token.
pub async fn token(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<TokenBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.use_token(&req.name, &req.token).await?;
    let view = svc.view();
    audit(
        &state,
        "public_name_changed",
        json!({"mode": "desec", "name": view["name"]}),
        unix_now(),
    )
    .await;
    Ok(Json(view))
}

/// `POST /api/public-name/desec/credentials`: the deSEC login Allod created. Needs a
/// fresh NIP-98 signature, like the recovery phrase (SPEC §11.1).
pub async fn credentials(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    reauthenticate(&state, &auth, &headers, "POST", "/api/public-name/desec/credentials", &body)?;
    let (email, password) = service(&state)?
        .desec_credentials()
        .ok_or_else(|| ApiError::not_found("Allod did not create a deSEC account"))?;
    audit(&state, "desec_credentials_revealed", json!({"by": auth.require_owner()?}), unix_now())
        .await;
    Ok(Json(json!({"email": email, "password": password.as_str()})))
}

#[derive(Deserialize)]
pub struct AskQuery {
    domain: String,
}

/// `GET /api/internal/tls-ask?domain=`: Caddy's on-demand TLS check. 200 only for the
/// current public name; Caddy blocks `/api/internal/*` on its public sites.
pub async fn tls_ask(State(state): State<AppState>, Query(q): Query<AskQuery>) -> StatusCode {
    match &state.public_name {
        Some(svc) if svc.allows_certificate(&q.domain) => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}
