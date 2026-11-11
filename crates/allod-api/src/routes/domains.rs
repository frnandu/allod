//! Domains, DNS providers, gateways and their addresses (SPEC §12.1, §12.7, §12.8).

use std::sync::Arc;

use allod_core::domains::lnvps::FiatCap;
use allod_core::domains::prism::SplitInput;
use allod_core::domains::{parse_target, DomainError, DomainService};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

impl From<DomainError> for ApiError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::BadRequest(m) => ApiError::bad_request(m),
            DomainError::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", m),
            DomainError::Conflict(m) => ApiError::conflict(m),
            DomainError::Upstream(m) => ApiError::new(StatusCode::BAD_GATEWAY, "UPSTREAM", m),
            DomainError::Internal(m) => ApiError::internal(m),
        }
    }
}

fn service(state: &AppState) -> Result<&Arc<DomainService>, ApiError> {
    state.domains.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::CONFLICT, "NOT_SUPPORTED", "domains are not available here")
    })
}

async fn view(svc: &DomainService) -> Result<Json<Value>, ApiError> {
    Ok(Json(svc.view().await?))
}

/// A field that may be absent (keep), `null` (clear) or a value.
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `GET /api/domains` → domains, DNS providers, gateways.
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    view(service(&state)?).await
}

/// Owner's NIP-05 mappings.
pub async fn nip05_list(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({"names": service(&state)?.nip05_list().await?})))
}

#[derive(Deserialize)]
pub struct NewNip05 {
    domain_id: i64,
    username: String,
    npub: String,
    account_id: Option<i64>,
}

pub async fn nip05_add(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewNip05>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.nip05_add_for_account(req.domain_id, &req.username, &req.npub, req.account_id).await?;
    audit(
        &state,
        "nip05_added",
        json!({"domain_id": req.domain_id, "username": req.username, "account_id": req.account_id}),
        unix_now(),
    )
    .await;
    Ok(Json(json!({"names": svc.nip05_list().await?})))
}

#[derive(Deserialize)]
pub struct Nip05Patch {
    domain_id: Option<i64>,
    username: Option<String>,
    npub: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    account_id: Option<Option<i64>>,
}

pub async fn nip05_update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<Nip05Patch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    if req.domain_id.is_none()
        && req.username.is_none()
        && req.npub.is_none()
        && req.account_id.is_none()
    {
        return Err(ApiError::bad_request("provide a field to change"));
    }
    let svc = service(&state)?;
    svc.nip05_update(
        id,
        req.domain_id,
        req.username.as_deref(),
        req.npub.as_deref(),
        req.account_id,
    )
    .await?;
    audit(&state, "nip05_changed", json!({"id": id, "domain_id": req.domain_id, "username": req.username, "account_id": req.account_id}), unix_now()).await;
    Ok(Json(json!({"names": svc.nip05_list().await?})))
}

