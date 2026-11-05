//! First-run setup (SPEC §3.2) and node credentials (SPEC §2.3.3).
//!
//! Managed mode: Allod creates or restores the recovery phrase, hands it to ldk-server
//! through the handoff directory, then (once ldk-server runs) creates its own scoped
//! macaroon with the admin credential ldk-server published, and stops using the admin
//! credential. Standalone mode: the owner provides ldk-server credentials directly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use allod_db::{config_keys, ConfigEntry, Db};
use allod_keys::{secret_names, write_file_with_mode, RecoveryPhrase, SecretStore};
use allod_ln::{missing_permissions, LdkServerBackend, LdkServerConfig, REQUIRED_PERMISSIONS};
use allod_types::{DeploymentMode, Network};
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// Handoff file names shared with ldk-server's entrypoint.
mod handoff {
    pub const MNEMONIC: &str = "keys_mnemonic";
    pub const RESCAN_FROM_HEIGHT: &str = "rescan_from_height";
    pub const TLS_CERT: &str = "tls.crt";
    pub const ADMIN_MACAROON: &str = "admin.macaroon";
    /// Written by Allod once it has its own macaroon; the entrypoint then stops
    /// publishing the admin macaroon.
    pub const PROVISIONED: &str = ".provisioned";
}

const CONFIG_SETUP_MODE: &str = "setup.mode";
const CONFIG_SETUP_COMPLETED_AT: &str = "setup.completed_at";
/// How long one `getblockchaininfo` may take. A syncing bitcoind holds its chain lock
/// while it connects a block or flushes its cache, so on a small machine the call can
/// take a minute; the query keeps running after a progress request stops waiting.
const BITCOIND_QUERY_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a progress request waits for a fresh answer before it returns the last one.
const BITCOIND_WAIT: Duration = Duration::from_secs(8);
/// Sync history returned with the progress, so the UI can estimate the time left from
/// its first response: answers at least `BITCOIND_HISTORY_STEP` apart, for this long.
const BITCOIND_HISTORY_SECS: i64 = 15 * 60;
const BITCOIND_HISTORY_STEP: i64 = 10;
/// Words the owner re-enters to prove the backup (SPEC §13.3).
pub const BACKUP_CHECK_WORDS: usize = 3;
/// Macaroon name Allod creates on ldk-server.
pub const MACAROON_NAME: &str = "allod";

/// Read-only bitcoind RPC access for sync progress (SPEC §2.3.4).
#[derive(Debug, Clone)]
pub struct BitcoindRpc {
    pub url: String,
    pub user: String,
    pub pass: String,
}

/// Static setup configuration.
#[derive(Debug, Clone)]
pub struct SetupConfig {
    pub mode: DeploymentMode,
    /// Secret files directory (`{data_dir}/secrets`).
    pub secrets_dir: PathBuf,
    /// Managed-mode handoff directory shared with ldk-server.
    pub handoff_dir: PathBuf,
    /// ldk-server gRPC address in managed mode.
    pub managed_ldk_address: String,
    /// Expected network, if configured.
    pub network: Option<Network>,
    pub bitcoind: Option<BitcoindRpc>,
}

/// What the owner asked for.
#[derive(Debug)]
pub enum SetupRequest {
    /// Managed: new recovery phrase.
    Create,
    /// Managed: existing recovery phrase, optionally with the height of the wallet's
    /// first transaction (passed to ldk-server's `--rescan-from-height`).
    Restore { phrase: Zeroizing<String>, birthday_height: Option<u32> },
    /// Standalone: an existing ldk-server.
    Standalone {
        address: String,
        tls_cert_pem: String,
        macaroon_hex: Zeroizing<String>,
        phrase: Option<Zeroizing<String>>,
    },
}

/// Result of a successful setup.
#[derive(Debug)]
pub struct SetupOutcome {
    /// The phrase, when Allod generated it (shown once for backup).
    pub generated_phrase: Option<Zeroizing<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("setup is already complete")]
    AlreadyComplete,
    #[error("{0}")]
    InvalidRequest(String),
    #[error("ldk-server already has a recovery phrase; restore with that phrase instead")]
    LdkAlreadyInitialized,
    #[error("ldk-server: {0}")]
    Backend(String),
    #[error("the ldk-server macaroon lacks permissions: {}", .0.join(", "))]
    MissingPermissions(Vec<&'static str>),
    #[error("ldk-server runs on {actual} but Allod is configured for {expected}")]
    WrongNetwork { expected: Network, actual: Network },
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<allod_db::DbError> for SetupError {
    fn from(e: allod_db::DbError) -> Self {
        SetupError::Internal(e.to_string())
    }
}

impl From<std::io::Error> for SetupError {
    fn from(e: std::io::Error) -> Self {
        SetupError::Internal(e.to_string())
    }
}

/// Managed credential provisioning state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum ProvisionState {
    /// Not managed mode, or setup not complete.
    NotApplicable,
    /// Waiting for ldk-server (reason).
    Waiting(String),
    /// Allod has its own macaroon.
    Done,
}

/// `GET /api/setup/progress`.
#[derive(Debug, Clone, Serialize)]
pub struct SetupProgress {
    pub setup_completed: bool,
    pub mode: DeploymentMode,
    /// not_started | waiting_for_ldk_server | done | not_applicable
    pub seed_handoff: &'static str,
    pub credentials: ProvisionState,
    pub node: Option<serde_json::Value>,
    pub bitcoind: Option<serde_json::Value>,
}

