//! Local gossip sidecar: builds the Lightning graph from P2P gossip, checks channel
//! funding outputs against bitcoind's UTXO set, and serves the result to ldk-node as
//! Rapid Gossip Sync snapshots (SPEC §2.3.2, rule 5).

mod peers;
mod rgs;
mod verify;

use std::collections::HashMap;
use std::io::{IsTerminal, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clap::Parser;
use lightning::bitcoin::constants::ChainHash;
use lightning::bitcoin::secp256k1::PublicKey;
use lightning::bitcoin::Network;
use lightning::ln::msgs::SocketAddress;
use lightning::ln::peer_handler::{
    ErroringMessageHandler, IgnoringMessageHandler, MessageHandler, PeerManager,
};
use lightning::routing::gossip::{NetworkGraph, P2PGossipSync};
use lightning::routing::utxo::{UtxoLookup, UtxoLookupError, UtxoResult};
use lightning::sign::KeysManager;
use lightning::util::logger::{Level, Logger, Record};
use lightning::util::ser::{ReadableArgs, Writeable};
use lightning_net_tokio::SocketDescriptor;
use rand::seq::SliceRandom;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::peers::{Candidate, Source};
use crate::rgs::{Inclusion, SnapshotStats};
use crate::verify::{Bitcoind, Check, Verdict};

const GRAPH_FILE: &str = "network_graph.bin";
const VERDICTS_FILE: &str = "verdicts.json";
const SEED_FILE: &str = "node_seed";
/// A rebuilt snapshot is reused this long; ldk-node asks once per hour.
const SNAPSHOT_CACHE: Duration = Duration::from_secs(60);
const PERSIST_INTERVAL: Duration = Duration::from_secs(600);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(15);
/// A peer that failed to connect is not retried for this long (static peers excepted).
const PEER_RETRY_AFTER: Duration = Duration::from_secs(600);
/// DNS seeds are asked at most this often.
const DNS_REQUERY: Duration = Duration::from_secs(600);
/// How many top-connected graph nodes the random peer choice is drawn from.
const GRAPH_POOL: usize = 40;
const SCAN_RETRY: Duration = Duration::from_secs(300);
/// Interval of the "still alive" summary log line.
const SUMMARY_INTERVAL: Duration = Duration::from_secs(300);
/// How long `ALLOD_GOSSIP_VERIFY=auto` waits for bitcoind before serving unverified.
const BITCOIND_WAIT_ATTEMPTS: u32 = 24;

/// Funding-check state; `auto` starts in `DECIDING` until bitcoind answers or gives up.
const VERIFY_DECIDING: u8 = 0;
const VERIFY_ON: u8 = 1;
const VERIFY_OFF: u8 = 2;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, env = "ALLOD_GOSSIP_LISTEN", default_value = "0.0.0.0:8090")]
    listen: SocketAddr,
    /// bitcoin | signet | testnet4 | regtest
    #[arg(long, env = "ALLOD_GOSSIP_NETWORK", default_value = "bitcoin")]
    network: String,
    #[arg(long, env = "ALLOD_GOSSIP_DATA_DIR", default_value = "/data")]
    data_dir: PathBuf,
    /// Comma-separated `pubkey@host:port` peers that are always kept connected.
    /// Optional: peers are otherwise discovered (DNS seeds, then our own graph).
    #[arg(long, env = "ALLOD_GOSSIP_PEERS", default_value = "")]
    peers: String,
    /// BOLT 10 DNS seeds. Empty: the network's default seeds; `none`: no seeding.
    #[arg(long, env = "ALLOD_GOSSIP_DNS_SEEDS", default_value = "")]
    dns_seeds: String,
    /// Number of discovered gossip peers kept connected (besides ALLOD_GOSSIP_PEERS).
    #[arg(long, env = "ALLOD_GOSSIP_TARGET_PEERS", default_value_t = 6)]
    target_peers: usize,
    /// SOCKS5 proxy (host:port) for `.onion` peers; without it they are skipped.
    #[arg(long, env = "ALLOD_GOSSIP_TOR_PROXY")]
    tor_proxy: Option<String>,
    /// Check funding outputs against bitcoind's UTXO set and only serve channels that
    /// pass: `true`, `false`, or `auto` (on when bitcoind answers at startup).
    #[arg(long, env = "ALLOD_GOSSIP_VERIFY", default_value = "auto")]
    verify: String,
    #[arg(long, env = "ALLOD_GOSSIP_BITCOIND_URL")]
    bitcoind_url: Option<String>,
    #[arg(long, env = "BITCOIND_RPC_USER", default_value = "")]
    bitcoind_user: String,
    #[arg(long, env = "BITCOIND_RPC_PASS", default_value = "", hide_env_values = true)]
    bitcoind_pass: String,
    /// Seconds between UTXO scans. Each scan reads the whole UTXO set (minutes).
    #[arg(long, env = "ALLOD_GOSSIP_SCAN_INTERVAL_SECS", default_value_t = 1800)]
    scan_interval_secs: u64,
    /// The graph counts as synced once it has not changed for this long.
    #[arg(long, env = "ALLOD_GOSSIP_SETTLE_SECS", default_value_t = 45)]
    settle_secs: u64,
}

struct Log;

