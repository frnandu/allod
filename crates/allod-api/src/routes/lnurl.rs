//! LNURL-pay for Lightning addresses on the own domain (SPEC §12): public, no auth.
//! Answers only for the domain in the `Host` header and its usernames.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::state::AppState;
use allod_core::domains::lnurl::LnurlFailure;

/// Bound decoding and allocation before callback parameter validation.
const MAX_CALLBACK_QUERY_BYTES: usize = 24 * 1024;

fn host(headers: &HeaderMap) -> String {
    headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default().to_string()
}

/// Web wallets call these from other origins (LUD-01).
fn answer(r: Result<Value, LnurlFailure>) -> Response {
    let (status, body) = match r {
        Ok(v) => (StatusCode::OK, v),
        Err(f) => (StatusCode::from_u16(f.status).unwrap_or(StatusCode::BAD_REQUEST), f.body()),
    };
    let mut res = (status, Json(body)).into_response();
    res.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    res
}

fn unavailable() -> Response {
    answer(Err(LnurlFailure { status: 404, reason: "no such Lightning address".into() }))
}

pub(crate) fn too_many() -> Response {
    answer(Err(LnurlFailure { status: 429, reason: "too many requests".into() }))
}

/// `GET /.well-known/lnurlp/{username}`
pub async fn pay_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Response {
    let Some(svc) = &state.domains else { return unavailable() };
    answer(svc.lnurl_pay_request(&host(&headers), &username).await)
}

/// `GET /lnurlp/{username}/callback?amount=&comment=&payerdata=&nostr=`
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    uri: Uri,
) -> Response {
    let Some(svc) = &state.domains else { return unavailable() };
    if uri.query().is_some_and(|q| q.len() > MAX_CALLBACK_QUERY_BYTES) {
        return answer(Err(LnurlFailure {
            status: 400,
            reason: "callback query is too large".into(),
        }));
    }
    let Query(q) = match Query::<HashMap<String, String>>::try_from_uri(&uri) {
        Ok(q) => q,
        Err(_) => {
            return answer(Err(LnurlFailure {
                status: 400,
                reason: "callback query is invalid".into(),
            }));
        }
    };
    answer(svc.lnurl_callback(&host(&headers), &username, &q).await)
}

/// `GET /lnurlp/{username}/verify/{payment_hash}` (LUD-21)
pub async fn verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((username, hash)): Path<(String, String)>,
) -> Response {
    let Some(svc) = &state.domains else { return unavailable() };
    answer(svc.lnurl_verify(&host(&headers), &username, &hash).await)
}
