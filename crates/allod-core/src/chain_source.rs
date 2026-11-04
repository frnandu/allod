//! Chain source chosen in Settings (SPEC §2.3.2). The owner can switch ldk-server
//! between the stack's default (`ALLOD_CHAIN` and the `.env` settings), an external
//! Bitcoin Core node over RPC, and an external Electrum or Esplora server (Alby,
//! mempool.space, Blockstream or their own). Allod never runs an Electrum or Esplora
//! server itself.
//!
//! Allod writes the choice into the handoff directory; ldk-server's entrypoint applies
//! it, restarts the node when it changes, and reports the source the node started with
//! in a second file. Each request carries an id that the entrypoint echoes back, so
//! Allod knows when the node has restarted with it.

use std::path::PathBuf;

use allod_types::Network;
use serde::{Deserialize, Serialize};

/// Written by Allod: the owner's choice (may hold an RPC password, mode 0600).
pub const REQUEST: &str = "chain-source.json";
/// Written by ldk-server's entrypoint: the source the node started with (no secrets).
pub const ACTIVE: &str = "chain-source.active.json";

/// What the node reads the chain from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// The stack's configuration (`ALLOD_CHAIN` and `.env`).
    Default,
    /// A Bitcoin Core node over JSON-RPC.
    Bitcoind,
    Electrum,
    Esplora,
}

/// The owner's choice as stored for ldk-server's entrypoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainSource {
    pub kind: SourceKind,
    /// Electrum or Esplora server URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Bitcoin Core RPC `host:port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_password: Option<String>,
    /// The stack's standby bitcoind (`standby_bitcoind`), which Allod watches like the
    /// default one. ldk-server's entrypoint ignores it: to the node it is a `bitcoind`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standby: bool,
    /// Echoed back by the entrypoint in the active file.
    #[serde(default)]
    pub id: String,
}

/// A chain source without its secret, for the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceView {
    pub kind: SourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_user: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub standby: bool,
}

impl ChainSource {
    pub fn view(&self) -> SourceView {
        SourceView {
            kind: self.kind,
            url: self.url.clone(),
            rpc_address: self.rpc_address.clone(),
            rpc_user: self.rpc_user.clone(),
            standby: self.standby,
        }
    }
}

/// What ldk-server's entrypoint reports after a start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSource {
    /// Effective `LDK_CHAIN` (bundled, bundled-rest, external-bitcoind, electrum, esplora).
    pub mode: String,
    /// bitcoind, electrum or esplora.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_address: Option<String>,
    /// Id of the request it applied; empty when it ran on the `.env` settings alone.
    #[serde(default, skip_serializing)]
    pub request_id: Option<String>,
}

/// A public server offered in Settings.
#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: SourceKind,
    pub url: &'static str,
}

/// Public servers for `network`. Regtest has none: point it at your own server.
pub fn presets(network: Network) -> Vec<Preset> {
    use SourceKind::*;
    let p = |id, name, kind, url| Preset { id, name, kind, url };
    match network {
        Network::Bitcoin => vec![
            p("alby-esplora", "Alby", Esplora, "https://electrs.getalbypro.com"),
            p("alby-electrum", "Alby", Electrum, "ssl://electrum.getalbypro.com:50002"),
            p("mempool-esplora", "mempool.space", Esplora, "https://mempool.space/api"),
            p("blockstream-esplora", "Blockstream", Esplora, "https://blockstream.info/api"),
            p(
                "blockstream-electrum",
                "Blockstream",
                Electrum,
                "ssl://electrum.blockstream.info:50002",
            ),
        ],
        Network::Signet => {
            vec![p("mempool-esplora", "mempool.space", Esplora, "https://mempool.space/signet/api")]
        }
        Network::Testnet4 => vec![p(
            "mempool-esplora",
            "mempool.space",
            Esplora,
            "https://mempool.space/testnet4/api",
        )],
        Network::Testnet => vec![
            p("mempool-esplora", "mempool.space", Esplora, "https://mempool.space/testnet/api"),
            p(
                "blockstream-esplora",
                "Blockstream",
                Esplora,
                "https://blockstream.info/testnet/api",
            ),
            p(
                "blockstream-electrum",
                "Blockstream",
                Electrum,
                "ssl://electrum.blockstream.info:60002",
            ),
        ],
        Network::Regtest => Vec::new(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChainSourceError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

fn bad<T>(m: &str) -> Result<T, ChainSourceError> {
    Err(ChainSourceError::BadRequest(m.into()))
}

/// No characters that could break the generated config or a shell argument.
fn plain(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && !s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\\' | '\''))
}

fn host_port(s: &str) -> bool {
    s.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty() && !host.contains('/') && port.parse::<u16>().is_ok_and(|p| p > 0)
    })
}