impl Logger for Log {
    fn log(&self, record: Record) {
        let module = record.module_path;
        match record.level {
            Level::Gossip | Level::Trace => tracing::trace!(module, "{}", record.args),
            Level::Debug => tracing::debug!(module, "{}", record.args),
            Level::Info => tracing::info!(module, "{}", record.args),
            Level::Warn => tracing::warn!(module, "{}", record.args),
            Level::Error => tracing::error!(module, "{}", record.args),
        }
    }
}

/// P2P gossip without chain lookups, as ldk-node does with Electrum or Esplora.
/// Funding is checked separately in batches (see [`verify`]).
struct NoLookup;

impl UtxoLookup for NoLookup {
    fn get_utxo(
        &self,
        _chain_hash: &ChainHash,
        _short_channel_id: u64,
        _notifier: Arc<lightning::util::wakers::Notifier>,
    ) -> UtxoResult {
        UtxoResult::Sync(Err(UtxoLookupError::UnknownTx))
    }
}

type Graph = NetworkGraph<Arc<Log>>;
type Gossip = P2PGossipSync<Arc<Graph>, NoLookup, Arc<Log>>;
type Peers = PeerManager<
    SocketDescriptor,
    ErroringMessageHandler,
    Arc<Gossip>,
    IgnoringMessageHandler,
    Arc<Log>,
    IgnoringMessageHandler,
    Arc<KeysManager>,
    IgnoringMessageHandler,
>;

#[derive(Debug, Clone, Default, Serialize)]
struct VerifyStatus {
    enabled: bool,
    scans_completed: u64,
    scanning: bool,
    scan_progress: Option<f64>,
    last_scan_started_at: Option<u64>,
    last_scan_finished_at: Option<u64>,
    last_scan_secs: Option<u64>,
    last_scan_height: Option<u32>,
    last_scan_txouts: Option<u64>,
    unspent_channels: usize,
    missing_channels: usize,
    pending_channels: usize,
    unspent_capacity_sats: u64,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct SnapshotStatus {
    served: u64,
    last_served_at: Option<u64>,
    last_bytes: Option<usize>,
    channels: Option<usize>,
    nodes: Option<usize>,
    updates: Option<usize>,
    channels_with_capacity: Option<usize>,
    build_ms: Option<u64>,
}

/// `GET /status`. Allod reads this for the node page.
#[derive(Debug, Clone, Default, Serialize)]
struct Status {
    /// starting | syncing | verifying | ready | degraded
    state: &'static str,
    ready: bool,
    network: String,
    started_at: u64,
    ready_at: Option<u64>,
    initial_sync_secs: Option<u64>,
    /// Wanted peer count: discovered target plus static peers.
    peers_configured: usize,
    peers_connected: usize,
    /// Connected peers by origin: static, graph, dns.
    peers_by_source: HashMap<&'static str, usize>,
    peer_list: Vec<PeerStatus>,
    dns_seeds: Vec<String>,
    dns_last_query_at: Option<u64>,
    dns_nodes_found: usize,
    dns_errors: Vec<String>,
    graph_channels: usize,
    graph_nodes: usize,
    last_graph_change_at: Option<u64>,
    verify: VerifyStatus,
    snapshot: SnapshotStatus,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PeerStatus {
    node_id: String,
    addr: String,
    source: Source,
}

#[derive(Serialize, Deserialize, Default)]
struct StoredVerdicts {
    height: Option<u32>,
    scanned_at: Option<u64>,
    verdicts: Vec<(u64, Verdict)>,
}

struct App {
    network: Network,
    chain_hash: ChainHash,
    data_dir: PathBuf,
    graph: Arc<Graph>,
    peers: Arc<Peers>,
    keys: Arc<KeysManager>,
    static_peers: Vec<Candidate>,
    seeds: Vec<String>,
    target_peers: usize,
    /// Origin and address of every peer we dialed, for status and logs.
    dialed: Mutex<HashMap<PublicKey, (Source, String)>>,
    tor_proxy: Option<String>,
    /// bitcoind client when funding checks may run (`true` or `auto`).
    bitcoind: Option<Bitcoind>,
    /// `VERIFY_DECIDING`, `VERIFY_ON` or `VERIFY_OFF`.
    verify: AtomicU8,
    started: Instant,
    settled: AtomicBool,
    settle: Duration,
    ready_at: AtomicU64,
    verdicts: RwLock<HashMap<u64, Verdict>>,
    status: Mutex<Status>,
    snapshot: Mutex<Option<(Instant, Arc<Vec<u8>>)>>,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn parse_network(name: &str) -> Result<Network, String> {
    match name {
        "bitcoin" | "mainnet" => Ok(Network::Bitcoin),
        "signet" => Ok(Network::Signet),
        "testnet4" => Ok(Network::Testnet4),
        "testnet" => Ok(Network::Testnet),
        "regtest" => Ok(Network::Regtest),
        other => Err(format!("unknown network {other}")),
    }
}

/// Writes `bytes` to `path` atomically (temp file + rename).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)
}

fn load_seed(dir: &Path) -> std::io::Result<[u8; 32]> {
    let path = dir.join(SEED_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(seed);
        }
    }
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    write_atomic(&path, &seed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(seed)
}

fn load_graph(dir: &Path, network: Network, log: Arc<Log>) -> Graph {
    let path = dir.join(GRAPH_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => match Graph::read(&mut bytes.as_slice(), Arc::clone(&log)) {
            Ok(graph) if graph.get_chain_hash() == ChainHash::using_genesis_block(network) => {
                let ro = graph.read_only();
                tracing::info!(
                    channels = ro.channels().len(),
                    nodes = ro.nodes().len(),
                    "loaded saved graph"
                );
                drop(ro);
                graph
            }
            Ok(_) => {
                tracing::warn!("saved graph is for another network; starting empty");
                NetworkGraph::new(network, log)
            }
            Err(e) => {
                tracing::warn!(error = ?e, "saved graph unreadable; starting empty");
                NetworkGraph::new(network, log)
            }
        },
        Err(_) => {
            tracing::info!("no saved graph; starting empty");
            NetworkGraph::new(network, log)
        }
    }
}

impl App {
    fn verify_state(&self) -> u8 {
        self.verify.load(Ordering::Relaxed)
    }