/// Setup and credentials service.
pub struct SetupService {
    db: Db,
    cfg: SetupConfig,
    secrets: SecretStore,
    lock: Mutex<()>,
    complete: AtomicBool,
    http: reqwest::Client,
    /// Standby bitcoind; watched instead of `cfg.bitcoind` once the node uses it.
    standby: Option<std::sync::Arc<crate::standby_bitcoind::StandbyBitcoind>>,
    /// Last answer of the watched bitcoind, so progress survives slow replies.
    bitcoind_cache: std::sync::Arc<std::sync::Mutex<BitcoindCache>>,
    /// Held by the one bitcoind query in flight.
    bitcoind_query: std::sync::Arc<Mutex<()>>,
    bitcoind_wait: Duration,
    /// Status file the bundled bitcoind writes from its log (`sync-status.sh`).
    sync_status_file: Option<PathBuf>,
}

/// Log figures older than this are not used (the bundled node is not running, or the
/// stack watches another node).
const SYNC_STATUS_MAX_AGE: i64 = 30 * 60;

/// The watched bitcoind's last answer (`SetupService::bitcoind_info`).
#[derive(Debug, Default)]
struct BitcoindCache {
    /// RPC URL the entries belong to (the watched node can change to the standby).
    url: String,
    /// Last good reply and when it arrived (unix seconds).
    ok: Option<(serde_json::Value, i64)>,
    /// Error of the latest query, if it failed.
    error: Option<String>,
    /// Recent good answers: (unix seconds, blocks, verification progress).
    history: std::collections::VecDeque<(i64, serde_json::Value, serde_json::Value)>,
    /// Latest tip from bitcoind's log: (unix seconds, height, verification progress).
    log: Option<(i64, u64, f64)>,
}

impl BitcoindCache {
    fn store(&mut self, url: &str, result: Result<serde_json::Value, String>, now: i64) {
        if self.url != url {
            *self = BitcoindCache { url: url.to_string(), ..Default::default() };
        }
        match result {
            Ok(v) => {
                self.remember(now, v["blocks"].clone(), v["verification_progress"].clone());
                self.ok = Some((v, now));
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// A tip read from bitcoind's log (`sync-status.sh`); older readings are ignored.
    fn store_log(&mut self, at: i64, height: u64, progress: f64) {
        if self.log.is_some_and(|(t, ..)| at <= t) {
            return;
        }
        self.log = Some((at, height, progress));
        self.remember(at, json!(height), json!(progress));
    }

    fn remember(&mut self, at: i64, blocks: serde_json::Value, progress: serde_json::Value) {
        if self.history.back().is_none_or(|(t, ..)| at - t >= BITCOIND_HISTORY_STEP) {
            self.history.push_back((at, blocks, progress));
        }
        while self.history.front().is_some_and(|(t, ..)| at - t > BITCOIND_HISTORY_SECS) {
            self.history.pop_front();
        }
    }

    /// The log's tip when it is recent, newer than the last RPC answer and on the same
    /// chain as it (not far below, e.g. a status file of a node no longer watched).
    fn fresh_log(&self, now: i64) -> Option<(i64, u64, f64)> {
        let (at, height, progress) = self.log?;
        if now - at > SYNC_STATUS_MAX_AGE {
            return None;
        }
        match &self.ok {
            Some((v, ok_at)) => {
                let blocks = v["blocks"].as_u64().unwrap_or(0);
                (at > *ok_at && height + 100 >= blocks).then_some((at, height, progress))
            }
            None => Some((at, height, progress)),
        }
    }

    /// The last good reply with `updated_at` (and `last_error` when the latest query
    /// failed), or the error when bitcoind never answered. The height and progress come
    /// from the log when it is newer: RPC can lag minutes behind a syncing node.
    fn view(&self, url: &str, now: i64) -> serde_json::Value {
        let log = self.fresh_log(now);
        let history = || -> serde_json::Value {
            self.history.iter().map(|(t, b, vp)| json!([t, b, vp])).collect()
        };
        match (&self.ok, &self.error, log) {
            (Some((v, at)), error, log) if self.url == url => {
                let mut v = v.clone();
                v["updated_at"] = json!(at);
                if let Some((log_at, height, progress)) = log {
                    v["blocks"] = json!(height);
                    v["verification_progress"] = json!(progress);
                    if v["headers"].as_u64().is_some_and(|h| h < height) {
                        v["headers"] = json!(height);
                    }
                    v["updated_at"] = json!(log_at);
                } else if let Some(e) = error {
                    v["last_error"] = json!(e);
                }
                v["history"] = history();
                v
            }
            (_, _, Some((log_at, height, progress))) => json!({
                "blocks": height,
                "verification_progress": progress,
                "initial_block_download": progress < 0.9999,
                "updated_at": log_at,
                "history": history(),
            }),
            (None, Some(e), None) if self.url == url => json!({"error": e}),
            _ => json!({"error": "waiting for bitcoind"}),
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

impl std::fmt::Debug for SetupService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupService").field("mode", &self.cfg.mode).finish()
    }
}

impl SetupService {
    pub async fn new(db: Db, cfg: SetupConfig) -> Result<Self, SetupError> {
        let complete =
            db.config_get(config_keys::SETUP_COMPLETED).await?.is_some_and(|e| e.value == "true");
        let secrets = SecretStore::new(&cfg.secrets_dir);
        Ok(SetupService {
            db,
            secrets,
            cfg,
            lock: Mutex::new(()),
            complete: AtomicBool::new(complete),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| SetupError::Internal(e.to_string()))?,
            standby: None,
            bitcoind_cache: Default::default(),
            bitcoind_query: Default::default(),
            bitcoind_wait: BITCOIND_WAIT,
            sync_status_file: None,
        })
    }

    /// Adds the stack's standby bitcoind (`standby_bitcoind`).
    pub fn with_standby(
        mut self,
        standby: Option<std::sync::Arc<crate::standby_bitcoind::StandbyBitcoind>>,
    ) -> Self {
        self.standby = standby;
        self
    }

    /// The status file the bundled bitcoind writes from its log (`sync-status.sh`, in the
    /// control directory it shares with Allod).
    pub fn with_sync_status_file(mut self, path: Option<PathBuf>) -> Self {
        self.sync_status_file = path;
        self
    }

    /// Reads the bundled bitcoind's status file into the cache, if there is a new one.
    fn read_sync_status(&self) {
        let Some(path) = &self.sync_status_file else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        if let (Some(at), Some(height), Some(progress)) =
            (v["at"].as_i64(), v["height"].as_u64(), v["progress"].as_f64())
        {
            self.bitcoind_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .store_log(at, height, progress);
        }
    }

    /// The stack's standby bitcoind, when the platform ships one.
    pub fn standby(&self) -> Option<&std::sync::Arc<crate::standby_bitcoind::StandbyBitcoind>> {
        self.standby.as_ref()
    }

    /// The bitcoind Allod watches for sync progress and fee estimates: the standby node
    /// once the owner switched the Lightning node to it, else the configured one.
    fn watched_bitcoind(&self) -> Option<BitcoindRpc> {
        if let Some(standby) = &self.standby {
            let store = crate::chain_source::ChainSourceStore::new(&self.cfg.handoff_dir);
            if standby.enabled() && store.wants_standby() {
                return Some(standby.rpc());
            }
        }
        self.cfg.bitcoind.clone()
    }

    /// Checks that the configured bitcoind (`cfg.bitcoind`, the stack's default chain
    /// source) answers RPC. `None` when none is configured.
    pub async fn probe_default_bitcoind(&self) -> Option<Result<(), String>> {
        let rpc = self.cfg.bitcoind.as_ref()?;
        let body =
            json!({"jsonrpc": "1.0", "id": "allod", "method": "getblockchaininfo", "params": []});
        let resp = match self
            .http
            .post(&rpc.url)
            .basic_auth(&rpc.user, Some(&rpc.pass))
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Some(Err(e.to_string())),
        };
        let status = resp.status();
        // bitcoind answers RPC errors with 500 and a JSON body; anything with a result
        // means the node is there.
        let value: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return Some(Err(format!("bitcoind answered HTTP {status}"))),
        };
        Some(if value["result"].is_null() {
            Err(format!("bitcoind: {}", value["error"]))
        } else {
            Ok(())
        })
    }

    /// The configured network (`ALLOD_NETWORK`), if any.
    pub fn network(&self) -> Option<Network> {
        self.cfg.network
    }

    pub fn mode(&self) -> DeploymentMode {
        self.cfg.mode
    }

    /// Cached completion flag. Use [`SetupService::check_complete`] where another
    /// process (the `allod setup` CLI) may have completed setup meanwhile.
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
    }

