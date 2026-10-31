use std::path::PathBuf;
use std::sync::Arc;

use allod_core::SetupService;
use allod_db::Db;
use allod_ln::types::GraphStats;
use allod_ln::{LdkServerBackend, LdkServerConfig, LightningBackend};
use allod_types::DeploymentInfo;
use serde::{Deserialize, Serialize};
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
    /// P2P gossip graph and pruned-block recovery status.
    pub gossip: Arc<GossipMonitor>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProxyStatus {
    pub rpc_requests: u64,
    pub local_block_hits: u64,
    pub pruned_block_requests: u64,
    pub recovery_jobs: u64,
    pub blocks_recovered: u64,
    pub blocks_failed: u64,
    pub bytes_downloaded: u64,
    pub blocks_queued: usize,
    pub blocks_fetching: usize,
    pub eligible_bitcoin_peers: usize,
    pub last_fetch_at: Option<u64>,
    pub last_error_at: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GossipStatus {
    pub source: &'static str,
    pub state: &'static str,
    pub channels: u64,
    pub nodes: u64,
    pub last_graph_change_at: Option<u64>,
    pub proxy: Option<ProxyStatus>,
    pub proxy_error: Option<String>,
}

struct GossipCache {
    graph: Option<(tokio::time::Instant, GraphStats)>,
    proxy: Option<(tokio::time::Instant, ProxyStatus)>,
    proxy_error: Option<String>,
    last_graph_change_at: Option<u64>,
}

/// Short-lived cache: ldk-server graph list RPCs return every ID, so UI polling must
/// not fan those large responses out on every browser refresh.
pub struct GossipMonitor {
    proxy_url: Option<String>,
    http: reqwest::Client,
    cache: Mutex<GossipCache>,
}

impl GossipMonitor {
    pub fn new(proxy_url: Option<String>) -> Self {
        Self {
            proxy_url,
            http: reqwest::Client::new(),
            cache: Mutex::new(GossipCache {
                graph: None,
                proxy: None,
                proxy_error: None,
                last_graph_change_at: None,
            }),
        }
    }

    pub async fn snapshot(
        &self,
        backend: &dyn LightningBackend,
    ) -> Result<GossipStatus, allod_ln::BackendError> {
        let mut cache = self.cache.lock().await;
        let now = tokio::time::Instant::now();
        let graph_stale = cache
            .graph
            .as_ref()
            .is_none_or(|(at, _)| now.duration_since(*at) >= std::time::Duration::from_secs(30));
        if graph_stale {
            let graph = backend.graph_stats().await?;
            let changed = cache.graph.as_ref().is_none_or(|(_, old)| *old != graph);
            if changed {
                cache.last_graph_change_at = Some(crate::auth::unix_now() as u64);
            }
            cache.graph = Some((now, graph));
        }

        let proxy_stale = cache
            .proxy
            .as_ref()
            .is_none_or(|(at, _)| now.duration_since(*at) >= std::time::Duration::from_secs(5));
        if proxy_stale {
            if let Some(url) = &self.proxy_url {
                match self.http.get(url).timeout(std::time::Duration::from_secs(3)).send().await {
                    Ok(response) => match response.error_for_status() {
                        Ok(response) => match response.json::<ProxyStatus>().await {
                            Ok(status) => {
                                cache.proxy = Some((now, status));
                                cache.proxy_error = None;
                            }
                            Err(error) => cache.proxy_error = Some(error.to_string()),
                        },
                        Err(error) => cache.proxy_error = Some(error.to_string()),
                    },
                    Err(error) => cache.proxy_error = Some(error.to_string()),
                }
            }
        }

        let graph = cache.graph.as_ref().map(|(_, graph)| *graph).unwrap_or_default();
        let proxy = cache.proxy.as_ref().map(|(_, proxy)| proxy.clone());
        let active_recovery =
            proxy.as_ref().is_some_and(|p| p.blocks_queued > 0 || p.blocks_fetching > 0);
        let recent_graph_change = cache
            .last_graph_change_at
            .is_some_and(|at| (crate::auth::unix_now() as u64).saturating_sub(at) < 300);
        let state = if cache.proxy_error.is_some()
            || proxy.as_ref().and_then(|p| p.last_error.as_ref()).is_some()
        {
            "degraded"
        } else if active_recovery || recent_graph_change {
            "learning"
        } else if graph.channels > 0 {
            "active"
        } else {
            "waiting"
        };
        Ok(GossipStatus {
            source: "p2p",
            state,
            channels: graph.channels,
            nodes: graph.nodes,
            last_graph_change_at: cache.last_graph_change_at,
            proxy,
            proxy_error: cache.proxy_error.clone(),
        })
    }
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