    fn ready(&self) -> bool {
        self.settled.load(Ordering::Relaxed)
            && match self.verify_state() {
                VERIFY_ON => !self.verdicts.read().expect("lock").is_empty(),
                VERIFY_OFF => true,
                _ => false,
            }
    }

    fn persist(&self) {
        let graph = self.graph.encode();
        if let Err(e) = write_atomic(&self.data_dir.join(GRAPH_FILE), &graph) {
            tracing::warn!(error = %e, "could not save graph");
        }
        if self.bitcoind.is_some() {
            let status = self.status.lock().expect("lock").verify.clone();
            let stored = StoredVerdicts {
                height: status.last_scan_height,
                scanned_at: status.last_scan_finished_at,
                verdicts: self
                    .verdicts
                    .read()
                    .expect("lock")
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect(),
            };
            match serde_json::to_vec(&stored) {
                Ok(bytes) => {
                    if let Err(e) = write_atomic(&self.data_dir.join(VERDICTS_FILE), &bytes) {
                        tracing::warn!(error = %e, "could not save funding checks");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "could not encode funding checks"),
            }
        }
    }

    fn load_verdicts(&self) {
        let Ok(bytes) = std::fs::read(self.data_dir.join(VERDICTS_FILE)) else { return };
        let Ok(stored) = serde_json::from_slice::<StoredVerdicts>(&bytes) else {
            tracing::warn!("saved funding checks unreadable; rescanning");
            return;
        };
        let count = stored.verdicts.len();
        *self.verdicts.write().expect("lock") = stored.verdicts.into_iter().collect();
        let mut status = self.status.lock().expect("lock");
        status.verify.last_scan_height = stored.height;
        status.verify.last_scan_finished_at = stored.scanned_at;
        tracing::info!(channels = count, height = ?stored.height, "loaded saved funding checks");
    }

    /// Recomputes the counters that depend on the graph and the funding checks.
    fn refresh_counts(&self) {
        let graph = self.graph.read_only();
        let channels = graph.channels().len();
        let nodes = graph.nodes().len();
        let verdicts = self.verdicts.read().expect("lock");
        let (mut unspent, mut missing, mut pending, mut capacity) = (0, 0, 0, 0u64);
        for scid in graph.channels().unordered_iter().map(|(scid, _)| scid) {
            match verdicts.get(scid) {
                Some(Verdict::Unspent { funding_sats }) => {
                    unspent += 1;
                    capacity += funding_sats;
                }
                Some(Verdict::Missing) => missing += 1,
                None => pending += 1,
            }
        }
        drop(verdicts);
        drop(graph);
        let mut status = self.status.lock().expect("lock");
        status.graph_channels = channels;
        status.graph_nodes = nodes;
        let connected = self.peers.list_peers();
        status.peers_connected = connected.len();
        let dialed = self.dialed.lock().expect("lock");
        status.peer_list = connected
            .iter()
            .map(|p| {
                let (source, addr) = dialed
                    .get(&p.counterparty_node_id)
                    .cloned()
                    .unwrap_or((Source::Dns, String::new()));
                PeerStatus { node_id: p.counterparty_node_id.to_string(), addr, source }
            })
            .collect();
        drop(dialed);
        let mut by_source: HashMap<&'static str, usize> = HashMap::new();
        for p in &status.peer_list {
            let name = match p.source {
                Source::Static => "static",
                Source::Graph => "graph",
                Source::Dns => "dns",
            };
            *by_source.entry(name).or_default() += 1;
        }
        status.peers_by_source = by_source;
        status.verify.unspent_channels = unspent;
        status.verify.missing_channels = missing;
        status.verify.pending_channels = pending;
        status.verify.unspent_capacity_sats = capacity;
    }

    fn status(&self) -> Status {
        self.refresh_counts();
        let ready = self.ready();
        let mut status = self.status.lock().expect("lock").clone();
        status.ready = ready;
        status.verify.enabled = self.verify_state() == VERIFY_ON;
        let ready_at = self.ready_at.load(Ordering::Relaxed);
        status.ready_at = (ready_at != 0).then_some(ready_at);
        status.state = if status.last_error.is_some() || status.verify.last_error.is_some() {
            "degraded"
        } else if !self.settled.load(Ordering::Relaxed) {
            if status.graph_channels == 0 && status.peers_connected == 0 {
                "starting"
            } else {
                "syncing"
            }
        } else if !ready || status.verify.scanning {
            "verifying"
        } else {
            "ready"
        };
        status
    }

    fn mark_ready_if_needed(&self) {
        if self.ready() && self.ready_at.load(Ordering::Relaxed) == 0 {
            self.ready_at.store(unix_now(), Ordering::Relaxed);
            tracing::info!(secs = self.started.elapsed().as_secs(), "snapshots ready");
        }
    }

    fn build_snapshot(&self) -> (Arc<Vec<u8>>, SnapshotStats, u64) {
        let started = Instant::now();
        let verdicts = self.verdicts.read().expect("lock").clone();
        let verify = self.verify_state() == VERIFY_ON;
        let (bytes, stats) = rgs::encode(&self.graph, self.chain_hash, unix_now(), |scid| {
            if !verify {
                return Inclusion::Include { funding_sats: None };
            }
            match verdicts.get(&scid) {
                Some(Verdict::Unspent { funding_sats }) => {
                    Inclusion::Include { funding_sats: Some(*funding_sats) }
                }
                _ => Inclusion::Skip,
            }
        });
        (Arc::new(bytes), stats, started.elapsed().as_millis() as u64)
    }

    fn snapshot(&self) -> Arc<Vec<u8>> {
        let mut cache = self.snapshot.lock().expect("lock");
        if let Some((at, bytes)) = cache.as_ref() {
            if at.elapsed() < SNAPSHOT_CACHE {
                return Arc::clone(bytes);
            }
        }
        let (bytes, stats, build_ms) = self.build_snapshot();
        *cache = Some((Instant::now(), Arc::clone(&bytes)));
        let mut status = self.status.lock().expect("lock");
        status.snapshot.last_bytes = Some(bytes.len());
        status.snapshot.channels = Some(stats.channels);
        status.snapshot.nodes = Some(stats.nodes);
        status.snapshot.updates = Some(stats.updates);
        status.snapshot.channels_with_capacity = Some(stats.channels_with_capacity);
        status.snapshot.build_ms = Some(build_ms);
        tracing::info!(
            bytes = bytes.len(),
            channels = stats.channels,
            nodes = stats.nodes,
            build_ms,
            "built snapshot"
        );
        bytes
    }
}

async fn connect(app: &Arc<App>, key: PublicKey, addr: &str) -> bool {
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    if host.ends_with(".onion") {
        let Some(proxy) = &app.tor_proxy else {
            tracing::warn!(peer = %key, "skipping onion peer: no tor proxy configured");
            return false;
        };
        let Ok(onion) = SocketAddress::from_str(addr) else { return false };
        let Some(proxy_addr) = tokio::net::lookup_host(proxy).await.ok().and_then(|mut a| a.next())
        else {
            tracing::warn!(proxy, "tor proxy does not resolve");
            return false;
        };
        if let Some(fut) = lightning_net_tokio::tor_connect_outbound(
            Arc::clone(&app.peers),
            key,
            onion,
            proxy_addr,
            Arc::clone(&app.keys),
        )
        .await
        {
            tracing::info!(peer = %key, addr, "connected gossip peer over tor");
            tokio::spawn(fut);
            return true;
        }
        tracing::info!(peer = %key, addr, "gossip peer connection over tor failed");
        return false;
    }
    let Some(socket) = tokio::net::lookup_host(addr).await.ok().and_then(|mut a| a.next()) else {
        tracing::info!(peer = %key, addr, "peer address does not resolve");
        return false;
    };
    match lightning_net_tokio::connect_outbound(Arc::clone(&app.peers), key, socket).await {
        Some(fut) => {
            tracing::info!(peer = %key, %socket, "connected gossip peer");
            tokio::spawn(fut);
            true
        }
        None => {
            tracing::info!(peer = %key, %socket, "gossip peer connection failed");
            false
        }
    }
}

/// Keeps static peers connected and tops discovered peers up to `target_peers`:
/// well-connected nodes from our graph first, DNS seed answers when the graph has none
/// (first start) or runs out.
async fn maintain_peers(app: Arc<App>) {
    let mut failed: HashMap<PublicKey, Instant> = HashMap::new();
    let mut dns_pool: Vec<Candidate> = Vec::new();
    let mut dns_asked: Option<Instant> = None;
    loop {
        let connected: std::collections::HashSet<PublicKey> =
            app.peers.list_peers().into_iter().map(|p| p.counterparty_node_id).collect();

        for peer in &app.static_peers {
            if !connected.contains(&peer.key) {
                app.dialed.lock().expect("lock").insert(peer.key, (peer.source, peer.addr.clone()));
                connect(&app, peer.key, &peer.addr).await;
            }
        }

        let static_keys: std::collections::HashSet<PublicKey> =
            app.static_peers.iter().map(|p| p.key).collect();
        let discovered = connected.iter().filter(|k| !static_keys.contains(k)).count();
        let missing = app.target_peers.saturating_sub(discovered);
        if missing > 0 {
            failed.retain(|_, at| at.elapsed() < PEER_RETRY_AFTER);
            let usable = |c: &Candidate| {
                !connected.contains(&c.key)
                    && !failed.contains_key(&c.key)
                    && !static_keys.contains(&c.key)
            };
            let mut candidates: Vec<Candidate> = {
                let graph = Arc::clone(&app.graph);
                let onion = app.tor_proxy.is_some();
                tokio::task::spawn_blocking(move || {
                    peers::graph_candidates(&graph, onion, GRAPH_POOL)
                })
                .await
                .unwrap_or_default()
            };
            candidates.retain(|c| usable(c));
            if candidates.len() < missing
                && !app.seeds.is_empty()
                && dns_asked.is_none_or(|at| at.elapsed() >= DNS_REQUERY)
            {
                tracing::info!(seeds = ?app.seeds, "asking BOLT 10 DNS seeds for peers");
                let (found, errors) = peers::dns_candidates(&app.seeds).await;
                dns_asked = Some(Instant::now());
                {
                    let mut status = app.status.lock().expect("lock");
                    status.dns_last_query_at = Some(unix_now());
                    status.dns_nodes_found = found.len();
                    status.dns_errors = errors;
                }
                dns_pool = found;
                dns_pool.shuffle(&mut rand::thread_rng());
            }
            candidates.extend(dns_pool.iter().filter(|c| usable(c)).cloned());
            if candidates.is_empty() {
                tracing::warn!(
                    connected = connected.len(),
                    "no gossip peer candidates: graph has no reachable nodes yet and DNS seeds gave none"
                );
            } else {
                tracing::info!(
                    wanted = missing,
                    candidates = candidates.len(),
                    from_graph = candidates.iter().filter(|c| c.source == Source::Graph).count(),
                    "connecting more gossip peers"
                );
            }
            // Dial a few extra in parallel: some announced addresses are stale.
            let mut attempts = tokio::task::JoinSet::new();
            for candidate in candidates.into_iter().take(missing + 2) {
                app.dialed
                    .lock()
                    .expect("lock")
                    .insert(candidate.key, (candidate.source, candidate.addr.clone()));
                let a = Arc::clone(&app);
                attempts.spawn(async move {
                    let ok = connect(&a, candidate.key, &candidate.addr).await;
                    (candidate.key, ok)
                });
            }
            while let Some(Ok((key, ok))) = attempts.join_next().await {
                if !ok {
                    failed.insert(key, Instant::now());
                }
            }
        }
        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

fn spawn_background(app: Arc<App>, scan_interval: Duration) {
    // Peer connections, kept up.
    tokio::spawn(maintain_peers(Arc::clone(&app)));

    // LDK housekeeping.
    let a = Arc::clone(&app);
    tokio::spawn(async move {
        let mut ticks = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            a.peers.process_events();
            ticks += 1;
            if ticks % 10 == 0 {
                a.peers.timer_tick_occurred();
            }
            if ticks % 3600 == 0 {
                a.graph.remove_stale_channels_and_tracking();
            }
        }
    });

    // Sync progress: the graph is "settled" once it stopped changing for a while.
    let a = Arc::clone(&app);
    tokio::spawn(async move {
        let mut last = (0usize, 0usize);
        let mut last_logged = (0usize, 0usize);
        let mut last_change = Instant::now();
        let mut last_peers = 0usize;
        let mut ticks = 0u64;
        let mut waiting_logged = false;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            ticks += 1;
            let counts = {
                let g = a.graph.read_only();
                (g.channels().len(), g.nodes().len())
            };
            let peers = a.peers.list_peers().len();
            if peers != last_peers {
                tracing::info!(
                    connected = peers,
                    wanted = a.target_peers + a.static_peers.len(),
                    "gossip peers changed"
                );
                last_peers = peers;
            }
            let settled = a.settled.load(Ordering::Relaxed);
            if !settled && ticks % 2 == 0 {
                tracing::info!(
                    channels = counts.0,
                    nodes = counts.1,
                    new_channels = counts.0.saturating_sub(last_logged.0),
                    peers,
                    "syncing gossip graph"
                );
                last_logged = counts;
            }
            if settled && !a.ready() && !waiting_logged {
                waiting_logged = true;
                let reason = match a.verify_state() {
                    VERIFY_DECIDING => "waiting for bitcoind",
                    _ => "waiting for the first funding scan",
                };
                tracing::info!(reason, "graph synced; snapshots not served yet");
            }
            if settled && ticks % (SUMMARY_INTERVAL.as_secs() / 5) == 0 {
                let s = a.status();
                tracing::info!(
                    state = s.state,
                    channels = s.graph_channels,
                    nodes = s.graph_nodes,
                    peers = s.peers_connected,
                    unspent = s.verify.unspent_channels,
                    missing = s.verify.missing_channels,
                    pending = s.verify.pending_channels,
                    snapshots_served = s.snapshot.served,
                    "status"
                );
            }
            if counts != last {
                last = counts;
                last_change = Instant::now();
                a.status.lock().expect("lock").last_graph_change_at = Some(unix_now());
            }
            if !a.settled.load(Ordering::Relaxed)
                && counts.0 > 0
                && last_change.elapsed() >= a.settle
            {
                let secs = a.started.elapsed().as_secs().saturating_sub(a.settle.as_secs());
                a.settled.store(true, Ordering::Relaxed);
                a.status.lock().expect("lock").initial_sync_secs = Some(secs);
                tracing::info!(
                    channels = counts.0,
                    nodes = counts.1,
                    secs,
                    "gossip graph synced (no change for {}s)",
                    a.settle.as_secs()
                );
            }
            a.mark_ready_if_needed();
        }
    });

    // Persistence.
    let a = Arc::clone(&app);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PERSIST_INTERVAL).await;
            let a = Arc::clone(&a);
            let _ = tokio::task::spawn_blocking(move || a.persist()).await;
        }
    });

    // Funding checks.
    if let Some(bitcoind) = app.bitcoind.clone() {
        let a = Arc::clone(&app);
        tokio::spawn(async move {
            if !wait_for_bitcoind(&a, &bitcoind).await {
                return;
            }
            if !a.settled.load(Ordering::Relaxed) {
                tracing::info!("funding scan starts once the gossip graph has synced");
            }
            while !a.settled.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            loop {
                let wait = match run_scan(&a, &bitcoind).await {
                    Ok(()) => scan_interval,
                    Err(e) => {
                        tracing::warn!(error = %e, "funding scan failed");
                        a.status.lock().expect("lock").verify.last_error = Some(e);
                        SCAN_RETRY
                    }
                };
                a.mark_ready_if_needed();
                tracing::info!(in_secs = wait.as_secs(), "next funding scan scheduled");
                tokio::time::sleep(wait).await;
            }
        });
    }
}

