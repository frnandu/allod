use std::path::PathBuf;
use std::sync::Arc;

use allod_core::SetupService;
use allod_db::Db;
use allod_ln::{LdkServerBackend, LdkServerConfig, LightningBackend};
use allod_types::DeploymentInfo;
use tokio::sync::Mutex;

/// Shared request state.
#[derive(Clone)]
pub struct AppState {
    /// Allod version string.
    pub version: String,
    /// Database pool.
    pub db: Db,
    /// Lazily connected ldk-server client.
    pub ldk: LdkSource,
    /// Deployment info for `/api/info`.
    pub deployment: DeploymentInfo,
    /// In-memory sessions and login challenges.
    pub auth: Arc<crate::auth::AuthState>,
    /// `ALLOD_PUBLIC_URL`, used to check NIP-98 `u` tags.
    pub public_url: Option<String>,
    /// First-run setup and node credentials.
    pub setup: Arc<SetupService>,
    /// Fiat exchange rates (server-side, cached).
    pub fiat: Arc<allod_core::FiatService>,
    /// The wallet, set once the Lightning node is reachable.
    pub wallet: Arc<tokio::sync::OnceCell<Arc<allod_core::Wallet>>>,
    /// NWC connections, set together with the wallet once setup is complete.
    pub apps: Arc<tokio::sync::OnceCell<Arc<allod_core::AppService>>>,
    /// Nostr contacts and profiles (set at startup; absent in tests).
    pub contacts: Arc<tokio::sync::OnceCell<Arc<allod_nwc::contacts::Contacts>>>,
    /// Fast sync (AssumeUTXO) for the bundled bitcoind; None when not offered.
    pub fast_sync: Option<Arc<allod_core::fastsync::FastSync>>,
}

/// Where the Lightning backend comes from: a fixed backend (tests), explicit
/// ldk-server credential files from the environment (`ALLOD_LDK_SERVER_*`), or what
/// setup stored (SPEC §4.3). The client is built on first success, cached, and its
/// event stream started. `lsps2_lsp` mirrors the configured LSPS2 LSP (SPEC §2.3) so
/// the backend can report just-in-time receive as available.
#[derive(Clone)]
pub struct LdkSource {
    inner: Arc<Inner>,
}

struct Inner {
    fixed: Option<Arc<dyn LightningBackend>>,
    override_files: Option<(String, PathBuf, PathBuf)>,
    setup: Option<Arc<SetupService>>,
    lsps2_lsp: Option<String>,
    cached: Mutex<Option<Arc<dyn LightningBackend>>>,
}

impl LdkSource {
    /// `override_files` is `(address, tls_cert_file, macaroon_file)`.
    pub fn new(
        override_files: Option<(String, PathBuf, PathBuf)>,
        setup: Option<Arc<SetupService>>,
        lsps2_lsp: Option<String>,
    ) -> Self {
        LdkSource {
            inner: Arc::new(Inner {
                fixed: None,
                override_files,
                setup,
                lsps2_lsp,
                cached: Mutex::new(None),
            }),
        }
    }

    /// Always returns `backend` (tests).
    pub fn fixed(backend: Arc<dyn LightningBackend>) -> Self {
        LdkSource {
            inner: Arc::new(Inner {
                fixed: Some(backend),
                override_files: None,
                setup: None,
                lsps2_lsp: None,
                cached: Mutex::new(None),
            }),
        }
    }

    /// Returns the backend, or why there is none yet.
    pub async fn backend(&self) -> Result<Arc<dyn LightningBackend>, String> {
        if let Some(b) = &self.inner.fixed {
            return Ok(b.clone());
        }
        let mut cached = self.inner.cached.lock().await;
        if let Some(backend) = cached.as_ref() {
            return Ok(backend.clone());
        }
        let config = if let Some((address, cert, macaroon)) = &self.inner.override_files {
            LdkServerConfig::from_files(address.clone(), cert, macaroon)
                .map_err(|e| e.to_string())?
        } else if let Some(setup) = &self.inner.setup {
            match setup.node_credentials().await.map_err(|e| e.to_string())? {
                Some(c) => c,
                None if !setup.check_complete().await => return Err("setup not complete".into()),
                None => return Err("ldk-server credentials not provisioned yet".into()),
            }
        } else {
            return Err("ldk-server not configured".into());
        };
        let backend = LdkServerBackend::new(&config)
            .map_err(|e| e.to_string())?
            .with_lsps2_lsp(self.inner.lsps2_lsp.clone());
        backend.start_events();
        let backend: Arc<dyn LightningBackend> = Arc::new(backend);
        *cached = Some(backend.clone());
        Ok(backend)
    }
}
