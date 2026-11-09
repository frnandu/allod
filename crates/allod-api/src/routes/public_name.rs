//! Lightning addresses on third-party services: twelve.cash and clank.money names, ZEUS
//! Pay and Alby (SPEC §12.3–§12.6). Own domains are in `routes::domains`.

use std::sync::Arc;

use allod_core::public_name::{PublicNameError, PublicNameService};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::error::ApiError;
use crate::routes::auth::audit;
use crate::AppState;

impl From<PublicNameError> for ApiError {
    fn from(e: PublicNameError) -> Self {
        match e {
            PublicNameError::BadRequest(m) => ApiError::bad_request(m),
            PublicNameError::Conflict(m) => ApiError::conflict(m),
            PublicNameError::Upstream(m) => ApiError::new(StatusCode::BAD_GATEWAY, "UPSTREAM", m),
            PublicNameError::Internal(m) => ApiError::internal(m),
        }
    }
}

fn service(state: &AppState) -> Result<&Arc<PublicNameService>, ApiError> {
    state.public_name.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::CONFLICT, "NOT_SUPPORTED", "public names are not available here")
    })
}

/// `GET /api/public-name`.
pub async fn get(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let svc = service(&state)?;
    Ok(Json(full_view(&state, svc).await))
}

/// The view with which services are available on this network. Every route that
/// answers the view uses it, so the page never loses the flags after a change.
async fn full_view(state: &AppState, svc: &PublicNameService) -> Value {
    let mut view = svc.view();
    view["twelve_cash_available"] =
        require_mainnet(state, svc.twelve_any_network(), "twelve.cash").await.is_ok().into();
    view["clank_available"] =
        require_mainnet(state, svc.clank_any_network(), "clank.money").await.is_ok().into();
    view["zeus_available"] =
        require_mainnet(state, svc.zeus_any_network(), "ZEUS Pay").await.is_ok().into();
    view["alby_available"] =
        require_mainnet(state, svc.alby_any_network(), "Alby").await.is_ok().into();
    view
}

/// `POST /api/public-name/refresh`: reconcile now.
pub async fn refresh(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.reconcile().await;
    Ok(Json(full_view(&state, svc).await))
}

// ------------------------------------------------------------- twelve.cash (§12.3)

fn target(raw: &str) -> Result<Option<i64>, ApiError> {
    match raw {
        "main" => Ok(None),
        id => id
            .parse()
            .map(Some)
            .map_err(|_| ApiError::bad_request("target is main or an account id")),
    }
}