/// Waits until bitcoind answers. In `auto` mode, gives up after two minutes and serves
/// unverified channels; with `true`, keeps waiting. Returns whether checks are on.
async fn wait_for_bitcoind(app: &Arc<App>, bitcoind: &Bitcoind) -> bool {
    let auto = app.verify_state() == VERIFY_DECIDING;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match bitcoind.ping().await {
            Ok(()) => {
                tracing::info!(attempt, "bitcoind reachable: funding checks on");
                app.verify.store(VERIFY_ON, Ordering::Relaxed);
                app.status.lock().expect("lock").last_error = None;
                return true;
            }
            Err(e) if auto && attempt >= BITCOIND_WAIT_ATTEMPTS => {
                tracing::warn!(
                    error = %e,
                    "bitcoind did not answer within {}s: funding checks off, serving unverified channels without capacity (set ALLOD_GOSSIP_VERIFY=true to wait for it)",
                    BITCOIND_WAIT_ATTEMPTS * 5
                );
                app.verify.store(VERIFY_OFF, Ordering::Relaxed);
                app.status.lock().expect("lock").last_error = None;
                app.mark_ready_if_needed();
                return false;
            }
            Err(e) => {
                tracing::info!(attempt, error = %e, "waiting for bitcoind RPC before funding checks");
                app.status.lock().expect("lock").last_error =
                    Some(format!("waiting for bitcoind: {e}"));
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_scan(app: &Arc<App>, bitcoind: &Bitcoind) -> Result<(), String> {
    let (checks, unannounced, bad_keys) = {
        let graph = app.graph.read_only();
        let mut checks = Vec::new();
        let (mut unannounced, mut bad_keys) = (0usize, 0usize);
        for (scid, info) in graph.channels().unordered_iter() {
            // Channels from a loaded graph always carry their signed announcement; one
            // without it cannot be checked and is never served while checks are on.
            let Some(announcement) = info.announcement_message.as_ref() else {
                unannounced += 1;
                continue;
            };
            let contents = &announcement.contents;
            match verify::funding_script(&contents.bitcoin_key_1, &contents.bitcoin_key_2) {
                Some(script_pubkey) => checks.push(Check { scid: *scid, script_pubkey }),
                None => bad_keys += 1,
            }
        }
        (checks, unannounced, bad_keys)
    };
    let previous_txouts = app.status.lock().expect("lock").verify.last_scan_txouts;
    let started = Instant::now();
    {
        let mut status = app.status.lock().expect("lock");
        status.verify.scanning = true;
        status.verify.scan_progress = Some(0.0);
        status.verify.last_scan_started_at = Some(unix_now());
    }
    tracing::info!(
        channels = checks.len(),
        skipped_no_announcement = unannounced,
        skipped_invalid_keys = bad_keys,
        last_utxo_count = ?previous_txouts,
        "funding scan: rebuilt each channel's 2-of-2 P2WSH script from its announced bitcoin keys"
    );

    let progress_app = Arc::clone(app);
    let progress_btc = bitcoind.clone();
    let progress = tokio::spawn(async move {
        let started = Instant::now();
        let mut ticks = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            ticks += 1;
            let p = progress_btc.scan_progress().await;
            if let Some(p) = p {
                progress_app.status.lock().expect("lock").verify.scan_progress = Some(p);
            }
            if ticks % 3 == 0 {
                let secs = started.elapsed().as_secs();
                match p {
                    Some(p) if p > 0.0 => {
                        let eta = (secs as f64 * (100.0 - p) / p).round() as u64;
                        tracing::info!(
                            progress_pct = p,
                            elapsed_secs = secs,
                            eta_secs = eta,
                            "UTXO scan: bitcoind is reading its chainstate"
                        );
                    }
                    _ => tracing::info!(
                        elapsed_secs = secs,
                        "UTXO scan: bitcoind is parsing the descriptors or has not reported progress yet"
                    ),
                }
            }
        }
    });
    let result = bitcoind.scan(&checks).await;
    progress.abort();

    {
        let mut status = app.status.lock().expect("lock");
        status.verify.scanning = false;
        status.verify.scan_progress = None;
    }
    let scan = result?;
    let secs = started.elapsed().as_secs();
    let previous = app.verdicts.read().expect("lock").clone();
    let (mut unspent, mut capacity, mut newly_missing, mut newly_unspent, mut first_seen) =
        (0usize, 0u64, 0usize, 0usize, 0usize);
    for (scid, verdict) in &scan.verdicts {
        let before = previous.get(scid);
        match verdict {
            Verdict::Unspent { funding_sats } => {
                unspent += 1;
                capacity += funding_sats;
                if matches!(before, Some(Verdict::Missing)) {
                    newly_unspent += 1;
                }
            }
            Verdict::Missing => {
                if matches!(before, Some(Verdict::Unspent { .. })) {
                    newly_missing += 1;
                }
            }
        }
        if before.is_none() {
            first_seen += 1;
        }
    }
    let missing = checks.len() - unspent;
    tracing::info!(
        secs,
        height = scan.height,
        utxos_scanned = scan.txouts,
        outputs_returned = scan.unspents,
        outputs_not_matching_a_channel = scan.unmatched,
        "UTXO scan done"
    );
    tracing::info!(
        unspent,
        missing,
        capacity_btc = format!("{:.2}", capacity as f64 / 1e8),
        closed_since_last_scan = newly_missing,
        found_since_last_scan = newly_unspent,
        checked_first_time = first_seen,
        "funding scan finished: unspent channels are served with capacity, missing ones are left out"
    );
    {
        let mut status = app.status.lock().expect("lock");
        status.verify.scans_completed += 1;
        status.verify.last_scan_finished_at = Some(unix_now());
        status.verify.last_scan_secs = Some(secs);
        status.verify.last_scan_height = Some(scan.height);
        status.verify.last_scan_txouts = Some(scan.txouts);
        status.verify.last_error = None;
    }
    *app.verdicts.write().expect("lock") = scan.verdicts;
    // New funding data: the next request gets a fresh snapshot.
    *app.snapshot.lock().expect("lock") = None;
    let a = Arc::clone(app);
    let _ = tokio::task::spawn_blocking(move || a.persist()).await;
    tracing::info!("saved graph and funding checks");
    Ok(())
}

async fn status(State(app): State<Arc<App>>) -> Json<Status> {
    Json(app.status())
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct AliasQuery {
    ids: String,
}

/// Most node ids one `/aliases` request may ask for.
const MAX_ALIAS_IDS: usize = 500;

/// `GET /aliases?ids=<hex>,<hex>`: aliases from the node announcements this sidecar
/// received over P2P gossip. Rapid Gossip Sync snapshots cannot carry aliases, so
/// ldk-node's own graph has none in RGS mode; Allod asks here instead. Unknown nodes and
/// nodes without an alias are left out. Untrusted text: Allod sanitizes it.
async fn aliases(
    State(app): State<Arc<App>>,
    axum::extract::Query(q): axum::extract::Query<AliasQuery>,
) -> Json<HashMap<String, String>> {
    let graph = app.graph.read_only();
    let out = q
        .ids
        .split(',')
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| !id.is_empty())
        .take(MAX_ALIAS_IDS)
        .filter_map(|id| {
            let node_id = lightning::routing::gossip::NodeId::from_str(&id).ok()?;
            let alias = graph.node(&node_id)?.announcement_info.as_ref()?.alias().to_string();
            (!alias.trim().is_empty()).then_some((id, alias))
        })
        .collect();
    Json(out)
}

/// `/snapshot` answers readiness (the ldk-server entrypoint probes it with HEAD);
/// `/snapshot/<timestamp>` returns the snapshot. ldk-node builds the URL as
/// `{rgs_server_url}/{timestamp}`, so repeated slashes are tolerated.
async fn snapshot_route(State(app): State<Arc<App>>, method: Method, uri: Uri) -> Response {
    let segments: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    if segments.first() != Some(&"snapshot") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !app.ready() {
        tracing::debug!(%method, path = uri.path(), "snapshot requested before ready: 503");
        return (StatusCode::SERVICE_UNAVAILABLE, "gossip graph not ready\n").into_response();
    }
    let wants_snapshot =
        segments.len() >= 2 && segments.last().is_some_and(|s| s.parse::<u32>().is_ok());
    if !wants_snapshot || method == Method::HEAD {
        return (StatusCode::OK, "ready\n").into_response();
    }
    let a = Arc::clone(&app);
    let Ok(bytes) = tokio::task::spawn_blocking(move || a.snapshot()).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    {
        let mut status = app.status.lock().expect("lock");
        status.snapshot.served += 1;
        status.snapshot.last_served_at = Some(unix_now());
    }
    tracing::info!(path = uri.path(), bytes = bytes.len(), "served snapshot");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(bytes.as_ref().clone()))
        .expect("valid response")
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/health", get(health))
        .route("/aliases", get(aliases))
        .fallback(snapshot_route)
        .with_state(app)
}

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))
        .expect("valid log filter");
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let args = Args::parse();
    let network = parse_network(&args.network).unwrap_or_else(|e| panic!("{e}"));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        network = %args.network,
        data_dir = %args.data_dir.display(),
        verify = %args.verify,
        bitcoind = args.bitcoind_url.as_deref().unwrap_or("none"),
        scan_interval_secs = args.scan_interval_secs,
        settle_secs = args.settle_secs,
        "allod-gossip starting"
    );
    std::fs::create_dir_all(&args.data_dir).expect("create data dir");
    let url = args.bitcoind_url.clone().filter(|u| !u.is_empty());
    let client =
        url.map(|u| Bitcoind::new(u, args.bitcoind_user.clone(), args.bitcoind_pass.clone()));
    let (bitcoind, verify) = match (args.verify.as_str(), client) {
        ("false" | "off" | "0", _) => (None, VERIFY_OFF),
        ("true" | "on" | "1", Some(client)) => (Some(client), VERIFY_ON),
        ("true" | "on" | "1", None) => {
            panic!("ALLOD_GOSSIP_VERIFY=true needs ALLOD_GOSSIP_BITCOIND_URL")
        }
        ("auto", Some(client)) => (Some(client), VERIFY_DECIDING),
        ("auto", None) => (None, VERIFY_OFF),
        (other, _) => panic!("ALLOD_GOSSIP_VERIFY must be true, false or auto, not {other}"),
    };
    if verify == VERIFY_OFF {
        tracing::warn!(
            "funding checks disabled: snapshots carry unverified channels without capacity"
        );
    }

    let log = Arc::new(Log);
    let graph = Arc::new(load_graph(&args.data_dir, network, Arc::clone(&log)));
    let seed = load_seed(&args.data_dir).expect("node seed");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock");
    let keys = Arc::new(KeysManager::new(&seed, now.as_secs(), now.subsec_nanos(), true));
    let gossip =
        Arc::new(P2PGossipSync::new(Arc::clone(&graph), None::<NoLookup>, Arc::clone(&log)));
    let mut ephemeral = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut ephemeral);
    let peers = Arc::new(PeerManager::new(
        MessageHandler {
            chan_handler: ErroringMessageHandler::new(),
            route_handler: gossip,
            onion_message_handler: IgnoringMessageHandler {},
            custom_message_handler: IgnoringMessageHandler {},
            send_only_message_handler: IgnoringMessageHandler {},
        },
        now.as_secs() as u32,
        &ephemeral,
        Arc::clone(&log),
        Arc::clone(&keys),
    ));

    let static_peers = peers::parse_static(&args.peers);
    let seeds = peers::parse_seeds(&args.dns_seeds, network);
    if static_peers.is_empty() && seeds.is_empty() && graph.read_only().nodes().is_empty() {
        tracing::warn!(
            "no ALLOD_GOSSIP_PEERS, no DNS seeds for this network and an empty graph: the graph cannot sync"
        );
    }
    let loaded_channels = graph.read_only().channels().len();
    let app = Arc::new(App {
        network,
        chain_hash: ChainHash::using_genesis_block(network),
        data_dir: args.data_dir.clone(),
        graph,
        peers,
        keys,
        static_peers,
        seeds,
        target_peers: args.target_peers,
        dialed: Mutex::new(HashMap::new()),
        tor_proxy: args.tor_proxy.clone().filter(|p| !p.is_empty()),
        bitcoind,
        verify: AtomicU8::new(verify),
        started: Instant::now(),
        // A saved graph is served right away; live gossip keeps it current.
        settled: AtomicBool::new(loaded_channels > 0),
        settle: Duration::from_secs(args.settle_secs.max(5)),
        ready_at: AtomicU64::new(0),
        verdicts: RwLock::new(HashMap::new()),
        status: Mutex::new(Status {
            network: args.network.clone(),
            started_at: unix_now(),
            ..Default::default()
        }),
        snapshot: Mutex::new(None),
    });
    {
        let mut status = app.status.lock().expect("lock");
        status.peers_configured = app.target_peers + app.static_peers.len();
        status.dns_seeds = app.seeds.clone();
    }
    app.load_verdicts();
    app.mark_ready_if_needed();
    tracing::info!(
        network = ?app.network,
        listen = %args.listen,
        static_peers = app.static_peers.len(),
        target_peers = app.target_peers,
        dns_seeds = ?app.seeds,
        verify = %args.verify,
        "gossip sidecar starting"
    );
    spawn_background(Arc::clone(&app), Duration::from_secs(args.scan_interval_secs.max(60)));

    let listener = tokio::net::TcpListener::bind(args.listen).await.expect("bind");
    let shutdown_app = Arc::clone(&app);
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            tracing::info!("shutting down, saving graph");
            let _ = tokio::task::spawn_blocking(move || shutdown_app.persist()).await;
        })
        .await
        .expect("serve");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks() {
        assert_eq!(parse_network("bitcoin").unwrap(), Network::Bitcoin);
        assert_eq!(parse_network("regtest").unwrap(), Network::Regtest);
        assert!(parse_network("nope").is_err());
    }
}
