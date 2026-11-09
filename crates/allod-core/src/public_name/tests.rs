use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::*;

/// What the mock twelve.cash and clank.money saw, and how they answer.
#[derive(Default)]
struct Mock {
    twelve: Vec<Value>,
    /// clank.money: registrations seen (username → bip321Uri) and whether the next
    /// paid registration fails once.
    clank: Vec<(String, String)>,
    clank_fail_once: bool,
    /// ZEUS Pay: the address held (handle), the NWC string it was given, deletes seen,
    /// and whether delete fails.
    zeus: Option<String>,
    zeus_nwc: Option<String>,
    zeus_deletes: usize,
    zeus_delete_fails: bool,
    /// ZEUS Pay+: renames need it when `zeus_plus_required`; an order is "paid" by the
    /// next status call (the wallet balance shows the real payment).
    zeus_plus_required: bool,
    zeus_plus_until: Option<i64>,
    zeus_plus_pending: bool,
    zeus_orders: usize,
    /// twelve.cash purchases: pay codes (id, user, lno), whether the checkout was seen
    /// paid, redeemed ids, and whether the MDK CSRF header matched the cookie.
    twelve_codes: Vec<(String, String, String)>,
    twelve_checkout_paid: bool,
    twelve_redeemed: Vec<String>,
    twelve_redeem_too_early: bool,
    mdk_csrf_ok: bool,
}

type M = Arc<Mutex<Mock>>;

async fn twelve_random(State(m): State<M>, Json(body): Json<Value>) -> impl IntoResponse {
    let input = body["json"].clone();
    if !input["lno"].as_str().unwrap_or_default().starts_with("lno") {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": {"json": {"message": "bad"}}})));
    }
    let mut m = m.lock().unwrap();
    m.twelve.push(input);
    let n = m.twelve.len();
    (
        StatusCode::OK,
        Json(json!({"result": {"data": {"json": {
            "id": "u1", "userName": format!("brave.penguin{n}"), "domain": "twelve.cash", "status": "ACTIVE"
        }}}})),
    )
}

const MOCK_PREIMAGE: &str = "000000000000000000000000000000000000000000000000000000000000feed";

async fn clank_register(
    State(m): State<M>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let user = body["username"].as_str().unwrap_or_default().to_string();
    let uri = body["bip321Uri"].as_str().unwrap_or_default().to_string();
    if user == "taken" {
        return (StatusCode::CONFLICT, Json(json!({"error": {"code": "username_unavailable"}})));
    }
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if auth.is_empty() {
        let invoice = allod_ln::mock::MockBackend::make_invoice(Some(999_000), &"ab".repeat(32));
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({"error": {"code": "payment_required"}, "macaroon": format!("mac-{user}"),
                "invoice": invoice, "paymentHash": "ab".repeat(32), "amountSats": 999})),
        );
    }
    if auth != format!("L402 mac-{user}:{MOCK_PREIMAGE}") {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": "bad_l402"}})));
    }
    let mut m = m.lock().unwrap();
    if std::mem::take(&mut m.clank_fail_once) {
        return (StatusCode::BAD_GATEWAY, Json(json!({"error": {"message": "busy"}})));
    }
    m.clank.push((user.clone(), uri));
    (StatusCode::CREATED, Json(json!({"username": user, "managementToken": format!("tok-{user}")})))
}