    /// True when setup is complete, re-reading the database while the cached flag is
    /// still false (setup may have run in another process).
    pub async fn check_complete(&self) -> bool {
        if self.is_complete() {
            return true;
        }
        let done = matches!(
            self.db.config_get(config_keys::SETUP_COMPLETED).await,
            Ok(Some(e)) if e.value == "true"
        );
        if done {
            self.complete.store(true, Ordering::SeqCst);
        }
        done
    }

    fn handoff(&self, name: &str) -> PathBuf {
        self.cfg.handoff_dir.join(name)
    }

    /// ldk-server already has its own phrase (it published credentials before).
    fn ldk_initialized(&self) -> bool {
        self.handoff(handoff::TLS_CERT).exists() || self.handoff(handoff::PROVISIONED).exists()
    }

    /// Runs first-run setup for `owner_pubkey` (already proven by the caller).
    pub async fn run(
        &self,
        req: SetupRequest,
        owner_pubkey: &str,
        now: i64,
    ) -> Result<SetupOutcome, SetupError> {
        let _guard = self.lock.lock().await;
        if self.check_complete().await {
            return Err(SetupError::AlreadyComplete);
        }
        // A crash after the owner was stored but before completion may be resumed by
        // that same owner only.
        let owners = self.db.owners().await?;
        if !owners.is_empty() && !owners.iter().any(|o| o.pubkey == owner_pubkey) {
            return Err(SetupError::AlreadyComplete);
        }

        let mut config = vec![entry(CONFIG_SETUP_MODE, self.cfg.mode.as_str())];
        let (phrase, generated) = match (&req, self.cfg.mode) {
            (SetupRequest::Create, DeploymentMode::Managed) => {
                if self.ldk_initialized() {
                    return Err(SetupError::LdkAlreadyInitialized);
                }
                let p =
                    RecoveryPhrase::generate().map_err(|e| SetupError::Internal(e.to_string()))?;
                (p, true)
            }
            (SetupRequest::Restore { phrase, .. }, DeploymentMode::Managed) => {
                (parse_phrase(phrase)?, false)
            }
            (
                SetupRequest::Standalone { address, tls_cert_pem, macaroon_hex, phrase },
                DeploymentMode::Standalone,
            ) => {
                let creds = LdkServerConfig {
                    address: address.trim().to_string(),
                    tls_cert_pem: tls_cert_pem.as_bytes().to_vec(),
                    macaroon_hex: macaroon_hex.trim().to_ascii_lowercase(),
                };
                self.check_backend(&creds).await?;
                self.secrets.write(secret_names::LDK_SERVER_MACAROON, &creds.macaroon_hex)?;
                config.push(entry(config_keys::LDK_SERVER_ADDRESS, &creds.address));
                config.push(entry(config_keys::LDK_SERVER_CERT, tls_cert_pem));
                match phrase {
                    Some(p) => (parse_phrase(p)?, false),
                    None => (
                        RecoveryPhrase::generate()
                            .map_err(|e| SetupError::Internal(e.to_string()))?,
                        true,
                    ),
                }
            }
            (_, mode) => {
                return Err(SetupError::InvalidRequest(format!(
                    "this request is not valid for {mode} mode"
                )))
            }
        };

        if self.cfg.mode == DeploymentMode::Managed {
            config.push(entry(config_keys::LDK_SERVER_ADDRESS, &self.cfg.managed_ldk_address));
            if self.ldk_initialized() {
                tracing::warn!("ldk-server already has a recovery phrase; not handing one over");
            } else {
                let words = phrase.expose_words();
                write_file_with_mode(&self.handoff(handoff::MNEMONIC), &words, 0o400)?;
                if let SetupRequest::Restore { birthday_height: Some(h), .. } = &req {
                    write_file_with_mode(
                        &self.handoff(handoff::RESCAN_FROM_HEIGHT),
                        &h.to_string(),
                        0o400,
                    )?;
                }
            }
        }

        self.secrets.write(secret_names::RECOVERY_PHRASE, &phrase.expose_words())?;
        self.db.add_owner(owner_pubkey, "setup", now).await?;
        config.push(entry(CONFIG_SETUP_COMPLETED_AT, &now.to_string()));
        config.push(entry(config_keys::SETUP_COMPLETED, "true"));
        // A phrase the owner typed in is already backed up; a new one is not until checked.
        config.push(entry(config_keys::BACKUP_VERIFIED, if generated { "false" } else { "true" }));
        self.db.config_set_many(&config).await?;
        self.complete.store(true, Ordering::SeqCst);
        let data =
            json!({"mode": self.cfg.mode.as_str(), "owner": owner_pubkey, "generated": generated});
        let _ = self.db.audit("setup_completed", None, &data.to_string(), now).await;
        tracing::info!(mode = %self.cfg.mode, "setup complete");
        Ok(SetupOutcome { generated_phrase: generated.then(|| phrase.expose_words()) })
    }

