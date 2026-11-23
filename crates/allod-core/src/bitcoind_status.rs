//! Bitcoin Core status for the Node page (`GET /api/node/bitcoind`): version, chain tip,
//! peers, mempool, disk, warnings, and the background validation that follows a fast sync
//! (AssumeUTXO).
//!
//! Two queries, cached apart: the chain (`getblockchaininfo`, `getnetworkinfo`,
//! `getchainstates`, each its own request so they run side by side) and the mempool
//! (`getmempoolinfo`, `uptime`, the fee rate diagram). The chain calls wait for bitcoind's
//! chain lock, which background validation holds for minutes at a time on a small machine;
//! the mempool calls do not, so the mempool and fees stay current while the chain figures
//! wait (`updated_at` and `mempool_updated_at` tell their age).
//!
//! bitcoind refuses a whole batch (HTTP 403) when its `rpcwhitelist` lacks one of the
//! methods. The calls are then made one by one, and the refused ones are left out for ten
//! minutes, so a node with a stricter whitelist still shows what it allows without a
//! warning in its log at every poll. A bitcoind that cannot be reached is likely
//! restarting, often with a new whitelist: the refusals are forgotten then.
//!
//! With a loaded UTXO snapshot bitcoind runs two chainstates: the snapshot chain, which
//! the node already uses, and a background chain that validates every block from genesis
//! up to the snapshot's base block. Until it gets there the snapshot's UTXO set is trusted
//! on the strength of the hash built into the Bitcoin Core release.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde_json::{json, Value};

use crate::setup::BitcoindRpc;

/// How long one status request may take. Background validation and a syncing tip hold
/// bitcoind's chain lock while they connect and flush blocks: a 2-vCPU VPS validating
/// history took 175 s to answer `getblockchaininfo`.
pub(crate) const STATUS_QUERY_TIMEOUT: Duration = Duration::from_secs(300);

/// The chain calls, sent side by side (one request each) since each waits for the lock.
const CHAIN_METHODS: [&str; 3] = ["getblockchaininfo", "getnetworkinfo", "getchainstates"];
/// The mempool calls: no chain lock, so they answer at once.
const MEMPOOL_METHODS: [&str; 2] = ["getmempoolinfo", "uptime"];

/// The watched bitcoind's last status answer (`SetupService::bitcoind_status`).
#[derive(Debug, Default)]
pub(crate) struct StatusCache {
    /// RPC URL the entries belong to (the watched node can change to the standby).
    pub url: String,
    /// Last good chain answer and when it arrived (unix seconds).
    pub ok: Option<(Value, i64)>,
    /// Error of the latest chain query, if it failed.
    pub error: Option<String>,
    /// What earlier chain queries learned about this node.
    pub known: Known,
    /// Last good mempool answer and when it arrived, kept apart from the chain's.
    pub mempool: Option<(Value, i64)>,
    /// What earlier mempool queries learned (refused methods, projected blocks).
    pub mempool_known: Known,
}

/// The snapshot's base block: never changes, so it is asked once.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Base {
    pub hash: String,
    pub height: u64,
    /// Transactions in the chain up to the base block (from the snapshot's metadata).
    pub tx_count: Option<u64>,
}

/// Carried from one query to the next (per watched node).
#[derive(Debug, Default, Clone)]
pub(crate) struct Known {
    pub base: Option<Base>,
    /// Methods bitcoind's `rpcwhitelist` refused, and when (unix seconds).
    pub forbidden: HashMap<String, i64>,
    /// Background validation progress: (unix seconds, transactions validated).
    pub bg_samples: VecDeque<(i64, u64)>,
    /// `getblockstats` of recent blocks by hash: a block never changes.
    pub block_stats: HashMap<String, Value>,
    /// Projected mempool blocks and when they were computed (unix seconds).
    pub mempool_blocks: Option<(i64, Value)>,
}

/// Confirmed blocks shown next to the projected ones.
const RECENT_BLOCKS: u64 = 6;
/// Projected blocks shown; the rest of the mempool is summed up in the last one.
const PROJECTED_BLOCKS: usize = 6;
/// The fee rate diagram covers the whole mempool: asked at most this often.
const MEMPOOL_BLOCKS_EVERY: i64 = 30;
/// A full 300 MB mempool's diagram runs to a few MB of JSON.
const DIAGRAM_RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
/// Block weight a miner fills: 4 MWU less Bitcoin Core's default reserve
/// (`-blockreservedweight`) for the coinbase.
const BLOCK_WEIGHT: u64 = 4_000_000 - 8_000;
/// What `getblockstats` computes for the recent blocks (fewer stats, less work).
const BLOCK_STATS: [&str; 10] = [
    "height",
    "blockhash",
    "time",
    "txs",
    "total_size",
    "total_weight",
    "totalfee",
    "feerate_percentiles",
    "minfeerate",
    "maxfeerate",
];

/// Samples kept for the time-left estimate. Queries only run while someone has the page
/// open, so the gaps between visits give it a long baseline.
const ETA_KEEP_SECS: i64 = 6 * 3600;
/// The estimate uses the last hour once it spans this much (the rate drifts: later
/// blocks hold bigger transactions), else every sample kept.
const ETA_RECENT_SECS: i64 = 3600;
const ETA_RECENT_MIN_SPAN: i64 = 10 * 60;
/// Shortest span to estimate from at all: bitcoind validates in bursts between flushes.
const ETA_MIN_SPAN: i64 = 60;
const ETA_SAMPLE_STEP: i64 = 10;

impl Known {
    /// Records the background chain's transaction count; `None` (no background
    /// validation, or no counts) forgets the samples.
    fn sample_background(&mut self, now: i64, txs: Option<u64>) {
        let Some(txs) = txs else {
            self.bg_samples.clear();
            return;
        };
        // Going backwards: another snapshot or a reindex. Start over.
        if self.bg_samples.back().is_some_and(|(_, t)| txs < *t) {
            self.bg_samples.clear();
        }
        if self.bg_samples.back().is_none_or(|(at, _)| now - at >= ETA_SAMPLE_STEP) {
            self.bg_samples.push_back((now, txs));
        }
        while self.bg_samples.front().is_some_and(|(at, _)| now - at > ETA_KEEP_SECS) {
            self.bg_samples.pop_front();
        }
    }

