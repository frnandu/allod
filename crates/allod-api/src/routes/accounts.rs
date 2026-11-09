//! Accounts (SPEC §10, §11.3, M6): isolated balances on the node, each with its own NWC
//! connection, funded from and returned to Main by internal transfers.

use allod_core::apps::{AppView, NewApp};
use allod_core::wallet::accounts::StatementQuery;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{reauthenticate, unix_now, Auth};
use crate::error::ApiError;
use crate::routes::apps::apps;
use crate::routes::auth::audit;
use crate::routes::wallet::wallet;
use crate::AppState;

/// The account `id`, or 404 (connections are not accounts).
async fn account(state: &AppState, id: i64) -> Result<allod_core::App, ApiError> {
    match apps(state)?.get(id).await {
        Ok(a) if a.is_account() => Ok(a),
        Ok(_) | Err(allod_core::AppError::NotFound) => {
            Err(ApiError::not_found("account not found"))
        }
        Err(e) => Err(e.into()),
    }
}

async fn view(state: &AppState, app: allod_core::App) -> Result<AppView, ApiError> {
    Ok(apps(state)?.view(app, &*wallet(state)?).await?)
}

/// `GET /api/accounts` → `{items, total}`, oldest first. Closed accounts appear only
/// while they still hold funds.
pub async fn list(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let mut items = Vec::new();
    for app in apps(&state)?.list_accounts().await? {
        items.push(view(&state, app).await?);
    }
    Ok(Json(json!({"total": items.len(), "items": items})))
}

/// `GET /api/accounts/summary` → `{count, total_balance_msat}`: open accounts, and the
/// balance of every account (including closed ones that still hold funds).
pub async fn summary(State(state): State<AppState>, _auth: Auth) -> Result<Json<Value>, ApiError> {
    let accounts = apps(&state)?.list_accounts().await?;
    let w = wallet(&state)?;
    let mut total = 0i64;
    for a in &accounts {
        total += w.app_balance(Some(a.id)).await?;
    }
    let count = accounts.iter().filter(|a| a.closed_at.is_none()).count();
    Ok(Json(json!({"count": count, "total_balance_msat": total})))
}