    async fn check_backend(&self, creds: &LdkServerConfig) -> Result<(), SetupError> {
        let backend =
            LdkServerBackend::new(creds).map_err(|e| SetupError::InvalidRequest(e.to_string()))?;
        let info = backend.node_info().await.map_err(|e| SetupError::Backend(e.to_string()))?;
        if let (Some(expected), Some(actual)) = (self.cfg.network, info.network) {
            if expected != actual {
                return Err(SetupError::WrongNetwork { expected, actual });
            }
        }
        let granted =
            backend.permissions().await.map_err(|e| SetupError::Backend(e.to_string()))?;
        let missing = missing_permissions(&granted);
        if !missing.is_empty() {
            return Err(SetupError::MissingPermissions(missing));
        }
        Ok(())
    }

    /// Whether the owner proved they backed up the recovery phrase (SPEC §13.3).
    pub async fn backup_verified(&self) -> Result<bool, SetupError> {
        let entry = self.db.config_get(config_keys::BACKUP_VERIFIED).await?;
        Ok(entry.is_none_or(|e| e.value != "false"))
    }

    /// Checks `words` (0-based position → word, at least [`BACKUP_CHECK_WORDS`]) against
    /// the stored phrase and marks the backup verified when all match.
    pub async fn verify_backup(
        &self,
        words: &std::collections::BTreeMap<usize, String>,
    ) -> Result<bool, SetupError> {
        if words.len() < BACKUP_CHECK_WORDS {
            return Err(SetupError::InvalidRequest(format!(
                "check at least {BACKUP_CHECK_WORDS} words"
            )));
        }
        let phrase = self
            .recovery_phrase()?
            .ok_or_else(|| SetupError::InvalidRequest("no recovery phrase stored".into()))?;
        let stored: Vec<&str> = phrase.split_whitespace().collect();
        let ok = words
            .iter()
            .all(|(i, w)| stored.get(*i).is_some_and(|s| s.eq_ignore_ascii_case(w.trim())));
        if ok {
            self.db.config_set(config_keys::BACKUP_VERIFIED, "true", false).await?;
        }
        Ok(ok)
    }

    /// The recovery phrase, once setup has run.
    pub fn recovery_phrase(&self) -> Result<Option<Zeroizing<String>>, SetupError> {
        Ok(self.secrets.read(secret_names::RECOVERY_PHRASE)?)
    }

    /// Credentials Allod uses for ldk-server: stored address and certificate plus the
    /// macaroon secret file. None until they exist.
    pub async fn node_credentials(&self) -> Result<Option<LdkServerConfig>, SetupError> {
        let address = self.db.config_get(config_keys::LDK_SERVER_ADDRESS).await?;
        let cert = self.db.config_get(config_keys::LDK_SERVER_CERT).await?;
        let macaroon = self.secrets.read(secret_names::LDK_SERVER_MACAROON)?;
        Ok(match (address, cert, macaroon) {
            (Some(a), Some(c), Some(m)) => Some(LdkServerConfig {
                address: a.value,
                tls_cert_pem: c.value.into_bytes(),
                macaroon_hex: m.to_string(),
            }),
            _ => None,
        })
    }