    /// Transactions per second over the recent samples, when they span long enough.
    fn background_rate(&self) -> Option<f64> {
        let &(last_at, last) = self.bg_samples.back()?;
        let recent = self.bg_samples.iter().find(|(at, _)| last_at - at <= ETA_RECENT_SECS)?;
        let &(first_at, first) = if last_at - recent.0 >= ETA_RECENT_MIN_SPAN {
            recent
        } else {
            self.bg_samples.front()?
        };
        let span = last_at - first_at;
        (span >= ETA_MIN_SPAN && last > first).then(|| (last - first) as f64 / span as f64)
    }
}

/// How long a refused method is left out before it is tried again (the owner may have
/// widened the whitelist meanwhile).
const FORBIDDEN_RETRY_SECS: i64 = 600;

const NOT_ALLOWED: &str = "not allowed by bitcoind's rpcwhitelist";

impl Known {
    /// Whether a call (an id, `method#tag`) is left out: refusals are per method.
    fn skips(&self, id: &str, now: i64) -> bool {
        self.forbidden.get(method(id)).is_some_and(|at| now - at < FORBIDDEN_RETRY_SECS)
    }
}

impl StatusCache {
    /// Starts over when the watched node changed.
    fn for_url(&mut self, url: &str) {
        if self.url != url {
            *self = StatusCache { url: url.to_string(), ..Default::default() };
        }
    }

    /// Keeps a mempool answer; a failed one leaves the last good one in place.
    pub fn store_mempool(&mut self, url: &str, result: Result<Value, String>, now: i64) {
        self.for_url(url);
        if let Ok(v) = result {
            self.mempool = Some((v, now));
        }
    }

    pub fn store(&mut self, url: &str, result: Result<Value, String>, now: i64) {
        self.for_url(url);
        match result {
            Ok(v) => {
                self.ok = Some((v, now));
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The last good chain answer with `updated_at` (and `last_error` when the latest
    /// query failed), or the error when bitcoind never answered; the mempool part laid
    /// over it with its own `mempool_updated_at`.
    pub fn view(&self, url: &str) -> Value {
        let mut v = match (&self.ok, &self.error) {
            (Some((v, at)), error) if self.url == url => {
                let mut v = v.clone();
                v["updated_at"] = json!(at);
                if let Some(e) = error {
                    v["last_error"] = json!(e);
                }
                v
            }
            (None, Some(e)) if self.url == url => json!({"error": e}),
            _ => json!({"error": "waiting for bitcoind"}),
        };
        if let Some((m, at)) = self.mempool.as_ref().filter(|_| self.url == url) {
            for key in ["mempool", "mempool_blocks", "uptime"] {
                v[key] = m[key].clone();
            }
            v["mempool_updated_at"] = json!(at);
            if let Some(errors) = m["errors"].as_object().filter(|e| !e.is_empty()) {
                if !v["errors"].is_object() {
                    v["errors"] = json!({});
                }
                for (k, e) in errors {
                    v["errors"][k] = e.clone();
                }
            }
        }
        v
    }
}

/// The method of a call id (`getblockhash#3` → `getblockhash`).
fn method(id: &str) -> &str {
    id.split('#').next().unwrap_or(id)
}

#[derive(Debug, PartialEq)]
enum BatchError {
    /// HTTP 403: a method is not in the user's `rpcwhitelist`.
    Forbidden,
    /// No connection: bitcoind is down or restarting.
    Unreachable(String),
    Other(String),
}

type Batch = HashMap<String, Result<Value, String>>;

/// One JSON-RPC batch; each reply is keyed by call id, `Err` with bitcoind's message. The
/// id is the method name, plus `#tag` where one method is called twice (`getchaintxstats`).
async fn batch(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    calls: &[(&str, Value)],
) -> Result<Batch, BatchError> {
    let body: Vec<Value> = calls
        .iter()
        .map(|(id, p)| json!({"jsonrpc": "1.0", "id": id, "method": method(id), "params": p}))
        .collect();
    let resp = http
        .post(&rpc.url)
        .basic_auth(&rpc.user, Some(&rpc.pass))
        .timeout(STATUS_QUERY_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                BatchError::Unreachable(e.to_string())
            } else {
                BatchError::Other(e.to_string())
            }
        })?;
    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(BatchError::Forbidden);
    }
    let limit = if calls.iter().any(|(id, _)| id.starts_with("getmempoolfeeratediagram")) {
        DIAGRAM_RESPONSE_LIMIT
    } else {
        allod_lnurl::JSON_RESPONSE_LIMIT
    };
    let value: Value = allod_lnurl::bounded_response_json(resp, limit)
        .await
        .map_err(|e| BatchError::Other(format!("bitcoind answered HTTP {status}: {e}")))?;
    let replies = match value {
        Value::Array(a) => a,
        // A single error object: the whole batch was refused (e.g. still warming up).
        other => {
            return Err(BatchError::Other(
                rpc_error(&other["error"]).unwrap_or_else(|| other.to_string()),
            ))
        }
    };
    Ok(replies
        .into_iter()
        .filter_map(|r| {
            let id = r["id"].as_str()?.to_string();
            let result = match rpc_error(&r["error"]) {
                Some(e) => Err(e),
                None => Ok(r["result"].clone()),
            };
            Some((id, result))
        })
        .collect())
}

/// [`batch`], minus the methods refused recently; after a 403 each call goes alone so
/// the allowed ones still answer, and the refused ones are remembered in `known`.
async fn calls(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    calls: &[(&str, Value)],
    known: &mut Known,
    now: i64,
) -> Result<Batch, String> {
    let mut out = Batch::new();
    let wanted: Vec<(&str, Value)> = calls
        .iter()
        .filter(|(m, _)| {
            let skip = known.skips(m, now);
            if skip {
                out.insert(m.to_string(), Err(NOT_ALLOWED.into()));
            }
            !skip
        })
        .cloned()
        .collect();
    if wanted.is_empty() {
        return Ok(out);
    }
    match batch(http, rpc, &wanted).await {
        Ok(r) => {
            for (id, _) in &wanted {
                known.forbidden.remove(method(id));
            }
            out.extend(r);
            Ok(out)
        }
        Err(BatchError::Unreachable(e)) => {
            known.forbidden.clear();
            Err(e)
        }
        Err(BatchError::Other(e)) => Err(e),
        Err(BatchError::Forbidden) => {
            for call in &wanted {
                let id = call.0;
                // Another call of this method was refused a moment ago.
                if known.skips(id, now) {
                    out.insert(id.to_string(), Err(NOT_ALLOWED.into()));
                    continue;
                }
                match batch(http, rpc, std::slice::from_ref(call)).await {
                    Ok(r) => {
                        known.forbidden.remove(method(id));
                        out.extend(r);
                    }
                    Err(BatchError::Forbidden) => {
                        known.forbidden.insert(method(id).to_string(), now);
                        out.insert(id.to_string(), Err(NOT_ALLOWED.into()));
                    }
                    Err(BatchError::Other(e) | BatchError::Unreachable(e)) => {
                        out.insert(id.to_string(), Err(e));
                    }
                }
            }
            Ok(out)
        }
    }
}

/// [`calls`] with each call in its own request, all at once: bitcoind runs a batch's
/// calls one after another, so one call waiting for the chain lock would hold up the rest.
async fn calls_side_by_side(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    list: &[(&str, Value)],
    known: &mut Known,
    now: i64,
) -> Result<Batch, String> {
    let mut out = Batch::new();
    let wanted: Vec<&(&str, Value)> = list
        .iter()
        .filter(|(id, _)| {
            let skip = known.skips(id, now);
            if skip {
                out.insert(id.to_string(), Err(NOT_ALLOWED.into()));
            }
            !skip
        })
        .collect();
    let replies = futures_util::future::join_all(
        wanted.iter().map(|call| batch(http, rpc, std::slice::from_ref(*call))),
    )
    .await;
    let mut unreachable = None;
    for (call, reply) in wanted.iter().zip(replies) {
        let id = call.0;
        match reply {
            Ok(r) => {
                known.forbidden.remove(method(id));
                out.extend(r);
            }
            Err(BatchError::Forbidden) => {
                known.forbidden.insert(method(id).to_string(), now);
                out.insert(id.to_string(), Err(NOT_ALLOWED.into()));
            }
            Err(BatchError::Unreachable(e)) => {
                unreachable = Some(e.clone());
                out.insert(id.to_string(), Err(e));
            }
            Err(BatchError::Other(e)) => {
                out.insert(id.to_string(), Err(e));
            }
        }
    }
    // Nothing answered and the connection failed: likely restarting, maybe with a new
    // whitelist.
    if let Some(e) = unreachable.filter(|_| out.values().all(Result::is_err)) {
        known.forbidden.clear();
        return Err(e);
    }
    Ok(out)
}

fn rpc_error(e: &Value) -> Option<String> {
    match e {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        e => Some(e["message"].as_str().map(str::to_string).unwrap_or_else(|| e.to_string())),
    }
}

/// The mempool part: `mempool`, `uptime` and the projected `mempool_blocks` (the diagram
/// at most every [`MEMPOOL_BLOCKS_EVERY`] seconds), plus `errors` of failed calls.
pub(crate) async fn query_mempool(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    known: &mut Known,
    now: i64,
) -> Result<Value, String> {
    let mut list: Vec<(&str, Value)> = MEMPOOL_METHODS.iter().map(|m| (*m, json!([]))).collect();
    let diagram_due =
        known.mempool_blocks.as_ref().is_none_or(|(at, _)| now - at >= MEMPOOL_BLOCKS_EVERY);
    if diagram_due {
        list.push(("getmempoolfeeratediagram", json!([])));
    }
    let mut replies = calls(http, rpc, &list, known, now).await?;
    if let Some(Ok(d)) = replies.remove("getmempoolfeeratediagram") {
        known.mempool_blocks = Some((now, project_blocks(&d, PROJECTED_BLOCKS)));
    }
    let mut errors = serde_json::Map::new();
    let mut ok = |name: &str| match replies.remove(name) {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            errors.insert(name.to_string(), json!(e));
            Value::Null
        }
        None => Value::Null,
    };
    let (m, uptime) = (ok("getmempoolinfo"), ok("uptime"));
    Ok(json!({
        "mempool": mempool_json(&m),
        "uptime": uptime,
        "mempool_blocks": known.mempool_blocks.as_ref().map(|(_, b)| b.clone()),
        "errors": errors,
    }))
}

