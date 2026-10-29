//! API tests for node (M2) and wallet (M3) routes against the mock backend.

use std::sync::Arc;
use std::time::Duration;

use allod_api::{AppState, AuthState, LdkSource};
use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use allod_ln::LightningBackend;
use allod_types::{DeploymentInfo, DeploymentMode, Platform};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    app: Router,
    token: String,
    node: Arc<MockBackend>,
    auth: Arc<AuthState>,
    db: allod_db::Db,
    _dir: tempfile::TempDir,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/t.db", dir.path().display());
    let db = allod_db::Db::connect(&url).await.unwrap();
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
    setup.run(allod_core::SetupRequest::Create, &"ab".repeat(32), now()).await.unwrap();
    let node = Arc::new(MockBackend::new());
    let backend: Arc<dyn LightningBackend> = node.clone();
    let pool = allod_db::Db::any_pool(&url).await.unwrap();
    let words = setup.recovery_phrase().unwrap().unwrap();
    let keys =
        allod_keys::NostrKeys::from_phrase(&allod_keys::RecoveryPhrase::parse(&words).unwrap())
            .unwrap();
    let apps = allod_core::AppService::new(
        pool.clone(),
        Arc::new(keys),
        vec!["wss://relay.example".into()],
        vec![],
    );
    let apps_cell = Arc::new(tokio::sync::OnceCell::new());
    let _ = apps_cell.set(apps);
    let wallet = allod_core::Wallet::new(
        pool,
        backend.clone(),
        allod_core::WalletConfig {
            sync_timeout: Duration::from_millis(300),
            poll_interval: Duration::from_millis(20),
            ..Default::default()
        },
    );
    wallet.reconcile().await.unwrap();
    let cell = Arc::new(tokio::sync::OnceCell::new());
    let _ = cell.set(wallet);
    let auth = Arc::new(AuthState::new());
    let (token, _) = auth.issue_session(&"ab".repeat(32), now()).unwrap();
    let app = allod_api::router(AppState {
        version: "test".into(),
        db: db.clone(),
        ldk: LdkSource::fixed(backend),
        deployment: DeploymentInfo {
            mode: DeploymentMode::Managed,
            platform: Platform::Docker,
            chain_mode: None,
        },
        auth: auth.clone(),
        public_url: None,
        setup,
        fiat: Arc::new(allod_core::FiatService::new()),
        wallet: cell,
        apps: apps_cell,
        contacts: Default::default(),
        fast_sync: None,
    });
    H { app, token, node, auth, db, _dir: dir }
}

impl H {
    async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.req_as(&self.token.clone(), method, path, body).await
    }

    async fn req_as(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self.app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, v)
    }
}