/// `GET /api/accounts/{id}`
pub async fn get(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let app = account(&state, id).await?;
    let requests = apps(&state)?.requests(id, 20).await?;
    let mut v = serde_json::to_value(view(&state, app).await?).unwrap_or_default();
    v["requests"] = serde_json::to_value(requests).unwrap_or_default();
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct StatementParams {
    from: Option<i64>,
    until: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// `GET /api/accounts/{id}/statement?from=&until=&limit=&offset=` → ledger postings
/// with the balance after each, newest first. `main` is the Main account.
pub async fn statement(
    State(state): State<AppState>,
    _auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<StatementParams>,
) -> Result<Json<Value>, ApiError> {
    let app_id = if id == "main" {
        None
    } else {
        let id: i64 =
            id.parse().map_err(|_| ApiError::bad_request("id must be main or a number"))?;
        account(&state, id).await?;
        Some(id)
    };
    let s = wallet(&state)?
        .statement(
            app_id,
            &StatementQuery {
                from: q.from,
                until: q.until,
                limit: q.limit.unwrap_or(50),
                offset: q.offset.unwrap_or(0),
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(s).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct NewAccount {
    name: String,
    lud16_username: Option<String>,
}

/// `POST /api/accounts {name, lud16_username?}` → `{account, pairing_uri, bip353?}`.
/// The connection URI is returned once (SPEC §10).
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<NewAccount>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    auth.require_full()?;
    let now = unix_now();
    // A username becomes an address on the first domain (SPEC §12.1).
    let wanted = req
        .lud16_username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(|u| allod_core::apps::validate_username(&u.to_ascii_lowercase()))
        .transpose()?;
    let domain = match (&wanted, &state.domains) {
        (None, _) => None,
        (Some(_), None) => return Err(ApiError::bad_request("add a domain first")),
        (Some(u), Some(svc)) => {
            let d = svc
                .first_domain()
                .await
                .ok_or_else(|| ApiError::bad_request("add a domain first (Settings → Domains)"))?;
            if !svc.username_free(d.id, u).await {
                return Err(ApiError::bad_request(format!("{u}@{} is taken", d.name)));
            }
            Some((svc.clone(), d))
        }
    };
    let (app, uri) = apps(&state)?
        .create(
            NewApp { name: req.name, kind: Some("account".into()), ..Default::default() },
            None,
            now,
        )
        .await?;
    audit(&state, "app_created", json!({"app_id": app.id, "kind": "account"}), now).await;
    let mut bip353 = Value::Null;
    if let (Some(u), Some((svc, d))) = (wanted, domain) {
        match svc.add_address(d.id, &u, Some(app.id)).await {
            Ok(a) => {
                let txt = match (a.offer_id.as_deref(), state.wallet.get()) {
                    (Some(o), Some(w)) => w
                        .offers(Some(Some(app.id)))
                        .await
                        .ok()
                        .and_then(|l| l.into_iter().find(|x| x.offer_id == o))
                        .map(|x| format!("bitcoin:?lno={}", x.offer)),
                    _ => None,
                };
                bip353 = json!({
                    "address": format!("{u}@{}", d.name),
                    "name": format!("{}.{}.", allod_core::public_name::bip353_subname(&u), d.name),
                    "txt": txt,
                    "offer_id": a.offer_id,
                });
            }
            Err(e) => tracing::info!(error = %e, "no address for the new account"),
        }
    }
    let app = view(&state, app).await?;
    Ok((StatusCode::CREATED, Json(json!({"account": app, "pairing_uri": uri, "bip353": bip353}))))
}

#[derive(Deserialize)]
pub struct Amount {
    amount_msat: u64,
}

/// `POST /api/accounts/{id}/topup {amount_msat}`: internal transfer from Main.
pub async fn topup(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<Amount>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let a = account(&state, id).await?;
    let tx = wallet(&state)?
        .transfer(None, Some(id), req.amount_msat, format!("Top up {}", a.name))
        .await?;
    Ok(Json(serde_json::to_value(tx).unwrap_or_default()))
}

/// `POST /api/accounts/{id}/withdraw {amount_msat}`: internal transfer to Main.
pub async fn withdraw(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<Amount>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    let a = account(&state, id).await?;
    let tx = wallet(&state)?
        .transfer(Some(id), None, req.amount_msat, format!("Withdraw from {}", a.name))
        .await?;
    Ok(Json(serde_json::to_value(tx).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct CloseParams {
    #[serde(default)]
    sweep: bool,
}

/// `DELETE /api/accounts/{id}?sweep=true`: closes the account. Without `sweep` it must
/// be empty. Sweeping a balance to Main needs a fresh NIP-98 signature over this exact
/// request (SPEC §11.1).
pub async fn close(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<CloseParams>,
) -> Result<Json<Value>, ApiError> {
    auth.require_full()?;
    account(&state, id).await?;
    let w = wallet(&state)?;
    if q.sweep && w.app_balance(Some(id)).await? > 0 {
        let path = format!("/api/accounts/{id}?sweep=true");
        reauthenticate(&state, &auth, &headers, "DELETE", &path, b"")?;
    }
    let swept = w.close_account(id, q.sweep).await?;
    let svc = apps(&state)?;
    svc.notify_changed();
    if let Some(public_name) = &state.public_name {
        public_name.trigger();
    }
    audit(
        &state,
        "app_deleted",
        json!({"app_id": id, "kind": "account", "swept_msat": swept.as_ref().map(|t| t.amount_msat)}),
        unix_now(),
    )
    .await;
    let app = svc.get(id).await?;
    Ok(Json(json!({"account": view(&state, app).await?, "sweep": swept})))
}