/// Queries the chain status; `known` carries the snapshot base height and refused
/// methods between queries.
pub(crate) async fn query(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    known: &mut Known,
    now: i64,
) -> Result<Value, String> {
    let list: Vec<(&str, Value)> = CHAIN_METHODS.iter().map(|m| (*m, json!([]))).collect();
    let mut replies = calls_side_by_side(http, rpc, &list, known, now).await?;
    if let Some(Err(e)) = replies.get("getblockchaininfo") {
        // Without the chain there is nothing to show (e.g. "Loading block index…").
        return Err(e.clone());
    }
    let chainstates = replies.get("getchainstates").and_then(|r| r.as_ref().ok()).cloned();
    let snapshot_hash = chainstates.as_ref().and_then(snapshot_blockhash);
    // The background chain's tip, while it validates up to the snapshot.
    let bg_hash = chainstates.as_ref().and_then(background_tip);
    let cached = known.base.take().filter(|b| Some(&b.hash) == snapshot_hash.as_ref());
    let mut extra: Vec<(&str, Value)> = Vec::new();
    if let Some(hash) = &snapshot_hash {
        if cached.is_none() {
            extra.push(("getblockheader", json!([hash, true])));
        }
        if cached.as_ref().is_none_or(|b| b.tx_count.is_none()) {
            extra.push(("getchaintxstats#base", json!([1, hash])));
        }
    }
    if let Some(hash) = &bg_hash {
        extra.push(("getchaintxstats#bg", json!([1, hash])));
    }
    // Recent blocks and the mempool, once synced (before that there is neither).
    let info = replies.get("getblockchaininfo").and_then(|r| r.as_ref().ok());
    let synced = info.is_some_and(|c| c["initialblockdownload"] == false);
    let tip = info.and_then(|c| c["blocks"].as_u64()).filter(|_| synced);
    let hash_ids: Vec<(String, u64)> = tip
        .map(|t| {
            (0..RECENT_BLOCKS.min(t + 1)).map(|i| (format!("getblockhash#{i}"), t - i)).collect()
        })
        .unwrap_or_default();
    for (id, height) in &hash_ids {
        extra.push((id.as_str(), json!([height])));
    }
    let mut more = if extra.is_empty() {
        Batch::new()
    } else {
        calls(http, rpc, &extra, known, now).await.unwrap_or_default()
    };
    let mut take = |id: &str| more.remove(id).and_then(|r| r.ok());
    let header_height = take("getblockheader").and_then(|h| h["height"].as_u64());
    let base_txs = take("getchaintxstats#base").and_then(|t| t["txcount"].as_u64());
    let bg_txs = take("getchaintxstats#bg").and_then(|t| t["txcount"].as_u64());
    let hashes: Vec<String> = hash_ids
        .iter()
        .filter_map(|(id, _)| take(id).and_then(|h| h.as_str().map(str::to_string)))
        .collect();
    let recent = recent_blocks(http, rpc, known, &hashes, now).await;
    known.base = match (snapshot_hash, cached) {
        (Some(_), Some(mut b)) => {
            b.tx_count = b.tx_count.or(base_txs);
            Some(b)
        }
        (Some(hash), None) => header_height.map(|height| Base { hash, height, tx_count: base_txs }),
        (None, _) => None,
    };
    let mut get = |m: &str| match replies.remove(m) {
        Some(Ok(v)) => Ok(v),
        Some(Err(e)) => Err(e),
        None => Err("no reply".to_string()),
    };
    let r = Replies {
        chain: get("getblockchaininfo"),
        network: get("getnetworkinfo"),
        // The mempool query fills these in (`StatusCache::view`).
        mempool: Ok(Value::Null),
        chainstates: get("getchainstates"),
        uptime: Ok(Value::Null),
    };
    let mut v = shape(r, known.base.as_ref(), bg_txs);
    v["recent_blocks"] = recent;
    let bg = &mut v["background_validation"];
    let counted = bg["progress_basis"] == "transactions";
    known.sample_background(now, if counted { bg_txs } else { None });
    if counted {
        let rate = known.background_rate();
        let left = bg["target_transactions"].as_u64().zip(bg_txs).map(|(t, d)| t.saturating_sub(d));
        bg["tx_per_sec"] = json!(rate);
        bg["eta_secs"] = json!(rate.zip(left).map(|(r, l)| (l as f64 / r).round() as u64));
    }
    Ok(v)
}

