//! Funding checks against bitcoind's UTXO set with `scantxoutset`.
//!
//! A channel announcement names both funding keys, so the expected 2-of-2 P2WSH output
//! script can be rebuilt without the funding transaction. One `scantxoutset` pass over
//! the UTXO set then finds every still-unspent funding output with its txid, output
//! index, amount and confirmation height. This works on a pruned node: it reads the
//! chainstate, never old blocks.

use std::collections::HashMap;
use std::time::Duration;

use lightning::bitcoin::secp256k1::PublicKey;
use lightning::bitcoin::ScriptBuf;
use lightning::ln::chan_utils::make_funding_redeemscript;
use lightning::routing::gossip::NodeId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const BITCOIND_RESPONSE_LIMIT: usize = 64 * 1024 * 1024;

/// Result of checking one channel's funding output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// The funding output is unspent and matches the short channel id.
    Unspent { funding_sats: u64 },
    /// No matching unspent output: closed, never confirmed, or a fake announcement.
    Missing,
}

/// One channel to check.
#[derive(Debug, Clone)]
pub struct Check {
    pub scid: u64,
    pub script_pubkey: ScriptBuf,
}

pub fn scid_block(scid: u64) -> u32 {
    (scid >> 40) as u32
}

pub fn scid_vout(scid: u64) -> u32 {
    (scid & 0xffff) as u32
}

/// The P2WSH output script LDK expects for a channel funded by these two keys.
pub fn funding_script(bitcoin_key_1: &NodeId, bitcoin_key_2: &NodeId) -> Option<ScriptBuf> {
    let a = PublicKey::from_slice(bitcoin_key_1.as_slice()).ok()?;
    let b = PublicKey::from_slice(bitcoin_key_2.as_slice()).ok()?;
    Some(make_funding_redeemscript(&a, &b).to_p2wsh())
}

/// One entry of `scantxoutset`'s `unspents`.
#[derive(Debug, Clone, Deserialize)]
pub struct Unspent {
    pub txid: String,
    pub vout: u32,
    #[serde(rename = "scriptPubKey")]
    pub script_pubkey: String,
    /// BTC, as Core reports it.
    pub amount: f64,
    pub height: u32,
}

/// bitcoind's chain sync state from `getblockchaininfo`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChainSync {
    /// `initialblockdownload` is false.
    pub synced: bool,
    pub blocks: u64,
    pub headers: u64,
    /// `verificationprogress`, 0.0 to 1.0.
    pub progress: f64,
}

/// A completed scan.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub height: u32,
    pub txouts: u64,
    /// Unspent outputs bitcoind returned for the requested scripts.
    pub unspents: usize,
    /// Of those, outputs that fund no served channel: wrong height or output index,
    /// or claimed by several short channel ids.
    pub unmatched: usize,
    pub verdicts: HashMap<u64, Verdict>,
}

/// Matches scan results to channels: same script, same output index, same block height.
/// The transaction's position in the block is not checked (that needs the block), so
/// whoever holds a channel's funding keys could sign many short channel ids that differ
/// only in that position and all point at one output. An output can fund one channel,
/// so when several ids match the same `txid:vout`, all of them are `Missing`. Only the
/// key holder can sign such duplicates, so this never drops someone else's channel.
pub fn match_unspents(checks: &[Check], unspents: &[Unspent]) -> HashMap<u64, Verdict> {
    let mut by_script: HashMap<&[u8], Vec<&Unspent>> = HashMap::new();
    let decoded: Vec<(Vec<u8>, &Unspent)> =
        unspents.iter().filter_map(|u| hex_decode(&u.script_pubkey).map(|s| (s, u))).collect();
    for (script, unspent) in &decoded {
        by_script.entry(script.as_slice()).or_default().push(unspent);
    }
    let found: Vec<(u64, Option<&Unspent>)> = checks
        .iter()
        .map(|check| {
            let unspent = by_script.get(check.script_pubkey.as_bytes()).and_then(|candidates| {
                candidates
                    .iter()
                    .find(|u| u.height == scid_block(check.scid) && u.vout == scid_vout(check.scid))
                    .copied()
            });
            (check.scid, unspent)
        })
        .collect();
    let mut claims: HashMap<(&str, u32), usize> = HashMap::new();
    for unspent in found.iter().filter_map(|(_, u)| *u) {
        *claims.entry((unspent.txid.as_str(), unspent.vout)).or_default() += 1;
    }
    found
        .into_iter()
        .map(|(scid, unspent)| {
            let verdict = match unspent {
                Some(u) if claims[&(u.txid.as_str(), u.vout)] == 1 => {
                    Verdict::Unspent { funding_sats: btc_to_sats(u.amount) }
                }
                _ => Verdict::Missing,
            };
            (scid, verdict)
        })
        .collect()
}

