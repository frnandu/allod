//! HTTP-level tests of owner login, sessions, owners and API tokens (SPEC §11.1).

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

struct Harness {
    app: Router,
    db: allod_db::Db,
    setup: Arc<allod_core::SetupService>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
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
    let state = AppState {
        version: "test".into(),
        db: db.clone(),
        ldk: LdkSource::new(None, Some(setup.clone()), None),
        setup: setup.clone(),
        deployment: DeploymentInfo {
            mode: DeploymentMode::Standalone,
            platform: Platform::Docker,
            chain_mode: None,
            node_explorer_url: None,
        },
        auth: Arc::new(AuthState::new()),
        public_url: None,
        fiat: Arc::new(allod_core::FiatService::new()),
        wallet: Default::default(),
        apps: Default::default(),
        contacts: Default::default(),
        fast_sync: None,
        gossip: std::sync::Arc::new(allod_api::GossipMonitor::new(None)),
    };
    Harness { app: allod_api::router(state), db, setup, _dir: dir }
}

/// Completes setup with `keys` as the first owner.
async fn setup_owner(h: &Harness, keys: &Keys) {
    h.setup
        .run(allod_core::SetupRequest::Create, &keys.public_key().to_hex(), now())
        .await
        .unwrap();
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn nip98(keys: &Keys, path: &str, method: &str, body: &[u8]) -> String {
    let mut tags = vec![
        Tag::parse(["u", &format!("http://{HOST}{path}")]).unwrap(),
        Tag::parse(["method", method]).unwrap(),
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

fn get(path: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path).header(header::HOST, HOST);
    if let Some(t) = bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

async fn login(h: &Harness, keys: &Keys) -> (StatusCode, Value) {
    let (_, c) = call(&h.app, get("/api/auth/challenge", None)).await;
    let body = json!({"challenge": c["challenge"]}).to_string();
    let req = Request::post("/api/auth/login")
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, nip98(keys, "/api/auth/login", "POST", body.as_bytes()))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    call(&h.app, req).await
}

#[tokio::test]
async fn owner_logs_in_and_out() {
    let h = harness().await;
    let owner = Keys::generate();
    setup_owner(&h, &owner).await;

    let (status, body) = login(&h, &owner).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["pubkey"], owner.public_key().to_hex());

    let (status, who) = call(&h.app, get("/api/auth/session", Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(who["kind"], "owner");

    let logout = Request::post("/api/auth/logout")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&h.app, logout).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&h.app, get("/api/auth/session", Some(&token))).await.0,
        StatusCode::UNAUTHORIZED
    );

    let audit = h.db.audit_entries(Some("login"), 10).await.unwrap();
    assert_eq!(audit.len(), 1);
}

