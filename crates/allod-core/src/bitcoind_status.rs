//! Bitcoin Core status for the Node page (`GET /api/node/bitcoind`): version, chain tip,
//! peers, mempool, disk, warnings, and the background validation that follows a fast sync
//! (AssumeUTXO). Everything comes from one batched JSON-RPC request, so a busy bitcoind
//! holds one request in its queue rather than five.
//!
//! bitcoind refuses a whole batch (HTTP 403) when its `rpcwhitelist` lacks one of the
//! methods. The calls are then made one by one, and the refused ones are left out for an
//! hour, so a node with a stricter whitelist still shows what it allows without a warning
//! in its log at every poll.
//!
//! With a loaded UTXO snapshot bitcoind runs two chainstates: the snapshot chain, which
//! the node already uses, and a background chain that validates every block from genesis
//! up to the snapshot's base block. Until it gets there the snapshot's UTXO set is trusted
//! on the strength of the hash built into the Bitcoin Core release.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::setup::BitcoindRpc;

/// How long one status query may take. Background validation and a syncing tip hold
/// bitcoind's chain lock while they connect blocks, so a small machine can take a minute.
pub(crate) const STATUS_QUERY_TIMEOUT: Duration = Duration::from_secs(180);

/// Calls in the status batch. `uptime` and `getnetworkinfo` do not need the chain lock,
/// so they answer even while the chain calls queue.
const METHODS: [&str; 5] =
    ["getblockchaininfo", "getnetworkinfo", "getmempoolinfo", "getchainstates", "uptime"];

/// The watched bitcoind's last status answer (`SetupService::bitcoind_status`).
#[derive(Debug, Default)]
pub(crate) struct StatusCache {
    /// RPC URL the entries belong to (the watched node can change to the standby).
    pub url: String,
    /// Last good answer and when it arrived (unix seconds).
    pub ok: Option<(Value, i64)>,
    /// Error of the latest query, if it failed.
    pub error: Option<String>,
    /// What earlier queries learned about this node.
    pub known: Known,
}

/// Carried from one query to the next (per watched node).
#[derive(Debug, Default, Clone)]
pub(crate) struct Known {
    /// Snapshot base block hash and its height: it never changes, so it is asked once.
    pub base: Option<(String, u64)>,
    /// Methods bitcoind's `rpcwhitelist` refused, and when (unix seconds).
    pub forbidden: HashMap<String, i64>,
}

/// How long a refused method is left out before it is tried again (the owner may have
/// widened the whitelist meanwhile).
const FORBIDDEN_RETRY_SECS: i64 = 3600;

const NOT_ALLOWED: &str = "not allowed by bitcoind's rpcwhitelist";

impl Known {
    fn skips(&self, method: &str, now: i64) -> bool {
        self.forbidden.get(method).is_some_and(|at| now - at < FORBIDDEN_RETRY_SECS)
    }
}

