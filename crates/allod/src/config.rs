//! Process configuration (SPEC §3.1). Precedence: CLI flags > environment > TOML file > defaults.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use allod_types::{ChainMode, DeploymentMode, Network, Platform};
use anyhow::{bail, Context, Result};
use clap::Args;
use serde::Deserialize;

/// Flags and env vars for `allod serve`. Every field is optional so the file and
/// defaults can fill gaps.
#[derive(Debug, Default, Args)]
pub struct ConfigArgs {
    /// Path to a TOML config file.
    #[arg(long, env = "ALLOD_CONFIG", global = true)]
    pub config: Option<PathBuf>,
    /// Data directory (database, logs).
    #[arg(long, env = "ALLOD_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,
    /// Database URL: sqlite://... or postgres://...
    #[arg(long, env = "ALLOD_DATABASE_URL", global = true)]
    pub database_url: Option<String>,
    /// HTTP listen address.
    #[arg(long, env = "ALLOD_LISTEN", global = true)]
    pub listen: Option<SocketAddr>,
    /// Public HTTPS URL, when exposed.
    #[arg(long, env = "ALLOD_PUBLIC_URL", global = true)]
    pub public_url: Option<String>,
    /// Expected Bitcoin network; startup fails if ldk-server reports another.
    #[arg(long, env = "ALLOD_NETWORK", global = true)]
    pub network: Option<Network>,
    /// Comma-separated Nostr relay URLs.
    #[arg(long, env = "ALLOD_RELAYS", global = true, value_delimiter = ',')]
    pub relays: Option<Vec<String>>,
    /// Relay URLs written into connection links, when clients reach the relays at other
    /// addresses than Allod does (e.g. Docker-internal names). Defaults to `relays`.
    #[arg(long, env = "ALLOD_CLIENT_RELAYS", global = true, value_delimiter = ',')]
    pub client_relays: Option<Vec<String>>,
    /// Relays to read Nostr contacts and profiles from, separate from NWC relays.
    #[arg(long, env = "ALLOD_PROFILE_RELAYS", global = true, value_delimiter = ',')]
    pub profile_relays: Option<Vec<String>>,
    /// Log filter (tracing env-filter syntax).
    #[arg(long, env = "ALLOD_LOG", global = true)]
    pub log: Option<String>,
    /// Log format: text or json.
    #[arg(long, env = "ALLOD_LOG_FORMAT", global = true)]
    pub log_format: Option<LogFormat>,
    /// managed (bundled with ldk-server) or standalone.
    #[arg(long, env = "ALLOD_MODE", global = true)]
    pub mode: Option<DeploymentMode>,
    /// Chain mode of the bundle (informational).
    #[arg(long, env = "ALLOD_CHAIN", global = true)]
    pub chain: Option<ChainMode>,
    /// Host platform.
    #[arg(long, env = "ALLOD_PLATFORM", global = true)]
    pub platform: Option<Platform>,
    /// Managed-mode handoff directory shared with ldk-server.
    #[arg(long, env = "ALLOD_HANDOFF_DIR", global = true)]
    pub handoff_dir: Option<PathBuf>,
    /// Directory shared with the bundled bitcoind for fast sync requests (AssumeUTXO).
    #[arg(long, env = "ALLOD_CONTROL_DIR", global = true)]
    pub control_dir: Option<PathBuf>,
    /// ldk-server gRPC address (host:port).
    #[arg(long, env = "ALLOD_LDK_SERVER_ADDRESS", global = true)]
    pub ldk_server_address: Option<String>,
    /// ldk-server TLS certificate file.
    #[arg(long, env = "ALLOD_LDK_SERVER_CERT_FILE", global = true)]
    pub ldk_server_cert_file: Option<PathBuf>,
    /// ldk-server macaroon file.
    #[arg(long, env = "ALLOD_LDK_SERVER_MACAROON_FILE", global = true)]
    pub ldk_server_macaroon_file: Option<PathBuf>,
    /// bitcoind JSON-RPC URL for sync progress (read-only user).
    #[arg(long, env = "ALLOD_BITCOIND_RPC_URL", global = true)]
    pub bitcoind_rpc_url: Option<String>,
    /// bitcoind RPC user.
    #[arg(long, env = "ALLOD_BITCOIND_RPC_USER", global = true)]
    pub bitcoind_rpc_user: Option<String>,
    /// bitcoind RPC password.
    #[arg(long, env = "ALLOD_BITCOIND_RPC_PASS", global = true, hide_env_values = true)]
    pub bitcoind_rpc_pass: Option<String>,
    /// LSPS2 LSP for just-in-time channels, `pubkey@host:port` (SPEC §2.3). Set empty
    /// to disable; must match ldk-server's own liquidity source.
    #[arg(long, env = "ALLOD_LSPS2_LSP", global = true)]
    pub lsps2_lsp: Option<String>,
    /// Regtest only: complete setup at startup (SPEC §2.3.9).
    #[arg(long, env = "ALLOD_DEV_AUTO_SETUP", global = true)]
    pub dev_auto_setup: Option<bool>,
    /// Regtest only: owner registered by dev auto-setup (hex or npub).
    #[arg(long, env = "ALLOD_DEV_OWNER_PUBKEY", global = true)]
    pub dev_owner_pubkey: Option<String>,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines.
    Text,
    /// One JSON object per line.
    Json,
}

