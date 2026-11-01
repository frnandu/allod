use std::collections::HashMap;
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
    /// Network graph counts and the gossip sidecar status.
    pub gossip: Arc<GossipMonitor>,
}

/// `GET /status` of the local gossip sidecar (`allod-gossip`), see SPEC §2.3.2 rule 5.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SidecarStatus {
    pub state: String,
    pub ready: bool,
    pub started_at: u64,
    pub ready_at: Option<u64>,
    pub initial_sync_secs: Option<u64>,
    pub peers_configured: usize,
    pub peers_connected: usize,
    pub graph_channels: usize,
    pub graph_nodes: usize,
    pub last_graph_change_at: Option<u64>,
    pub verify: SidecarVerify,
    pub snapshot: SidecarSnapshot,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SidecarVerify {
    pub enabled: bool,
    pub scans_completed: u64,
    pub scanning: bool,
    pub scan_progress: Option<f64>,
    pub last_scan_started_at: Option<u64>,
    pub last_scan_finished_at: Option<u64>,
    pub last_scan_secs: Option<u64>,
    pub last_scan_height: Option<u32>,
    pub last_scan_txouts: Option<u64>,
    pub unspent_channels: usize,
    pub missing_channels: usize,
    pub pending_channels: usize,
    pub unspent_capacity_sats: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SidecarSnapshot {
    pub served: u64,
    pub last_served_at: Option<u64>,
    pub last_bytes: Option<u64>,
    pub channels: Option<u64>,
    pub nodes: Option<u64>,
    pub updates: Option<u64>,
    pub channels_with_capacity: Option<u64>,
    pub build_ms: Option<u64>,
}

/// Sidecar status for `/api/setup/progress`.
#[derive(Debug, Clone, Serialize)]
pub struct SidecarView {
    pub status: Option<SidecarStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GossipStatus {
    /// `p2p` (ldk-node's own gossip) or `sidecar` (Rapid Gossip Sync from `allod-gossip`).
    pub source: &'static str,
    pub state: &'static str,
    pub channels: u64,
    pub nodes: u64,
    pub last_graph_change_at: Option<u64>,
    /// Channels validated per hour over the last ~minute, `None` until a second sample exists.
    pub channels_per_hour: Option<f64>,
    pub nodes_per_hour: Option<f64>,
    pub sidecar: Option<SidecarStatus>,
    pub sidecar_error: Option<String>,
}

struct GossipCache {
    graph: Option<(tokio::time::Instant, GraphStats)>,
    /// Sample at least a minute old, so the growth rate is not just jitter between polls.
    baseline: Option<(tokio::time::Instant, GraphStats)>,
    sidecar: Option<(tokio::time::Instant, SidecarStatus)>,
    sidecar_error: Option<String>,
    last_graph_change_at: Option<u64>,
}

/// Short-lived cache: ldk-server graph list RPCs return every ID, so UI polling must
/// not fan those large responses out on every browser refresh.
pub struct GossipMonitor {
    sidecar_url: Option<String>,
    http: reqwest::Client,
    cache: Mutex<GossipCache>,
}

impl GossipMonitor {
    /// `sidecar_url` is the `allod-gossip` `/status` endpoint; when set, ldk-server takes
    /// its graph from that sidecar and the status reports it as the source.
    pub fn new(sidecar_url: Option<String>) -> Self {
        Self {
            sidecar_url: sidecar_url.filter(|u| !u.is_empty()),
            http: reqwest::Client::new(),
            cache: Mutex::new(GossipCache {
                graph: None,
                baseline: None,
                sidecar: None,
                sidecar_error: None,
                last_graph_change_at: None,
            }),
        }
    }

    async fn refresh_sidecar(&self, cache: &mut GossipCache, now: tokio::time::Instant) {
        let Some(url) = &self.sidecar_url else { return };
        let stale = cache
            .sidecar
            .as_ref()
            .is_none_or(|(at, _)| now.duration_since(*at) >= std::time::Duration::from_secs(5));
        if !stale && cache.sidecar_error.is_none() {
            return;
        }
        match fetch_json::<SidecarStatus>(&self.http, url).await {
            Ok(status) => {
                cache.sidecar = Some((now, status));
                cache.sidecar_error = None;
            }
            Err(error) => cache.sidecar_error = Some(error),
        }
    }

    /// Aliases the gossip sidecar knows for `node_ids` (its `/aliases` endpoint, next to
    /// `/status`). `None` when no sidecar is configured or it did not answer.
    pub async fn aliases(&self, node_ids: &[String]) -> Option<HashMap<String, String>> {
        let base = self.sidecar_url.as_deref()?.trim_end_matches('/').trim_end_matches("/status");
        let mut out = HashMap::new();
        for chunk in node_ids.chunks(200) {
            let url = format!("{base}/aliases?ids={}", chunk.join(","));
            match fetch_json::<HashMap<String, String>>(&self.http, &url).await {
                Ok(found) => out.extend(found),
                Err(error) => {
                    tracing::debug!(%error, "gossip sidecar aliases unavailable");
                    return None;
                }
            }
        }
        Some(out)
    }

    /// The gossip sidecar's status without asking ldk-server, for the setup screen: the
    /// sidecar runs before ldk-server starts. `None` when no sidecar is configured.
    pub async fn sidecar(&self) -> Option<SidecarView> {
        self.sidecar_url.as_ref()?;
        let mut cache = self.cache.lock().await;
        self.refresh_sidecar(&mut cache, tokio::time::Instant::now()).await;
        Some(SidecarView {
            status: cache.sidecar.as_ref().map(|(_, s)| s.clone()),
            error: cache.sidecar_error.clone(),
        })
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

        self.refresh_sidecar(&mut cache, now).await;

        let graph = cache.graph.as_ref().map(|(_, graph)| *graph).unwrap_or_default();
        // Growth rate over a window of at least a minute: the number the UI shows as progress.
        let mut channels_per_hour = None;
        let mut nodes_per_hour = None;
        match cache.baseline {
            Some((at, baseline))
                if now.duration_since(at) >= std::time::Duration::from_secs(60) =>
            {
                let hours = now.duration_since(at).as_secs_f64() / 3600.0;
                channels_per_hour =
                    Some((graph.channels.saturating_sub(baseline.channels) as f64 / hours).round());
                nodes_per_hour =
                    Some((graph.nodes.saturating_sub(baseline.nodes) as f64 / hours).round());
                cache.baseline = Some((now, graph));
            }
            Some(_) => {}
            None => cache.baseline = Some((now, graph)),
        }
        let recent_graph_change = cache
            .last_graph_change_at
            .is_some_and(|at| (crate::auth::unix_now() as u64).saturating_sub(at) < 300);
        let sidecar = cache.sidecar.as_ref().map(|(_, sidecar)| sidecar.clone());
        let sidecar_mode = self.sidecar_url.is_some();
        let sidecar_busy = sidecar.as_ref().is_some_and(|s| {
            matches!(s.state.as_str(), "starting" | "syncing" | "verifying") || s.verify.scanning
        });
        let sidecar_failed = cache.sidecar_error.is_some()
            || sidecar.as_ref().is_some_and(|s| s.state == "degraded");
        let state = if sidecar_failed {
            "degraded"
        } else if sidecar_busy || (!sidecar_mode && recent_graph_change) {
            "learning"
        } else if graph.channels > 0 {
            "active"
        } else {
            "waiting"
        };
        Ok(GossipStatus {
            source: if sidecar_mode { "sidecar" } else { "p2p" },
            state,
            channels: graph.channels,
            nodes: graph.nodes,
            last_graph_change_at: cache.last_graph_change_at,
            channels_per_hour,
            nodes_per_hour,
            sidecar,
            sidecar_error: cache.sidecar_error.clone(),
        })
    }
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<T, String> {
    let response = http
        .get(url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    response.json::<T>().await.map_err(|e| e.to_string())
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
    /// Node aliases from the node's own graph, for peers and channels.
    aliases: crate::aliases::AliasCache,
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
                aliases: Default::default(),
            }),
        }
    }

    /// In-memory alias cache shared by every request.
    pub(crate) fn aliases(&self) -> &crate::aliases::AliasCache {
        &self.inner.aliases
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
                aliases: Default::default(),
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