impl StatusCache {
    pub fn store(&mut self, url: &str, result: Result<Value, String>, now: i64) {
        if self.url != url {
            *self = StatusCache { url: url.to_string(), ..Default::default() };
        }
        match result {
            Ok(v) => {
                self.ok = Some((v, now));
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The last good answer with `updated_at` (and `last_error` when the latest query
    /// failed), or the error when bitcoind never answered.
    pub fn view(&self, url: &str) -> Value {
        match (&self.ok, &self.error) {
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
        }
    }
}

#[derive(Debug, PartialEq)]
enum BatchError {
    /// HTTP 403: a method is not in the user's `rpcwhitelist`.
    Forbidden,
    Other(String),
}

type Batch = HashMap<String, Result<Value, String>>;

/// One JSON-RPC batch; each reply is keyed by method name, `Err` with bitcoind's message.
async fn batch(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    calls: &[(&str, Value)],
) -> Result<Batch, BatchError> {
    let body: Vec<Value> = calls
        .iter()
        .map(|(m, p)| json!({"jsonrpc": "1.0", "id": m, "method": m, "params": p}))
        .collect();
    let resp = http
        .post(&rpc.url)
        .basic_auth(&rpc.user, Some(&rpc.pass))
        .timeout(STATUS_QUERY_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| BatchError::Other(e.to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(BatchError::Forbidden);
    }
    let value: Value = allod_lnurl::bounded_response_json(resp, allod_lnurl::JSON_RESPONSE_LIMIT)
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
            for (m, _) in &wanted {
                known.forbidden.remove(*m);
            }
            out.extend(r);
            Ok(out)
        }
        Err(BatchError::Other(e)) => Err(e),
        Err(BatchError::Forbidden) => {
            for call in &wanted {
                let m = call.0;
                match batch(http, rpc, std::slice::from_ref(call)).await {
                    Ok(r) => {
                        known.forbidden.remove(m);
                        out.extend(r);
                    }
                    Err(BatchError::Forbidden) => {
                        known.forbidden.insert(m.to_string(), now);
                        out.insert(m.to_string(), Err(NOT_ALLOWED.into()));
                    }
                    Err(BatchError::Other(e)) => {
                        out.insert(m.to_string(), Err(e));
                    }
                }
            }
            Ok(out)
        }
    }
}

fn rpc_error(e: &Value) -> Option<String> {
    match e {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        e => Some(e["message"].as_str().map(str::to_string).unwrap_or_else(|| e.to_string())),
    }
}

/// Queries the status; `known` carries the snapshot base height and refused methods
/// between queries.
pub(crate) async fn query(
    http: &reqwest::Client,
    rpc: &BitcoindRpc,
    known: &mut Known,
    now: i64,
) -> Result<Value, String> {
    let list: Vec<(&str, Value)> = METHODS.iter().map(|m| (*m, json!([]))).collect();
    let mut replies = calls(http, rpc, &list, known, now).await?;
    if let Some(Err(e)) = replies.get("getblockchaininfo") {
        // Without the chain there is nothing to show (e.g. "Loading block index…").
        return Err(e.clone());
    }
    let snapshot_hash =
        replies.get("getchainstates").and_then(|r| r.as_ref().ok()).and_then(snapshot_blockhash);
    known.base = match (snapshot_hash, known.base.take()) {
        (Some(hash), Some((h, height))) if h == hash => Some((h, height)),
        (Some(hash), _) => calls(http, rpc, &[("getblockheader", json!([hash, true]))], known, now)
            .await
            .ok()
            .and_then(|mut r| r.remove("getblockheader"))
            .and_then(|r| r.ok())
            .and_then(|h| h["height"].as_u64())
            .map(|height| (hash, height)),
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
        mempool: get("getmempoolinfo"),
        chainstates: get("getchainstates"),
        uptime: get("uptime"),
    };
    Ok(shape(r, known.base.as_ref().map(|b| b.1)))
}

/// The snapshot chainstate's base block, when one is loaded.
fn snapshot_blockhash(chainstates: &Value) -> Option<String> {
    chainstates["chainstates"]
        .as_array()?
        .iter()
        .find_map(|c| c["snapshot_blockhash"].as_str().map(str::to_string))
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
fn background(chainstates: &Value, base_height: Option<u64>) -> Value {
    let Some(list) = chainstates["chainstates"].as_array() else { return Value::Null };
    let Some(snapshot) = list.iter().find(|c| c["snapshot_blockhash"].is_string()) else {
        return Value::Null;
    };
    if snapshot["validated"] == true {
        return json!({"active": false, "complete": true, "target_height": base_height});
    }
    let bg = list.iter().find(|c| !c["snapshot_blockhash"].is_string());
    let blocks = bg.and_then(|c| c["blocks"].as_u64());
    let progress = match (blocks, base_height) {
        (Some(b), Some(t)) if t > 0 => json!((b as f64 / t as f64).min(1.0)),
        _ => Value::Null,
    };
    json!({
        "active": bg.is_some(),
        "complete": false,
        "blocks": blocks,
        "target_height": base_height,
        "progress": progress,
        // Bitcoin Core's own estimate (by transaction count), for the background chain.
        "verification_progress": bg.map(|c| c["verificationprogress"].clone()),
    })
}

/// The API shape of [`query`], split out so it can be tested without RPC.
pub(crate) fn shape(r: Replies, base_height: Option<u64>) -> Value {
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
    let mempool = if m.is_null() {
        Value::Null
    } else {
        json!({
            "loaded": m["loaded"],
            "transactions": m["size"],
            "vbytes": m["bytes"],
            "usage": m["usage"],
            "max": m["maxmempool"],
            "min_fee_sat_vb": sat_per_vb(&m["mempoolminfee"]),
            "total_fee_sat": m["total_fee"].as_f64().map(|f| (f * 1e8).round() as u64),
        })
    };
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
        "background_validation": background(&cs, base_height),
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
        let v = shape(replies(cs), Some(935_000));
        let bg = &v["background_validation"];
        assert_eq!(bg["active"], true, "{v}");
        assert_eq!(bg["complete"], false);
        assert_eq!(bg["blocks"], 300_000);
        assert_eq!(bg["target_height"], 935_000);
        assert!((bg["progress"].as_f64().unwrap() - 300_000.0 / 935_000.0).abs() < 1e-9);
        assert_eq!(bg["verification_progress"], 0.05);
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
        let v = shape(replies(done), Some(935_000));
        assert_eq!(v["background_validation"]["complete"], true);
        assert_eq!(v["background_validation"]["active"], false);

        let plain = json!({"chainstates": [{"blocks": 935_400, "validated": true}]});
        assert!(shape(replies(plain), None)["background_validation"].is_null());
    }

    #[test]
    fn failed_calls_are_listed_and_the_rest_is_kept() {
        let mut r = replies(Value::Null);
        r.chainstates = Err("Method not found".into());
        r.mempool = Err("busy".into());
        let v = shape(r, None);
        assert!(v["background_validation"].is_null());
        assert!(v["mempool"].is_null());
        assert_eq!(v["errors"]["getchainstates"], "Method not found");
        assert_eq!(v["blocks"], 935_400);
        // Pre-28 bitcoind: one warnings string.
        let mut r = replies(Value::Null);
        r.chain = Ok(json!({"blocks": 1, "warnings": "Unknown new rules activated"}));
        assert_eq!(
            shape(r, None)["warnings"],
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
                                    {"blocks": 467_500, "validated": true},
                                    {"blocks": 935_400, "snapshot_blockhash": "00ff", "validated": false}
                                ]}),
                                "uptime" => json!(60),
                                "getblockheader" => {
                                    hdr.fetch_add(1, Ordering::SeqCst);
                                    assert_eq!(c["params"][0], "00ff");
                                    json!({"height": 935_000})
                                }
                                _ => {
                                    return json!({"id": m, "result": null,
                                        "error": {"code": -32601, "message": "Method not found"}})
                                }
                            };
                            json!({"id": m, "result": result, "error": null})
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
    async fn query_batches_and_looks_up_the_base_once() {
        use std::sync::atomic::Ordering;
        let fake = fake_bitcoind(None).await;
        let http = reqwest::Client::new();
        let mut known = Known::default();
        let v = query(&http, &rpc(&fake.url), &mut known, 1000).await.unwrap();
        assert_eq!(known.base, Some(("00ff".to_string(), 935_000)));
        assert_eq!(v["background_validation"]["progress"], 0.5, "{v}");
        assert_eq!(v["uptime"], 60);
        query(&http, &rpc(&fake.url), &mut known, 1010).await.unwrap();
        assert_eq!(fake.headers.load(Ordering::SeqCst), 1);
        // One batch per query plus the one header lookup.
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_strict_rpcwhitelist_still_shows_what_it_allows() {
        use std::sync::atomic::Ordering;
        // The bundled read-only user before the wider whitelist.
        let fake =
            fake_bitcoind(Some(&["getblockchaininfo", "getnetworkinfo", "estimatesmartfee"])).await;
        let http = reqwest::Client::new();
        let mut known = Known::default();
        let v = query(&http, &rpc(&fake.url), &mut known, 1000).await.unwrap();
        assert_eq!(v["blocks"], 935_400, "{v}");
        assert_eq!(v["version"], 310100);
        assert!(v["mempool"].is_null());
        assert!(v["background_validation"].is_null());
        assert_eq!(v["errors"]["getmempoolinfo"], NOT_ALLOWED);
        assert_eq!(v["errors"]["getchainstates"], NOT_ALLOWED);
        // The refused batch, then each of the five calls alone.
        assert_eq!(fake.requests.load(Ordering::SeqCst), 6);
        let mut refused: Vec<_> = known.forbidden.keys().cloned().collect();
        refused.sort();
        assert_eq!(refused, ["getchainstates", "getmempoolinfo", "uptime"]);

        // Next poll: only the allowed calls, in one batch.
        let v = query(&http, &rpc(&fake.url), &mut known, 1015).await.unwrap();
        assert_eq!(v["blocks"], 935_400);
        assert_eq!(fake.requests.load(Ordering::SeqCst), 7);

        // An hour later the refused ones are tried again.
        query(&http, &rpc(&fake.url), &mut known, 1000 + FORBIDDEN_RETRY_SECS).await.unwrap();
        assert_eq!(fake.requests.load(Ordering::SeqCst), 7 + 6);
    }
}