async fn clank_get(State(m): State<M>, Path(user): Path<String>) -> StatusCode {
    if user == "taken" || m.lock().unwrap().clank.iter().any(|(u, _)| *u == user) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The mock node signs `m` as `sig{len}` (allod_ln::mock).
fn zeus_signed(b: &Value) -> bool {
    b["message"] == CHALLENGE && b["signature"] == format!("sig{}", CHALLENGE.len())
}

async fn zeus_api(
    State(m): State<M>,
    Path(op): Path<String>,
    Json(b): Json<Value>,
) -> impl IntoResponse {
    let mut m = m.lock().unwrap();
    let no = |e: &str| (StatusCode::BAD_REQUEST, Json(json!({"success": false, "error": e})));
    if op == "auth" {
        return (StatusCode::OK, Json(json!({"verification": CHALLENGE})));
    }
    if !zeus_signed(&b) {
        return no("bad signature");
    }
    match op.as_str() {
        "create" => {
            let nwc = b["nwc_string"].as_str().unwrap_or_default();
            if b["address_type"] != "nwc" || !nwc.starts_with("nostr+walletconnect://") {
                return no("bad nwc");
            }
            m.zeus_nwc = Some(nwc.into());
            m.zeus = Some("swift123".into());
            (
                StatusCode::OK,
                Json(json!({"success": true, "handle": "swift123", "domain": "zeuspay.com"})),
            )
        }
        "status" => {
            if std::mem::take(&mut m.zeus_plus_pending) {
                let from = m.zeus_plus_until.unwrap_or(0).max(now());
                m.zeus_plus_until = Some(from + 365 * 86_400);
            }
            (
                StatusCode::OK,
                Json(json!({"success": true, "handle": m.zeus, "domain": "zeuspay.com",
                    // ZEUS sends milliseconds here.
                    "plusExpiresAt": m.zeus_plus_until.map(|t| t * 1000), "plusAnnualFeeSats": 50_000})),
            )
        }
        "update" => {
            if m.zeus_plus_required && m.zeus_plus_until.is_none_or(|t| t < now()) {
                return no(
                    "You must have a ZEUS Pay+ subscription to change your handle or domain",
                );
            }
            let h = b["updates"]["handle"].as_str().unwrap_or_default().to_string();
            if h == "taken" {
                return no("handle taken");
            }
            m.zeus = Some(h.clone());
            (
                StatusCode::OK,
                Json(json!({"success": true, "handle": h, "domain": b["updates"]["domain"]})),
            )
        }
        "delete" => {
            m.zeus_deletes += 1;
            if m.zeus_delete_fails {
                return no("down");
            }
            m.zeus = None;
            (StatusCode::OK, Json(json!({"success": true})))
        }
        _ => no("unknown"),
    }
}

async fn zeus_plus_order(State(m): State<M>, Json(b): Json<Value>) -> impl IntoResponse {
    if !zeus_signed(&b) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "bad signature"})));
    }
    let mut m = m.lock().unwrap();
    m.zeus_orders += 1;
    m.zeus_plus_pending = true;
    let invoice = allod_ln::mock::MockBackend::make_invoice(
        Some(50_000_000),
        &format!("{:064x}", m.zeus_orders),
    );
    (StatusCode::OK, Json(json!({"bolt11": invoice})))
}

async fn zeus_lnurlp(State(m): State<M>, Path(handle): Path<String>) -> impl IntoResponse {
    if m.lock().unwrap().zeus.as_deref() == Some(handle.as_str()) {
        (StatusCode::OK, Json(json!({"tag": "payRequest", "callback": "x"})))
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"status": "ERROR"})))
    }
}

async fn twelve_create_code(State(m): State<M>, Json(b): Json<Value>) -> impl IntoResponse {
    let i = &b["json"];
    let user = i["userName"].as_str().unwrap_or_default().to_string();
    if user == "taken" {
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"error": {"json": {"message": "User name is taken", "data": {"code": "CONFLICT"}}}}),
            ),
        );
    }
    let mut m = m.lock().unwrap();
    let id = format!("pc{}", m.twelve_codes.len() + 1);
    m.twelve_codes.push((id.clone(), user.clone(), i["lno"].as_str().unwrap_or_default().into()));
    (
        StatusCode::OK,
        Json(
            json!({"result": {"data": {"json": {"payCodeId": id, "userName": user, "domain": "twelve.cash"}}}}),
        ),
    )
}

async fn twelve_redeem(State(m): State<M>, Json(b): Json<Value>) -> impl IntoResponse {
    let mut m = m.lock().unwrap();
    // Like the real server, this does not check payment; the test checks Allod's order.
    if !m.twelve_checkout_paid {
        m.twelve_redeem_too_early = true;
    }
    m.twelve_redeemed.push(b["json"]["payCodeId"].as_str().unwrap_or_default().into());
    (StatusCode::OK, Json(json!({"result": {"data": {"json": {"status": "ACTIVE"}}}})))
}