fn btc_to_sats(btc: f64) -> u64 {
    (btc * 100_000_000.0).round() as u64
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize)]
struct RpcRequest<'a, P> {
    jsonrpc: &'static str,
    id: &'static str,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct RpcReply<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: Option<String>,
}

/// The fields of `getblockchaininfo` this sidecar reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ChainInfo {
    initialblockdownload: Option<bool>,
    blocks: u64,
    headers: u64,
    verificationprogress: f64,
}

#[derive(Deserialize)]
struct ScanProgress {
    progress: Option<f64>,
}

/// The fields of a finished `scantxoutset` this sidecar reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ScanReply {
    success: bool,
    height: u32,
    txouts: u64,
    unspents: Vec<Unspent>,
}

/// Decodes a JSON-RPC answer into `T` in one pass. A scan answer is over ten megabytes:
/// going through a generic JSON tree first costs hundreds of megabytes of small
/// allocations that the allocator then keeps.
fn parse_reply<T: DeserializeOwned>(
    method: &str,
    status: u16,
    bytes: &[u8],
) -> Result<Option<T>, String> {
    let reply: RpcReply<T> = serde_json::from_slice(bytes).map_err(|e| {
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]);
        format!("bitcoind {method}: HTTP {status}: {} ({e})", text.trim())
    })?;
    if let Some(message) = reply.error.and_then(|e| e.message) {
        return Err(format!("bitcoind {method}: {message}"));
    }
    Ok(reply.result)
}

/// Minimal bitcoind JSON-RPC client for `scantxoutset`.
#[derive(Clone)]
pub struct Bitcoind {
    url: String,
    user: String,
    pass: String,
    http: reqwest::Client,
}

impl Bitcoind {
    pub fn new(url: String, user: String, pass: String) -> Self {
        Self { url, user, pass, http: reqwest::Client::new() }
    }

