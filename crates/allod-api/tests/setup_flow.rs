//! HTTP-level tests of first-run setup (SPEC §3.2–3.3).

use std::sync::Arc;

use allod_api::{AppState, AuthState, LdkSource};
use allod_types::{DeploymentInfo, DeploymentMode, Platform};
use axum::body::Body;
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
        },
        auth: Arc::new(AuthState::new()),
        public_url: None,
        setup,
        fiat: Arc::new(allod_core::FiatService::new()),
        wallet: Default::default(),
        apps: Default::default(),
        contacts: Default::default(),
        fast_sync: None,
        gossip: std::sync::Arc::new(allod_api::GossipMonitor::new(None)),
    })
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

#[tokio::test]
async fn create_setup_then_reveal_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(&dir).await;
    let owner = Keys::generate();

    let (status, info) = call(&app, Request::get("/api/info").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["setup_completed"], false);

    // Before setup, guarded routes answer SETUP_REQUIRED.
    let (status, err) = call(&app, Request::get("/api/owners").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"]["code"], "SETUP_REQUIRED");

    let body = json!({"mode": "create"}).to_string();
    let (status, created) = call(&app, post_signed(&owner, "/api/setup", &body, None)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let phrase = created["recovery_phrase"].as_str().unwrap().to_string();
    assert_eq!(phrase.split_whitespace().count(), 24);
    let token = created["token"].as_str().unwrap().to_string();
    assert_eq!(created["pubkey"], owner.public_key().to_hex());

    // The phrase was handed to ldk-server.
    let handed = std::fs::read_to_string(dir.path().join("handoff/keys_mnemonic")).unwrap();
    assert_eq!(handed.trim(), phrase);

    // Setup cannot run twice, not even by the same key.
    let (status, _) = call(&app, post_signed(&owner, "/api/setup", &body, None)).await;
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

#[tokio::test]
async fn setup_requires_a_valid_signature_and_body() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(&dir).await;
    let owner = Keys::generate();

    let unsigned = Request::post("/api/setup")
        .header(header::HOST, HOST)
        .body(Body::from(json!({"mode": "create"}).to_string()))
        .unwrap();
    assert_eq!(call(&app, unsigned).await.0, StatusCode::UNAUTHORIZED);

    let bad_phrase = json!({"mode": "restore", "recovery_phrase": "not a phrase"}).to_string();
    assert_eq!(
        call(&app, post_signed(&owner, "/api/setup", &bad_phrase, None)).await.0,
        StatusCode::BAD_REQUEST
    );
    let wrong_mode = json!({
        "mode": "standalone", "ldk_server_address": "x:1",
        "ldk_server_tls_cert_pem": "", "ldk_server_macaroon_hex": ""
    })
    .to_string();
    assert_eq!(
        call(&app, post_signed(&owner, "/api/setup", &wrong_mode, None)).await.0,
        StatusCode::BAD_REQUEST
    );

    let restore = json!({
        "mode": "restore",
        "recovery_phrase": "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        "birthday_height": 100
    })
    .to_string();
    let (status, body) = call(&app, post_signed(&owner, "/api/setup", &restore, None)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.get("recovery_phrase").is_none(), "a restored phrase is never echoed back");
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
        },
        auth: auth.clone(),
        public_url: None,
        setup: setup.clone(),
        fiat,
        wallet: Default::default(),
        apps: Default::default(),
        contacts: Default::default(),
        fast_sync: None,
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
