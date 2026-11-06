use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::*;

/// What the mock deSEC saw, and how it answers.
#[derive(Default)]
struct Mock {
    activated: bool,
    captcha_ok: bool,
    registrations: Vec<Value>,
    logins: usize,
    logouts: usize,
    policies: Vec<Value>,
    rrset_puts: Vec<Value>,
    updates: Vec<(String, String, String)>,
    ipv4: String,
}

type M = Arc<Mutex<Mock>>;

async fn captcha() -> Json<Value> {
    Json(json!({"id": "c1", "challenge": "iVBORw0KGgo=", "kind": "image"}))
}

async fn register(State(m): State<M>, Json(body): Json<Value>) -> impl IntoResponse {
    let mut m = m.lock().unwrap();
    if !m.captcha_ok {
        return (StatusCode::BAD_REQUEST, Json(json!({"captcha": ["wrong"]})));
    }
    m.registrations.push(body);
    (StatusCode::ACCEPTED, Json(json!({"detail": "Welcome!"})))
}

async fn login(State(m): State<M>, Json(body): Json<Value>) -> impl IntoResponse {
    let mut m = m.lock().unwrap();
    m.logins += 1;
    let reg = m.registrations.last().cloned().unwrap_or_default();
    if !m.activated || body["password"] != reg["password"] {
        return (StatusCode::FORBIDDEN, Json(json!({"detail": "no"})));
    }
    (StatusCode::OK, Json(json!({"id": "l1", "token": "login-token"})))
}

async fn logout(State(m): State<M>) -> StatusCode {
    m.lock().unwrap().logouts += 1;
    StatusCode::NO_CONTENT
}

async fn create_token(headers: HeaderMap) -> impl IntoResponse {
    assert_eq!(headers["authorization"], "Token login-token");
    (StatusCode::CREATED, Json(json!({"id": "t1", "token": "long-token"})))
}

async fn policy(State(m): State<M>, Path(id): Path<String>, Json(body): Json<Value>) -> StatusCode {
    assert_eq!(id, "t1");
    m.lock().unwrap().policies.push(body);
    StatusCode::CREATED
}