    /// Calls one RPC method and decodes its `result` straight into `T`; `None` when
    /// bitcoind answers `null`.
    async fn call<P: Serialize, T: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
        timeout: Duration,
    ) -> Result<Option<T>, String> {
        let body = RpcRequest { jsonrpc: "1.0", id: "allod-gossip", method, params };
        let response = self
            .http
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("bitcoind {method}: {e}"))?;
        let status = response.status();
        let bytes = allod_lnurl::bounded_response_bytes(response, BITCOIND_RESPONSE_LIMIT)
            .await
            .map_err(|e| format!("bitcoind {method}: {e}"))?;
        parse_reply(method, status.as_u16(), &bytes)
    }

    async fn chain_info(&self) -> Result<ChainInfo, String> {
        let info = self.call("getblockchaininfo", [(); 0], Duration::from_secs(10)).await?;
        Ok(info.unwrap_or_default())
    }

    /// Checks that bitcoind answers RPC calls.
    pub async fn ping(&self) -> Result<(), String> {
        self.chain_info().await.map(|_| ())
    }

    /// Chain sync state: `None` while bitcoind is still in initial block download, else
    /// `Some(height)`. `scantxoutset` reads the chainstate, so a scan before that point
    /// would see a partial UTXO set and fail most channels.
    pub async fn chain_sync(&self) -> Result<ChainSync, String> {
        let info = self.chain_info().await?;
        Ok(ChainSync {
            synced: info.initialblockdownload == Some(false),
            blocks: info.blocks,
            headers: info.headers,
            progress: info.verificationprogress,
        })
    }

    /// Progress of a running scan in percent, if one is running.
    pub async fn scan_progress(&self) -> Option<f64> {
        self.call::<_, ScanProgress>("scantxoutset", ["status"], Duration::from_secs(10))
            .await
            .ok()
            .flatten()
            .and_then(|p| p.progress)
    }

    /// Runs one scan for all `checks`. Core allows one scan at a time; a scan left over
    /// from a previous run of this process is aborted first.
    pub async fn scan(&self, checks: &[Check]) -> Result<ScanResult, String> {
        if let Ok(Some(true)) =
            self.call::<_, bool>("scantxoutset", ["abort"], Duration::from_secs(10)).await
        {
            tracing::info!("aborted a UTXO scan left running from a previous start");
        }
        let objects: Vec<String> = checks
            .iter()
            .map(|c| format!("raw({})", hex_encode(c.script_pubkey.as_bytes())))
            .collect();
        let request_bytes: usize = objects.iter().map(|o| o.len() + 3).sum();
        tracing::info!(
            descriptors = objects.len(),
            request_kib = request_bytes / 1024,
            "sending scantxoutset to bitcoind: one pass over the whole UTXO set, matching every funding script"
        );
        let result: ScanReply = self
            .call("scantxoutset", ("start", &objects), Duration::from_secs(4 * 3600))
            .await?
            .unwrap_or_default();
        drop(objects);
        if !result.success {
            return Err("scantxoutset did not complete".to_string());
        }
        let unspents = result.unspents;
        let verdicts = match_unspents(checks, &unspents);
        let matched = verdicts.values().filter(|v| matches!(v, Verdict::Unspent { .. })).count();
        Ok(ScanResult {
            height: result.height,
            txouts: result.txouts,
            unspents: unspents.len(),
            unmatched: unspents.len().saturating_sub(matched),
            verdicts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightning::bitcoin::secp256k1::{Secp256k1, SecretKey};

    fn key(i: u8) -> NodeId {
        let secp = Secp256k1::new();
        NodeId::from_pubkey(&PublicKey::from_secret_key(
            &secp,
            &SecretKey::from_slice(&[i; 32]).unwrap(),
        ))
    }

    #[test]
    fn funding_script_is_order_independent_p2wsh() {
        let ab = funding_script(&key(1), &key(2)).unwrap();
        assert_eq!(ab, funding_script(&key(2), &key(1)).unwrap());
        assert!(ab.is_p2wsh());
    }

    #[test]
    fn scan_reply_decodes_without_the_unused_fields() {
        let body = br#"{"result":{"success":true,"txouts":163267199,"height":975571,
            "bestblock":"00","unspents":[{"txid":"aa","vout":1,"scriptPubKey":"0020bb",
            "desc":"raw(0020bb)#x","amount":0.05,"coinbase":false,"height":800000,
            "blockhash":"cc","confirmations":7}],"total_amount":0.05},"error":null,"id":"x"}"#;
        let reply: ScanReply = parse_reply("scantxoutset", 200, body).unwrap().unwrap();
        assert!(reply.success);
        assert_eq!((reply.height, reply.txouts), (975_571, 163_267_199));
        assert_eq!(reply.unspents.len(), 1);
        assert_eq!(reply.unspents[0].script_pubkey, "0020bb");
        assert_eq!(btc_to_sats(reply.unspents[0].amount), 5_000_000);
    }

    #[test]
    fn rpc_errors_and_null_results() {
        let null = br#"{"result":null,"error":null,"id":"x"}"#;
        assert!(parse_reply::<ScanProgress>("scantxoutset", 200, null).unwrap().is_none());
        assert_eq!(
            parse_reply::<bool>("scantxoutset", 200, br#"{"result":true,"error":null}"#),
            Ok(Some(true))
        );
        let failed =
            br#"{"result":null,"error":{"code":-8,"message":"Scan already in progress"},"id":"x"}"#;
        assert_eq!(
            parse_reply::<ScanReply>("scantxoutset", 500, failed).unwrap_err(),
            "bitcoind scantxoutset: Scan already in progress"
        );
        let html = parse_reply::<ChainInfo>("getblockchaininfo", 401, b"Unauthorized").unwrap_err();
        assert!(html.starts_with("bitcoind getblockchaininfo: HTTP 401: Unauthorized"), "{html}");
    }

    #[test]
    fn matches_by_script_height_and_vout() {
        let script = funding_script(&key(1), &key(2)).unwrap();
        let other = funding_script(&key(3), &key(4)).unwrap();
        let scid = |height: u64, tx: u64, vout: u64| (height << 40) | (tx << 16) | vout;
        let checks = vec![
            Check { scid: scid(800_000, 12, 1), script_pubkey: script.clone() },
            // Same keys, different channel: wrong height in the UTXO set.
            Check { scid: scid(800_500, 3, 1), script_pubkey: script.clone() },
            Check { scid: scid(800_000, 9, 0), script_pubkey: other.clone() },
        ];
        let unspents = vec![
            Unspent {
                txid: "aa".repeat(32),
                vout: 1,
                script_pubkey: hex_encode(script.as_bytes()),
                amount: 0.05,
                height: 800_000,
            },
            Unspent {
                txid: "bb".repeat(32),
                vout: 1,
                script_pubkey: hex_encode(other.as_bytes()),
                amount: 1.0,
                height: 800_000,
            },
        ];
        let verdicts = match_unspents(&checks, &unspents);
        assert_eq!(verdicts[&scid(800_000, 12, 1)], Verdict::Unspent { funding_sats: 5_000_000 });
        assert_eq!(verdicts[&scid(800_500, 3, 1)], Verdict::Missing);
        // Right script and height, wrong output index.
        assert_eq!(verdicts[&scid(800_000, 9, 0)], Verdict::Missing);
    }

    #[test]
    fn duplicate_ids_for_one_output_all_fail() {
        let script = funding_script(&key(1), &key(2)).unwrap();
        let other = funding_script(&key(3), &key(4)).unwrap();
        let scid = |height: u64, tx: u64, vout: u64| (height << 40) | (tx << 16) | vout;
        let checks = vec![
            // Real channel and two copies that only change the tx position.
            Check { scid: scid(800_000, 12, 1), script_pubkey: script.clone() },
            Check { scid: scid(800_000, 0, 1), script_pubkey: script.clone() },
            Check { scid: scid(800_000, 4_000, 1), script_pubkey: script.clone() },
            // Unrelated channel in the same block keeps passing.
            Check { scid: scid(800_000, 9, 1), script_pubkey: other.clone() },
        ];
        let unspents = vec![
            Unspent {
                txid: "aa".repeat(32),
                vout: 1,
                script_pubkey: hex_encode(script.as_bytes()),
                amount: 0.05,
                height: 800_000,
            },
            Unspent {
                txid: "bb".repeat(32),
                vout: 1,
                script_pubkey: hex_encode(other.as_bytes()),
                amount: 1.0,
                height: 800_000,
            },
        ];
        let verdicts = match_unspents(&checks, &unspents);
        assert_eq!(verdicts[&scid(800_000, 12, 1)], Verdict::Missing);
        assert_eq!(verdicts[&scid(800_000, 0, 1)], Verdict::Missing);
        assert_eq!(verdicts[&scid(800_000, 4_000, 1)], Verdict::Missing);
        assert_eq!(verdicts[&scid(800_000, 9, 1)], Verdict::Unspent { funding_sats: 100_000_000 });
    }
}