/// twelve.cash and clank.money publish mainnet offers only. `any_network`: a test
/// stand-in is configured.
async fn require_mainnet(state: &AppState, any_network: bool, what: &str) -> Result<(), ApiError> {
    if any_network {
        return Ok(());
    }
    let network = match state.setup.network() {
        Some(n) => Some(n),
        None => match state.ldk.backend().await {
            Ok(b) => b.node_info().await.ok().and_then(|i| i.network),
            Err(_) => None,
        },
    };
    if network != Some(allod_types::Network::Bitcoin) {
        return Err(ApiError::conflict(format!("{what} names work on mainnet only")));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct TwelveBody {
    target: String,
    /// A name bought on twelve.cash, to link instead of registering a free one.
    address: Option<String>,
}

/// `POST /api/public-name/twelve-cash {target, address?}`: registers a free random
/// `name@twelve.cash` for the target's BOLT12 offer, or links a bought one.
pub async fn twelve_add(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<TwelveBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    require_mainnet(&state, svc.twelve_any_network(), "twelve.cash").await?;
    let t = target(&req.target)?;
    let entry = match req.address.as_deref().filter(|a| !a.trim().is_empty()) {
        Some(address) => svc.twelve_link(t, address).await?,
        None => svc.twelve_register(t).await?,
    };
    let address = format!("{}@{}", entry.user_name, entry.domain);
    audit(
        &state,
        "twelve_cash_added",
        json!({"target": req.target, "address": address}),
        unix_now(),
    )
    .await;
    Ok(Json(full_view(&state, svc).await))
}

#[derive(Deserialize)]
pub struct TargetQuery {
    target: String,
    /// The custom name the offer is for (its description).
    address: String,
}

/// `GET /api/public-name/twelve-cash/offer?target=&address=`: the offer to register on
/// the twelve.cash website for a custom name, described with that name.
pub async fn twelve_offer(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<TargetQuery>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let offer = svc.twelve_offer(target(&q.target)?, &q.address).await?;
    Ok(Json(json!({"offer": offer})))
}

/// `DELETE /api/public-name/twelve-cash/{target}`: forgets the address in Allod.
pub async fn twelve_forget(
    State(state): State<AppState>,
    auth: Auth,
    axum::extract::Path(raw): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.twelve_forget(target(&raw)?).await?;
    audit(&state, "twelve_cash_forgotten", json!({"target": raw}), unix_now()).await;
    Ok(Json(full_view(&state, svc).await))
}

// ------------------------------------------------------------- clank.money (§12.4)

#[derive(Deserialize)]
pub struct UsernameQuery {
    username: String,
}

/// `GET /api/public-name/clank/available?username=` → `{valid, available}`
/// (`available` null when clank.money does not say).
pub async fn clank_available(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<UsernameQuery>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let u = q.username.trim().to_ascii_lowercase();
    Ok(Json(match svc.clank_available(&u).await {
        Ok(a) => json!({"valid": true, "available": a}),
        Err(_) => json!({"valid": false, "available": null}),
    }))
}

#[derive(Deserialize)]
pub struct ClankBody {
    target: String,
    username: String,
    /// The most the owner agreed to pay, in sats.
    max_price_sats: u64,
    /// `main` or an account id; default: the target itself.
    pay_from: Option<String>,
}

/// `POST /api/public-name/clank {target, username, max_price_sats, pay_from?}`: buys
/// `username@clank.money` for the target, paid over L402.
pub async fn clank_add(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ClankBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    require_mainnet(&state, svc.clank_any_network(), "clank.money").await?;
    let t = target(&req.target)?;
    let pay_from = match req.pay_from.as_deref() {
        Some(p) => target(p)?,
        None => t,
    };
    let entry = svc.clank_register(t, &req.username, req.max_price_sats, pay_from).await?;
    audit(
        &state,
        "clank_name_bought",
        json!({"target": req.target, "username": entry.user_name, "price_sats": entry.price_sats,
            "state": entry.state}),
        unix_now(),
    )
    .await;
    Ok(Json(full_view(&state, svc).await))
}

/// `DELETE /api/public-name/clank/{target}`: forgets the name (and its management
/// token) in Allod.
pub async fn clank_forget(
    State(state): State<AppState>,
    auth: Auth,
    axum::extract::Path(raw): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.clank_forget(target(&raw)?).await?;
    audit(&state, "clank_name_forgotten", json!({"target": raw}), unix_now()).await;
    Ok(Json(full_view(&state, svc).await))
}

/// `POST /api/public-name/names/check`: looks up the DNS records of twelve.cash and
/// clank.money names not seen yet, now.
pub async fn names_check(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.check_names().await;
    Ok(Json(full_view(&state, svc).await))
}

// ------------------------------------------------------------- ZEUS Pay (§12.5)

#[derive(Deserialize)]
pub struct ZeusBody {
    handle: Option<String>,
    domain: Option<String>,
    /// `main` (default) or an account id: where its payments go.
    target: Option<String>,
}

/// `POST /api/public-name/zeus {handle?, domain?, target?}`: the node's ZEUS Pay LNURL
/// address, paying into Main or an account.
pub async fn zeus_add(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ZeusBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    require_mainnet(&state, svc.zeus_any_network(), "ZEUS Pay").await?;
    let t = target(req.target.as_deref().unwrap_or("main"))?;
    let result = svc.zeus_register(req.handle.as_deref(), req.domain.as_deref(), t).await;
    let view = full_view(&state, svc).await;
    audit(&state, "zeus_pay_added", json!({"address": view["zeus"]["address"]}), unix_now()).await;
    result?;
    Ok(Json(view))
}

/// `PUT /api/public-name/zeus {handle, domain?}`: renames the address.
pub async fn zeus_rename(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ZeusBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let handle = req.handle.ok_or_else(|| ApiError::bad_request("handle is required"))?;
    let domain = req.domain.unwrap_or_else(|| allod_core::public_name::zeus::DOMAINS[0].into());
    svc.zeus_rename(&handle, &domain).await?;
    let view = full_view(&state, svc).await;
    audit(&state, "zeus_pay_renamed", json!({"address": view["zeus"]["address"]}), unix_now())
        .await;
    Ok(Json(view))
}

#[derive(Deserialize)]
pub struct TargetBody {
    target: String,
}

/// `PUT /api/public-name/zeus/target {target}`: where the ZEUS Pay address's payments go
/// (`main` or an account id).
pub async fn zeus_target(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<TargetBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.zeus_set_target(target(&req.target)?).await?;
    audit(&state, "zeus_pay_target", json!({"target": req.target}), unix_now()).await;
    Ok(Json(full_view(&state, svc).await))
}

#[derive(Deserialize)]
pub struct ForceQuery {
    #[serde(default)]
    force: bool,
}

/// `DELETE /api/public-name/zeus?force=`: gives the address up at ZEUS Pay and revokes
/// its connection (`force`: also when ZEUS Pay cannot be reached).
pub async fn zeus_remove(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ForceQuery>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.zeus_remove(q.force).await?;
    audit(&state, "zeus_pay_removed", json!({"force": q.force}), unix_now()).await;
    Ok(Json(full_view(&state, svc).await))
}

#[derive(Deserialize)]
pub struct PlusBody {
    max_price_sats: Option<u64>,
    auto_renew: Option<bool>,
}

/// `POST /api/public-name/zeus/plus {max_price_sats}`: buys or extends ZEUS Pay+ by a
/// year, paid from Main.
pub async fn zeus_plus_buy(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<PlusBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    require_mainnet(&state, svc.zeus_any_network(), "ZEUS Pay").await?;
    let max =
        req.max_price_sats.ok_or_else(|| ApiError::bad_request("max_price_sats is required"))?;
    let e = svc.zeus_buy_plus(max).await?;
    audit(
        &state,
        "zeus_pay_plus_bought",
        json!({"fee_sats": e.plus_fee_sats, "until": e.plus_expires_at}),
        unix_now(),
    )
    .await;
    Ok(Json(full_view(&state, svc).await))
}

/// `PUT /api/public-name/zeus/plus {auto_renew, max_price_sats?}`: automatic renewal.
pub async fn zeus_plus_settings(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<PlusBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let on = req.auto_renew.ok_or_else(|| ApiError::bad_request("auto_renew is required"))?;
    svc.zeus_set_auto_renew(on, req.max_price_sats).await?;
    audit(
        &state,
        "zeus_pay_plus_auto_renew",
        json!({"auto_renew": on, "max_price_sats": req.max_price_sats}),
        unix_now(),
    )
    .await;
    Ok(Json(full_view(&state, svc).await))
}

/// `POST /api/public-name/zeus/refresh`: reads the address and ZEUS Pay+ state again.
pub async fn zeus_refresh(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.zeus_refresh().await?;
    Ok(Json(full_view(&state, svc).await))
}

#[derive(Deserialize)]
pub struct TwelveBuyBody {
    target: String,
    username: String,
    /// The most the owner agreed to pay, in sats.
    max_price_sats: u64,
    /// `main` or an account id; default: the target itself.
    pay_from: Option<String>,
}

/// `POST /api/public-name/twelve-cash/buy {target, username, max_price_sats, pay_from?}`:
/// buys `username@twelve.cash`, paid from the wallet through twelve.cash's checkout.
pub async fn twelve_buy(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<TwelveBuyBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    require_mainnet(&state, svc.twelve_any_network(), "twelve.cash").await?;
    let t = target(&req.target)?;
    let pay_from = match req.pay_from.as_deref() {
        Some(p) => target(p)?,
        None => t,
    };
    let e = svc.twelve_buy(t, &req.username, req.max_price_sats, pay_from).await?;
    audit(
        &state,
        "twelve_cash_bought",
        json!({"target": req.target, "address": format!("{}@{}", e.user_name, e.domain),
            "finished": e.purchase.is_none()}),
        unix_now(),
    )
    .await;
    Ok(Json(full_view(&state, svc).await))
}

// ------------------------------------------------------------- questions and Alby (§12.6, §13)

/// `PUT /api/public-name/prefs {wallets?, domain?, site?}`: the owner's answers (merged).
pub async fn set_prefs(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<allod_core::public_name::Prefs>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.set_prefs(req).await?;
    Ok(Json(full_view(&state, svc).await))
}

/// `DELETE /api/public-name/prefs`: start the questions over.
pub async fn reset_prefs(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.reset_prefs().await?;
    Ok(Json(full_view(&state, svc).await))
}

#[derive(Deserialize, Default)]
pub struct AlbyConnectionBody {
    /// `main` or an account id: whose invoices the connection makes.
    target: Option<String>,
    /// A new key for this existing connection instead of a new one.
    connection_id: Option<i64>,
}

/// `POST /api/public-name/alby/connection {target?, connection_id?}` → `{uri, connection_id}`:
/// the receive-only connection string to paste on the service's website (a new key each
/// time).
pub async fn alby_connection(
    State(state): State<AppState>,
    auth: Auth,
    body: Option<Json<AlbyConnectionBody>>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    require_mainnet(&state, svc.alby_any_network(), "Alby").await?;
    let req = body.map(|b| b.0).unwrap_or_default();
    let t = target(req.target.as_deref().unwrap_or("main"))?;
    let (id, uri) = svc.alby_connection(t, req.connection_id).await?;
    audit(&state, "alby_connection_issued", json!({"connection_id": id}), unix_now()).await;
    Ok(Json(json!({"uri": uri, "connection_id": id})))
}

#[derive(Deserialize)]
pub struct AlbyBody {
    connection_id: i64,
    address: String,
}

/// `POST /api/public-name/alby {address}`: links the address after checking it asks
/// this node for invoices.
pub async fn alby_link(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<AlbyBody>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    let e = svc.alby_link(req.connection_id, &req.address).await?;
    audit(&state, "alby_linked", json!({"address": e.address}), unix_now()).await;
    Ok(Json(full_view(&state, svc).await))
}

/// `DELETE /api/public-name/alby/{connection_id}`: forgets it and revokes its connection.
pub async fn alby_remove(
    State(state): State<AppState>,
    auth: Auth,
    axum::extract::Path(connection_id): axum::extract::Path<i64>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let svc = service(&state)?;
    svc.alby_remove(connection_id).await?;
    audit(&state, "alby_removed", json!({"connection_id": connection_id}), unix_now()).await;
    Ok(Json(full_view(&state, svc).await))
}

// ------------------------------------------------------------- DNS provider (§12.8)
