//! Domain types shared by every Allod crate.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Millisatoshis. All internal amounts use this unit (SPEC §6.1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Msat(pub u64);

impl Msat {
    /// Whole satoshis, rounded down.
    pub fn sats_floor(self) -> u64 {
        self.0 / 1000
    }
}

/// Error for parsing one of the string enums in this crate.
#[derive(Debug, thiserror::Error)]
#[error("invalid {kind}: {value}")]
pub struct ParseEnumError {
    kind: &'static str,
    value: String,
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident, $kind:literal { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The canonical string form.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err(ParseEnumError { kind: $kind, value: other.to_string() }),
                }
            }
        }
    };
}

string_enum!(
    /// Bitcoin network, using ldk-server's names.
    Network, "network" {
        Bitcoin => "bitcoin",
        Testnet => "testnet",
        Testnet4 => "testnet4",
        Signet => "signet",
        Regtest => "regtest",
    }
);

impl Network {
    /// NIP-47 name (`mainnet` instead of `bitcoin`).
    pub fn nip47_name(self) -> &'static str {
        match self {
            Network::Bitcoin => "mainnet",
            other => other.as_str(),
        }
    }
}

string_enum!(
    /// How the Allod stack reaches the Bitcoin chain (SPEC §2.3.2).
    ChainMode, "chain mode" {
        Bundled => "bundled",
        BundledRest => "bundled-rest",
        ExternalBitcoind => "external-bitcoind",
        Electrum => "electrum",
        Esplora => "esplora",
    }
);

string_enum!(
    /// Whether Allod manages its own ldk-server (SPEC §2.3.3).
    DeploymentMode, "deployment mode" {
        Managed => "managed",
        Standalone => "standalone",
    }
);

string_enum!(
    /// Host platform; only changes UI hints.
    Platform, "platform" {
        Docker => "docker",
        Umbrel => "umbrel",
        Startos => "startos",
        Other => "other",
    }
);

/// Overall health state reported by `GET /api/health` (SPEC §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Everything required works.
    Ok,
    /// Allod runs, but a dependency is missing or unreachable.
    Degraded,
    /// Allod cannot serve requests.
    Down,
}

/// Result of one health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// True when the check passed.
    pub ok: bool,
    /// Human-readable detail (error message or a short summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Check {
    /// A passing check.
    pub fn pass(detail: impl Into<String>) -> Self {
        Check { ok: true, detail: Some(detail.into()) }
    }

    /// A failing check.
    pub fn fail(detail: impl Into<String>) -> Self {
        Check { ok: false, detail: Some(detail.into()) }
    }
}

/// Lightning node summary used by `/api/info` and `/api/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSummary {
    /// Node public key (hex).
    pub pubkey: String,
    /// Height of the best block ldk-server has processed.
    pub block_height: u32,
    /// Hash of that block.
    pub block_hash: String,
}

/// Body of `GET /api/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Overall status.
    pub status: HealthStatus,
    /// Database reachability.
    pub db: Check,
    /// ldk-server reachability.
    pub ldk_server: Check,
    /// Node summary when ldk-server answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeSummary>,
}

/// Deployment info inside `GET /api/info` (SPEC §2.3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentInfo {
    /// Managed or standalone.
    pub mode: DeploymentMode,
    /// Host platform.
    pub platform: Platform,
    /// Chain mode, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_mode: Option<ChainMode>,
    /// Node explorer link template with a `{pubkey}` placeholder
    /// (`ALLOD_NODE_EXPLORER_URL`); `None` shows no links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_explorer_url: Option<String>,
}

/// Body of `GET /api/info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoResponse {
    /// Allod version.
    pub version: String,
    /// Whether first-run setup has completed (always false until M1).
    pub setup_completed: bool,
    /// Whether services are running.
    pub running: bool,
    /// Lightning backend type.
    pub backend_type: String,
    /// Network, when ldk-server answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    /// Deployment info.
    pub deployment: DeploymentInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip() {
        for mode in ChainMode::ALL {
            assert_eq!(mode.as_str().parse::<ChainMode>().unwrap(), *mode);
            let json = serde_json::to_string(mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
        }
        assert!("mainnet".parse::<Network>().is_err());
        assert_eq!(Network::Bitcoin.nip47_name(), "mainnet");
        assert_eq!(Network::Signet.nip47_name(), "signet");
    }

    #[test]
    fn health_serializes_lowercase() {
        let h = HealthResponse {
            status: HealthStatus::Degraded,
            db: Check::pass("sqlite"),
            ldk_server: Check::fail("not configured"),
            node: None,
        };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["status"], "degraded");
        assert!(v.get("node").is_none());
    }
}