/// Checks and normalises an Electrum or Esplora URL.
pub fn validate_url(kind: SourceKind, url: &str) -> Result<String, ChainSourceError> {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() {
        return bad("enter the server URL");
    }
    if !plain(url, 500) {
        return bad("the URL contains characters that are not allowed");
    }
    match kind {
        SourceKind::Electrum => {
            let rest = url.strip_prefix("ssl://").or_else(|| url.strip_prefix("tcp://"));
            let Some(rest) = rest else {
                return bad("an Electrum URL starts with ssl:// or tcp://, e.g. ssl://host:50002");
            };
            if !host_port(rest) {
                return bad("an Electrum URL needs a host and port, e.g. ssl://host:50002");
            }
        }
        SourceKind::Esplora => {
            let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
            if !rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/')) {
                return bad("an Esplora URL starts with https://, e.g. https://mempool.space/api");
            }
        }
        _ => return bad("not a server URL kind"),
    }
    Ok(url.to_string())
}

/// What the owner submits in Settings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceRequest {
    pub kind: Option<SourceKind>,
    pub url: Option<String>,
    pub rpc_address: Option<String>,
    pub rpc_user: Option<String>,
    /// Empty keeps the stored password when address and user are unchanged.
    pub rpc_password: Option<String>,
}

/// Reads and writes the chain source files in the handoff directory.
pub struct ChainSourceStore {
    dir: PathBuf,
}