#[tokio::test]
async fn node_channels_and_peers() {
    let h = harness().await;
    let (s, n) = h.req("GET", "/api/node", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(n["network"], "regtest");
    assert_eq!(n["capabilities"]["bolt12"], true);

    let peer = format!("03{}", "44".repeat(32));
    let (s, _) = h
        .req(
            "POST",
            "/api/channels",
            Some(json!({"pubkey": "nope", "address": "x:1", "amount_sat": 50000})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .req(
            "POST",
            "/api/channels",
            Some(json!({"pubkey": peer, "address": "x:1", "amount_sat": 1000})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "below the minimum channel size");
    let (s, opened) = h
        .req(
            "POST",
            "/api/channels",
            Some(json!({"pubkey": format!("{peer}@bob:9735"), "amount_sat": 50000})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{opened}");
    let ucid = opened["user_channel_id"].as_str().unwrap().to_string();
    let (_, list) = h.req("GET", "/api/channels", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (s, _) = h
        .req(
            "PATCH",
            &format!("/api/channels/{ucid}"),
            Some(json!({"counterparty": peer, "fee_ppm": 250})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = h.req("GET", "/api/channels", None).await;
    assert_eq!(list[0]["fee_ppm"], 250);
    let (s, _) = h.req("DELETE", &format!("/api/channels/{ucid}?counterparty={peer}"), None).await;
    assert_eq!(s, StatusCode::ACCEPTED);

    let (s, _) =
        h.req("POST", "/api/peers", Some(json!({"pubkey": format!("{peer}@bob:9735")}))).await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, peers) = h.req("GET", "/api/peers", None).await;
    assert_eq!(peers[0]["node_id"], peer);
    let (s, _) = h.req("DELETE", &format!("/api/peers/{peer}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let audit = h.db.audit_entries(Some("channel_opened"), 5).await.unwrap();
    assert_eq!(audit.len(), 1);
}

#[tokio::test]
async fn onchain_validation_and_balances() {
    let h = harness().await;
    let (s, a) = h.req("POST", "/api/onchain/address", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(a["address"].as_str().unwrap().starts_with("bcrt1"));
    let (s, _) = h
        .req("POST", "/api/onchain/send", Some(json!({"address": "bcrt1q", "amount_sat": 100})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "dust");
    let (s, _) = h
        .req(
            "POST",
            "/api/onchain/send",
            Some(json!({"address": "bcrt1q", "amount_sat": 1000, "send_all": true})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, sent) = h
        .req(
            "POST",
            "/api/onchain/send",
            Some(json!({"address": "bcrt1q", "amount_sat": 10000, "fee_rate_sat_vb": 2})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sent["txid"].as_str().unwrap().len(), 64);

    let (s, b) = h.req("GET", "/api/balances", None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["lightning"]["total_msat"], 1_000_000_000u64);
    assert_eq!(b["main_spendable_msat"], 1_000_000_000i64);
    assert_eq!(b["accounts_total_msat"], 0);
}

#[tokio::test]
async fn invoices_payments_and_history() {
    let h = harness().await;
    let (s, inv) = h
        .req("POST", "/api/invoices", Some(json!({"amount_msat": 21000, "description": "coffee"})))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert!(inv["payment_request"].as_str().unwrap().starts_with("lnbcrt"));

    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 1000 };
    let external = MockBackend::make_invoice(Some(50_000), &sha256_hex(b"x"));
    let (s, preview) =
        h.req("POST", "/api/payments/decode", Some(json!({"payment": external}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(preview["kind"], "bolt11");
    assert_eq!(preview["amount_msat"], 50_000);
    assert_eq!(preview["network_ok"], true);

    let (s, paid) = h.req("POST", "/api/payments", Some(json!({"payment": external}))).await;
    assert_eq!(s, StatusCode::OK, "{paid}");
    assert_eq!(paid["state"], "SETTLED");
    let (s, again) = h.req("POST", "/api/payments", Some(json!({"payment": external}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{again}");

    let (_, list) = h.req("GET", "/api/transactions", None).await;
    assert_eq!(list["total"], 1, "unpaid invoices are hidden by default");
    let (_, all) = h.req("GET", "/api/transactions?include_unpaid=true", None).await;
    assert_eq!(all["total"], 2);
    let id = paid["public_id"].as_str().unwrap();
    let (s, one) = h.req("GET", &format!("/api/transactions/{id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["fee_msat"], 1000);
    let (s, labeled) = h
        .req("PATCH", &format!("/api/transactions/{id}/labels"), Some(json!({"labels": ["rent"]})))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(labeled["labels"], json!(["rent"]));

    let (s, r) = h.req("POST", "/api/receive", Some(json!({"description": "tips"}))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(r["bip321"].as_str().unwrap().starts_with("bitcoin:?lno="));
    let (_, offers) = h.req("GET", "/api/offers", None).await;
    assert_eq!(offers.as_array().unwrap().len(), 1);

    let (s, check) = h.req("GET", "/api/ledger/check", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(check["ok"], true, "{check}");
}

#[tokio::test]
async fn readonly_tokens_cannot_move_money() {
    let h = harness().await;
    let (_, created) =
        h.req("POST", "/api/tokens", Some(json!({"name": "ro", "permission": "readonly"}))).await;
    let ro = created["token"].as_str().unwrap().to_string();
    let (s, _) = h.req_as(&ro, "GET", "/api/balances", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) =
        h.req_as(&ro, "POST", "/api/payments", Some(json!({"payment": "lnbcrtmock1h00"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h.req_as(&ro, "POST", "/api/channels", Some(json!({"pubkey": "x"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn event_tickets_are_single_use() {
    let h = harness().await;
    let (s, t) = h.req("POST", "/api/events/ticket", None).await;
    assert_eq!(s, StatusCode::OK);
    let ticket = t["ticket"].as_str().unwrap().to_string();
    let resp = h
        .app
        .clone()
        .oneshot(Request::get(format!("/api/events?ticket={ticket}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    drop(resp);
    let again = h
        .app
        .clone()
        .oneshot(Request::get(format!("/api/events?ticket={ticket}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED);
    assert!(!h.auth.take_ticket("nope", now()));
}

#[tokio::test]
async fn connections_crud() {
    let h = harness().await;
    let (s, v) = h
        .req(
            "POST",
            "/api/apps",
            Some(json!({"name": "Shop", "scopes": ["pay_invoice", "get_balance"], "max_amount_msat": 50_000_000, "budget_renewal": "weekly"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let uri = v["pairing_uri"].as_str().unwrap();
    let wallet_pk = v["app"]["wallet_pubkey"].as_str().unwrap();
    assert!(uri.starts_with(&format!(
        "nostr+walletconnect://{wallet_pk}?relay=wss%3A%2F%2Frelay.example&secret="
    )));
    assert_eq!(v["app"]["budget"]["total_budget"], 50_000_000);
    assert_eq!(v["app"]["budget"]["renewal_period"], "weekly");
    let id = v["app"]["id"].as_i64().unwrap();

    // Wallet keys differ per connection.
    let (_, v2) =
        h.req("POST", "/api/apps", Some(json!({"name": "Other", "scopes": ["get_balance"]}))).await;
    assert_ne!(v2["app"]["wallet_pubkey"], v["app"]["wallet_pubkey"]);
    // A client pubkey given: no pairing URI.
    let (s, v3) = h
        .req(
            "POST",
            "/api/apps",
            Some(json!({"name": "Own key", "pubkey": "cd".repeat(32), "scopes": []})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(v3["pairing_uri"].is_null());
    // sign_message is not grantable to isolated connections.
    let (s, _) = h
        .req(
            "POST",
            "/api/apps",
            Some(json!({"name": "Iso", "isolated": true, "scopes": ["sign_message"]})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, list) = h.req("GET", "/api/apps", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 3);

    let (s, v) = h
        .req(
            "PATCH",
            &format!("/api/apps/{id}"),
            Some(json!({"name": "Shop 2", "scopes": ["get_balance"], "expires_at": null})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "Shop 2");
    assert_eq!(v["scopes"], json!(["get_balance"]));

    let (s, v) = h.req("POST", &format!("/api/apps/{id}/regenerate"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["pairing_uri"].as_str().unwrap() != uri);

    let (s, v) = h.req("GET", &format!("/api/apps/{id}"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["requests"].is_array());
    let (s, _) = h.req("DELETE", &format!("/api/apps/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = h.req("GET", &format!("/api/apps/{id}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