async fn mdk(State(m): State<M>, headers: HeaderMap, Json(b): Json<Value>) -> impl IntoResponse {
    let mut m = m.lock().unwrap();
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let token =
        headers.get("x-moneydevkit-csrf-token").and_then(|v| v.to_str().ok()).unwrap_or("-");
    m.mdk_csrf_ok = cookie == format!("mdk_csrf={token}");
    let invoice = allod_ln::mock::MockBackend::make_invoice(Some(5_000_000), &"cd".repeat(32));
    let pending = json!({"data": {"id": "co1", "status": "PENDING_PAYMENT",
        "invoice": {"invoice": invoice, "amountSats": 5000}}});
    match b["handler"].as_str() {
        Some("create_checkout") => {
            assert_eq!(b["params"]["amount"], 5000);
            assert_eq!(
                b["params"]["metadata"]["payCodeId"],
                json!(m.twelve_codes.last().unwrap().0)
            );
            (StatusCode::OK, Json(json!({"data": {"id": "co1", "status": "UNCONFIRMED"}})))
        }
        Some("confirm_checkout") => (StatusCode::OK, Json(pending)),
        Some("get_checkout") => {
            // The payment reached the merchant by the time Allod asks.
            m.twelve_checkout_paid = true;
            (StatusCode::OK, Json(json!({"data": {"id": "co1", "status": "PAYMENT_RECEIVED"}})))
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"error": "bad handler"}))),
    }
}

async fn mock() -> (M, String) {
    let m: M = Arc::new(Mutex::new(Mock::default()));
    let app = Router::new()
        .route("/api/trpc/payCode.createRandomPayCode", post(twelve_random))
        .route("/api/v1/registrations", post(clank_register))
        .route("/api/v1/registrations/{user}", get(clank_get))
        .route("/api/lnurl/{op}", post(zeus_api))
        .route("/api/plus/order", post(zeus_plus_order))
        .route("/api/trpc/payCode.createPayCode", post(twelve_create_code))
        .route("/api/trpc/payCode.redeemPayCode", post(twelve_redeem))
        .route("/api/mdk", post(mdk))
        .route("/.well-known/lnurlp/{handle}", get(zeus_lnurlp))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (m, format!("http://{addr}"))
}

