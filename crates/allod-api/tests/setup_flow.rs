//! HTTP-level tests of first-run setup (SPEC §3.2–3.3).

use std::net::SocketAddr;
use std::sync::Arc;

use allod_api::{AppState, AuthState, LdkSource, TrustedSetupProxy};
use allod_types::{DeploymentInfo, DeploymentMode, Platform};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nostr::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:8029";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn app(dir: &tempfile::TempDir) -> Router {
    app_with_proxy(dir, None).await
}

async fn app_with_proxy(
    dir: &tempfile::TempDir,
    trusted_setup_proxy: Option<Arc<TrustedSetupProxy>>,
) -> Router {
    let db =
        allod_db::Db::connect(&format!("sqlite://{}/t.db", dir.path().display())).await.unwrap();
    std::fs::create_dir_all(dir.path().join("handoff")).unwrap();
    let setup = Arc::new(
        allod_core::SetupService::new(
            db.clone(),
            allod_core::SetupConfig {
                mode: DeploymentMode::Managed,
                secrets_dir: dir.path().join("secrets"),
                handoff_dir: dir.path().join("handoff"),
                managed_ldk_address: "ldk-server:3536".into(),
                network: None,
                bitcoind: None,
            },
        )
        .await
        .unwrap(),
    );
    allod_api::router(AppState {
        version: "test".into(),
        db,
        ldk: LdkSource::new(None, Some(setup.clone()), None),
        deployment: DeploymentInfo {
            mode: DeploymentMode::Managed,
            platform: Platform::Docker,
            chain_mode: None,
            node_explorer_url: None,
            skip_backup_check: false,
        },
        auth: Arc::new(AuthState::new()),
        public_url: None,
        setup,
        trusted_setup_proxy,
        fiat: Arc::new(allod_core::FiatService::new()),
        wallet: Default::default(),
        apps: Default::default(),
        contacts: Default::default(),
        push: Default::default(),
        fast_sync: None,
        host_network: None,
        chain_source: None,
        gossip: std::sync::Arc::new(allod_api::GossipMonitor::new(None)),
    })
}

#[tokio::test]
async fn authenticated_platform_proxy_can_open_setup_without_capability_in_url() {
    let dir = tempfile::tempdir().unwrap();
    let app =
        app_with_proxy(&dir, Some(Arc::new(TrustedSetupProxy::new("127.0.0.1".into())))).await;

    let mut direct = Request::get("/api/setup/claim").body(Body::empty()).unwrap();
    direct.extensions_mut().insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 1], 1234))));
    assert_eq!(call(&app, direct).await.0, StatusCode::UNAUTHORIZED);

    let mut proxied = Request::get("/api/setup/claim").body(Body::empty()).unwrap();
    proxied.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))));
    let response = app.clone().oneshot(proxied).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response.headers().contains_key(header::SET_COOKIE));
}