/// `getblockstats` for `hashes` (newest first), asking only for blocks not seen before.
async fn recent_blocks(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    known: &mut Known,
    hashes: &[String],
    now: i64,
) -> Value {
    let missing: Vec<&String> =
        hashes.iter().filter(|h| !known.block_stats.contains_key(*h)).collect();
    if !missing.is_empty() {
        let ids: Vec<String> = missing.iter().map(|h| format!("getblockstats#{h}")).collect();
        let list: Vec<(&str, Value)> = ids
            .iter()
            .zip(&missing)
            .map(|(id, h)| (id.as_str(), json!([h, BLOCK_STATS])))
            .collect();
        if let Ok(mut r) = calls(http, rpc, &list, known, now).await {
            for (id, h) in ids.iter().zip(&missing) {
                if let Some(Ok(stats)) = r.remove(id) {
                    known.block_stats.insert((*h).clone(), block_json(&stats));
                }
            }
        }
    }
    // Keep the cache to the blocks still shown (a reorg or new block drops the rest).
    known.block_stats.retain(|h, _| hashes.contains(h));
    hashes.iter().filter_map(|h| known.block_stats.get(h).cloned()).collect()
}

/// One confirmed block for the UI. Bitcoin Core gives fee rates in whole sat/vB.
fn block_json(s: &Value) -> Value {
    json!({
        "height": s["height"],
        "hash": s["blockhash"],
        "time": s["time"],
        "txs": s["txs"],
        "size": s["total_size"],
        "vsize": s["total_weight"].as_u64().map(|w| w.div_ceil(4)),
        "fee_sat": s["totalfee"],
        "min_rate": s["minfeerate"],
        "median_rate": s["feerate_percentiles"][2],
        "max_rate": s["maxfeerate"],
        "percentiles": s["feerate_percentiles"],
    })
}

/// Projects the mempool into blocks the way a miner fills them: chunks (from
/// `getmempoolfeeratediagram`, cumulative weight and fee in mining order) go into a
/// block until the next one no longer fits. The first `max - 1` blocks are listed; the
/// last entry sums up everything after them (`count` blocks).
fn project_blocks(diagram: &Value, max: usize) -> Value {
    let Some(points) = diagram.as_array() else { return Value::Null };
    let point = |p: &Value| {
        (p["weight"].as_u64().unwrap_or(0), (p["fee"].as_f64().unwrap_or(0.0) * 1e8).round() as u64)
    };
    let chunks: Vec<(u64, u64)> = points
        .windows(2)
        .map(|w| {
            let (w0, f0) = point(&w[0]);
            let (w1, f1) = point(&w[1]);
            (w1.saturating_sub(w0), f1.saturating_sub(f0))
        })
        .filter(|(w, _)| *w > 0)
        .collect();
    let mut blocks: Vec<(Vec<(u64, u64)>, u64)> = Vec::new();
    for c in chunks {
        let next = match blocks.last() {
            None => true,
            // Once `max` blocks exist, the last one takes the rest of the mempool.
            Some((b, w)) => blocks.len() < max && !b.is_empty() && w + c.0 > BLOCK_WEIGHT,
        };
        if next {
            blocks.push((Vec::new(), 0));
        }
        let (b, w) = blocks.last_mut().expect("pushed above");
        *w += c.0;
        b.push(c);
    }
    let rate = |(w, f): (u64, u64)| f as f64 / (w as f64 / 4.0);
    let round = |r: f64| (r * 100.0).round() / 100.0;
    let n = blocks.len();
    let list: Vec<Value> = blocks
        .iter()
        .enumerate()
        .map(|(i, (b, w))| {
            let w = *w;
            let f: u64 = b.iter().map(|c| c.1).sum();
            // Weight-weighted median: the rate at half the block's weight.
            let mut acc = 0;
            let median = b
                .iter()
                .find(|c| {
                    acc += c.0;
                    acc * 2 >= w
                })
                .map(|c| rate(*c));
            let count = if i + 1 == n && i + 1 == max { w.div_ceil(BLOCK_WEIGHT) } else { 1 };
            json!({
                "vsize": w.div_ceil(4),
                "fee_sat": f,
                // Chunks come in mining order, highest fee rate first.
                "max_rate": b.first().map(|c| round(rate(*c))),
                "median_rate": median.map(round),
                "min_rate": b.last().map(|c| round(rate(*c))),
                "count": count,
            })
        })
        .collect();
    Value::Array(list)
}

/// The snapshot chainstate's base block, when one is loaded.
fn snapshot_blockhash(chainstates: &Value) -> Option<String> {
    chainstates["chainstates"]
        .as_array()?
        .iter()
        .find_map(|c| c["snapshot_blockhash"].as_str().map(str::to_string))
}