/// Same fields as [`ConfigArgs`], read from a TOML file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    data_dir: Option<PathBuf>,
    database_url: Option<String>,
    listen: Option<SocketAddr>,
    public_url: Option<String>,
    network: Option<Network>,
    relays: Option<Vec<String>>,
    client_relays: Option<Vec<String>>,
    profile_relays: Option<Vec<String>>,
    log: Option<String>,
    log_format: Option<LogFormat>,
    mode: Option<DeploymentMode>,
    chain: Option<ChainMode>,
    platform: Option<Platform>,
    handoff_dir: Option<PathBuf>,
    control_dir: Option<PathBuf>,
    ldk_server: Option<FileLdkServer>,
    bitcoind_rpc: Option<FileBitcoindRpc>,
    lsp: Option<FileLsp>,
    dev_auto_setup: Option<bool>,
    dev_owner_pubkey: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBitcoindRpc {
    url: Option<String>,
    user: Option<String>,
    pass: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLsp {
    /// LSPS2 LSP for just-in-time channels: `pubkey@host:port`.
    lsps2: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLdkServer {
    address: Option<String>,
    cert_file: Option<PathBuf>,
    macaroon_file: Option<PathBuf>,
}

/// Resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub database_url: String,
    pub listen: SocketAddr,
    pub public_url: Option<String>,
    pub network: Option<Network>,
    pub relays: Vec<String>,
    /// Relays as clients see them, same order as `relays` (connection links).
    pub client_relays: Vec<String>,
    /// Relays for Nostr contacts and profiles, separate from NWC relays.
    pub profile_relays: Vec<String>,
    pub log: String,
    pub log_format: LogFormat,
    pub mode: DeploymentMode,
    pub chain: Option<ChainMode>,
    pub platform: Platform,
    pub handoff_dir: PathBuf,
    /// Shared with the bundled bitcoind (fast sync); None outside Docker.
    pub control_dir: Option<PathBuf>,
    pub ldk_server_address: Option<String>,
    pub ldk_server_cert_file: Option<PathBuf>,
    pub ldk_server_macaroon_file: Option<PathBuf>,
    pub bitcoind_rpc_url: Option<String>,
    pub bitcoind_rpc_user: Option<String>,
    pub bitcoind_rpc_pass: Option<String>,
    /// LSPS2 LSP (`pubkey@host:port`) both ldk-server and Allod use for just-in-time
    /// channels (SPEC §2.3). `None`: JIT receive is off.
    pub lsps2_lsp: Option<String>,
    pub dev_auto_setup: bool,
    pub dev_owner_pubkey: Option<String>,
}

/// Default ldk-server address in managed mode (the Compose service name).
pub const MANAGED_LDK_ADDRESS: &str = "ldk-server:3536";

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8029";
const DEFAULT_RELAYS: &[&str] = &["wss://relay.getalby.com/v1", "wss://nos.lol"];

impl Config {
    /// Merges flags/env with the optional file and defaults.
    pub fn load(args: ConfigArgs) -> Result<Self> {
        let file = match &args.config {
            Some(path) => read_file(path)?,
            None => FileConfig::default(),
        };
        let file_ldk = file.ldk_server.unwrap_or_default();
        let file_btc = file.bitcoind_rpc.unwrap_or_default();

        let data_dir = args.data_dir.or(file.data_dir).unwrap_or_else(default_data_dir);
        let database_url = args
            .database_url
            .or(file.database_url)
            .unwrap_or_else(|| format!("sqlite://{}", data_dir.join("allod.db").display()));
        let mode = args.mode.or(file.mode).unwrap_or(DeploymentMode::Standalone);
        let handoff_dir =
            args.handoff_dir.or(file.handoff_dir).unwrap_or_else(|| PathBuf::from("/handoff"));

        let config = Config {
            listen: args
                .listen
                .or(file.listen)
                .unwrap_or_else(|| DEFAULT_LISTEN.parse().expect("valid default")),
            public_url: args.public_url.or(file.public_url),
            network: args.network.or(file.network),
            relays: args
                .relays
                .or(file.relays)
                .unwrap_or_else(|| DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect()),
            client_relays: Vec::new(),
            profile_relays: Vec::new(),
            log: args.log.or(file.log).unwrap_or_else(|| "info".into()),
            log_format: args.log_format.or(file.log_format).unwrap_or(LogFormat::Text),
            chain: args.chain.or(file.chain),
            platform: args.platform.or(file.platform).unwrap_or(Platform::Docker),
            ldk_server_address: args.ldk_server_address.or(file_ldk.address),
            ldk_server_cert_file: args.ldk_server_cert_file.or(file_ldk.cert_file),
            ldk_server_macaroon_file: args.ldk_server_macaroon_file.or(file_ldk.macaroon_file),
            bitcoind_rpc_url: args.bitcoind_rpc_url.or(file_btc.url),
            bitcoind_rpc_user: args.bitcoind_rpc_user.or(file_btc.user),
            bitcoind_rpc_pass: args.bitcoind_rpc_pass.or(file_btc.pass),
            lsps2_lsp: args
                .lsps2_lsp
                .or(file.lsp.and_then(|l| l.lsps2))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            dev_auto_setup: args.dev_auto_setup.or(file.dev_auto_setup).unwrap_or(false),
            dev_owner_pubkey: args.dev_owner_pubkey.or(file.dev_owner_pubkey),
            data_dir,
            database_url,
            mode,
            handoff_dir,
            control_dir: args.control_dir.or(file.control_dir).filter(|d| d.is_dir()),
        };
        let mut config = config;
        config.client_relays = args
            .client_relays
            .or(file.client_relays)
            .map(|r| r.into_iter().filter(|s| !s.trim().is_empty()).collect::<Vec<_>>())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| config.relays.clone());
        config.profile_relays = args
            .profile_relays
            .or(file.profile_relays)
            .map(|r| r.into_iter().filter(|s| !s.trim().is_empty()).collect::<Vec<_>>())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| {
                allod_nwc::contacts::DEFAULT_PROFILE_RELAYS.iter().map(|s| s.to_string()).collect()
            });
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for relay in self.relays.iter().chain(&self.client_relays).chain(&self.profile_relays) {
            if !(relay.starts_with("wss://") || relay.starts_with("ws://")) {
                bail!("relay url must start with ws:// or wss://: {relay}");
            }
        }
        if let Some(lsp) = &self.lsps2_lsp {
            if !valid_lsps2_lsp(lsp) {
                bail!("ALLOD_LSPS2_LSP must be pubkey@host:port, got: {lsp}");
            }
        }
        if self.dev_auto_setup {
            if self.network != Some(Network::Regtest) {
                bail!("ALLOD_DEV_AUTO_SETUP is only allowed with ALLOD_NETWORK=regtest");
            }
            if self.dev_owner_pubkey.is_none() {
                bail!("ALLOD_DEV_AUTO_SETUP needs ALLOD_DEV_OWNER_PUBKEY");
            }
        }
        Ok(())
    }
}

