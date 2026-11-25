//! The watched bitcoind's chain tip, live, from its ZMQ `sequence` notifications.
//!
//! Every chain RPC (`getblockchaininfo`, even `getbestblockhash` or `waitfornewblock`)
//! waits for bitcoind's chain lock, and background validation after a fast sync holds it
//! for minutes at a time on a small machine. ZMQ needs no RPC and no lock: bitcoind
//! publishes a `C` (connected) or `D` (disconnected) event for each block of the active
//! chain as it happens. The endpoint comes from `getzmqnotifications`, which does not
//! take the lock either.
//!
//! The events carry a hash, not a height. The height is counted from bitcoind's last chain
//! answer (hash and height): when that hash is among the connected blocks seen, the tip is
//! that many blocks above it. When it is not, every block seen came after it, provided the
//! answer was asked for after the subscription started (else blocks in between may have
//! been missed). A gap in ZMQ's message counter, a reconnect or a disconnect of a block
//! not seen starts over.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use zeromq::{Socket, SocketEvent, SocketRecv};

/// Connected blocks kept (about two days).
const KEEP: usize = 288;

/// What the subscription has seen since it (re)started.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    /// Connected to bitcoind's publisher.
    pub up: bool,
    /// When the record (re)started (unix seconds): blocks before it may be missing.
    pub since: i64,
    /// Active-chain blocks connected since, oldest first: (hash, unix seconds seen).
    pub connected: VecDeque<(String, i64)>,
    /// Blocks disconnected since (reorgs).
    pub disconnected: VecDeque<String>,
    /// ZMQ's per-topic message counter of the last message.
    pub last_seq: Option<u32>,
}

/// The tip as ZMQ tells it, measured against a chain answer.
#[derive(Debug, PartialEq)]
pub(crate) struct LiveTip {
    pub hash: String,
    pub height: u64,
    /// When the newest block was seen; `None` when the chain answer is still the tip.
    pub seen_at: Option<i64>,
}

impl Seen {
    fn restart(&mut self, now: i64) {
        self.connected.clear();
        self.disconnected.clear();
        self.since = now;
    }

    /// One `sequence` message: topic, body (32-byte hash in RPC order, label, and for
    /// mempool events an 8-byte sequence), message counter (u32 LE).
    pub fn on_message(&mut self, body: &[u8], counter: Option<u32>, now: i64) {
        if let Some(c) = counter {
            if self.last_seq.is_some_and(|l| l.wrapping_add(1) != c) {
                // Messages were dropped, or bitcoind restarted.
                self.restart(now);
            }
            self.last_seq = Some(c);
        }
        if body.len() < 33 {
            return;
        }
        let hash = hex::encode(&body[..32]);
        match body[32] {
            b'C' => {
                self.connected.push_back((hash, now));
                if self.connected.len() > KEEP {
                    // Blocks before `since` are no longer all here.
                    self.connected.pop_front();
                    self.since = now;
                }
            }
            b'D' => {
                if self.connected.back().is_some_and(|(h, _)| *h == hash) {
                    self.connected.pop_back();
                    self.disconnected.push_back(hash);
                    if self.disconnected.len() > KEEP {
                        self.disconnected.pop_front();
                    }
                } else {
                    // A block from before the record: its height is no longer known.
                    self.restart(now);
                }
            }
            _ => {}
        }
    }

    /// The tip, given bitcoind's chain answer (`hash` at `height`, asked at `asked_at`).
    /// `None` when ZMQ cannot vouch for it.
    pub fn tip(&self, hash: &str, height: u64, asked_at: i64) -> Option<LiveTip> {
        if !self.up || self.disconnected.iter().any(|h| h == hash) {
            return None;
        }
        let n = self.connected.len() as u64;
        let newest = self.connected.back();
        match self.connected.iter().position(|(h, _)| h == hash) {
            Some(i) => Some(match newest {
                Some((h, at)) if (i as u64) < n - 1 => LiveTip {
                    hash: h.clone(),
                    height: height + (n - 1 - i as u64),
                    seen_at: Some(*at),
                },
                _ => LiveTip { hash: hash.to_string(), height, seen_at: None },
            }),
            // Every block seen came after the answer's tip.
            None if asked_at > self.since => Some(match newest {
                Some((h, at)) => {
                    LiveTip { hash: h.clone(), height: height + n, seen_at: Some(*at) }
                }
                None => LiveTip { hash: hash.to_string(), height, seen_at: None },
            }),
            None => None,
        }
    }
}