async fn domain(headers: HeaderMap, Path(name): Path<String>) -> StatusCode {
    if headers["authorization"] != "Token long-token" {
        return StatusCode::UNAUTHORIZED;
    }
    if name == "mynode.dedyn.io" {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn rrsets(
    State(m): State<M>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> StatusCode {
    assert_eq!(headers["authorization"], "Token long-token");
    m.lock().unwrap().rrset_puts.push(json!({"domain": name, "rrsets": body}));
    StatusCode::OK
}

async fn update(
    State(m): State<M>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> &'static str {
    m.lock().unwrap().updates.push((
        q["hostname"].clone(),
        q["myipv4"].clone(),
        q["myipv6"].clone(),
    ));
    "good"
}

async fn checkip(State(m): State<M>) -> String {
    m.lock().unwrap().ipv4.clone()
}

async fn mock() -> (M, String) {
    let m: M = Arc::new(Mutex::new(Mock {
        captcha_ok: true,
        ipv4: "203.0.113.7".into(),
        ..Default::default()
    }));
    let app = Router::new()
        .route("/api/v1/captcha/", post(captcha))
        .route("/api/v1/auth/", post(register))
        .route("/api/v1/auth/login/", post(login))
        .route("/api/v1/auth/logout/", post(logout))
        .route("/api/v1/auth/tokens/", post(create_token))
        .route("/api/v1/auth/tokens/{id}/policies/rrsets/", post(policy))
        .route("/api/v1/domains/{name}/", get(domain))
        .route("/api/v1/domains/{name}/rrsets/", put(rrsets))
        .route("/update/", get(update))
        .route("/checkip4/", get(checkip))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (m, format!("http://{addr}"))
}

async fn service(base: &str, dir: &std::path::Path) -> Arc<PublicNameService> {
    let db = Db::connect(&format!("sqlite://{}/t.db", dir.display())).await.unwrap();
    PublicNameService::load(
        db,
        SecretStore::new(dir.join("secrets")),
        PublicNameConfig {
            endpoints: Endpoints::under(base),
            skip_label_dns_check: true,
            // Nothing listens here, so the reachability check fails fast.
            tls_check_addr: Some("127.0.0.1:9".into()),
            ..Default::default()
        },
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap()
}

fn wait_out_login_gap(svc: &PublicNameService) {
    *svc.last_login.lock().unwrap() = None;
}

#[tokio::test]
async fn signup_waits_for_the_email_link_then_creates_a_restricted_token() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&base, dir.path()).await;

    let c = svc.captcha().await.unwrap();
    assert_eq!(c.id, "c1");
    svc.signup("MyNode", " owner@example.com ", ("c1", "ABCDE")).await.unwrap();
    {
        let m = m.lock().unwrap();
        let reg = &m.registrations[0];
        assert_eq!(reg["domain"], "mynode.dedyn.io");
        assert_eq!(reg["email"], "owner@example.com");
        assert_eq!(reg["captcha"], json!({"id": "c1", "solution": "ABCDE"}));
        assert_eq!(reg["outreach_preference"], false);
        assert_eq!(reg["password"].as_str().unwrap().len(), 64);
    }
    let v = svc.view();
    assert_eq!(v["mode"], "desec");
    assert_eq!(v["name"], "mynode.dedyn.io");
    assert_eq!(v["exposure"], "private");
    assert_eq!(v["desec"]["pending"]["email"], "owner@example.com");
    assert_eq!(v["desec"]["has_token"], false);

    // Link not clicked yet: login refused, still pending.
    svc.poll_activation().await;
    assert_eq!(svc.view()["desec"]["has_token"], false);
    // Throttled: a second poll right away does not log in again.
    svc.poll_activation().await;
    assert_eq!(m.lock().unwrap().logins, 1);

    m.lock().unwrap().activated = true;
    wait_out_login_gap(&svc);
    svc.poll_activation().await;
    let v = svc.view();
    assert_eq!(v["desec"]["has_token"], true);
    assert!(v["desec"]["pending"].is_null());
    let m = m.lock().unwrap();
    assert_eq!(m.logouts, 1, "the login token is thrown away");
    assert_eq!(
        m.policies,
        vec![
            json!({"domain": null, "subname": null, "type": null, "perm_write": false}),
            json!({"domain": "mynode.dedyn.io", "subname": null, "type": null, "perm_write": true}),
        ]
    );
    let token = std::fs::read_to_string(dir.path().join("secrets").join(DESEC_TOKEN)).unwrap();
    assert_eq!(token.trim(), "long-token");
}

#[tokio::test]
async fn a_wrong_captcha_is_reported_and_nothing_changes() {
    let (m, base) = mock().await;
    m.lock().unwrap().captcha_ok = false;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&base, dir.path()).await;
    let e = svc.signup("mynode", "o@example.com", ("c1", "x")).await.unwrap_err();
    assert!(matches!(e, PublicNameError::Desec(DesecError::Captcha)), "{e:?}");
    assert_eq!(svc.view()["mode"], "none");
    assert!(svc.signup("-bad", "o@example.com", ("c1", "x")).await.is_err());
    assert!(svc.signup("ok", "not-an-email", ("c1", "x")).await.is_err());
}

#[tokio::test]
async fn private_exposure_removes_address_records_once() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&base, dir.path()).await;
    svc.use_token("mynode.dedyn.io", "long-token").await.unwrap();
    svc.reconcile().await;
    svc.reconcile().await;
    let m = m.lock().unwrap();
    assert!(m.updates.is_empty(), "no IP is published");
    let puts: Vec<_> =
        m.rrset_puts.iter().filter(|p| p["rrsets"].as_array().unwrap().len() == 2).collect();
    assert_eq!(puts.len(), 1);
    assert_eq!(
        puts[0]["rrsets"][0],
        json!({"subname": "", "type": "A", "ttl": 3600, "records": []})
    );
    assert_eq!(puts[0]["rrsets"][1]["type"], "AAAA");
    assert!(!svc.allows_certificate("mynode.dedyn.io"));
}

#[tokio::test]
async fn public_exposure_follows_the_ip_and_allows_a_certificate() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&base, dir.path()).await;
    svc.use_token("mynode.dedyn.io", "long-token").await.unwrap();
    svc.set_exposure(Exposure::Public).await.unwrap();
    svc.reconcile().await;
    svc.reconcile().await;
    m.lock().unwrap().ipv4 = "198.51.100.9".into();
    svc.reconcile().await;
    {
        let m = m.lock().unwrap();
        assert_eq!(
            m.updates,
            vec![
                ("mynode.dedyn.io".into(), "203.0.113.7".into(), "preserve".into()),
                ("mynode.dedyn.io".into(), "198.51.100.9".into(), "preserve".into()),
            ],
            "updated only when the address changes"
        );
        let aaaa_deletes = m
            .rrset_puts
            .iter()
            .filter(|p| {
                p["rrsets"] == json!([{"subname": "", "type": "AAAA", "ttl": 3600, "records": []}])
            })
            .count();
        assert_eq!(aaaa_deletes, 1);
    }
    assert!(svc.allows_certificate("MyNode.dedyn.io."));
    assert!(!svc.allows_certificate("other.dedyn.io"));
    let v = svc.view();
    assert_eq!(v["status"]["ipv4"], "198.51.100.9");
    assert_eq!(v["status"]["reach"]["state"], "waiting_for_certificate");
}

#[tokio::test]
async fn a_token_for_another_domain_is_refused() {
    let (_m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&base, dir.path()).await;
    assert!(svc.use_token("other.dedyn.io", "long-token").await.is_err());
    assert!(svc.use_token("mynode.dedyn.io", "wrong").await.is_err());
    assert_eq!(svc.view()["mode"], "none");
}