async fn service(dir: &std::path::Path) -> Arc<PublicNameService> {
    let db = Db::connect(&format!("sqlite://{}/t.db", dir.display())).await.unwrap();
    PublicNameService::load(
        db,
        SecretStore::new(dir.join("secrets")),
        PublicNameConfig::default(),
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap()
}

/// A setting saved by an older version (a deSEC name) keeps the registry names.
#[tokio::test]
async fn an_unknown_stored_mode_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::connect(&format!("sqlite://{}/t.db", dir.path().display())).await.unwrap();
    let old = json!({"mode": "desec", "name": "x.example.net", "exposure": "public",
        "pending": null, "published": {}, "ipv4": "203.0.113.7",
        "twelve": {"main": {"user_name": "brave.penguin", "domain": "twelve.cash",
            "offer_id": "o1", "custom": false, "verified": true, "created_at": 1}}});
    db.config_set(CONFIG_KEY, &old.to_string(), false).await.unwrap();
    let svc = PublicNameService::load(
        db,
        SecretStore::new(dir.path().join("secrets")),
        PublicNameConfig::default(),
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap();
    let v = svc.view();
    assert_eq!(v["twelve_cash"][0]["address"], "brave.penguin@twelve.cash");
}

#[test]
fn names_are_validated() {
    assert!(valid_label("my-node1"));
    assert!(!valid_label("-x") && !valid_label("x-") && !valid_label("") && !valid_label("A"));
    assert!(!valid_label(&"a".repeat(64)));
    assert_eq!(normalize_domain("pay.example.com.").unwrap(), "pay.example.com");
    assert_eq!(normalize_domain("https://x.example.org:443/path").unwrap(), "x.example.org");
    for bad in ["localhost", "1.2.3.4", "no_underscore.com", "example.c", ""] {
        assert!(normalize_domain(bad).is_err(), "{bad}");
    }
    assert_eq!(url_domain("https://a.example.com/x").as_deref(), Some("a.example.com"));
    assert_eq!(url_domain("http://127.0.0.1:8029"), None);
}

/// The service with a wallet and accounts on the mock node, and the registries at `base`.
async fn service_with_wallet(
    base: &str,
    dir: &std::path::Path,
) -> (Arc<PublicNameService>, Arc<AppService>) {
    let url = format!("sqlite://{}/w.db", dir.display());
    let db = Db::connect(&url).await.unwrap();
    let pool = Db::any_pool(&url).await.unwrap();
    let backend: Arc<dyn allod_ln::LightningBackend> = Arc::new(allod_ln::mock::MockBackend::new());
    let wallet = Wallet::new(pool.clone(), backend, Default::default());
    let phrase = allod_keys::RecoveryPhrase::generate().unwrap();
    let keys = allod_keys::NostrKeys::from_phrase(&phrase).unwrap();
    let apps = AppService::new(pool, Arc::new(keys), vec!["wss://relay.example".into()], vec![]);
    let (wallet_cell, apps_cell) = (Arc::new(OnceCell::new()), Arc::new(OnceCell::new()));
    let _ = wallet_cell.set(wallet);
    let _ = apps_cell.set(apps.clone());
    let svc = PublicNameService::load(
        db,
        SecretStore::new(dir.join("secrets")),
        PublicNameConfig {
            twelve_cash_url: Some(base.to_string()),
            clank_url: Some(base.to_string()),
            zeus_url: Some(base.to_string()),
            lnurl_http: true,
        },
        wallet_cell,
        apps_cell,
    )
    .await
    .unwrap();
    (svc, apps)
}

#[tokio::test]
async fn twelve_cash_names_for_main_and_accounts() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, apps) = service_with_wallet(&base, dir.path()).await;
    let (acct, _) = apps
        .create(
            crate::apps::NewApp {
                name: "Kid".into(),
                kind: Some("account".into()),
                ..Default::default()
            },
            None,
            now(),
        )
        .await
        .unwrap();

    let main = svc.twelve_register(None).await.unwrap();
    assert_eq!(main.user_name, "brave.penguin1");
    assert!(!main.custom);
    let kid = svc.twelve_register(Some(acct.id)).await.unwrap();
    assert_ne!(kid.offer_id, main.offer_id, "each wallet has its own offer");
    let wallet = svc.wallet.get().unwrap().clone();
    let kid_offer = wallet
        .offers(Some(Some(acct.id)))
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.offer_id == kid.offer_id)
        .unwrap();
    assert_eq!(kid_offer.description.as_deref(), Some("Payment to Kid (twelve.cash)"));
    assert!(matches!(svc.twelve_register(None).await, Err(PublicNameError::Conflict(_))));
    {
        let m = m.lock().unwrap();
        assert_eq!(m.twelve.len(), 2);
        assert_eq!(m.twelve[0]["domain"], "twelve.cash");
        assert!(m.twelve[0]["lno"].as_str().unwrap().starts_with("lno1"));
    }
    // A custom name gets its own offer, described with the name, and asking again
    // shows the same one.
    let offer = svc.twelve_offer(Some(acct.id), "Satoshi@twelve.cash").await.unwrap();
    assert!(m.lock().unwrap().twelve[1]["lno"] != json!(offer));
    assert_eq!(svc.twelve_offer(Some(acct.id), "satoshi@twelve.cash").await.unwrap(), offer);
    let named = wallet.offers(Some(Some(acct.id))).await.unwrap();
    let named = named.iter().find(|o| o.offer == offer).unwrap();
    assert_eq!(named.description.as_deref(), Some("₿satoshi@twelve.cash"));

    let v = svc.view();
    let list = v["twelve_cash"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list
        .iter()
        .any(|e| e["target"] == "main" && e["address"] == "brave.penguin1@twelve.cash"));

    // A closed account's address is reported inactive.
    svc.wallet.get().unwrap().close_account(acct.id, false).await.unwrap();
    svc.reconcile().await;
    let v = svc.view();
    let kid_view = v["twelve_cash"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["target"].as_str() == Some(acct.id.to_string().as_str()))
        .unwrap()
        .clone();
    assert_eq!(kid_view["active"], false);
    assert!(matches!(
        svc.twelve_register(Some(acct.id)).await,
        Err(PublicNameError::BadRequest(_))
    ));

    svc.twelve_forget(None).await.unwrap();
    assert_eq!(svc.view()["twelve_cash"].as_array().unwrap().len(), 1);
    assert!(svc.twelve_forget(None).await.is_err());
    assert!(svc.twelve_link(None, "someone@example.com").await.is_err());
}