    /// Managed mode: one attempt to obtain Allod' own scoped macaroon (SPEC §2.3.3).
    pub async fn provision_managed(&self, now: i64) -> Result<ProvisionState, SetupError> {
        if self.cfg.mode != DeploymentMode::Managed || !self.check_complete().await {
            return Ok(ProvisionState::NotApplicable);
        }
        if self.node_credentials().await?.is_some() {
            return Ok(ProvisionState::Done);
        }
        let _guard = self.lock.lock().await;
        let cert_path = self.handoff(handoff::TLS_CERT);
        let admin_path = self.handoff(handoff::ADMIN_MACAROON);
        if !cert_path.exists() || !admin_path.exists() {
            let why = if self.handoff(handoff::MNEMONIC).exists() {
                "ldk-server has not picked up the recovery phrase yet (bitcoind may still be syncing)"
            } else {
                "waiting for ldk-server to publish its credentials"
            };
            return Ok(ProvisionState::Waiting(why.into()));
        }
        let admin =
            LdkServerConfig::from_files(&self.cfg.managed_ldk_address, &cert_path, &admin_path)
                .map_err(|e| SetupError::Backend(e.to_string()))?;
        let backend =
            LdkServerBackend::new(&admin).map_err(|e| SetupError::Backend(e.to_string()))?;
        if let Err(e) = backend.node_info().await {
            return Ok(ProvisionState::Waiting(format!("ldk-server not reachable yet: {e}")));
        }
        let mut perms: Vec<&str> = REQUIRED_PERMISSIONS.to_vec();
        perms.extend(allod_ln::OPTIONAL_PERMISSIONS);
        let token = match backend.create_macaroon(MACAROON_NAME, &perms).await {
            Ok(t) => t,
            // The name exists from an earlier, interrupted attempt.
            Err(_) => backend
                .create_macaroon(&format!("{MACAROON_NAME}-{now}"), &perms)
                .await
                .map_err(|e| SetupError::Backend(e.to_string()))?,
        };
        let cert_pem = std::fs::read_to_string(&cert_path)?;
        // Check the new macaroon before relying on it.
        let scoped = LdkServerConfig {
            address: self.cfg.managed_ldk_address.clone(),
            tls_cert_pem: cert_pem.clone().into_bytes(),
            macaroon_hex: token.clone(),
        };
        let check =
            LdkServerBackend::new(&scoped).map_err(|e| SetupError::Backend(e.to_string()))?;
        check.node_info().await.map_err(|e| SetupError::Backend(e.to_string()))?;

        self.secrets.write(secret_names::LDK_SERVER_MACAROON, &token)?;
        self.db.config_set(config_keys::LDK_SERVER_CERT, &cert_pem, false).await?;
        write_file_with_mode(&self.handoff(handoff::PROVISIONED), &now.to_string(), 0o644)?;
        if let Err(e) = std::fs::remove_file(&admin_path) {
            tracing::warn!(error = %e, "could not remove the admin macaroon from the handoff directory");
        }
        let _ = self.db.audit("ldk_credentials_provisioned", None, "{}", now).await;
        tracing::info!("created a scoped ldk-server macaroon for Allod");
        Ok(ProvisionState::Done)
    }

    /// Progress for the setup wizard (SPEC §2.3.4).
    pub async fn progress(&self) -> SetupProgress {
        let complete = self.check_complete().await;
        let seed_handoff = match self.cfg.mode {
            DeploymentMode::Standalone => "not_applicable",
            DeploymentMode::Managed if !complete => "not_started",
            DeploymentMode::Managed if self.handoff(handoff::MNEMONIC).exists() => {
                "waiting_for_ldk_server"
            }
            DeploymentMode::Managed => "done",
        };
        let credentials = match self.node_credentials().await {
            Ok(Some(_)) => ProvisionState::Done,
            Ok(None) if !complete => ProvisionState::NotApplicable,
            Ok(None) => {
                ProvisionState::Waiting("ldk-server credentials not provisioned yet".into())
            }
            Err(e) => ProvisionState::Waiting(e.to_string()),
        };
        let node = match self.node_credentials().await {
            Ok(Some(creds)) => match LdkServerBackend::new(&creds) {
                Ok(b) => match b.node_info().await {
                    Ok(i) => Some(json!({
                        "pubkey": i.summary.pubkey,
                        "block_height": i.summary.block_height,
                        "network": i.network.map(|n| n.to_string()),
                    })),
                    Err(e) => Some(json!({"error": e.to_string()})),
                },
                Err(e) => Some(json!({"error": e.to_string()})),
            },
            _ => None,
        };
        SetupProgress {
            setup_completed: complete,
            mode: self.cfg.mode,
            seed_handoff,
            credentials,
            node,
            bitcoind: self.bitcoind_info().await,
        }
    }

    /// The watched bitcoind's sync state. One query runs at a time and keeps running
    /// when this request stops waiting for it, so a bitcoind that is slow to answer
    /// (busy connecting blocks or flushing its cache) still updates what the next
    /// request returns: the last good answer with its age, never a gap.
    async fn bitcoind_info(&self) -> Option<serde_json::Value> {
        let rpc = self.watched_bitcoind()?;
        self.read_sync_status();
        let started = self.start_bitcoind_query(&rpc);
        // With current figures from the log there is no need to wait for RPC.
        let log_current = {
            let cache = self.bitcoind_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.log.is_some_and(|(at, ..)| unix_now() - at < 30)
        };
        if let (Some(done), false) = (started, log_current) {
            let _ = tokio::time::timeout(self.bitcoind_wait, done).await;
        }
        self.read_sync_status();
        let cache = self.bitcoind_cache.lock().unwrap_or_else(|e| e.into_inner());
        Some(cache.view(&rpc.url, unix_now()))
    }