#[tokio::test]
async fn turning_off_deletes_the_bip353_records_and_the_token() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&base, dir.path()).await;
    svc.use_token("mynode.dedyn.io", "long-token").await.unwrap();
    {
        let mut s = svc.stored();
        s.published.insert(bip353_subname("alice"), "bitcoin:?lno=lno1x".into());
        svc.save(s).await.unwrap();
    }
    svc.disable().await.unwrap();
    let m = m.lock().unwrap();
    let last = m.rrset_puts.last().unwrap();
    assert_eq!(
        last["rrsets"],
        json!([{"subname": "alice.user._bitcoin-payment", "type": "TXT", "ttl": 3600, "records": []}])
    );
    assert!(!dir.path().join("secrets").join(DESEC_TOKEN).exists());
    assert_eq!(svc.view()["mode"], "none");
    assert!(svc.lightning_domain().is_none());
}

#[tokio::test]
async fn own_domain_and_env_url() {
    let dir = tempfile::tempdir().unwrap();
    let svc = service("http://127.0.0.1:9", dir.path()).await;
    svc.use_own("https://Pay.Example.com/", Exposure::Public).await.unwrap();
    assert_eq!(svc.lightning_domain().as_deref(), Some("pay.example.com"));
    assert!(svc.allows_certificate("pay.example.com"));

    let db = Db::connect(&format!("sqlite://{}/e.db", dir.path().display())).await.unwrap();
    let env = PublicNameService::load(
        db,
        SecretStore::new(dir.path().join("s2")),
        PublicNameConfig {
            env_public_url: Some("https://wallet.example.org".into()),
            ..Default::default()
        },
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(env.lightning_domain().as_deref(), Some("wallet.example.org"));
    assert!(matches!(
        env.use_own("a.example.com", Exposure::Private).await,
        Err(PublicNameError::Conflict(_))
    ));
    assert_eq!(env.view()["env_public_url"], "https://wallet.example.org");
}

#[test]
fn txt_changes_cover_new_changed_and_removed_names() {
    let published: BTreeMap<String, String> =
        [("a".into(), "1".into()), ("b".into(), "2".into())].into_iter().collect();
    let desired: BTreeMap<String, String> =
        [("a".into(), "1".into()), ("b".into(), "3".into()), ("c".into(), "4".into())]
            .into_iter()
            .collect();
    let changes = txt_changes(&published, &desired);
    let names: Vec<_> = changes.iter().map(|c| c["subname"].as_str().unwrap()).collect();
    assert_eq!(names, ["b", "c"]);
    let removed = txt_changes(&desired, &BTreeMap::new());
    assert_eq!(removed.len(), 3);
    assert!(removed.iter().all(|c| c["records"] == json!([])));
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

/// A deSEC name with a token, plus a wallet and accounts on the mock node.
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
            endpoints: Endpoints::under(base),
            skip_label_dns_check: true,
            ..Default::default()
        },
        wallet_cell,
        apps_cell,
    )
    .await
    .unwrap();
    (svc, apps)
}

#[tokio::test]
async fn bip353_records_follow_the_accounts() {
    let (m, base) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let (svc, apps) = service_with_wallet(&base, dir.path()).await;
    svc.use_token("mynode.dedyn.io", "long-token").await.unwrap();
    let account = |name: &str, user: Option<&str>| crate::apps::NewApp {
        name: name.into(),
        kind: Some("account".into()),
        lud16_username: user.map(str::to_string),
        ..Default::default()
    };
    let (alice, _) = apps.create(account("Alice", Some("alice")), None, now()).await.unwrap();
    apps.create(account("No address", None), None, now()).await.unwrap();
    svc.reconcile().await;

    let txt_puts = |m: &Mock| -> Vec<Value> {
        m.rrset_puts
            .iter()
            .flat_map(|p| p["rrsets"].as_array().unwrap().clone())
            .filter(|r| r["type"] == "TXT")
            .collect()
    };
    {
        let m = m.lock().unwrap();
        let txt = txt_puts(&m);
        assert_eq!(txt.len(), 1, "{txt:?}");
        assert_eq!(txt[0]["subname"], "alice.user._bitcoin-payment");
        let record = txt[0]["records"][0].as_str().unwrap();
        assert!(record.starts_with("\"bitcoin:?lno=lno1"), "{record}");
    }
    let v = svc.view();
    assert_eq!(v["records"][0]["address"], "alice@mynode.dedyn.io");
    assert_eq!(v["records"][0]["published"], true);

    // Nothing changed: nothing is written again.
    svc.reconcile().await;
    assert_eq!(txt_puts(&m.lock().unwrap()).len(), 1);

    // Closing the account deletes its record.
    svc.wallet.get().unwrap().close_account(alice.id, false).await.unwrap();
    svc.reconcile().await;
    let m = m.lock().unwrap();
    let txt = txt_puts(&m);
    assert_eq!(txt.len(), 2);
    assert_eq!(
        txt[1],
        json!({"subname": "alice.user._bitcoin-payment", "type": "TXT", "ttl": 3600, "records": []})
    );
}