#[tokio::test]
async fn clank_names_are_paid_once_and_point_at_their_own_offer() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, _apps) = service_with_wallet(&base, dir.path()).await;
    let wallet = svc.wallet.get().unwrap().clone();
    wallet.reconcile().await.unwrap();
    let before = wallet.balance(None).await.unwrap();
    assert!(before > 2_000_000, "Main can pay: {before}");

    assert_eq!(svc.clank_available("free-name").await.unwrap(), Some(true));
    assert_eq!(svc.clank_available("taken").await.unwrap(), Some(false));
    assert!(svc.clank_available("-bad").await.is_err());

    // Taken, and too expensive: nothing is paid, and the new offer is disabled.
    assert!(matches!(
        svc.clank_register(None, "taken", 2_000, None).await,
        Err(PublicNameError::Conflict(_))
    ));
    let e = svc.clank_register(None, "mynode", 500, None).await.unwrap_err();
    assert!(e.to_string().contains("999 sats"), "{e}");
    assert_eq!(wallet.balance(None).await.unwrap(), before);
    let offers = wallet.offers(Some(None)).await.unwrap();
    assert!(offers.iter().all(|o| o.state == "disabled"), "{offers:?}");

    let entry = svc.clank_register(None, "MyNode", 1_000, None).await.unwrap();
    assert_eq!((entry.user_name.as_str(), entry.state), ("mynode", ClankState::Registered));
    assert_eq!(entry.price_sats, 999);
    let spent = before - wallet.balance(None).await.unwrap();
    assert!((999_000..=1_010_000).contains(&spent), "price plus fee, paid once: {spent}");
    let offer = wallet
        .offers(Some(None))
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.offer_id == entry.offer_id)
        .unwrap();
    assert_eq!(offer.description.as_deref(), Some("₿mynode@clank.money"));
    assert_eq!(offer.state, "active");
    assert_eq!(
        m.lock().unwrap().clank,
        vec![("mynode".to_string(), format!("bitcoin:?lno={}", offer.offer))]
    );
    assert_eq!(
        svc.secrets.read("clank_token_mynode").unwrap().as_deref().map(|s| s.as_str()),
        Some("tok-mynode")
    );
    assert!(!svc.secrets.exists("clank_l402_main"), "the payment proof is no longer kept");
    assert!(matches!(
        svc.clank_register(None, "other", 1_000, None).await,
        Err(PublicNameError::Conflict(_))
    ));
    let v = svc.view();
    assert_eq!(v["clank"][0]["address"], "mynode@clank.money");
    assert_eq!(v["clank"][0]["state"], "registered");

    svc.clank_forget(None).await.unwrap();
    assert!(!svc.secrets.exists("clank_token_mynode"));
    assert!(svc.view()["clank"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_paid_clank_registration_is_finished_later() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, _apps) = service_with_wallet(&base, dir.path()).await;
    let wallet = svc.wallet.get().unwrap().clone();
    wallet.reconcile().await.unwrap();
    m.lock().unwrap().clank_fail_once = true;

    let entry = svc.clank_register(None, "later", 1_000, None).await.unwrap();
    assert_eq!(entry.state, ClankState::Paid, "paid, not registered yet");
    assert!(entry.error.is_some());
    let after_pay = wallet.balance(None).await.unwrap();
    assert!(svc.clank_forget(None).await.is_err(), "cannot forget mid-registration");

    // The background check sends the proof again; no second payment.
    svc.reconcile().await;
    let v = svc.view();
    assert_eq!(v["clank"][0]["state"], "registered", "{v}");
    assert_eq!(wallet.balance(None).await.unwrap(), after_pay);
    assert_eq!(m.lock().unwrap().clank.len(), 1);
}

#[tokio::test]
async fn new_names_are_checked_soon_then_on_the_normal_schedule() {
    let (_m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, _apps) = service_with_wallet(&base, dir.path()).await;
    assert_eq!(svc.next_name_check(), None, "nothing to check");
    let e = svc.twelve_register(None).await.unwrap();
    let c = e.created_at;
    assert_eq!(svc.next_name_check(), Some(c + 10));
    for (checked, next) in
        [(c + 10, Some(c + 30)), (c + 31, Some(c + 120)), (c + 120, Some(c + 300))]
    {
        *svc.last_name_check.lock().unwrap() = Some(checked);
        assert_eq!(svc.next_name_check(), next);
    }
    *svc.last_name_check.lock().unwrap() = Some(c + 300);
    assert_eq!(svc.next_name_check(), None, "then the 5-minute reconcile takes over");

    // A seen name needs no more checks.
    *svc.last_name_check.lock().unwrap() = None;
    let mut s = svc.stored();
    s.twelve.get_mut("main").unwrap().verified = true;
    svc.save(s).await.unwrap();
    assert_eq!(svc.next_name_check(), None);
}

