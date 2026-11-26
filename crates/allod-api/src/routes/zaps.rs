//! Sending zaps: NIP-57 to a Lightning address or LNURL, NIP-B1 to a BOLT12 offer. The
//! browser signs public zaps with the owner's signer; anonymous ones are signed here with
//! a throwaway key.

use allod_lnurl::zap;
use allod_nwc::zaps::Bolt12Zap;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use nostr::prelude::*;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::contacts::contacts;
use crate::routes::wallet::wallet;
use crate::AppState;

/// The secret anonymous zap keys are derived from.
fn secret(state: &AppState) -> Result<String, ApiError> {
    state
        .apps
        .get()
        .and_then(|a| a.zap_key().ok())
        .map(|k| k.expose_secret_hex().to_string())
        .ok_or_else(|| {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "NOT_READY", "zaps are not ready yet")
        })
}

#[derive(Deserialize)]
pub struct Check {
    payment: String,
}

/// `POST /api/zaps/check {payment}` → `{zaps}`: whether a Lightning address or LNURL
/// accepts NIP-57 zaps.
pub async fn check(
    State(state): State<AppState>,
    _auth: Auth,
    Json(req): Json<Check>,
) -> Result<Json<Value>, ApiError> {
    let params = wallet(&state)?.lnurl_params(req.payment.trim()).await?;
    Ok(Json(json!({"zaps": params.allows_nostr})))
}

#[derive(Deserialize)]
pub struct Prepare {
    /// The recipient (hex).
    pubkey: String,
    /// The Lightning address, LNURL or BOLT12 offer that will be paid.
    payment: String,
    amount_msat: u64,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    anonymous: bool,
}

/// `POST /api/zaps` → `{template}` to sign and send as `zap` with the payment, or for an
/// anonymous zap `{event}`, already signed.
pub async fn prepare(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<Prepare>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    if PublicKey::from_hex(&req.pubkey).is_err() {
        return Err(ApiError::bad_request("pubkey must be a hex public key"));
    }
    if req.comment.chars().count() > zap::COMMENT_MAX {
        return Err(ApiError::bad_request("the zap comment is limited to 280 characters"));
    }
    if req.amount_msat == 0 || req.amount_msat > i64::MAX as u64 {
        return Err(ApiError::bad_request("enter an amount to zap"));
    }
    let contacts = contacts(&state)?;
    let mut random = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let zap_id = hex::encode(random);
    let now = unix_now().max(0) as u64;
    let payment = req.payment.trim();
    let template = if payment.to_ascii_lowercase().starts_with("lno1") {
        let profile = contacts.profiles(std::slice::from_ref(&req.pubkey), false).await.pop();
        let announcement = profile
            .filter(|p| {
                p.targets.iter().any(|t| t.zappable && t.value.eq_ignore_ascii_case(payment))
            })
            .and_then(|p| p.offer_event)
            .ok_or_else(|| {
                ApiError::bad_request("the recipient has not announced this offer for zaps")
            })?;
        zap::intent_template(
            &req.pubkey,
            req.amount_msat,
            &req.comment,
            &announcement,
            &zap_id,
            now,
        )
    } else {
        let relays = contacts.zap_relays(&req.pubkey).await;
        zap::request_template(
            &req.pubkey,
            req.amount_msat,
            &req.comment,
            &relays,
            req.anonymous,
            now,
        )
    };
    if !req.anonymous {
        return Ok(Json(json!({"template": template})));
    }
    let keys = zap::anonymous_keys(&secret(&state)?, &zap_id).map_err(ApiError::internal)?;
    let event = template.sign(&keys).map_err(ApiError::internal)?;
    Ok(Json(json!({"event": event})))
}

#[derive(Deserialize)]
pub struct Receipt {
    /// The signed kind 9736, once the payer signed the template.
    event: Option<Value>,
}

/// `POST /api/zaps/{transaction}/receipt {event?}`: publishes the zap of a paid BOLT12
/// zap → `{status, template?}`; status `sign` asks for the template to be signed.
pub async fn receipt(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<Receipt>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let wallet = wallet(&state)?;
    wallet.transaction(&id).await?;
    let signed = req.event.map(|e| e.to_string());
    let outcome = allod_nwc::zaps::bolt12_zap(&wallet, &secret(&state)?, &id, signed.as_deref())
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(match outcome {
        Bolt12Zap::NotAZap => json!({"status": "none"}),
        Bolt12Zap::Waiting => json!({"status": "waiting"}),
        Bolt12Zap::NoProof => json!({"status": "no_proof"}),
        Bolt12Zap::Sign(template) => json!({"status": "sign", "template": template}),
        Bolt12Zap::NotAccepted => json!({"status": "not_accepted"}),
        Bolt12Zap::Published(event_id) => json!({"status": "published", "event_id": event_id}),
    }))
}