/// The background chainstate's tip while a snapshot is not validated yet.
fn background_tip(chainstates: &Value) -> Option<String> {
    let list = chainstates["chainstates"].as_array()?;
    let snapshot = list.iter().find(|c| c["snapshot_blockhash"].is_string())?;
    if snapshot["validated"] == true {
        return None;
    }
    list.iter()
        .find(|c| !c["snapshot_blockhash"].is_string())
        .and_then(|c| c["bestblockhash"].as_str().map(str::to_string))
}

pub(crate) struct Replies {
    pub chain: Result<Value, String>,
    pub network: Result<Value, String>,
    pub mempool: Result<Value, String>,
    pub chainstates: Result<Value, String>,
    pub uptime: Result<Value, String>,
}

/// Bitcoin Core reports fee rates in BTC/kvB.
fn sat_per_vb(v: &Value) -> Value {
    v.as_f64().map(|r| json!((r * 100_000.0 * 1000.0).round() / 1000.0)).unwrap_or(Value::Null)
}

/// `warnings` is a list since Bitcoin Core 28 and a string before (or with
/// `-deprecatedrpc=warnings`).
fn warnings(v: &Value, out: &mut Vec<String>) {
    let mut add = |s: &str| {
        let s = s.trim();
        if !s.is_empty() && !out.iter().any(|w| w == s) {
            out.push(s.to_string());
        }
    };
    match v {
        Value::String(s) => add(s),
        Value::Array(a) => a.iter().filter_map(Value::as_str).for_each(add),
        _ => {}
    }
}

/// Background validation from `getchainstates`: `null` without a snapshot. With two
/// chainstates the one without `snapshot_blockhash` is the background chain; once it
/// reached the base block bitcoind drops it and marks the snapshot chain validated.
///
/// Progress is transactions validated over transactions up to the base block, which
/// tracks the work done: by blocks, the nearly empty early years would show half done
/// long before it is. Bitcoin Core's own `verificationprogress` (in its log too) is not
/// it either: that counts against the whole chain up to today, not up to the snapshot.
/// By blocks only when bitcoind cannot tell the transaction counts.
fn background(chainstates: &Value, base: Option<&Base>, bg_txs: Option<u64>) -> Value {
    let Some(list) = chainstates["chainstates"].as_array() else { return Value::Null };
    let Some(snapshot) = list.iter().find(|c| c["snapshot_blockhash"].is_string()) else {
        return Value::Null;
    };
    let base_height = base.map(|b| b.height);
    if snapshot["validated"] == true {
        return json!({"active": false, "complete": true, "target_height": base_height});
    }
    let bg = list.iter().find(|c| !c["snapshot_blockhash"].is_string());
    let blocks = bg.and_then(|c| c["blocks"].as_u64());
    let base_txs = base.and_then(|b| b.tx_count);
    let ratio = |a: u64, b: u64| json!((a as f64 / b as f64).min(1.0));
    let (progress, basis) = match (bg_txs, base_txs, blocks, base_height) {
        (Some(a), Some(b), ..) if b > 0 => (ratio(a, b), json!("transactions")),
        (_, _, Some(a), Some(b)) if b > 0 => (ratio(a, b), json!("blocks")),
        _ => (Value::Null, Value::Null),
    };
    json!({
        "active": bg.is_some(),
        "complete": false,
        "blocks": blocks,
        "target_height": base_height,
        "transactions": bg_txs,
        "target_transactions": base_txs,
        "progress": progress,
        "progress_basis": basis,
        // Bitcoin Core's own figure for the background chain (against today's tip).
        "verification_progress": bg.map(|c| c["verificationprogress"].clone()),
    })
}

/// `getmempoolinfo` for the API; `null` without an answer.
fn mempool_json(m: &Value) -> Value {
    if m.is_null() {
        return Value::Null;
    }
    json!({
        "loaded": m["loaded"],
        "transactions": m["size"],
        "vbytes": m["bytes"],
        "usage": m["usage"],
        "max": m["maxmempool"],
        "min_fee_sat_vb": sat_per_vb(&m["mempoolminfee"]),
        "total_fee_sat": m["total_fee"].as_f64().map(|f| (f * 1e8).round() as u64),
    })
}