fn read_file(path: &Path) -> Result<FileConfig> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse config {}", path.display()))
}

/// `pubkey@host:port` of an LSPS2 LSP (compressed pubkey, IPv4/IPv6/host and port).
fn valid_lsps2_lsp(lsp: &str) -> bool {
    let Some((pubkey, addr)) = lsp.rsplit_once('@') else { return false };
    let valid_pk = pubkey.len() == 66
        && pubkey.chars().all(|c| c.is_ascii_hexdigit())
        && (pubkey.starts_with("02") || pubkey.starts_with("03"));
    let Some((host, port)) = addr.rsplit_once(':') else { return false };
    let valid_port = !port.is_empty() && port.chars().all(|c| c.is_ascii_digit());
    valid_pk && !host.is_empty() && valid_port
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".allod"))
        .unwrap_or_else(|| PathBuf::from(".allod"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::load(ConfigArgs { data_dir: Some("/tmp/v".into()), ..Default::default() })
            .unwrap();
        assert_eq!(c.database_url, "sqlite:///tmp/v/allod.db");
        assert_eq!(c.listen.to_string(), DEFAULT_LISTEN);
        assert_eq!(c.mode, DeploymentMode::Standalone);
        assert!(c.ldk_server_cert_file.is_none());
        assert_eq!(
            c.profile_relays,
            allod_nwc::contacts::DEFAULT_PROFILE_RELAYS
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn contact_relays_do_not_include_nwc_relays() {
        let nwc = "wss://nwc-only.example".to_string();
        let contacts = "wss://profiles.example".to_string();
        let defaults =
            Config::load(ConfigArgs { relays: Some(vec![nwc.clone()]), ..Default::default() })
                .unwrap();
        assert!(!defaults.profile_relays.contains(&nwc));
        let c = Config::load(ConfigArgs {
            relays: Some(vec![nwc.clone()]),
            profile_relays: Some(vec![contacts.clone()]),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(c.relays, vec![nwc]);
        assert_eq!(c.profile_relays, vec![contacts]);
    }

    #[test]
    fn flags_override_file_and_managed_defaults_to_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("allod.toml");
        std::fs::write(
            &path,
            "listen = \"0.0.0.0:9000\"\nmode = \"managed\"\nhandoff_dir = \"/h\"\n\
             relays = [\"ws://relay:8080\"]\n[ldk_server]\naddress = \"ldk:3536\"\n",
        )
        .unwrap();
        let c = Config::load(ConfigArgs {
            config: Some(path),
            listen: Some("127.0.0.1:1".parse().unwrap()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(c.listen.to_string(), "127.0.0.1:1");
        assert_eq!(c.mode, DeploymentMode::Managed);
        assert_eq!(c.ldk_server_address.as_deref(), Some("ldk:3536"));
        // Managed mode never defaults to the admin credential in the handoff directory.
        assert_eq!(c.ldk_server_cert_file, None);
        assert_eq!(c.handoff_dir, PathBuf::from("/h"));
        assert_eq!(c.relays, vec!["ws://relay:8080"]);
    }

    #[test]
    fn rejects_bad_relay_and_unknown_keys() {
        let err = Config::load(ConfigArgs {
            relays: Some(vec!["https://x".into()]),
            ..Default::default()
        });
        assert!(err.is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "nope = 1\n").unwrap();
        assert!(Config::load(ConfigArgs { config: Some(path), ..Default::default() }).is_err());
    }

    #[test]
    fn lsps2_lsp_from_file_env_or_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("allod.toml");
        std::fs::write(
            &path,
            "[lsp]\nlsps2 = \"0217890e3aad8d35bc054f43acc00084b25229ecff0ab68debd82883ad65ee8266@69.59.18.144:9735\"\n",
        )
        .unwrap();
        let c = Config::load(ConfigArgs { config: Some(path), ..Default::default() }).unwrap();
        assert_eq!(
            c.lsps2_lsp.as_deref(),
            Some("0217890e3aad8d35bc054f43acc00084b25229ecff0ab68debd82883ad65ee8266@69.59.18.144:9735")
        );
        // The flag beats the file; an explicit empty value turns the LSP off.
        let c = Config::load(ConfigArgs {
            lsps2_lsp: Some("02".to_string() + &"11".repeat(32) + "@lsp.example:9735"),
            ..Default::default()
        })
        .unwrap();
        assert!(c.lsps2_lsp.as_deref().unwrap().ends_with("@lsp.example:9735"));
        let c = Config::load(ConfigArgs { lsps2_lsp: Some(String::new()), ..Default::default() })
            .unwrap();
        assert!(c.lsps2_lsp.is_none());
        // Malformed values are rejected.
        for bad in ["no-at-sign", "0211@no-port", "0211@host:notaport", "zz@example:9735"] {
            assert!(Config::load(ConfigArgs { lsps2_lsp: Some(bad.into()), ..Default::default() })
                .is_err());
        }
    }

    #[test]
    fn dev_auto_setup_only_on_regtest_with_owner() {
        let base = || ConfigArgs {
            dev_auto_setup: Some(true),
            dev_owner_pubkey: Some("aa".into()),
            network: Some(Network::Regtest),
            ..Default::default()
        };
        assert!(Config::load(base()).is_ok());
        assert!(Config::load(ConfigArgs { network: Some(Network::Signet), ..base() }).is_err());
        assert!(Config::load(ConfigArgs { network: None, ..base() }).is_err());
        assert!(Config::load(ConfigArgs { dev_owner_pubkey: None, ..base() }).is_err());
    }
}