fn nip98(keys: &Keys, path: &str, body: &[u8]) -> String {
    let mut tags = vec![
        Tag::parse(["u", &format!("http://{HOST}{path}")]).unwrap(),
        Tag::parse(["method", "POST"]).unwrap(),
    ];
    if !body.is_empty() {
        tags.push(Tag::parse(["payload", &hex::encode(Sha256::digest(body))]).unwrap());
    }
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags(tags)
        .custom_created_at(Timestamp::from(now() as u64))
        .finalize(keys)
        .unwrap();
    format!("Nostr {}", B64.encode(event.as_json()))
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let value =
        if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

async fn setup_cookie(app: &Router, dir: &tempfile::TempDir) -> String {
    let token = std::fs::read_to_string(dir.path().join("secrets/setup_bootstrap")).unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/setup/claim")
                .header(header::HOST, HOST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"token": token.trim()}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    resp.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

fn post_signed(keys: &Keys, path: &str, body: &str, session: Option<&str>) -> Request<Body> {
    let mut b = Request::post(path)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, nip98(keys, path, body.as_bytes()));
    if let Some(s) = session {
        b = b.header("x-allod-session", s);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn post_setup(keys: &Keys, body: &str, cookie: &str) -> Request<Body> {
    let mut req = post_signed(keys, "/api/setup", body, None);
    req.headers_mut().insert(header::COOKIE, cookie.parse().unwrap());
    req
}

#[tokio::test]
async fn create_setup_then_reveal_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(&dir).await;
    let owner = Keys::generate();
    let setup_cookie = setup_cookie(&app, &dir).await;

    let (status, info) = call(&app, Request::get("/api/info").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["setup_completed"], false);

    // Before setup, guarded routes answer SETUP_REQUIRED.
    let (status, err) = call(&app, Request::get("/api/owners").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"]["code"], "SETUP_REQUIRED");

    let body = json!({"mode": "create"}).to_string();
    let (status, created) = call(&app, post_setup(&owner, &body, &setup_cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let phrase = created["recovery_phrase"].as_str().unwrap().to_string();
    assert_eq!(phrase.split_whitespace().count(), 24);
    let token = created["token"].as_str().unwrap().to_string();
    assert_eq!(created["pubkey"], owner.public_key().to_hex());
    assert!(!dir.path().join("secrets/setup_bootstrap").exists());

    // The phrase was handed to ldk-server.
    let handed = std::fs::read_to_string(dir.path().join("handoff/keys_mnemonic")).unwrap();
    assert_eq!(handed.trim(), phrase);

    // Setup cannot run twice, not even by the same key.
    let (status, _) = call(&app, post_setup(&owner, &body, &setup_cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (_, info) = call(&app, Request::get("/api/info").body(Body::empty()).unwrap()).await;
    assert_eq!(info["setup_completed"], true);

    let progress = Request::get("/api/setup/progress")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, p) = call(&app, progress).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["seed_handoff"], "waiting_for_ldk_server");

    // Revealing the phrase needs a fresh signature.
    let plain = Request::post("/api/recovery-phrase")
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&app, plain).await.0, StatusCode::UNAUTHORIZED);
    let (status, revealed) =
        call(&app, post_signed(&owner, "/api/recovery-phrase", "", Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{revealed}");
    assert_eq!(revealed["recovery_phrase"], phrase);
}

fn bearer(method: &str, path: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

#[tokio::test]
async fn generated_phrase_stays_unverified_until_checked() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(&dir).await;
    let owner = Keys::generate();
    let setup_cookie = setup_cookie(&app, &dir).await;
    let body = json!({"mode": "create"}).to_string();
    let (_, created) = call(&app, post_setup(&owner, &body, &setup_cookie)).await;
    let token = created["token"].as_str().unwrap().to_string();
    let words: Vec<String> =
        created["recovery_phrase"].as_str().unwrap().split(' ').map(String::from).collect();

    let (status, b) = call(&app, bearer("GET", "/api/backup", &token, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["verified"], false);

    // Too few words, then a wrong word: still unverified.
    let few = json!({"words": {"0": words[0]}});
    let (status, _) = call(&app, bearer("POST", "/api/backup/verify", &token, Some(few))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let wrong = json!({"words": {"0": words[0], "5": words[5], "9": "zzzz"}});
    let (status, err) = call(&app, bearer("POST", "/api/backup/verify", &token, Some(wrong))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "WRONG_WORDS");
    let (_, b) = call(&app, bearer("GET", "/api/backup", &token, None)).await;
    assert_eq!(b["verified"], false);

    let right = json!({"words": {"0": words[0], "5": words[5].to_uppercase(), "23": words[23]}});
    let (status, _) = call(&app, bearer("POST", "/api/backup/verify", &token, Some(right))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, b) = call(&app, bearer("GET", "/api/backup", &token, None)).await;
    assert_eq!(b["verified"], true);
}

#[tokio::test]
async fn setup_requires_a_valid_signature_and_body() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(&dir).await;
    let owner = Keys::generate();
    let setup_cookie = setup_cookie(&app, &dir).await;

    let unsigned = Request::post("/api/setup")
        .header(header::HOST, HOST)
        .body(Body::from(json!({"mode": "create"}).to_string()))
        .unwrap();
    assert_eq!(call(&app, unsigned).await.0, StatusCode::UNAUTHORIZED);

    // Any valid Nostr key still needs host-issued browser claim.
    assert_eq!(
        call(&app, post_signed(&owner, "/api/setup", r#"{"mode":"create"}"#, None)).await.0,
        StatusCode::UNAUTHORIZED
    );

    let bad_phrase = json!({"mode": "restore", "recovery_phrase": "not a phrase"}).to_string();
    assert_eq!(
        call(&app, post_setup(&owner, &bad_phrase, &setup_cookie)).await.0,
        StatusCode::BAD_REQUEST
    );
    let stranger = Keys::generate();
    assert_eq!(
        call(&app, post_setup(&stranger, r#"{"mode":"create"}"#, &setup_cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let wrong_mode = json!({
        "mode": "standalone", "ldk_server_address": "x:1",
        "ldk_server_tls_cert_pem": "", "ldk_server_macaroon_hex": ""
    })
    .to_string();
    assert_eq!(
        call(&app, post_setup(&owner, &wrong_mode, &setup_cookie)).await.0,
        StatusCode::BAD_REQUEST
    );

    let restore = json!({
        "mode": "restore",
        "recovery_phrase": "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        "birthday_height": 100
    })
    .to_string();
    let (status, body) = call(&app, post_setup(&owner, &restore, &setup_cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.get("recovery_phrase").is_none(), "a restored phrase is never echoed back");
    // The owner typed the phrase in, so it counts as backed up.
    let token = body["token"].as_str().unwrap();
    let (_, b) = call(&app, bearer("GET", "/api/backup", token, None)).await;
    assert_eq!(b["verified"], true);
}

#[tokio::test]
async fn rotating_bootstrap_invalidates_existing_browser_claim() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(&dir).await;
    let cookie = setup_cookie(&app, &dir).await;
    std::fs::write(dir.path().join("secrets/setup_bootstrap"), format!("{}\n", "aa".repeat(32)))
        .unwrap();
    let status = call(
        &app,
        Request::get("/api/setup/claim")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fiat_settings_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let db =
        allod_db::Db::connect(&format!("sqlite://{}/t.db", dir.path().display())).await.unwrap();
    std::fs::create_dir_all(dir.path().join("handoff")).unwrap();
    let setup = Arc::new(
        allod_core::SetupService::new(
            db.clone(),
            allod_core::SetupConfig {
                mode: DeploymentMode::Managed,
                secrets_dir: dir.path().join("secrets"),
                handoff_dir: dir.path().join("handoff"),
                managed_ldk_address: "ldk-server:3536".into(),
                network: None,
                bitcoind: None,
            },
        )
        .await
        .unwrap(),
    );
    let fiat = Arc::new(allod_core::FiatService::new());
    // No network in tests: preload the source the settings will use.
    fiat.preload(
        allod_core::RateSource::Yadio,
        [("EUR".to_string(), 73000.0), ("ARS".to_string(), 1.3e8)].into(),
    )
    .await;
    let auth = Arc::new(AuthState::new());
    let app = allod_api::router(AppState {
        version: "test".into(),
        db: db.clone(),
        ldk: LdkSource::new(None, Some(setup.clone()), None),
        deployment: DeploymentInfo {
            mode: DeploymentMode::Managed,
            platform: Platform::Docker,
            chain_mode: None,
            node_explorer_url: None,
            skip_backup_check: false,
        },
        auth: auth.clone(),
        public_url: None,
        setup: setup.clone(),
        trusted_setup_proxy: None,
        fiat,
        wallet: Default::default(),
        apps: Default::default(),
        contacts: Default::default(),
        push: Default::default(),
        fast_sync: None,
        host_network: None,
        chain_source: None,
        gossip: std::sync::Arc::new(allod_api::GossipMonitor::new(None)),
    });
    let owner = Keys::generate();
    setup.run(allod_core::SetupRequest::Create, &owner.public_key().to_hex(), now()).await.unwrap();
    let (token, _) = auth.issue_session(&owner.public_key().to_hex(), now()).unwrap();
    let bearer = format!("Bearer {token}");

    let get = |path: &str| {
        Request::get(path).header(header::AUTHORIZATION, &bearer).body(Body::empty()).unwrap()
    };
    let (_, s) = call(&app, get("/api/settings")).await;
    assert_eq!(s["fiat_currency"], "USD", "USD until the owner picks another");
    assert_eq!(s["fiat_source"], "yadio");

    let (status, list) = call(&app, get("/api/fiat/currencies")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["currencies"], json!(["ARS", "EUR"]));

    let patch = |body: Value| {
        Request::patch("/api/settings")
            .header(header::AUTHORIZATION, &bearer)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    assert_eq!(call(&app, patch(json!({"fiat_currency": "XYZ"}))).await.0, StatusCode::BAD_REQUEST);
    let (status, s) = call(&app, patch(json!({"fiat_currency": "ars"}))).await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["fiat_currency"], "ARS");
    let (_, r) = call(&app, get("/api/fiat/rate")).await;
    assert_eq!(r["btc_price"], 1.3e8);
    let (_, s) = call(&app, patch(json!({"fiat_currency": "none"}))).await;
    assert_eq!(s["fiat_currency"], "none");
}