    /// Samples the watched bitcoind in the background (no waiting), so the sync history
    /// is there when someone opens the UI. Returns false once bitcoind reports it is
    /// synced, or when there is no bitcoind to watch.
    pub fn sample_bitcoind(&self) -> bool {
        let Some(rpc) = self.watched_bitcoind() else { return false };
        self.read_sync_status();
        self.start_bitcoind_query(&rpc);
        let cache = self.bitcoind_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.url != rpc.url
            || cache.ok.as_ref().is_none_or(|(v, _)| v["initial_block_download"] != false)
    }

    /// Starts a query unless one is running; the receiver fires when it has finished.
    fn start_bitcoind_query(
        &self,
        rpc: &BitcoindRpc,
    ) -> Option<tokio::sync::oneshot::Receiver<()>> {
        let guard = self.bitcoind_query.clone().try_lock_owned().ok()?;
        let (done, wait) = tokio::sync::oneshot::channel::<()>();
        let http = self.http.clone();
        let cache = self.bitcoind_cache.clone();
        let rpc = rpc.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let result = query_bitcoind(&http, &rpc).await;
            cache.lock().unwrap_or_else(|e| e.into_inner()).store(&rpc.url, result, unix_now());
            let _ = done.send(());
        });
        Some(wait)
    }

    /// bitcoind's fee estimates at the block targets ldk-node uses, as reported by
    /// `GET /api/fees`. `None` in light modes, which have no bitcoind of Allod's own.
    pub async fn fee_estimates(&self) -> Option<serde_json::Value> {
        let rpc = &self.watched_bitcoind()?;
        Some(fee_estimates_json(&self.http, rpc).await)
    }
}