#[tokio::test]
async fn strangers_and_replays_are_rejected() {
    let h = harness().await;
    let owner = Keys::generate();
    setup_owner(&h, &owner).await;

    let (status, _) = login(&h, &Keys::generate()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Same challenge twice.
    let (_, c) = call(&h.app, get("/api/auth/challenge", None)).await;
    let body = json!({"challenge": c["challenge"]}).to_string();
    let auth = nip98(&owner, "/api/auth/login", "POST", body.as_bytes());
    let req = || {
        Request::post("/api/auth/login")
            .header(header::HOST, HOST)
            .header(header::AUTHORIZATION, auth.clone())
            .body(Body::from(body.clone()))
            .unwrap()
    };
    assert_eq!(call(&h.app, req()).await.0, StatusCode::OK);
    assert_eq!(call(&h.app, req()).await.0, StatusCode::UNAUTHORIZED);

    // Signed for another host.
    let wrong = Request::post("/api/auth/login")
        .header(header::HOST, "evil.example")
        .header(header::AUTHORIZATION, nip98(&owner, "/api/auth/login", "POST", body.as_bytes()))
        .body(Body::from(body.clone()))
        .unwrap();
    assert_eq!(call(&h.app, wrong).await.0, StatusCode::UNAUTHORIZED);

    assert_eq!(call(&h.app, get("/api/owners", None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&h.app, get("/api/owners", Some("garbage"))).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn malformed_login_flood_is_limited_without_audit_growth() {
    let h = harness().await;
    let owner = Keys::generate();
    setup_owner(&h, &owner).await;

    for i in 0..40 {
        let req = Request::post("/api/auth/login")
            .header(header::HOST, HOST)
            .header(header::AUTHORIZATION, "Nostr !!!")
            .body(Body::from("{}"))
            .unwrap();
        let status = call(&h.app, req).await.0;
        assert_eq!(
            status,
            if i < 30 { StatusCode::UNAUTHORIZED } else { StatusCode::TOO_MANY_REQUESTS }
        );
    }
    assert!(h.db.audit_entries(Some("login_failed"), 100).await.unwrap().is_empty());

    // A different peer still has its own login budget.
    let (_, challenge) = call(&h.app, get("/api/auth/challenge", None)).await;
    let body = json!({"challenge": challenge["challenge"]}).to_string();
    let mut req = Request::post("/api/auth/login")
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, nip98(&owner, "/api/auth/login", "POST", body.as_bytes()))
        .body(Body::from(body))
        .unwrap();
    req.extensions_mut().insert(axum::extract::connect_info::ConnectInfo(
        "127.0.0.2:1234".parse::<std::net::SocketAddr>().unwrap(),
    ));
    assert_eq!(call(&h.app, req).await.0, StatusCode::OK);
}

#[tokio::test]
async fn adding_owners_needs_reauthentication() {
    let h = harness().await;
    let owner = Keys::generate();
    setup_owner(&h, &owner).await;
    let (_, body) = login(&h, &owner).await;
    let token = body["token"].as_str().unwrap().to_string();

    let second = Keys::generate();
    let npub = ToBech32::to_bech32(&second.public_key()).unwrap();
    let payload = json!({"pubkey": npub, "label": "phone"}).to_string();

    // A session alone is not enough.
    let plain = Request::post("/api/owners")
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(payload.clone()))
        .unwrap();
    assert_eq!(call(&h.app, plain).await.0, StatusCode::UNAUTHORIZED);

    // Re-authentication signed by someone else.
    let other = Request::post("/api/owners")
        .header(header::HOST, HOST)
        .header("x-allod-session", &token)
        .header(header::AUTHORIZATION, nip98(&second, "/api/owners", "POST", payload.as_bytes()))
        .body(Body::from(payload.clone()))
        .unwrap();
    assert_eq!(call(&h.app, other).await.0, StatusCode::FORBIDDEN);

    let ok = Request::post("/api/owners")
        .header(header::HOST, HOST)
        .header("x-allod-session", &token)
        .header(header::AUTHORIZATION, nip98(&owner, "/api/owners", "POST", payload.as_bytes()))
        .body(Body::from(payload.clone()))
        .unwrap();
    let (status, created) = call(&h.app, ok).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["pubkey"], second.public_key().to_hex());
    assert_eq!(h.db.owner_count().await.unwrap(), 2);

    // The new owner can log in; removing yourself works while another owner exists.
    assert_eq!(login(&h, &second).await.0, StatusCode::OK);
    let me = owner.public_key().to_hex();
    let path = format!("/api/owners/{me}");
    let del = Request::delete(&path)
        .header(header::HOST, HOST)
        .header("x-allod-session", &token)
        .header(header::AUTHORIZATION, nip98(&owner, &path, "DELETE", b""))
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&h.app, del).await.0, StatusCode::NO_CONTENT);
    // A removed owner's session stops working at once.
    assert_eq!(
        call(&h.app, get("/api/auth/session", Some(&token))).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn api_tokens_respect_permissions() {
    let h = harness().await;
    let owner = Keys::generate();
    setup_owner(&h, &owner).await;
    let (_, body) = login(&h, &owner).await;
    let session = body["token"].as_str().unwrap().to_string();

    let create = Request::post("/api/tokens")
        .header(header::AUTHORIZATION, format!("Bearer {session}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": "ci", "permission": "readonly"}).to_string()))
        .unwrap();
    let (status, created) = call(&h.app, create).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let api_token = created["token"].as_str().unwrap().to_string();
    assert!(api_token.starts_with("allod_"));

    let (status, who) = call(&h.app, get("/api/auth/session", Some(&api_token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(who["kind"], "api_token");
    assert_eq!(who["permission"], "readonly");

    // Tokens cannot list or mint tokens.
    assert_eq!(call(&h.app, get("/api/tokens", Some(&api_token))).await.0, StatusCode::FORBIDDEN);

    // Only the hash is stored.
    let stored = h.db.api_tokens().await.unwrap();
    assert_ne!(stored[0].token_hash, api_token);

    let id = created["id"].as_i64().unwrap();
    let del = Request::delete(format!("/api/tokens/{id}"))
        .header(header::AUTHORIZATION, format!("Bearer {session}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&h.app, del).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&h.app, get("/api/auth/session", Some(&api_token))).await.0,
        StatusCode::UNAUTHORIZED
    );
}
