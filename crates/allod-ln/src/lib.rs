//! Lightning backend (SPEC §5): the [`LightningBackend`] trait, its ldk-server
//! implementation with the event hub, and a `mock::MockBackend` for tests.

mod backend;
mod ldk;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod types;

use std::path::Path;

use allod_types::{Network, NodeSummary};

pub use backend::{BackendError, BackendResult, LightningBackend};
pub use ldk::LdkServerBackend;

/// Node info.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Pubkey, best block.
    pub summary: NodeSummary,
    /// Network the node runs on.
    pub network: Option<Network>,
    /// Node alias, if set.
    pub alias: Option<String>,
    /// ldk-server version string.
    pub version: String,
    pub listening_addresses: Vec<String>,
    pub announcement_addresses: Vec<String>,
    /// `pubkey@host:port` strings peers can connect to.
    pub node_uris: Vec<String>,
}

/// Channel counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelCounts {
    /// All known channels.
    pub total: usize,
    /// Channels that can send and receive now.
    pub usable: usize,
}

/// ldk-server permissions Allod needs (SPEC §3.2). `channels:force_close` is optional.
pub const REQUIRED_PERMISSIONS: &[&str] = &[
    "node:read",
    "onchain:receive",
    "onchain:send",
    "invoices:create",
    "payments:read",
    "payments:claim",
    "payments:send",
    "channels:read",
    "channels:manage",
    "peers:read",
    "peers:manage",
    "messages:sign",
    "utilities:read",
    "events:read",
    "graph:read",
];

/// Optional permissions; without them the UI hides the matching actions.
pub const OPTIONAL_PERMISSIONS: &[&str] = &["channels:force_close"];

/// Required permissions missing from `granted` ("admin" grants everything).
pub fn missing_permissions(granted: &[String]) -> Vec<&'static str> {
    if granted.iter().any(|p| p == "admin") {
        return Vec::new();
    }
    REQUIRED_PERMISSIONS.iter().copied().filter(|p| !granted.iter().any(|g| g == p)).collect()
}

/// Where and how to reach ldk-server.
#[derive(Debug, Clone)]
pub struct LdkServerConfig {
    /// `host:port` of the gRPC service.
    pub address: String,
    /// PEM certificate of the server.
    pub tls_cert_pem: Vec<u8>,
    /// Reusable hex-encoded v2 macaroon.
    pub macaroon_hex: String,
}

impl LdkServerConfig {
    /// Reads the certificate and macaroon from files. The macaroon file may hold
    /// hex text (ldk-server's `admin.macaroon`) or raw bytes.
    pub fn from_files(
        address: impl Into<String>,
        cert_path: &Path,
        macaroon_path: &Path,
    ) -> Result<Self, BackendError> {
        let tls_cert_pem = std::fs::read(cert_path)
            .map_err(|e| BackendError::Credentials(format!("read {}: {e}", cert_path.display())))?;
        let raw = std::fs::read(macaroon_path).map_err(|e| {
            BackendError::Credentials(format!("read {}: {e}", macaroon_path.display()))
        })?;
        Ok(LdkServerConfig {
            address: address.into(),
            tls_cert_pem,
            macaroon_hex: macaroon_to_hex(&raw),
        })
    }
}

fn macaroon_to_hex(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let trimmed = text.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        trimmed.to_ascii_lowercase()
    } else {
        raw.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_permission_detection() {
        assert!(missing_permissions(&["admin".into()]).is_empty());
        let all: Vec<String> = REQUIRED_PERMISSIONS.iter().map(|p| p.to_string()).collect();
        assert!(missing_permissions(&all).is_empty());
        assert_eq!(missing_permissions(&all[1..]), vec!["node:read"]);
    }

    #[test]
    fn macaroon_hex_detection() {
        assert_eq!(macaroon_to_hex(b"0201AB\n"), "0201ab");
        assert_eq!(macaroon_to_hex(&[0x02, 0x01, 0xff]), "0201ff");
    }
}
