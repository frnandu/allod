//! Shared harness for API tests against the mock backend.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use allod_api::{AppState, AuthState, LdkSource};
use allod_ln::mock::MockBackend;
use allod_ln::LightningBackend;
use allod_types::{DeploymentInfo, DeploymentMode, Platform};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tower::ServiceExt;

pub struct H {
    pub app: Router,
    pub token: String,
    pub node: Arc<MockBackend>,
    pub auth: Arc<AuthState>,
    pub db: allod_db::Db,
    pub push: Arc<allod_core::PushService>,
    #[allow(dead_code)]
    pub public_name: Arc<allod_core::PublicNameService>,
    pub dir: tempfile::TempDir,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

pub async fn harness() -> H {
    harness_for(&"ab".repeat(32)).await
}

/// A set-up Allod whose owner (and session) is `owner` (hex pubkey).
pub async fn harness_for(owner: &str) -> H {
    harness_with(owner, None, false).await
}

/// A set-up Allod in the given chain mode.
pub async fn harness_chain(chain_mode: allod_types::ChainMode) -> H {
    harness_with(&"ab".repeat(32), Some(chain_mode), false).await
}

/// Like [`harness_chain`], with a standby bitcoind whose control directory is
/// `{dir}/control` (SPEC §2.3.2).
#[allow(dead_code)]
pub async fn harness_standby(chain_mode: allod_types::ChainMode) -> H {
    harness_with(&"ab".repeat(32), Some(chain_mode), true).await
}

async fn harness_with(owner: &str, chain_mode: Option<allod_types::ChainMode>, standby: bool) -> H {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/t.db", dir.path().display());
    let db = allod_db::Db::connect(&url).await.unwrap();
    std::fs::create_dir_all(dir.path().join("handoff")).unwrap();
    let standby_control = standby.then(|| dir.path().join("control"));
    let standby = standby.then(|| {
        std::fs::create_dir_all(dir.path().join("control")).unwrap();
        Arc::new(allod_core::standby_bitcoind::StandbyBitcoind::new(
            dir.path().join("control"),
            "allod_bitcoind_1:8332",
            "ldk",
            "standby-pass",
        ))
    });
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
        .unwrap()
        .with_standby(standby),
    );
    setup.run(allod_core::SetupRequest::Create, owner, now()).await.unwrap();
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
    let pool_for_push = pool.clone();
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
    let push = allod_core::PushService::new(pool_for_push, &[5; 32], wallet.clone(), None);
    let push_cell = Arc::new(tokio::sync::OnceCell::new());
    let _ = push_cell.set(push.clone());
    let cell = Arc::new(tokio::sync::OnceCell::new());
    let _ = cell.set(wallet);
    let public_name = allod_core::PublicNameService::load(
        db.clone(),
        allod_keys::SecretStore::new(dir.path().join("secrets")),
        allod_core::PublicNameConfig {
            platform: Some(Platform::Docker),
            // No deSEC here: tests use an own domain; nothing listens on port 9.
            endpoints: allod_core::public_name::Endpoints::under("http://127.0.0.1:9"),
            tls_check_addr: Some("127.0.0.1:9".into()),
            skip_label_dns_check: true,
            ..Default::default()
        },
        cell.clone(),
        apps_cell.clone(),
    )
    .await
    .unwrap();
    let auth = Arc::new(AuthState::new());
    let (token, _) = auth.issue_session(owner, now()).unwrap();
    let app = allod_api::router(AppState {
        version: "test".into(),
        db: db.clone(),
        ldk: LdkSource::fixed(backend),
        deployment: DeploymentInfo {
            mode: DeploymentMode::Managed,
            platform: Platform::Docker,
            chain_mode,
            node_explorer_url: None,
            skip_backup_check: false,
        },
        auth: auth.clone(),
        public_url: None,
        setup,
        trusted_setup_proxy: None,
        fiat: Arc::new(allod_core::FiatService::new()),
        wallet: cell,
        apps: apps_cell,
        contacts: Default::default(),
        push: push_cell,
        // The standby bitcoind runs the fast sync helper in the same control directory.
        fast_sync: standby_control
            .clone()
            .map(|d| Arc::new(allod_core::fastsync::FastSync::new(d))),
        host_network: None,
        chain_source: Some(Arc::new(allod_core::chain_source::ChainSourceStore::new(
            dir.path().join("handoff"),
        ))),
        public_name: Some(public_name.clone()),
        gossip: Arc::new(allod_api::GossipMonitor::new(None)),
    });
    H { app, token, node, auth, db, push, public_name, dir }
}

impl H {
    pub async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.req_as(&self.token.clone(), method, path, body).await
    }

    pub async fn req_as(
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