/// One `getblockchaininfo` (plus fee estimates once synced), shaped for the progress API.
async fn query_bitcoind(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
) -> Result<serde_json::Value, String> {
    let body =
        json!({"jsonrpc": "1.0", "id": "allod", "method": "getblockchaininfo", "params": []});
    let resp = http
        .post(&rpc.url)
        .basic_auth(&rpc.user, Some(&rpc.pass))
        .timeout(BITCOIND_QUERY_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let r = &value["result"];
    if r.is_null() {
        return Err(match &value["error"]["message"] {
            serde_json::Value::String(m) => m.clone(),
            _ => value["error"].to_string(),
        });
    }
    // After the sync, ldk-server waits on mainnet until bitcoind can estimate fees
    // (images/ldk-server/ldk-entrypoint.sh); the UI says so instead of "starting".
    // Nothing to ask while bitcoind is still syncing (the estimator only learns from
    // blocks and mempool transactions it has already seen).
    let fees = if r["initialblockdownload"] == false {
        fee_estimates_json(http, rpc).await
    } else {
        serde_json::Value::Null
    };
    Ok(json!({
        "chain": r["chain"],
        "blocks": r["blocks"],
        "headers": r["headers"],
        "verification_progress": r["verificationprogress"],
        "initial_block_download": r["initialblockdownload"],
        "pruned": r["pruned"],
        "prune_height": r["pruneheight"],
        "fees": fees,
    }))
}

/// Block targets bitcoind's estimator is asked for. Kept in step with
/// `images/ldk-server/ldk-entrypoint.sh`, which waits for all of them on mainnet.
pub const FEE_TARGETS: [u32; 5] = [2, 6, 12, 144, 1008];

/// `estimatesmartfee` for one target, in sat/vB: `None` while bitcoind has too little
/// data, `Err` when bitcoind cannot be asked at all.
async fn fee_estimate(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    blocks: u32,
) -> Result<Option<f64>, String> {
    let body =
        json!({"jsonrpc": "1.0", "id": "allod", "method": "estimatesmartfee", "params": [blocks]});
    let resp = http
        .post(&rpc.url)
        .basic_auth(&rpc.user, Some(&rpc.pass))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !v["error"].is_null() {
        return Err(format!("bitcoind: {}", v["error"]));
    }
    Ok(v["result"]["feerate"].as_f64().map(sat_per_vb))
}

/// Bitcoin Core reports BTC/kvB (1 BTC per 1000 vbytes).
fn sat_per_vb(feerate_btc_kvb: f64) -> f64 {
    feerate_btc_kvb * 100_000.0
}

/// Every target with its rate (null where bitcoind has no estimate yet), the list still
/// missing, and why, if bitcoind could not be asked at all.
async fn fee_estimates_json(http: &reqwest::Client, rpc: &BitcoindRpc) -> serde_json::Value {
    let mut rates = Vec::with_capacity(FEE_TARGETS.len());
    for blocks in FEE_TARGETS {
        let rate = fee_estimate(http, rpc, blocks).await;
        let failed = rate.is_err();
        rates.push((blocks, rate));
        if failed {
            break;
        }
    }
    fees_value(rates)
}

/// The API shape of [`fee_estimates_json`], split out so it can be tested without RPC.
fn fees_value(rates: Vec<(u32, Result<Option<f64>, String>)>) -> serde_json::Value {
    let mut targets = Vec::with_capacity(rates.len());
    let mut missing = Vec::new();
    let mut error = None;
    for (blocks, rate) in rates {
        match rate {
            Ok(rate) => {
                if rate.is_none() {
                    missing.push(blocks);
                }
                targets.push(json!({"blocks": blocks, "sat_per_vb": rate}));
            }
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    json!({
        "available": error.is_none() && missing.is_empty(),
        "targets": targets,
        "missing": missing,
        "error": error,
    })
}

fn entry(key: &str, value: &str) -> ConfigEntry {
    ConfigEntry { key: key.into(), value: value.into(), encrypted: false }
}

fn parse_phrase(words: &str) -> Result<RecoveryPhrase, SetupError> {
    RecoveryPhrase::parse(words).map_err(|e| SetupError::InvalidRequest(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct T {
        svc: SetupService,
        handoff: PathBuf,
        _dir: tempfile::TempDir,
    }

    async fn managed() -> T {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}/v.db", dir.path().display())).await.unwrap();
        let handoff = dir.path().join("handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let cfg = SetupConfig {
            mode: DeploymentMode::Managed,
            secrets_dir: dir.path().join("secrets"),
            handoff_dir: handoff.clone(),
            managed_ldk_address: "ldk-server:3536".into(),
            network: Some(Network::Regtest),
            bitcoind: None,
        };
        T { svc: SetupService::new(db, cfg).await.unwrap(), handoff, _dir: dir }
    }

    /// Fake bitcoind RPC: call `i` answers after `steps[i].0` ms with height `steps[i].1`,
    /// or an RPC error when it is `None` (the last step repeats).
    async fn fake_bitcoind(steps: Vec<(u64, Option<u64>)>) -> String {
        use std::sync::atomic::AtomicUsize;
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move || {
                let calls = calls.clone();
                let steps = steps.clone();
                async move {
                    let i = calls.fetch_add(1, Ordering::SeqCst).min(steps.len() - 1);
                    let (delay_ms, height) = steps[i];
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    axum::Json(match height {
                        Some(h) => json!({"result": {
                            "chain": "main", "blocks": h, "headers": 1000,
                            "verificationprogress": 0.5, "initialblockdownload": true,
                            "pruned": true
                        }, "error": null}),
                        None => json!({"result": null, "error": {"code": -28, "message": "Loading block index…"}}),
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    async fn watching(url: String) -> T {
        let mut t = managed().await;
        t.svc.cfg.bitcoind = Some(BitcoindRpc { url, user: "u".into(), pass: "p".into() });
        t.svc.bitcoind_wait = Duration::from_millis(200);
        t
    }

    #[tokio::test]
    async fn slow_bitcoind_keeps_the_last_progress() {
        // Answers at once, then takes 600 ms (longer than a request waits), then fails.
        let url = fake_bitcoind(vec![(0, Some(100)), (600, Some(150)), (0, None)]).await;
        let t = watching(url).await;

        let b = t.svc.progress().await.bitcoind.unwrap();
        assert_eq!(b["blocks"], 100);
        assert!(b["updated_at"].as_i64().unwrap() > 0);
        assert_eq!(b["history"][0][1], 100, "{b}");
        assert_eq!(b["history"][0][2], 0.5);
        assert!(b.get("error").is_none());

        // The slow answer is not waited for: the last one comes back instead.
        let b = t.svc.progress().await.bitcoind.unwrap();
        assert_eq!(b["blocks"], 100, "{b}");
        // While that query runs, no second one starts.
        let b = t.svc.progress().await.bitcoind.unwrap();
        assert_eq!(b["blocks"], 100);

        // It finishes in the background and the next request sees it.
        tokio::time::sleep(Duration::from_millis(700)).await;
        let b = t.svc.progress().await.bitcoind.unwrap();
        // This request started the third query, which failed: last good height stays.
        assert_eq!(b["blocks"], 150, "{b}");
        assert_eq!(b["last_error"], "Loading block index…");
        assert!(b.get("error").is_none());
    }

    #[tokio::test]
    async fn busy_bitcoind_progress_comes_from_its_log() {
        // Answers once, then is stuck waiting for its chain lock.
        let url = fake_bitcoind(vec![(0, Some(100)), (60_000, Some(999))]).await;
        let mut t = watching(url).await;
        let status = t._dir.path().join("sync-status.json");
        t.svc.sync_status_file = Some(status.clone());
        let b = t.svc.progress().await.bitcoind.unwrap();
        assert_eq!(b["blocks"], 100);
        let rpc_at = b["updated_at"].as_i64().unwrap();

        // bitcoind logs new tips while RPC waits: the log's figures are used.
        let now = unix_now() + 1;
        std::fs::write(
            &status,
            format!(r#"{{"height":150,"progress":0.6,"background_height":0,"at":{now}}}"#),
        )
        .unwrap();
        let b = t.svc.progress().await.bitcoind.unwrap();
        assert_eq!(b["blocks"], 150, "{b}");
        assert_eq!(b["verification_progress"], 0.6);
        assert_eq!(b["headers"], 1000, "headers still from RPC");
        assert_eq!(b["initial_block_download"], true);
        assert_eq!(b["updated_at"], now);
        assert!(b["updated_at"].as_i64().unwrap() > rpc_at);

        // A file far below the node (another node's, or stale) is not used.
        std::fs::write(
            &status,
            format!(r#"{{"height":5,"progress":0.1,"background_height":0,"at":{}}}"#, now + 5),
        )
        .unwrap();
        let mut c = BitcoindCache::default();
        c.store("u", Ok(json!({"blocks": 900, "headers": 1000})), now);
        c.store_log(now + 5, 5, 0.1);
        assert_eq!(c.view("u", now + 6)["blocks"], 900);
        // Nor one older than the RPC answer, nor an old one.
        let mut c = BitcoindCache::default();
        c.store("u", Ok(json!({"blocks": 900, "headers": 1000})), now);
        c.store_log(now - 1, 950, 0.9);
        assert_eq!(c.view("u", now)["blocks"], 900);
        let mut c = BitcoindCache::default();
        c.store_log(now, 950, 0.9);
        assert_eq!(c.view("u", now + 10)["blocks"], 950, "no RPC answer yet: the log alone");
        assert_eq!(c.view("u", now + SYNC_STATUS_MAX_AGE + 1)["error"], "waiting for bitcoind");
    }

    #[tokio::test]
    async fn bitcoind_that_never_answered_reports_its_error() {
        let url = fake_bitcoind(vec![(0, None)]).await;
        let t = watching(url).await;
        let b = t.svc.progress().await.bitcoind.unwrap();
        assert_eq!(b["error"], "Loading block index…");
        assert!(b.get("blocks").is_none());
    }

    #[tokio::test]
    async fn managed_create_hands_phrase_to_ldk_server() {
        let t = managed().await;
        assert!(!t.svc.is_complete());
        let out = t.svc.run(SetupRequest::Create, "aa", 10).await.unwrap();
        let words = out.generated_phrase.unwrap();
        assert_eq!(words.split_whitespace().count(), 24);
        let handed = std::fs::read_to_string(t.handoff.join("keys_mnemonic")).unwrap();
        assert_eq!(handed.trim(), words.as_str());
        assert_eq!(t.svc.recovery_phrase().unwrap().unwrap().as_str(), words.as_str());
        assert!(t.svc.is_complete());
        assert!(matches!(
            t.svc.run(SetupRequest::Create, "aa", 11).await,
            Err(SetupError::AlreadyComplete)
        ));
        let p = t.svc.progress().await;
        assert_eq!(p.seed_handoff, "waiting_for_ldk_server");
        assert!(matches!(p.credentials, ProvisionState::Waiting(_)));
        assert!(matches!(t.svc.provision_managed(12).await.unwrap(), ProvisionState::Waiting(_)));
    }

    #[tokio::test]
    async fn managed_restore_writes_birthday() {
        let t = managed().await;
        let words = "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
                     abandon abandon about";
        let req = SetupRequest::Restore {
            phrase: Zeroizing::new(words.into()),
            birthday_height: Some(800_000),
        };
        let out = t.svc.run(req, "aa", 10).await.unwrap();
        assert!(out.generated_phrase.is_none());
        let h = std::fs::read_to_string(t.handoff.join("rescan_from_height")).unwrap();
        assert_eq!(h.trim(), "800000");
    }

    #[tokio::test]
    async fn refuses_create_when_ldk_server_has_a_phrase_and_bad_input() {
        let t = managed().await;
        std::fs::write(t.handoff.join("tls.crt"), "cert").unwrap();
        assert!(matches!(
            t.svc.run(SetupRequest::Create, "aa", 1).await,
            Err(SetupError::LdkAlreadyInitialized)
        ));
        let bad = SetupRequest::Restore {
            phrase: Zeroizing::new("not words".into()),
            birthday_height: None,
        };
        assert!(matches!(t.svc.run(bad, "aa", 1).await, Err(SetupError::InvalidRequest(_))));
        let wrong_mode = SetupRequest::Standalone {
            address: "x:1".into(),
            tls_cert_pem: String::new(),
            macaroon_hex: Zeroizing::new(String::new()),
            phrase: None,
        };
        assert!(matches!(t.svc.run(wrong_mode, "aa", 1).await, Err(SetupError::InvalidRequest(_))));
        assert!(!t.svc.is_complete());
    }

    #[tokio::test]
    async fn notices_setup_done_by_another_process() {
        let t = managed().await;
        assert!(!t.svc.check_complete().await);
        // Same database, separate service instance, as the `allod setup` CLI would be.
        let cli = SetupService::new(t.svc.db.clone(), t.svc.cfg.clone()).await.unwrap();
        cli.run(SetupRequest::Create, "aa", 1).await.unwrap();
        assert!(!t.svc.is_complete(), "cached flag is stale until checked");
        assert!(t.svc.check_complete().await);
        assert!(t.svc.is_complete());
        assert!(matches!(
            t.svc.run(SetupRequest::Create, "aa", 2).await,
            Err(SetupError::AlreadyComplete)
        ));
    }

    #[tokio::test]
    async fn interrupted_setup_resumes_only_for_same_owner() {
        let t = managed().await;
        t.svc.db.add_owner("aa", "setup", 1).await.unwrap();
        assert!(matches!(
            t.svc.run(SetupRequest::Create, "bb", 2).await,
            Err(SetupError::AlreadyComplete)
        ));
        assert!(t.svc.run(SetupRequest::Create, "aa", 2).await.is_ok());
    }

    #[test]
    fn fee_rates_are_sat_per_vbyte() {
        // 150 sat/kvB is 0.15 sat/vB; Bitcoin Core reports BTC/kvB.
        assert!((sat_per_vb(0.000015) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn missing_fee_targets_are_listed() {
        let v = fees_value(vec![(2, Ok(Some(1.5))), (6, Ok(None)), (12, Ok(Some(2.0)))]);
        assert_eq!(v["available"], false);
        assert_eq!(v["targets"][0]["sat_per_vb"], 1.5);
        assert!(v["targets"][1]["sat_per_vb"].is_null());
        assert_eq!(v["missing"], json!([6]));
        assert!(v["error"].is_null());
    }

    #[test]
    fn complete_fee_targets_are_available() {
        let v = fees_value(vec![(2, Ok(Some(1.0))), (6, Ok(Some(2.0)))]);
        assert_eq!(v["available"], true);
        assert!(v["missing"].as_array().unwrap().is_empty());
    }

    #[test]
    fn fee_estimator_failure_stops_and_reports() {
        let v = fees_value(vec![(2, Ok(Some(1.0))), (6, Err("connection refused".into()))]);
        assert_eq!(v["available"], false);
        assert_eq!(v["error"], "connection refused");
        // The targets asked before the failure are kept: they are still useful.
        assert_eq!(v["targets"].as_array().unwrap().len(), 1);
    }
}