impl ChainSourceStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        ChainSourceStore { dir: dir.into() }
    }

    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        serde_json::from_str(&std::fs::read_to_string(self.dir.join(name)).ok()?).ok()
    }

    /// The owner's choice, if any.
    pub fn requested(&self) -> Option<ChainSource> {
        self.read(REQUEST)
    }

    /// The source the node runs with, as its entrypoint reported it.
    pub fn active(&self) -> Option<ActiveSource> {
        self.read(ACTIVE)
    }

    /// Whether the node still has to restart to apply the owner's choice.
    pub fn restart_pending(&self) -> bool {
        match self.requested() {
            None => false,
            Some(r) => self.active().and_then(|a| a.request_id).as_deref() != Some(r.id.as_str()),
        }
    }

    /// Whether the node reads the chain from the stack's default bitcoind, the one
    /// Allod watches for sync progress and fee estimates. Unknown counts as yes.
    pub fn uses_default_bitcoind(&self) -> bool {
        let Some(active) = self.active() else { return true };
        let custom = self.requested().is_some_and(|r| {
            r.kind == SourceKind::Bitcoind
                && !r.standby
                && active.request_id.as_deref() == Some(r.id.as_str())
        });
        active.kind == "bitcoind" && !custom
    }

    /// Whether the node is meant to run on the stack's default source (`ALLOD_CHAIN`):
    /// the owner never picked another one in Settings, or picked the default again.
    pub fn default_in_use(&self) -> bool {
        self.requested().is_none_or(|r| r.kind == SourceKind::Default)
    }

    /// Whether the owner chose the standby bitcoind (applied or still restarting).
    pub fn wants_standby(&self) -> bool {
        self.requested().is_some_and(|r| r.standby)
    }

    /// Points ldk-server at the standby bitcoind (`standby_bitcoind`).
    pub fn request_standby(
        &self,
        standby: &crate::standby_bitcoind::StandbyBitcoind,
    ) -> Result<ChainSource, ChainSourceError> {
        let source = ChainSource {
            kind: SourceKind::Bitcoind,
            url: None,
            rpc_address: Some(standby.rpc_address().to_string()),
            rpc_user: Some(standby.rpc_user().to_string()),
            rpc_password: Some(standby.rpc_pass().to_string()),
            standby: true,
            id: new_id(),
        };
        self.write(&source)?;
        Ok(source)
    }

    fn write(&self, source: &ChainSource) -> Result<(), ChainSourceError> {
        std::fs::create_dir_all(&self.dir)?;
        let text = serde_json::to_string(source).expect("serializable");
        allod_keys::write_file_with_mode(&self.dir.join(REQUEST), &text, 0o600)?;
        Ok(())
    }

    /// Checks and saves the owner's choice. ldk-server's entrypoint notices it and
    /// restarts the node with the new source.
    pub fn request(&self, req: SourceRequest) -> Result<ChainSource, ChainSourceError> {
        let Some(kind) = req.kind else { return bad("choose a chain source") };
        let mut source = ChainSource {
            kind,
            url: None,
            rpc_address: None,
            rpc_user: None,
            rpc_password: None,
            standby: false,
            id: new_id(),
        };
        match kind {
            SourceKind::Default => {}
            SourceKind::Electrum | SourceKind::Esplora => {
                source.url = Some(validate_url(kind, req.url.as_deref().unwrap_or(""))?);
            }
            SourceKind::Bitcoind => {
                let address = req.rpc_address.as_deref().unwrap_or("").trim();
                let address =
                    address.strip_prefix("http://").unwrap_or(address).trim_end_matches('/');
                if !plain(address, 300) || !host_port(address) {
                    return bad(
                        "enter the node's RPC address as host:port, e.g. 192.168.1.10:8332",
                    );
                }
                let user = req.rpc_user.as_deref().unwrap_or("").trim();
                if !plain(user, 200) || user.contains(':') {
                    return bad("enter the RPC user name (no spaces, quotes or colons)");
                }
                let mut password = req.rpc_password.unwrap_or_default();
                if password.is_empty() {
                    // Keep the stored password when only re-saving the same node.
                    password = self
                        .requested()
                        .filter(|r| {
                            r.kind == SourceKind::Bitcoind
                                && r.rpc_address.as_deref() == Some(address)
                                && r.rpc_user.as_deref() == Some(user)
                        })
                        .and_then(|r| r.rpc_password)
                        .unwrap_or_default();
                }
                if !plain(&password, 500) {
                    return bad("enter the RPC password (no spaces or quotes)");
                }
                source.rpc_address = Some(address.to_string());
                source.rpc_user = Some(user.to_string());
                source.rpc_password = Some(password);
            }
        }
        self.write(&source)?;
        Ok(source)
    }
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    hex::encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: SourceKind) -> SourceRequest {
        SourceRequest { kind: Some(kind), ..Default::default() }
    }

    #[test]
    fn validates_urls() {
        use SourceKind::*;
        assert_eq!(
            validate_url(Electrum, " ssl://h.example:50002 ").unwrap(),
            "ssl://h.example:50002"
        );
        assert!(validate_url(Electrum, "tcp://10.0.0.2:50001").is_ok());
        assert!(validate_url(Electrum, "https://h.example").is_err());
        assert!(validate_url(Electrum, "ssl://h.example").is_err());
        assert!(validate_url(Electrum, "ssl://:50002").is_err());
        assert_eq!(
            validate_url(Esplora, "https://mempool.space/api/").unwrap(),
            "https://mempool.space/api"
        );
        assert!(validate_url(Esplora, "http://abc.onion/api").is_ok());
        assert!(validate_url(Esplora, "ssl://h:1").is_err());
        assert!(validate_url(Esplora, "https://").is_err());
        assert!(validate_url(Esplora, "https://a\"b").is_err());
        assert!(validate_url(Esplora, "").is_err());
    }

    #[test]
    fn presets_are_valid() {
        for n in [Network::Bitcoin, Network::Signet, Network::Testnet4, Network::Testnet] {
            for p in presets(n) {
                assert_eq!(validate_url(p.kind, p.url).unwrap(), p.url, "{}", p.id);
            }
        }
        assert!(presets(Network::Regtest).is_empty());
    }

    #[test]
    fn bitcoind_request_keeps_password() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainSourceStore::new(dir.path());
        let mut r = req(SourceKind::Bitcoind);
        r.rpc_address = Some("10.0.0.5:8332".into());
        r.rpc_user = Some("ldk".into());
        assert!(store.request(r.clone()).is_err(), "password required");
        r.rpc_password = Some("secret".into());
        store.request(r.clone()).unwrap();
        r.rpc_password = Some(String::new());
        let s = store.request(r.clone()).unwrap();
        assert_eq!(s.rpc_password.as_deref(), Some("secret"));
        assert!(serde_json::to_string(&s.view()).unwrap().find("secret").is_none());
        r.rpc_user = Some("other".into());
        assert!(store.request(r.clone()).is_err(), "new user needs a password");
        r.rpc_address = Some("10.0.0.5".into());
        r.rpc_password = Some("x".into());
        assert!(store.request(r).is_err(), "port required");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(REQUEST)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn request_and_active() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainSourceStore::new(dir.path());
        assert!(store.requested().is_none());
        assert!(!store.restart_pending());
        assert!(store.request(SourceRequest::default()).is_err());
        let mut r = req(SourceKind::Esplora);
        r.url = Some("ftp://x".into());
        assert!(store.request(r.clone()).is_err());
        r.url = Some("https://mempool.space/api".into());
        let s = store.request(r).unwrap();
        assert_eq!(store.requested(), Some(s.clone()));
        assert!(store.restart_pending());
        std::fs::write(
            dir.path().join(ACTIVE),
            format!(
                r#"{{"mode":"esplora","kind":"esplora","url":"https://mempool.space/api","request_id":"{}"}}"#,
                s.id
            ),
        )
        .unwrap();
        assert!(!store.restart_pending());
        assert_eq!(store.active().unwrap().mode, "esplora");
        assert!(!store.uses_default_bitcoind());
        std::fs::write(dir.path().join(ACTIVE), r#"{"mode":"bundled","kind":"bitcoind"}"#).unwrap();
        assert!(store.uses_default_bitcoind());
        let d = store.request(req(SourceKind::Default)).unwrap();
        assert_ne!(d.id, s.id);
        assert!(store.restart_pending());
    }

    #[test]
    fn standby_counts_as_watched_bitcoind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainSourceStore::new(dir.path());
        let standby = crate::standby_bitcoind::StandbyBitcoind::new(
            dir.path(),
            "allod_bitcoind_1:8332",
            "ldk",
            "pw",
        );
        assert!(!store.wants_standby());
        let s = store.request_standby(&standby).unwrap();
        assert!(store.wants_standby());
        let raw = std::fs::read_to_string(dir.path().join(REQUEST)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // ldk-server's entrypoint reads it as an ordinary Bitcoin Core source.
        assert_eq!(v["kind"], "bitcoind");
        assert_eq!(v["rpc_address"], "allod_bitcoind_1:8332");
        assert_eq!(v["rpc_password"], "pw");
        assert!(s.view().standby);
        std::fs::write(
            dir.path().join(ACTIVE),
            format!(
                r#"{{"mode":"external-bitcoind","kind":"bitcoind","rpc_address":"allod_bitcoind_1:8332","request_id":"{}"}}"#,
                s.id
            ),
        )
        .unwrap();
        assert!(store.uses_default_bitcoind(), "Allod watches the standby node itself");
        // Re-saving the same node from the Settings form is a custom node again.
        let mut r = req(SourceKind::Bitcoind);
        r.rpc_address = Some("allod_bitcoind_1:8332".into());
        r.rpc_user = Some("ldk".into());
        r.rpc_password = Some("pw".into());
        store.request(r).unwrap();
        assert!(!store.wants_standby());
    }
}