/// The API shape of [`query`], split out so it can be tested without RPC.
pub(crate) fn shape(r: Replies, base: Option<&Base>, bg_txs: Option<u64>) -> Value {
    let mut errors = serde_json::Map::new();
    let mut ok = |name: &str, r: Result<Value, String>| match r {
        Ok(v) => v,
        Err(e) => {
            errors.insert(name.to_string(), json!(e));
            Value::Null
        }
    };
    let c = ok("getblockchaininfo", r.chain);
    let n = ok("getnetworkinfo", r.network);
    let m = ok("getmempoolinfo", r.mempool);
    let cs = ok("getchainstates", r.chainstates);
    let uptime = ok("uptime", r.uptime);

    let mut warn = Vec::new();
    warnings(&c["warnings"], &mut warn);
    warnings(&n["warnings"], &mut warn);

    let networks: Vec<Value> = n["networks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|x| x["reachable"] == true)
                .filter_map(|x| x["name"].as_str().map(|s| json!(s)))
                .collect()
        })
        .unwrap_or_default();
    let peers = if n.is_null() {
        Value::Null
    } else {
        json!({
            "total": n["connections"],
            "inbound": n["connections_in"],
            "outbound": n["connections_out"],
            "network_active": n["networkactive"],
            "reachable": networks,
        })
    };
    let mempool = mempool_json(&m);
    json!({
        "version": n["version"],
        "subversion": n["subversion"],
        "chain": c["chain"],
        "blocks": c["blocks"],
        "headers": c["headers"],
        "best_block_hash": c["bestblockhash"],
        // `time` arrived in Bitcoin Core 28; `mediantime` is older.
        "best_block_time": c["time"],
        "median_time": c["mediantime"],
        "verification_progress": c["verificationprogress"],
        "initial_block_download": c["initialblockdownload"],
        "size_on_disk": c["size_on_disk"],
        "pruned": c["pruned"],
        "prune_height": c["pruneheight"],
        "prune_target_size": c["prune_target_size"],
        "uptime": uptime,
        "peers": peers,
        "mempool": mempool,
        "relay_fee_sat_vb": sat_per_vb(&n["relayfee"]),
        "background_validation": background(&cs, base, bg_txs),
        "warnings": warn,
        "errors": errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> Value {
        json!({
            "chain": "main", "blocks": 935_400, "headers": 935_400,
            "bestblockhash": "00ab", "time": 1_790_000_000, "mediantime": 1_789_998_000,
            "verificationprogress": 0.99999, "initialblockdownload": false,
            "size_on_disk": 12_000_000_000u64, "pruned": true, "pruneheight": 930_000,
            "automatic_pruning": true, "prune_target_size": 10_000_000_000u64,
            "warnings": ["Clock skew"]
        })
    }

    fn network() -> Value {
        json!({
            "version": 310100, "subversion": "/Satoshi:31.1.0/",
            "connections": 10, "connections_in": 0, "connections_out": 10,
            "networkactive": true, "relayfee": 0.000001,
            "networks": [{"name": "ipv4", "reachable": true}, {"name": "onion", "reachable": false}],
            "warnings": ["Clock skew", ""]
        })
    }

    fn replies(chainstates: Value) -> Replies {
        Replies {
            chain: Ok(chain()),
            network: Ok(network()),
            mempool: Ok(json!({
                "loaded": true, "size": 120, "bytes": 50_000, "usage": 300_000,
                "maxmempool": 300_000_000, "mempoolminfee": 0.000001, "total_fee": 0.00123
            })),
            chainstates: Ok(chainstates),
            uptime: Ok(json!(3600)),
        }
    }

    #[test]
    fn background_validation_after_a_snapshot() {
        let cs = json!({"headers": 935_400, "chainstates": [
            {"blocks": 300_000, "verificationprogress": 0.05, "validated": true},
            {"blocks": 935_400, "verificationprogress": 0.99999,
             "snapshot_blockhash": "00ff", "validated": false}
        ]});
        assert_eq!(snapshot_blockhash(&cs).as_deref(), Some("00ff"));
        let base = Base { hash: "00ff".into(), height: 935_000, tx_count: Some(1_200_000_000) };
        let v = shape(replies(cs.clone()), Some(&base), Some(300_000_000));
        let bg = &v["background_validation"];
        assert_eq!(bg["active"], true, "{v}");
        assert_eq!(bg["complete"], false);
        assert_eq!(bg["blocks"], 300_000);
        assert_eq!(bg["target_height"], 935_000);
        // By transactions: a quarter done, though about a third of the blocks are.
        assert_eq!(bg["progress"], 0.25);
        assert_eq!(bg["progress_basis"], "transactions");
        assert_eq!(bg["transactions"], 300_000_000);
        assert_eq!(bg["target_transactions"], 1_200_000_000);
        assert_eq!(bg["verification_progress"], 0.05);

        // Without transaction counts: by blocks.
        let base = Base { tx_count: None, ..base };
        let bg = &shape(replies(cs), Some(&base), None)["background_validation"];
        assert!((bg["progress"].as_f64().unwrap() - 300_000.0 / 935_000.0).abs() < 1e-9);
        assert_eq!(bg["progress_basis"], "blocks");
        assert_eq!(v["version"], 310100);
        assert_eq!(v["peers"]["outbound"], 10);
        assert_eq!(v["peers"]["reachable"], json!(["ipv4"]));
        assert_eq!(v["mempool"]["min_fee_sat_vb"], 0.1);
        assert_eq!(v["mempool"]["total_fee_sat"], 123_000);
        assert_eq!(v["relay_fee_sat_vb"], 0.1);
        assert_eq!(v["warnings"], json!(["Clock skew"]));
        assert_eq!(v["errors"], json!({}));
    }

    #[test]
    fn validated_snapshot_and_plain_sync() {
        let done = json!({"chainstates": [
            {"blocks": 935_400, "snapshot_blockhash": "00ff", "validated": true}
        ]});
        let base = Base { hash: "00ff".into(), height: 935_000, tx_count: None };
        let v = shape(replies(done), Some(&base), None);
        assert_eq!(v["background_validation"]["complete"], true);
        assert_eq!(v["background_validation"]["active"], false);

        let plain = json!({"chainstates": [{"blocks": 935_400, "validated": true}]});
        assert!(shape(replies(plain), None, None)["background_validation"].is_null());
    }

    #[test]
    fn failed_calls_are_listed_and_the_rest_is_kept() {
        let mut r = replies(Value::Null);
        r.chainstates = Err("Method not found".into());
        r.mempool = Err("busy".into());
        let v = shape(r, None, None);
        assert!(v["background_validation"].is_null());
        assert!(v["mempool"].is_null());
        assert_eq!(v["errors"]["getchainstates"], "Method not found");
        assert_eq!(v["blocks"], 935_400);
        // Pre-28 bitcoind: one warnings string.
        let mut r = replies(Value::Null);
        r.chain = Ok(json!({"blocks": 1, "warnings": "Unknown new rules activated"}));
        assert_eq!(
            shape(r, None, None)["warnings"],
            json!(["Unknown new rules activated", "Clock skew"])
        );
    }

    #[test]
    fn cache_keeps_the_last_answer() {
        let mut c = StatusCache::default();
        assert_eq!(c.view("u")["error"], "waiting for bitcoind");
        c.store("u", Err("refused".into()), 1);
        assert_eq!(c.view("u")["error"], "refused");
        c.store("u", Ok(json!({"blocks": 5})), 2);
        c.store("u", Err("timeout".into()), 3);
        let v = c.view("u");
        assert_eq!(v["blocks"], 5);
        assert_eq!(v["updated_at"], 2);
        assert_eq!(v["last_error"], "timeout");
        assert_eq!(c.view("other")["error"], "waiting for bitcoind");
    }

    /// Fake bitcoind answering batches like Bitcoin Core: HTTP 403 for the whole batch
    /// when it holds a method outside `allowed` (`None`: everything allowed). Counts
    /// requests and `getblockheader` lookups.
    struct Fake {
        url: String,
        requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        headers: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    async fn fake_bitcoind(allowed: Option<&'static [&'static str]>) -> Fake {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let headers = std::sync::Arc::new(AtomicUsize::new(0));
        let (req, hdr) = (requests.clone(), headers.clone());
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move |axum::Json(calls): axum::Json<Vec<Value>>| {
                let (req, hdr) = (req.clone(), hdr.clone());
                async move {
                    use axum::response::IntoResponse;
                    req.fetch_add(1, Ordering::SeqCst);
                    let refused = calls.iter().any(|c| {
                        allowed.is_some_and(|a| !a.contains(&c["method"].as_str().unwrap()))
                    });
                    if refused {
                        return axum::http::StatusCode::FORBIDDEN.into_response();
                    }
                    let replies: Vec<Value> = calls
                        .iter()
                        .map(|c| {
                            let m = c["method"].as_str().unwrap();
                            let result = match m {
                                "getblockchaininfo" => chain(),
                                "getnetworkinfo" => network(),
                                "getmempoolinfo" => json!({"loaded": true, "size": 1}),
                                "getchainstates" => json!({"chainstates": [
                                    {"blocks": 467_500, "bestblockhash": "00aa", "validated": true},
                                    {"blocks": 935_400, "snapshot_blockhash": "00ff", "validated": false}
                                ]}),
                                "uptime" => json!(60),
                                "getblockheader" => {
                                    hdr.fetch_add(1, Ordering::SeqCst);
                                    assert_eq!(c["params"][0], "00ff");
                                    json!({"height": 935_000})
                                }
                                "getblockhash" => json!(format!("h{}", c["params"][0])),
                                "getblockstats" => {
                                    hdr.fetch_add(100, Ordering::SeqCst);
                                    let h = c["params"][0].as_str().unwrap();
                                    json!({"blockhash": h, "height": h[1..].parse::<u64>().unwrap(),
                                        "txs": 3000, "time": 1_790_000_000, "total_size": 1_500_000,
                                        "total_weight": 3_990_001, "totalfee": 1_000_000,
                                        "minfeerate": 1, "maxfeerate": 200,
                                        "feerate_percentiles": [1, 2, 3, 5, 9]})
                                }
                                "getmempoolfeeratediagram" => json!([
                                    {"weight": 0, "fee": 0.0},
                                    {"weight": 2_000_000, "fee": 0.05},
                                    {"weight": 3_500_000, "fee": 0.0575}
                                ]),
                                "getchaintxstats" => match c["params"][1].as_str() {
                                    Some("00ff") => json!({"txcount": 1_200_000_000u64}),
                                    Some("00aa") => json!({"txcount": 300_000_000u64}),
                                    other => panic!("getchaintxstats {other:?}"),
                                },
                                _ => {
                                    return json!({"id": m, "result": null,
                                        "error": {"code": -32601, "message": "Method not found"}})
                                }
                            };
                            json!({"id": c["id"], "result": result, "error": null})
                        })
                        .collect();
                    axum::Json(Value::Array(replies)).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Fake { url, requests, headers }
    }

    fn rpc(url: &str) -> BitcoindRpc {
        BitcoindRpc { url: url.into(), user: "u".into(), pass: "p".into() }
    }

    #[tokio::test]
    async fn chain_and_mempool_are_asked_apart() {
        use std::sync::atomic::Ordering;
        let fake = fake_bitcoind(None).await;
        let http = reqwest::Client::new();
        let mut known = Known::default();
        let v = query(&http, &rpc(&fake.url), &mut known, 1000).await.unwrap();
        let base = Base { hash: "00ff".into(), height: 935_000, tx_count: Some(1_200_000_000) };
        assert_eq!(known.base.as_ref(), Some(&base));
        assert_eq!(v["background_validation"]["progress"], 0.25, "{v}");
        assert_eq!(v["background_validation"]["progress_basis"], "transactions");
        // Six recent blocks, newest first.
        assert_eq!(v["recent_blocks"].as_array().unwrap().len(), 6, "{v}");
        assert_eq!(v["recent_blocks"][0]["height"], 935_400);
        assert_eq!(v["recent_blocks"][0]["median_rate"], 3);
        assert_eq!(v["recent_blocks"][0]["vsize"], 997_501);
        // The mempool is not the chain query's: it does not wait for the chain lock.
        assert!(v["mempool"].is_null() && v["uptime"].is_null(), "{v}");
        query(&http, &rpc(&fake.url), &mut known, 1010).await.unwrap();
        // One header lookup and six block stats, each only once (cached by hash).
        assert_eq!(fake.headers.load(Ordering::SeqCst), 1 + 600);
        // First query: the three chain calls side by side, then counts + hashes, then
        // block stats. Second (10 s later): the chain calls, then counts + hashes.
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3 + 1 + 1 + 3 + 1);

        let mut mknown = Known::default();
        let m = query_mempool(&http, &rpc(&fake.url), &mut mknown, 1000).await.unwrap();
        assert_eq!(m["uptime"], 60, "{m}");
        assert_eq!(m["mempool"]["transactions"], 1);
        assert_eq!(m["mempool_blocks"][0]["median_rate"], 10.0);
        // One batch; 10 s later the diagram is not due yet.
        query_mempool(&http, &rpc(&fake.url), &mut mknown, 1010).await.unwrap();
        assert_eq!(fake.requests.load(Ordering::SeqCst), 9 + 2);
        assert_eq!(mknown.mempool_blocks.as_ref().unwrap().0, 1000);
    }

    #[test]
    fn the_view_lays_the_mempool_over_the_chain() {
        let mut c = StatusCache::default();
        c.store_mempool(
            "u",
            Ok(json!({"mempool": {"transactions": 5}, "uptime": 60, "mempool_blocks": [],
                "errors": {"getmempoolfeeratediagram": "busy"}})),
            200,
        );
        // Before the chain ever answered: its error, with the mempool already there.
        let v = c.view("u");
        assert_eq!(v["error"], "waiting for bitcoind", "{v}");
        assert_eq!(v["mempool"]["transactions"], 5);
        c.store("u", Ok(json!({"blocks": 7, "errors": {}})), 100);
        let v = c.view("u");
        assert_eq!((v["blocks"].clone(), v["updated_at"].clone()), (json!(7), json!(100)));
        assert_eq!((v["uptime"].clone(), v["mempool_updated_at"].clone()), (json!(60), json!(200)));
        assert_eq!(v["errors"]["getmempoolfeeratediagram"], "busy");
        // A failed mempool query keeps the last good one.
        c.store_mempool("u", Err("timeout".into()), 300);
        assert_eq!(c.view("u")["mempool_updated_at"], 200);
    }

    #[tokio::test]
    async fn without_getchaintxstats_progress_is_by_blocks() {
        let fake = fake_bitcoind(Some(&[
            "getblockchaininfo",
            "getnetworkinfo",
            "getmempoolinfo",
            "getchainstates",
            "getblockheader",
            "uptime",
        ]))
        .await;
        let http = reqwest::Client::new();
        let mut known = Known::default();
        let v = query(&http, &rpc(&fake.url), &mut known, 1000).await.unwrap();
        let bg = &v["background_validation"];
        assert_eq!(bg["progress"], 0.5, "{v}");
        assert_eq!(bg["progress_basis"], "blocks");
        assert_eq!(known.base.as_ref().map(|b| b.height), Some(935_000));
        assert!(known.skips("getchaintxstats#bg", 1010));
    }

    #[tokio::test]
    async fn a_strict_rpcwhitelist_still_shows_what_it_allows() {
        use std::sync::atomic::Ordering;
        // The bundled read-only user before the wider whitelist.
        let fake =
            fake_bitcoind(Some(&["getblockchaininfo", "getnetworkinfo", "estimatesmartfee"])).await;
        let http = reqwest::Client::new();
        let (mut known, mut mknown) = (Known::default(), Known::default());
        let count = || fake.requests.load(Ordering::SeqCst);
        let v = query(&http, &rpc(&fake.url), &mut known, 1000).await.unwrap();
        assert_eq!(v["blocks"], 935_400, "{v}");
        assert_eq!(v["version"], 310100);
        assert!(v["background_validation"].is_null());
        assert_eq!(v["errors"]["getchainstates"], NOT_ALLOWED);
        // The chain calls alone (one refused); the refused batch of block hashes, then the
        // first alone (a refused method is not asked again under another id).
        assert_eq!(count(), 3 + 2);
        let m = query_mempool(&http, &rpc(&fake.url), &mut mknown, 1000).await.unwrap();
        assert!(m["mempool"].is_null());
        assert_eq!(m["errors"]["getmempoolinfo"], NOT_ALLOWED);
        // The refused mempool batch, then each of its three calls alone.
        assert_eq!(count(), 5 + 4);
        let mut refused: Vec<_> =
            known.forbidden.keys().chain(mknown.forbidden.keys()).cloned().collect();
        refused.sort();
        assert_eq!(
            refused,
            [
                "getblockhash",
                "getchainstates",
                "getmempoolfeeratediagram",
                "getmempoolinfo",
                "uptime"
            ]
        );

        // Next poll: only the allowed calls.
        let v = query(&http, &rpc(&fake.url), &mut known, 1015).await.unwrap();
        query_mempool(&http, &rpc(&fake.url), &mut mknown, 1015).await.unwrap();
        assert_eq!(v["blocks"], 935_400);
        assert_eq!(count(), 9 + 2);

        // Ten minutes later the refused ones are tried again.
        let later = 1000 + FORBIDDEN_RETRY_SECS;
        query(&http, &rpc(&fake.url), &mut known, later).await.unwrap();
        query_mempool(&http, &rpc(&fake.url), &mut mknown, later).await.unwrap();
        assert_eq!(count(), 11 + 5 + 4);
    }

    #[tokio::test]
    async fn an_unreachable_bitcoind_forgets_the_refusals() {
        // A closed port: bitcoind restarting.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let mut known = Known::default();
        known.forbidden.insert("getmempoolinfo".into(), 1000);
        let http = reqwest::Client::new();
        assert!(query(&http, &rpc(&url), &mut known, 1010).await.is_err());
        assert!(known.forbidden.is_empty());
    }

    #[test]
    fn time_left_from_the_recent_rate() {
        let mut k = Known::default();
        k.sample_background(0, Some(1_000));
        assert_eq!(k.background_rate(), None, "one sample is no rate");
        k.sample_background(30, Some(4_000));
        assert_eq!(k.background_rate(), None, "too short a span");
        k.sample_background(100, Some(11_000));
        assert_eq!(k.background_rate(), Some(100.0));
        // Long enough: the last hour only, not the slower start.
        k.sample_background(3_000, Some(500_000));
        k.sample_background(4_000, Some(1_000_000));
        k.sample_background(5_000, Some(1_500_000));
        assert_eq!(k.background_rate(), Some(500.0));
        // Backwards (another snapshot): start over.
        k.sample_background(5_100, Some(10));
        assert_eq!(k.bg_samples.len(), 1);
        k.sample_background(5_200, None);
        assert!(k.bg_samples.is_empty());
    }

    #[tokio::test]
    async fn query_estimates_the_time_left() {
        let fake = fake_bitcoind(None).await;
        let http = reqwest::Client::new();
        let mut known = Known::default();
        // The fake's count does not move: no rate yet, so no estimate.
        let v = query(&http, &rpc(&fake.url), &mut known, 1000).await.unwrap();
        assert!(v["background_validation"]["eta_secs"].is_null(), "{v}");
        // 300 M done of 1.2 G at 1 M/s: 900 M left, 900 s.
        known.bg_samples.clear();
        known.bg_samples.push_back((0, 200_000_000));
        let v = query(&http, &rpc(&fake.url), &mut known, 100).await.unwrap();
        assert_eq!(v["background_validation"]["tx_per_sec"], 1_000_000.0);
        assert_eq!(v["background_validation"]["eta_secs"], 900);
    }

    #[test]
    fn projected_blocks_fill_like_a_miner() {
        let points = |chunks: &[(u64, f64)]| {
            let (mut w, mut f) = (0, 0.0);
            let mut v = vec![json!({"weight": 0, "fee": 0.0})];
            for (cw, cf) in chunks {
                w += cw;
                f += cf;
                v.push(json!({"weight": w, "fee": f}));
            }
            Value::Array(v)
        };
        // 10, 2 and 1 sat/vB, then 2.9 MWU at 0.5 sat/vB in small chunks.
        let mut chunks = vec![(2_000_000, 0.05), (1_500_000, 0.0075), (1_000_000, 0.0025)];
        chunks.extend(std::iter::repeat_n((100_000, 0.000125), 29));
        let d = points(&chunks);

        let b = project_blocks(&d, 6);
        let b = b.as_array().unwrap();
        assert_eq!(b.len(), 2);
        // The 1 sat/vB chunk does not fit next to the first two: it opens block 2.
        assert_eq!(b[0]["vsize"], 875_000);
        assert_eq!(b[0]["max_rate"], 10.0);
        assert_eq!(b[0]["median_rate"], 10.0);
        assert_eq!(b[0]["min_rate"], 2.0);
        assert_eq!(b[0]["fee_sat"], 5_750_000);
        assert_eq!(b[1]["max_rate"], 1.0);
        assert_eq!(b[1]["min_rate"], 0.5);
        assert_eq!(b[1]["count"], 1);

        // With room for one block, the last entry sums up the rest.
        let b = project_blocks(&d, 1);
        assert_eq!(b[0]["count"], 2);
        assert_eq!(b[0]["vsize"], 1_850_000);

        assert_eq!(project_blocks(&json!([]), 6), json!([]));
    }
}