#[tokio::test]
async fn zeus_pay_address_for_main() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, apps) = service_with_wallet(&base, dir.path()).await;

    assert!(svc.zeus_register(Some("Bad Name"), None).await.is_err());
    assert!(svc.zeus_register(None, Some("example.com")).await.is_err());
    let e = svc.zeus_register(Some("mynode"), None).await.unwrap();
    assert_eq!((e.handle.as_str(), e.domain.as_str()), ("mynode", "zeuspay.com"));
    let nwc = m.lock().unwrap().zeus_nwc.clone().unwrap();
    assert!(nwc.contains("secret="), "ZEUS Pay got a usable connection string");

    // A receive-only connection on Main.
    let app = apps.get(e.connection_id).await.unwrap();
    assert!(!app.isolated, "invoices credit Main");
    assert!(app.own_rows_only(), "ZEUS Pay sees only its own invoices");
    let mut scopes = app.scopes.clone();
    scopes.sort();
    assert_eq!(scopes, ["get_info", "lookup_invoice", "make_invoice"]);
    assert!(matches!(svc.zeus_register(None, None).await, Err(PublicNameError::Conflict(_))));

    // Its LNURL endpoint answers: verified.
    assert!(!svc.view()["zeus"]["verified"].as_bool().unwrap());
    svc.check_names().await;
    let v = svc.view();
    assert_eq!(v["zeus"]["address"], "mynode@zeuspay.com");
    assert_eq!(v["zeus"]["verified"], true);

    // Renames: refused keeps the old name; accepted needs a new check.
    assert!(svc.zeus_rename("taken", "zeuspay.com").await.is_err());
    assert_eq!(svc.view()["zeus"]["handle"], "mynode");
    svc.zeus_rename("satoshi", "zeusnuts.com").await.unwrap();
    assert_eq!(svc.view()["zeus"]["address"], "satoshi@zeusnuts.com");
    assert_eq!(svc.view()["zeus"]["verified"], false);

    // Removing: ZEUS Pay down stops it unless forced; forced revokes the connection.
    m.lock().unwrap().zeus_delete_fails = true;
    assert!(svc.zeus_remove(false).await.is_err());
    assert!(!svc.view()["zeus"].is_null());
    svc.zeus_remove(true).await.unwrap();
    assert!(svc.view()["zeus"].is_null());
    assert!(apps.get(e.connection_id).await.is_err(), "connection revoked");
    assert_eq!(m.lock().unwrap().zeus_deletes, 2);
}

#[tokio::test]
async fn a_refused_zeus_name_keeps_the_address() {
    let (_m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, _apps) = service_with_wallet(&base, dir.path()).await;
    let e = svc.zeus_register(Some("taken"), None).await.unwrap_err();
    assert!(e.to_string().contains("registered as swift123@zeuspay.com"), "{e}");
    assert_eq!(svc.view()["zeus"]["handle"], "swift123");
}

#[tokio::test]
async fn zeus_pay_plus_is_bought_within_the_cap_and_renewed() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, _apps) = service_with_wallet(&base, dir.path()).await;
    let wallet = svc.wallet.get().unwrap().clone();
    wallet.reconcile().await.unwrap();
    m.lock().unwrap().zeus_plus_required = true;

    let e = svc.zeus_register(Some("mynode"), None).await.unwrap_err();
    assert!(e.to_string().contains("ZEUS Pay+"), "{e}");
    assert_eq!(svc.view()["zeus"]["plus_fee_sats"], 50_000);
    assert_eq!(svc.view()["zeus"]["plus_active"], false);

    // Over the cap: nothing ordered or paid.
    let before = wallet.balance(None).await.unwrap();
    let e = svc.zeus_buy_plus(10_000).await.unwrap_err();
    assert!(e.to_string().contains("50000 sats"), "{e}");
    assert_eq!(m.lock().unwrap().zeus_orders, 0);
    assert_eq!(wallet.balance(None).await.unwrap(), before);

    let e = svc.zeus_buy_plus(60_000).await.unwrap();
    assert!(e.plus_expires_at.unwrap() > now() + 360 * 86_400, "{e:?}");
    assert!(e.plus_error.is_none(), "{e:?}");
    let spent = before - wallet.balance(None).await.unwrap();
    assert!((50_000_000..=50_600_000).contains(&spent), "price plus routing fee: {spent}");
    assert_eq!(svc.view()["zeus"]["plus_active"], true);
    svc.zeus_rename("mynode", "zeuspay.com").await.unwrap();
    assert_eq!(svc.view()["zeus"]["address"], "mynode@zeuspay.com");

    // Auto-renew needs a cap; it renews only close to the end.
    assert!(svc.zeus_set_auto_renew(true, None).await.is_err());
    svc.zeus_set_auto_renew(true, Some(60_000)).await.unwrap();
    svc.zeus_upkeep().await;
    assert_eq!(m.lock().unwrap().zeus_orders, 1, "not due yet");
    let soon = now() + 86_400;
    m.lock().unwrap().zeus_plus_until = Some(soon);
    let mut s = svc.stored();
    s.zeus.as_mut().unwrap().plus_expires_at = Some(soon);
    svc.save(s).await.unwrap();
    svc.zeus_upkeep().await;
    assert_eq!(m.lock().unwrap().zeus_orders, 2, "renewed");
    let until = svc.stored().zeus.unwrap().plus_expires_at.unwrap();
    assert!(until > soon + 360 * 86_400);
    // A second run right away does not pay again.
    svc.zeus_upkeep().await;
    assert_eq!(m.lock().unwrap().zeus_orders, 2);
}

