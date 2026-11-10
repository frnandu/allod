//! Public NIP-05 discovery, scoped by Host to a served own domain.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::AppState;

#[derive(Deserialize)]
pub struct Lookup {
    name: Option<String>,
}

pub async fn lookup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Lookup>,
) -> Response {
    let Some(name) = q.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    let Some(svc) = &state.domains else { return StatusCode::NOT_FOUND.into_response() };
    let body = match svc.nip05_names(host, name).await {
        Ok(Some(body)) => body,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "NIP-05 lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}