pub async fn nip05_remove(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    auth.require_full()?;
    service(&state)?.nip05_remove(id).await?;
    audit(&state, "nip05_removed", json!({"id": id}), unix_now()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/domains/check`: reconcile now.
pub async fn check(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.reconcile().await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct NewDomain {
    name: String,
    dns_provider_id: Option<i64>,
    gateway_id: Option<i64>,
    #[serde(default)]
    direct: bool,
}

/// `POST /api/domains {name, dns_provider_id?, gateway_id?, direct?}`.
pub async fn add(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewDomain>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let id = svc.add_domain(&req.name, req.dns_provider_id, req.gateway_id, req.direct).await?;
    audit(&state, "domain_added", json!({"id": id, "name": req.name}), unix_now()).await;
    svc.reconcile().await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct DomainPatch {
    #[serde(default, deserialize_with = "double_option")]
    dns_provider_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double_option")]
    gateway_id: Option<Option<i64>>,
    direct: Option<bool>,
}

/// `PATCH /api/domains/{id} {dns_provider_id?, gateway_id?, direct?}` (`null` clears).
pub async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<DomainPatch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.update_domain(id, req.dns_provider_id, req.gateway_id, req.direct).await?;
    audit(&state, "domain_changed", json!({"id": id}), unix_now()).await;
    svc.reconcile().await;
    view(svc).await
}

/// `DELETE /api/domains/{id}`: its addresses and Allod's records go too.
pub async fn remove(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.remove_domain(id).await?;
    audit(&state, "domain_removed", json!({"id": id}), unix_now()).await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct NewAddress {
    username: String,
    /// `main` or an account id.
    target: String,
    /// What it answers (both when left out).
    lnurl: Option<bool>,
    bip353: Option<bool>,
}

/// `POST /api/domains/{id}/addresses {username, target}`.
pub async fn add_address(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<NewAddress>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    if req.lnurl == Some(false) && req.bip353 == Some(false) {
        return Err(ApiError::bad_request("an address needs LNURL, BIP-353 or both"));
    }
    let a = svc.add_address(id, &req.username, parse_target(&req.target)?).await?;
    if req.lnurl.is_some() || req.bip353.is_some() {
        svc.set_address_kinds(a.id, req.lnurl, req.bip353).await?;
    }
    audit(
        &state,
        "lightning_address_added",
        json!({"domain_id": id, "username": a.username, "target": req.target}),
        unix_now(),
    )
    .await;
    svc.reconcile().await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct AddressPatch {
    domain_id: Option<i64>,
    username: Option<String>,
    lnurl: Option<bool>,
    bip353: Option<bool>,
    /// `main` or an account id: the account it pays into.
    target: Option<String>,
}

/// `PATCH /api/addresses/{id} {lnurl?, target?}`: BIP-353 only or LNURL too (on a served
/// domain), and which account it pays into.
pub async fn update_address(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<AddressPatch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    if req.domain_id.is_some() || req.username.is_some() {
        let current = svc.address_by_id(id).await?;
        svc.rename_address(
            id,
            req.domain_id.unwrap_or(current.domain_id),
            req.username.as_deref().unwrap_or(&current.username),
        )
        .await?;
    }
    if let Some(t) = req.target.as_deref() {
        svc.set_address_target(id, parse_target(t)?).await?;
    }
    if req.lnurl.is_some() || req.bip353.is_some() {
        svc.set_address_kinds(id, req.lnurl, req.bip353).await?;
    }
    audit(
        &state,
        "lightning_address_changed",
        json!({"id": id, "domain_id": req.domain_id, "username": req.username, "lnurl": req.lnurl, "bip353": req.bip353, "target": req.target}),
        unix_now(),
    )
    .await;
    svc.reconcile().await;
    view(svc).await
}

/// `DELETE /api/addresses/{id}`.
pub async fn remove_address(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.remove_address(id).await?;
    audit(&state, "lightning_address_removed", json!({"id": id}), unix_now()).await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct PrismSplitInput {
    /// The stored split being edited. Omitted: a split with this destination is kept, or a
    /// new one is added.
    #[serde(default)]
    id: Option<i64>,
    destination: String,
    /// 1..10000; the rest of the payment stays in this wallet.
    percent_bps: i64,
    /// The owner's own note: what or who the destination is. Omitted: keep the stored one.
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
pub struct PrismPatch {
    splits: Vec<PrismSplitInput>,
}

/// `GET /api/addresses/{id}/prism`: its splits and what they forwarded so far.
pub async fn get_prism(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(service(&state)?.prism_view(id).await?))
}

#[derive(Deserialize)]
pub struct PayoutHistoryQuery {
    /// The id of the oldest payout already shown; page before it.
    before: Option<i64>,
    /// 1..100, 20 by default.
    limit: Option<i64>,
}

/// `GET /api/addresses/{id}/prism/payouts?before=…&limit=…`: every payout recorded for
/// the address, newest first.
pub async fn prism_payouts(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<i64>,
    Query(q): Query<PayoutHistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(service(&state)?.prism_payout_history(id, q.before, q.limit.unwrap_or(20)).await?))
}

/// `PUT /api/addresses/{id}/prism` with
/// `{splits: [{id?, destination, percent_bps, note?}]}`: replaces them. Sending the id of
/// a stored split edits it in place, so its note and what it failed to send stay with it
/// (also when its destination changes).
pub async fn set_prism(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<PrismPatch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let list: Vec<SplitInput> = req
        .splits
        .into_iter()
        .map(|s| SplitInput {
            id: s.id,
            destination: s.destination,
            percent_bps: s.percent_bps,
            note: s.note,
        })
        .collect();
    let splits = svc.set_prism_splits_inputs(id, &list).await?;
    audit(
        &state,
        "prism_splits_changed",
        json!({"address_id": id, "splits": splits.iter().map(|s| json!({
            "destination": s.destination, "percent_bps": s.percent_bps,
        })).collect::<Vec<_>>()}),
        unix_now(),
    )
    .await;
    // A payment that arrived while the splits were being edited is split by what is saved.
    svc.wake_prism();
    view(svc).await
}

/// `GET /api/prism/failures`: every address whose splits could not be paid, for warnings.
pub async fn prism_failures(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(service(&state)?.prism_failures().await?))
}

#[derive(Deserialize)]
pub struct ResetFailed {
    split_id: i64,
}

/// `POST /api/addresses/{id}/prism/reset` with `{split_id}`: clears what that split failed
/// to send. Nothing is paid; the payouts stop counting and are never retried.
pub async fn reset_prism_failed(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<ResetFailed>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let cleared = svc.prism_reset_failed(id, req.split_id).await?;
    audit(
        &state,
        "prism_failed_cleared",
        json!({"address_id": id, "split_id": req.split_id, "payouts": cleared}),
        unix_now(),
    )
    .await;
    view(svc).await
}

/// `POST /api/addresses/{id}/prism/retry`: pays failed splits again now.
pub async fn retry_prism(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let retried = svc.prism_retry(id).await?;
    audit(
        &state,
        "prism_splits_retried",
        json!({"address_id": id, "payouts": retried}),
        unix_now(),
    )
    .await;
    svc.wake_prism();
    view(svc).await
}

#[derive(Deserialize)]
pub struct NewProvider {
    kind: String,
    label: Option<String>,
    token: String,
}

/// `POST /api/dns-providers {kind, label?, token}`.
pub async fn add_provider(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewProvider>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let id = svc.add_provider(&req.kind, req.label.as_deref(), &req.token).await?;
    audit(&state, "dns_provider_added", json!({"id": id, "kind": req.kind}), unix_now()).await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct ProviderPatch {
    label: Option<String>,
    token: Option<String>,
}

/// `PATCH /api/dns-providers/{id} {label?, token?}`.
pub async fn update_provider(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<ProviderPatch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.update_provider(id, req.label.as_deref(), req.token.as_deref()).await?;
    audit(&state, "dns_provider_changed", json!({"id": id}), unix_now()).await;
    svc.reconcile().await;
    view(svc).await
}

/// `DELETE /api/dns-providers/{id}`.
pub async fn remove_provider(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.remove_provider(id).await?;
    audit(&state, "dns_provider_removed", json!({"id": id}), unix_now()).await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct NewGateway {
    /// `manual` (the owner's server) or `lnvps` (rented by Allod).
    provider: String,
    label: Option<String>,
    ipv4: Option<String>,
    mode: Option<String>,
    /// LNVPS: most the first month may cost.
    max_sats: Option<u64>,
    #[serde(default)]
    auto_renew: bool,
    renew_max_sats: Option<u64>,
    /// LNVPS: a cap in a fiat currency (VAT and fees included) instead of the sats caps.
    cap: Option<CapBody>,
}

#[derive(Deserialize)]
pub struct CapBody {
    currency: String,
    amount: f64,
}

impl From<CapBody> for FiatCap {
    fn from(c: CapBody) -> Self {
        FiatCap { currency: c.currency, amount: c.amount }
    }
}

/// `POST /api/gateways {provider: manual, ipv4, mode?, label?}` or
/// `{provider: lnvps, max_sats, auto_renew?, renew_max_sats?, label?}`.
pub async fn add_gateway(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewGateway>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let id = match req.provider.as_str() {
        "manual" => {
            let ip =
                req.ipv4.as_deref().ok_or_else(|| ApiError::bad_request("ipv4 is required"))?;
            svc.add_gateway(req.label.as_deref(), ip, req.mode.as_deref().unwrap_or("dedicated"))
                .await?
        }
        "lnvps" => {
            let cap = req.cap.map(FiatCap::from);
            let max = match (req.max_sats, &cap) {
                (Some(m), _) => m,
                (None, Some(_)) => 0,
                (None, None) => return Err(ApiError::bad_request("max_sats or cap is required")),
            };
            if req.auto_renew && req.renew_max_sats.is_none() && cap.is_none() {
                return Err(ApiError::bad_request("automatic renewal needs renew_max_sats or cap"));
            }
            svc.lnvps_buy(req.label.as_deref(), max, req.auto_renew, req.renew_max_sats, cap)
                .await?
        }
        _ => return Err(ApiError::bad_request("provider is manual or lnvps")),
    };
    audit(&state, "gateway_added", json!({"id": id, "provider": req.provider}), unix_now()).await;
    view(svc).await
}

#[derive(Deserialize)]
pub struct GatewayPatch {
    label: Option<String>,
    ipv4: Option<String>,
    mode: Option<String>,
    auto_renew: Option<bool>,
    renew_max_sats: Option<i64>,
    /// LNVPS fiat cap; `null` goes back to the sats caps.
    #[serde(default, deserialize_with = "double_option")]
    cap: Option<Option<CapBody>>,
}

/// `PATCH /api/gateways/{id}`.
pub async fn update_gateway(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<GatewayPatch>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.update_gateway(
        id,
        req.label.as_deref(),
        req.ipv4.as_deref(),
        req.mode.as_deref(),
        req.auto_renew,
        req.renew_max_sats,
    )
    .await?;
    if let Some(cap) = req.cap {
        svc.lnvps_set_cap(id, cap.map(FiatCap::from)).await?;
    }
    audit(&state, "gateway_changed", json!({"id": id}), unix_now()).await;
    svc.reconcile().await;
    view(svc).await
}

/// `DELETE /api/gateways/{id}`.
pub async fn remove_gateway(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.remove_gateway(id).await?;
    audit(&state, "gateway_removed", json!({"id": id}), unix_now()).await;
    view(svc).await
}

/// `GET /api/gateways/{id}/setup`: the setup script (it holds the gateway's private
/// WireGuard key).
pub async fn setup_script(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    auth.require_full()?;
    let script = service(&state)?.setup_script(id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        script,
    )
        .into_response())
}

/// `GET /api/gateways/lnvps/quote`: what a gateway at LNVPS costs now.
pub async fn lnvps_quote(
    State(state): State<AppState>,
    _auth: Auth,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(service(&state)?.lnvps_quote().await?))
}

/// `POST /api/dns-providers/desec/captcha`: a captcha from deSEC for the owner,
/// `{id, challenge}` (base64 PNG).
pub async fn desec_captcha(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(service(&state)?.desec_captcha().await?))
}

#[derive(Deserialize)]
pub struct SignupBody {
    captcha_id: String,
    solution: String,
}

/// `POST /api/dns-providers/desec/signup {captcha_id, solution}`: Allod registers its
/// own deSEC account; activation and the token follow in the background.
pub async fn desec_signup(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<SignupBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.desec_signup(&req.captcha_id, &req.solution).await?;
    audit(&state, "desec_signup", json!({}), unix_now()).await;
    view(svc).await
}

/// `GET /api/dns-providers/desec/credentials`: the deSEC account's email and password,
/// for logging in at desec.io.
pub async fn desec_credentials(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(service(&state)?.desec_credentials().await?))
}

/// `POST /api/dns-providers/njalla/signup`: Allod signs up at Njalla with its own
/// address; confirmation and the API token follow in the background.
pub async fn njalla_signup(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.njalla_signup().await?;
    audit(&state, "njalla_signup", json!({}), unix_now()).await;
    view(svc).await
}

/// `GET /api/dns-providers/njalla/credentials`: the Njalla account's email and password.
pub async fn njalla_credentials(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(service(&state)?.njalla_credentials().await?))
}

#[derive(Deserialize)]
pub struct NjallaSearchQuery {
    provider_id: i64,
    q: String,
}

/// `GET /api/njalla/search?provider_id=&q=`: names at Njalla with status and price.
pub async fn njalla_search(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<NjallaSearchQuery>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    Ok(Json(service(&state)?.njalla_search(q.provider_id, &q.q).await?))
}

#[derive(Deserialize)]
pub struct NjallaBuyBody {
    provider_id: i64,
    domain: String,
    years: Option<u32>,
    gateway_id: Option<i64>,
    #[serde(default)]
    direct: bool,
}

/// `POST /api/njalla/buy {provider_id, domain, years?, gateway_id?, direct?}`: starts
/// buying a domain; the view's `njalla_purchase` carries the top-up to pay, if any.
pub async fn njalla_buy(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NjallaBuyBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.njalla_buy(
        req.provider_id,
        &req.domain,
        req.years.unwrap_or(1),
        req.gateway_id,
        req.direct,
    )
    .await?;
    audit(&state, "njalla_buy", json!({"domain": req.domain}), unix_now()).await;
    view(svc).await
}

/// `DELETE /api/njalla/buy`: stops waiting for the top-up (money sent stays in the Njalla
/// wallet), or forgets a finished purchase.
pub async fn njalla_buy_cancel(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.njalla_buy_cancel().await?;
    view(svc).await
}

#[derive(Deserialize)]
pub struct EmailBody {
    email: String,
}

/// `PUT /api/gateways/lnvps/email {email}`.
pub async fn lnvps_email(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<EmailBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.lnvps_set_email(&req.email).await?;
    view(svc).await
}

#[derive(Deserialize)]
pub struct AskQuery {
    domain: String,
}

/// `GET /api/internal/tls-ask?domain=`: Caddy's on-demand TLS check. 200 only for a
/// served domain; Caddy blocks `/api/internal/*` on its public sites.
pub async fn tls_ask(State(state): State<AppState>, Query(q): Query<AskQuery>) -> StatusCode {
    match &state.domains {
        Some(svc) if svc.allows_certificate(&q.domain).await => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}