/// A subscription to one bitcoind's `sequence` publisher; stops when dropped.
pub(crate) struct ZmqWatch {
    pub endpoint: String,
    pub seen: Arc<Mutex<Seen>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ZmqWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ZmqWatch {
    pub fn start(endpoint: String) -> Self {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let task = tokio::spawn(run(endpoint.clone(), seen.clone()));
        ZmqWatch { endpoint, seen, task }
    }

    /// [`overlay`] with what this subscription has seen.
    pub fn overlay(&self, v: &mut Value) {
        overlay(&self.seen.lock().unwrap_or_else(|e| e.into_inner()), v);
    }
}

/// Lays the live tip over a status view (`blocks`, `best_block_hash`, `asked_at`):
/// `tip_live` when ZMQ vouches for the tip, and a newer block with `tip_source: "zmq"` and
/// `tip_seen_at`.
pub(crate) fn overlay(seen: &Seen, v: &mut Value) {
    let (Some(hash), Some(height), Some(asked_at)) =
        (v["best_block_hash"].as_str(), v["blocks"].as_u64(), v["asked_at"].as_i64())
    else {
        return;
    };
    let Some(tip) = seen.tip(hash, height, asked_at) else { return };
    v["tip_live"] = json!(true);
    if let Some(seen_at) = tip.seen_at {
        v["blocks"] = json!(tip.height);
        v["best_block_hash"] = json!(tip.hash);
        v["best_block_time"] = Value::Null;
        v["tip_source"] = json!("zmq");
        v["tip_seen_at"] = json!(seen_at);
        if v["headers"].as_u64().is_some_and(|h| h < tip.height) {
            v["headers"] = json!(tip.height);
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Subscribes and records; reconnects (the socket retries by itself once connected).
async fn run(endpoint: String, seen: Arc<Mutex<Seen>>) {
    let mut backoff = std::time::Duration::from_secs(5);
    loop {
        let mut socket = zeromq::SubSocket::new();
        let mut events = socket.monitor();
        let started = async {
            socket.connect(&endpoint).await?;
            socket.subscribe("sequence").await
        };
        if let Err(e) = started.await {
            tracing::debug!(%endpoint, error = %e, "bitcoind ZMQ: cannot subscribe");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(300));
            continue;
        }
        backoff = std::time::Duration::from_secs(5);
        {
            let mut s = seen.lock().unwrap_or_else(|e| e.into_inner());
            s.up = true;
            s.last_seq = None;
            s.restart(unix_now());
        }
        tracing::info!(%endpoint, "bitcoind ZMQ: following the chain tip");
        loop {
            use futures_util::StreamExt;
            tokio::select! {
                msg = socket.recv() => match msg {
                    Ok(m) => {
                        let body = m.get(1).map(|b| b.to_vec()).unwrap_or_default();
                        let counter = m
                            .get(2)
                            .and_then(|c| <[u8; 4]>::try_from(c.as_ref()).ok())
                            .map(u32::from_le_bytes);
                        seen.lock().unwrap_or_else(|e| e.into_inner()).on_message(&body, counter, unix_now());
                    }
                    Err(e) => {
                        tracing::debug!(%endpoint, error = %e, "bitcoind ZMQ: receive failed");
                        break;
                    }
                },
                ev = events.next() => match ev {
                    Some(SocketEvent::Disconnected(_)) => {
                        seen.lock().unwrap_or_else(|e| e.into_inner()).up = false;
                    }
                    Some(SocketEvent::Connected(..)) => {
                        let mut s = seen.lock().unwrap_or_else(|e| e.into_inner());
                        s.up = true;
                        s.last_seq = None;
                        s.restart(unix_now());
                    }
                    Some(_) => {}
                    None => break,
                },
            }
        }
        seen.lock().unwrap_or_else(|e| e.into_inner()).up = false;
        tokio::time::sleep(backoff).await;
    }
}

/// The `sequence` publisher from `getzmqnotifications`, reachable at the RPC host when it
/// binds every interface (`tcp://0.0.0.0:28335` → `tcp://bitcoind:28335`).
pub(crate) fn sequence_endpoint(notifications: &Value, rpc_url: &str) -> Option<String> {
    let address = notifications.as_array()?.iter().find(|n| n["type"] == "pubsequence")?["address"]
        .as_str()?;
    let rest = address.strip_prefix("tcp://")?;
    let (host, port) = rest.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let wildcard = matches!(host, "0.0.0.0" | "*" | "[::]" | "::");
    if !wildcard {
        return Some(address.to_string());
    }
    let rpc_host = reqwest::Url::parse(rpc_url).ok()?.host_str()?.to_string();
    let rpc_host = if rpc_host.contains(':') && !rpc_host.starts_with('[') {
        format!("[{rpc_host}]")
    } else {
        rpc_host
    };
    Some(format!("tcp://{rpc_host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: u8, label: u8) -> Vec<u8> {
        let mut b = vec![n; 32];
        b.push(label);
        b
    }

    fn h(n: u8) -> String {
        hex::encode([n; 32])
    }

    fn up(since: i64) -> Seen {
        Seen { up: true, since, ..Default::default() }
    }

    #[test]
    fn counts_blocks_above_the_chain_answer() {
        let mut s = up(100);
        // Nothing seen yet: the answer is the tip only if asked after the start.
        assert_eq!(s.tip(&h(1), 10, 101), Some(LiveTip { hash: h(1), height: 10, seen_at: None }));
        assert_eq!(s.tip(&h(1), 10, 100), None);
        s.on_message(&body(1, b'C'), Some(0), 200);
        s.on_message(&body(9, b'A'), Some(1), 201); // a mempool event
        s.on_message(&body(2, b'C'), Some(2), 300);
        // The answer's tip was seen: one block above it.
        assert_eq!(
            s.tip(&h(1), 10, 50),
            Some(LiveTip { hash: h(2), height: 11, seen_at: Some(300) })
        );
        assert_eq!(s.tip(&h(2), 11, 50), Some(LiveTip { hash: h(2), height: 11, seen_at: None }));
        // An older tip asked after the start: every block seen is above it.
        assert_eq!(
            s.tip(&h(0), 9, 150),
            Some(LiveTip { hash: h(2), height: 11, seen_at: Some(300) })
        );
        // Asked before the start: blocks may be missing.
        assert_eq!(s.tip(&h(0), 9, 90), None);
        s.up = false;
        assert_eq!(s.tip(&h(1), 10, 150), None);
    }

    #[test]
    fn a_reorg_and_a_gap_start_over() {
        let mut s = up(100);
        s.on_message(&body(1, b'C'), Some(0), 200);
        s.on_message(&body(2, b'C'), Some(1), 300);
        s.on_message(&body(2, b'D'), Some(2), 310);
        s.on_message(&body(3, b'C'), Some(3), 311);
        assert_eq!(
            s.tip(&h(1), 10, 150),
            Some(LiveTip { hash: h(3), height: 11, seen_at: Some(311) })
        );
        // A chain answer naming the block that was taken back is not vouched for.
        assert_eq!(s.tip(&h(2), 11, 305), None);
        // A missed message (counter 4 skipped): nothing is vouched for until a newer answer.
        s.on_message(&body(4, b'C'), Some(5), 400);
        assert_eq!(s.tip(&h(1), 10, 150), None);
        assert_eq!(
            s.tip(&h(3), 11, 401),
            Some(LiveTip { hash: h(4), height: 12, seen_at: Some(400) })
        );
        // A disconnect of a block not seen: its height is unknown.
        s.on_message(&body(7, b'D'), Some(6), 500);
        assert_eq!(s.tip(&h(4), 12, 401), None);
    }

    #[test]
    fn the_overlay_takes_the_newer_block() {
        let mut s = up(100);
        s.on_message(&body(2, b'C'), Some(0), 300);
        let mut v = json!({"blocks": 10, "headers": 10, "best_block_hash": h(1), "best_block_time": 5, "asked_at": 150});
        overlay(&s, &mut v);
        assert_eq!(v["blocks"], 11);
        assert_eq!(v["headers"], 11);
        assert_eq!(v["tip_source"], "zmq");
        assert_eq!(v["tip_seen_at"], 300);
        assert!(v["best_block_time"].is_null());
        assert_eq!(v["tip_live"], true);
        // No chain answer yet: nothing to count from.
        let mut v = json!({"chain_pending": true});
        overlay(&s, &mut v);
        assert!(v.get("tip_live").is_none());
    }

    #[test]
    fn finds_the_sequence_publisher() {
        let n = json!([
            {"type": "pubhashblock", "address": "tcp://0.0.0.0:28332", "hwm": 1000},
            {"type": "pubsequence", "address": "tcp://0.0.0.0:28335", "hwm": 1000}
        ]);
        assert_eq!(
            sequence_endpoint(&n, "http://bitcoind:8332/").as_deref(),
            Some("tcp://bitcoind:28335")
        );
        let n = json!([{"type": "pubsequence", "address": "tcp://10.21.21.8:28335"}]);
        assert_eq!(
            sequence_endpoint(&n, "http://bitcoind:8332/").as_deref(),
            Some("tcp://10.21.21.8:28335")
        );
        assert_eq!(sequence_endpoint(&json!([]), "http://bitcoind:8332/"), None);
    }
}
