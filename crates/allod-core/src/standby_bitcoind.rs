//! Standby bitcoind (SPEC §2.3.2): a pruned Bitcoin Core that the platform package ships
//! idle next to an external node (Umbrel: when Allod uses the Bitcoin Node app). If that
//! node goes away, Allod suggests starting it, so the Lightning node is not left without
//! a chain source.
//!
//! Allod only writes [`ENABLED`] into the control directory it shares with the bitcoind
//! container; the container waits for that file before starting bitcoind. Allod then
//! points ldk-server at it through the chain source request (`chain_source`).

use std::path::PathBuf;
use std::sync::Mutex;

use crate::BitcoindRpc;

/// Written by Allod: start the standby bitcoind. Never removed by Allod.
pub const ENABLED: &str = "bitcoind-enabled";

/// How long the node's bitcoind must be unreachable before Allod suggests the standby
/// node. Long enough to ride out a restart or an update of the external node's app.
pub const SUGGEST_AFTER_SECS: i64 = 180;

/// The standby node's RPC endpoint and the reachability of the external node it backs.
pub struct StandbyBitcoind {
    control_dir: PathBuf,
    rpc_address: String,
    rpc_user: String,
    rpc_pass: String,
    probe: Mutex<Probe>,
}

#[derive(Debug, Default, Clone)]
struct Probe {
    /// First failed probe of the current outage; None while reachable or unknown.
    unreachable_since: Option<i64>,
    last_error: Option<String>,
}

impl std::fmt::Debug for StandbyBitcoind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandbyBitcoind")
            .field("control_dir", &self.control_dir)
            .field("rpc_address", &self.rpc_address)
            .finish_non_exhaustive()
    }
}

impl StandbyBitcoind {
    pub fn new(
        control_dir: impl Into<PathBuf>,
        rpc_address: impl Into<String>,
        rpc_user: impl Into<String>,
        rpc_pass: impl Into<String>,
    ) -> Self {
        StandbyBitcoind {
            control_dir: control_dir.into(),
            rpc_address: rpc_address.into(),
            rpc_user: rpc_user.into(),
            rpc_pass: rpc_pass.into(),
            probe: Mutex::new(Probe::default()),
        }
    }

    /// RPC `host:port`, as ldk-server's chain source takes it.
    pub fn rpc_address(&self) -> &str {
        &self.rpc_address
    }

    pub fn rpc_user(&self) -> &str {
        &self.rpc_user
    }

    pub fn rpc_pass(&self) -> &str {
        &self.rpc_pass
    }

    /// The standby node's RPC, for sync progress and fee estimates once it is in use.
    pub fn rpc(&self) -> BitcoindRpc {
        BitcoindRpc {
            url: format!("http://{}", self.rpc_address),
            user: self.rpc_user.clone(),
            pass: self.rpc_pass.clone(),
        }
    }

    /// Whether the owner started it (the container then runs bitcoind).
    pub fn enabled(&self) -> bool {
        self.control_dir.join(ENABLED).exists()
    }

    /// Starts the standby node. Idempotent.
    pub fn enable(&self, now: i64) -> std::io::Result<()> {
        if self.enabled() {
            return Ok(());
        }
        let path = self.control_dir.join(ENABLED);
        let tmp = self.control_dir.join(format!("{ENABLED}.tmp"));
        std::fs::write(&tmp, format!("{{\"enabled_at\":{now}}}\n"))?;
        std::fs::rename(tmp, path)
    }

    /// Records one reachability check of the external node.
    pub fn record_probe(&self, result: Result<(), String>, now: i64) {
        let mut p = self.probe.lock().expect("probe lock");
        match result {
            Ok(()) => *p = Probe::default(),
            Err(e) => {
                p.unreachable_since.get_or_insert(now);
                p.last_error = Some(e);
            }
        }
    }

    /// Since when the external node has failed every check, and the last error.
    pub fn outage(&self) -> Option<(i64, String)> {
        let p = self.probe.lock().expect("probe lock").clone();
        Some((p.unreachable_since?, p.last_error.unwrap_or_default()))
    }

    /// Clears the outage, e.g. when the node no longer reads from the external node.
    pub fn reset_probe(&self) {
        *self.probe.lock().expect("probe lock") = Probe::default();
    }

    /// Whether the outage is long enough to suggest the standby node.
    pub fn outage_long_enough(&self, now: i64) -> bool {
        self.outage().is_some_and(|(since, _)| now - since >= SUGGEST_AFTER_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_writes_marker_once() {
        let dir = tempfile::tempdir().unwrap();
        // Not "pw": Debug shows the random temp dir path, which can contain those letters.
        let s = StandbyBitcoind::new(dir.path(), "allod_bitcoind_1:8332", "ldk", "rpc-pass:42");
        assert!(!s.enabled());
        s.enable(100).unwrap();
        assert!(s.enabled());
        let first = std::fs::read_to_string(dir.path().join(ENABLED)).unwrap();
        s.enable(200).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join(ENABLED)).unwrap(), first);
        assert_eq!(s.rpc().url, "http://allod_bitcoind_1:8332");
        assert!(!format!("{s:?}").contains("rpc-pass:42"), "Debug hides the password");
    }

    #[test]
    fn outage_needs_consecutive_failures() {
        let dir = tempfile::tempdir().unwrap();
        let s = StandbyBitcoind::new(dir.path(), "a:1", "u", "p");
        assert!(s.outage().is_none());
        s.record_probe(Err("refused".into()), 1000);
        s.record_probe(Err("timeout".into()), 1030);
        assert_eq!(s.outage(), Some((1000, "timeout".into())));
        assert!(!s.outage_long_enough(1000 + SUGGEST_AFTER_SECS - 1));
        assert!(s.outage_long_enough(1000 + SUGGEST_AFTER_SECS));
        s.record_probe(Ok(()), 1300);
        assert!(s.outage().is_none());
        s.record_probe(Err("refused".into()), 1400);
        assert!(!s.outage_long_enough(1400 + 10), "a new outage starts over");
        s.reset_probe();
        assert!(s.outage().is_none());
    }
}
