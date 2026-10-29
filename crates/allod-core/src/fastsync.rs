//! Optional fast sync (AssumeUTXO) for the bundled bitcoind. Allod only writes the
//! owner's choice into the control directory shared with the bitcoind container; the
//! helper there (`images/bitcoind/assumeutxo.sh`) downloads the snapshot, waits for the
//! headers and calls `loadtxoutset`, and reports progress back in the same directory.
//! bitcoind accepts only snapshots whose hash is built into its release.

use std::path::{Path, PathBuf};

use allod_types::Network;
use serde::Serialize;
use serde_json::Value;

const REQUEST: &str = "fastsync-request.json";
const STATUS: &str = "fastsync-status.json";

/// A snapshot the bundled Bitcoin Core release (31.x) accepts, from a public mirror.
#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub height: u32,
    pub url: &'static str,
    pub source: &'static str,
}

/// Snapshots for the bundled bitcoind's network, newest first.
pub fn suggestion(network: Network) -> Option<Suggestion> {
    let (height, url) = match network {
        Network::Bitcoin => (935_000, "https://files-vps02.jaonoctus.dev/utxo-935000.dat"),
        Network::Signet => (290_000, "https://files-vps02.jaonoctus.dev/utxo-signet-290000.dat"),
        Network::Testnet4 => {
            (120_000, "https://files-vps02.jaonoctus.dev/utxo-testnet4-120000.dat")
        }
        _ => return None,
    };
    Some(Suggestion { height, url, source: "bitcoin-snapshots.jaonoctus.dev" })
}

#[derive(Debug, thiserror::Error)]
pub enum FastSyncError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub struct FastSync {
    dir: PathBuf,
}

impl FastSync {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FastSync { dir: dir.into() }
    }

    fn path(&self, f: &str) -> PathBuf {
        self.dir.join(f)
    }

    /// Asks bitcoind's helper to fast-sync from `url`.
    pub fn request(&self, url: &str) -> Result<(), FastSyncError> {
        let url = url.trim();
        let ok = (url.starts_with("https://") || url.starts_with("http://"))
            && url.len() < 2000
            && !url.chars().any(|c| c.is_whitespace() || c == '"' || c == '\\');
        if !ok {
            return Err(FastSyncError::BadRequest(
                "enter an https:// URL of a UTXO snapshot".into(),
            ));
        }
        if matches!(
            self.state().as_deref(),
            Some(
                "downloading"
                    | "download_error"
                    | "waiting_for_headers"
                    | "loading"
                    | "load_interrupted"
                    | "loaded"
            )
        ) {
            return Err(FastSyncError::BadRequest("fast sync is already under way".into()));
        }
        let _ = std::fs::remove_file(self.path(STATUS));
        write_atomic(&self.path(REQUEST), &serde_json::json!({ "url": url }).to_string())?;
        Ok(())
    }

    /// Cancels a request that has not been loaded yet (the helper stops downloading).
    pub fn cancel(&self) -> Result<(), FastSyncError> {
        if matches!(self.state().as_deref(), Some("loading" | "loaded")) {
            return Err(FastSyncError::BadRequest("the snapshot is already being loaded".into()));
        }
        match std::fs::remove_file(self.path(REQUEST)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn requested_url(&self) -> Option<String> {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(self.path(REQUEST)).ok()?).ok()?;
        v.get("url")?.as_str().map(str::to_string)
    }

    /// The helper's last report, as written.
    pub fn status(&self) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(self.path(STATUS)).ok()?).ok()
    }

    fn state(&self) -> Option<String> {
        self.status()?.get("state")?.as_str().map(str::to_string)
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_status_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FastSync::new(dir.path());
        assert!(fs.request("ftp://x").is_err());
        assert!(fs.request("https://x/a\"b").is_err());
        fs.request("https://example.com/utxo-935000.dat").unwrap();
        assert_eq!(fs.requested_url().as_deref(), Some("https://example.com/utxo-935000.dat"));
        std::fs::write(
            dir.path().join(STATUS),
            r#"{"state":"downloading","bytes":10,"total":100}"#,
        )
        .unwrap();
        assert_eq!(fs.status().unwrap()["bytes"], 10);
        assert!(fs.request("https://other").is_err(), "already under way");
        fs.cancel().unwrap();
        assert!(fs.requested_url().is_none());
        std::fs::write(dir.path().join(STATUS), r#"{"state":"loading"}"#).unwrap();
        assert!(fs.cancel().is_err());
        assert_eq!(suggestion(Network::Bitcoin).unwrap().height, 935_000);
        assert!(suggestion(Network::Regtest).is_none());
    }
}