#[tokio::test]
async fn a_twelve_cash_name_is_paid_then_published() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, _apps) = service_with_wallet(&base, dir.path()).await;
    let wallet = svc.wallet.get().unwrap().clone();
    wallet.reconcile().await.unwrap();
    let before = wallet.balance(None).await.unwrap();

    assert!(svc.twelve_buy(None, "abc", 6_000, None).await.is_err(), "too short");
    assert!(svc.twelve_buy(None, "satoshi", 4_000, None).await.is_err(), "over the cap");
    let e = svc.twelve_buy(None, "taken", 6_000, None).await.unwrap_err();
    assert!(e.to_string().contains("taken"), "{e}");
    assert_eq!(wallet.balance(None).await.unwrap(), before, "nothing paid so far");

    let e = svc.twelve_buy(None, "Satoshi@twelve.cash", 6_000, None).await.unwrap();
    assert_eq!((e.user_name.as_str(), e.custom), ("Satoshi", true));
    assert!(e.purchase.is_none(), "finished: {:?}", e.purchase);
    let lno = {
        let m2 = m.lock().unwrap();
        assert!(m2.mdk_csrf_ok, "CSRF header matches the cookie");
        assert!(!m2.twelve_redeem_too_early, "published only after the payment was received");
        assert_eq!(m2.twelve_redeemed, vec!["pc1".to_string()]);
        m2.twelve_codes[0].2.clone()
    };
    let spent = before - wallet.balance(None).await.unwrap();
    assert!((5_000_000..=5_100_000).contains(&spent), "price plus fee, once: {spent}");
    let offer =
        wallet.offers(Some(None)).await.unwrap().into_iter().find(|o| o.offer == lno).unwrap();
    assert_eq!(offer.description.as_deref(), Some("₿Satoshi@twelve.cash"));
    assert_eq!(svc.view()["twelve_cash"][0]["purchase"], json!(null));
    assert!(svc.twelve_buy(None, "another", 6_000, None).await.is_err(), "one per wallet");
}

/// A stand-in LNURL address service: `/.well-known/lnurlp/me` answers with an invoice
/// made by `wallet` (as Alby does over NWC), `/.well-known/lnurlp/other` with someone
/// else's.
async fn lnurl_service(wallet: Arc<Wallet>) -> String {
    use axum::extract::Query;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route(
            "/.well-known/lnurlp/{user}",
            get(move |Path(user): Path<String>| {
                let cb = format!("http://{host}/cb/{user}");
                async move {
                    Json(json!({"tag": "payRequest", "callback": cb, "minSendable": 1000,
                        "maxSendable": 1_000_000_000, "metadata": "[[\"text/plain\",\"me\"]]"}))
                }
            }),
        )
        .route(
            "/cb/{user}",
            get(
                move |Path(user): Path<String>,
                      Query(q): Query<std::collections::HashMap<String, String>>| {
                    let wallet = wallet.clone();
                    async move {
                        let amount: u64 = q["amount"].parse().unwrap();
                        // As NWC makes them: `conn-<id>`: Main's, asked by connection <id>
                        // (owned by it); `acct-<id>`: of that account; `me`: Main's,
                        // made outside any connection; else another node.
                        let num = |p: &str| user.strip_prefix(p).and_then(|i| i.parse::<i64>().ok());
                        let owner = num("conn-").or(num("acct-"));
                        let pr = if user == "me" || owner.is_some() {
                            wallet
                                .create_invoice(crate::wallet::InvoiceRequest {
                                    app_id: owner,
                                    amount_msat: Some(amount),
                                    description: "me".into(),
                                    ..Default::default()
                                })
                                .await
                                .unwrap()
                                .payment_request
                                .unwrap()
                        } else {
                            allod_ln::mock::MockBackend::make_invoice(
                                Some(amount),
                                &"ef".repeat(32),
                            )
                        };
                        Json(json!({"pr": pr}))
                    }
                },
            ),
        );
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("{addr}")
}

#[tokio::test]
async fn an_alby_address_is_linked_only_when_it_asks_this_node() {
    let (_m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, apps) = service_with_wallet(&base, dir.path()).await;
    let wallet = svc.wallet.get().unwrap().clone();
    let host = lnurl_service(wallet).await;

    assert!(svc.alby_link(1, &format!("me@{host}")).await.is_err(), "connection string first");
    let (conn, uri) = svc.alby_connection(None, None).await.unwrap();
    assert!(uri.starts_with("nostr+walletconnect://") && uri.contains("secret="));
    assert_eq!(svc.view()["alby"][0]["connection_id"].as_i64(), Some(conn));
    assert_eq!(svc.view()["alby"][0]["target"], "main");
    let app = apps.get(conn).await.unwrap();
    assert!(!app.isolated, "invoices credit Main");
    assert_eq!(app.credit_account(), None);
    let mut scopes = app.scopes.clone();
    scopes.sort();
    assert_eq!(scopes, ["get_info", "list_transactions", "lookup_invoice", "make_invoice"]);
    assert!(app.own_rows_only(), "Alby sees only its own invoices, not Main's history");
    // Asking again for this one replaces the key, keeps the connection.
    let (same, again) = svc.alby_connection(None, Some(conn)).await.unwrap();
    assert_eq!(same, conn);
    assert_ne!(again, uri);

    // A second Alby address, for an account: its connection makes the account's invoices.
    let (kid, _) = apps
        .create(
            crate::apps::NewApp {
                name: "Kid".into(),
                kind: Some("account".into()),
                ..Default::default()
            },
            None,
            now(),
        )
        .await
        .unwrap();
    let (conn2, _) = svc.alby_connection(Some(kid.id), None).await.unwrap();
    assert_ne!(conn2, conn);
    assert_eq!(apps.get(conn2).await.unwrap().credit_account(), Some(kid.id));
    // Linking checks the address asks for this account's invoices, not Main's.
    let e = svc.alby_link(conn2, &format!("me@{host}")).await.unwrap_err();
    assert!(e.to_string().contains("another connection"), "{e}");
    svc.alby_link(conn2, &format!("acct-{}@{host}", kid.id)).await.unwrap();
    let e = svc.alby_link(conn, &format!("other@{host}")).await.unwrap_err();
    assert!(e.to_string().contains("another wallet"), "{e}");
    // Main's invoices made outside its connection do not count.
    let e = svc.alby_link(conn, &format!("me@{host}")).await.unwrap_err();
    assert!(e.to_string().contains("another connection"), "{e}");
    let e = svc.alby_link(conn, &format!("conn-{conn}@{host}")).await.unwrap();
    assert!(e.verified);
    assert_eq!(svc.view()["alby"][0]["address"], format!("conn-{conn}@{host}"));

    let v = svc.view();
    assert_eq!(v["alby"].as_array().unwrap().len(), 2);
    assert_eq!(v["alby"][1]["target"], kid.id.to_string());
    assert!(svc.alby_connection(Some(9_999), None).await.is_err(), "no such account");

    svc.alby_remove(conn).await.unwrap();
    assert_eq!(svc.view()["alby"].as_array().unwrap().len(), 1);
    assert!(apps.get(conn).await.is_err(), "connection revoked");
}

#[tokio::test]
async fn answers_are_remembered_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let svc = service(dir.path()).await;
    svc.set_prefs(Prefs { wallets: Some("modern".into()), ..Default::default() }).await.unwrap();
    svc.set_prefs(Prefs { domain: Some("buy".into()), ..Default::default() }).await.unwrap();
    assert_eq!(svc.view()["prefs"], json!({"wallets": "modern", "domain": "buy", "site": null}));
    assert!(svc
        .set_prefs(Prefs { site: Some("moon".into()), ..Default::default() })
        .await
        .is_err());
    svc.reset_prefs().await.unwrap();
    assert_eq!(svc.view()["prefs"]["wallets"], json!(null));
}
