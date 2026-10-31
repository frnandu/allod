//! Transparent bitcoind JSON-RPC proxy which recovers pruned blocks from Bitcoin peers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use clap::Parser;
use futures_util::future::{BoxFuture, FutureExt, Shared};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{Mutex, Semaphore};

type RecoveryResult = Result<(), Arc<String>>;
type RecoveryFuture = Shared<BoxFuture<'static, RecoveryResult>>;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, env = "BITCOIND_PROXY_LISTEN", default_value = "0.0.0.0:8332")]
    listen: SocketAddr,
    #[arg(long, env = "BITCOIND_PROXY_UPSTREAM_URL")]
    upstream_url: String,
    #[arg(long, env = "BITCOIND_RPC_USER")]
    rpc_user: String,
    #[arg(long, env = "BITCOIND_RPC_PASS", hide_env_values = true)]
    rpc_pass: String,
    #[arg(long, env = "BITCOIND_PROXY_MAX_FETCHES", default_value_t = 4)]
    max_fetches: usize,
    #[arg(long, env = "BITCOIND_PROXY_FETCH_TIMEOUT_SECS", default_value_t = 240)]
    fetch_timeout_secs: u64,
    #[arg(long, env = "BITCOIND_PROXY_PEER_TIMEOUT_SECS", default_value_t = 15)]
    peer_timeout_secs: u64,
}

#[derive(Default)]
struct Metrics {
    rpc_requests: AtomicU64,
    local_block_hits: AtomicU64,
    pruned_block_requests: AtomicU64,
    recovery_jobs: AtomicU64,
    blocks_recovered: AtomicU64,
    blocks_failed: AtomicU64,
    bytes_downloaded: AtomicU64,
    queued: AtomicUsize,
    fetching: AtomicUsize,
    eligible_peers: AtomicUsize,
    last_fetch_at: AtomicU64,
    last_error_at: AtomicU64,
    last_error: Mutex<Option<String>>,
}

#[derive(Debug, Serialize)]
struct ProxyStatus {
    rpc_requests: u64,
    local_block_hits: u64,
    pruned_block_requests: u64,
    recovery_jobs: u64,
    blocks_recovered: u64,
    blocks_failed: u64,
    bytes_downloaded: u64,
    blocks_queued: usize,
    blocks_fetching: usize,
    eligible_bitcoin_peers: usize,
    last_fetch_at: Option<u64>,
    last_error_at: Option<u64>,
    last_error: Option<String>,
}

impl Metrics {
    async fn snapshot(&self) -> ProxyStatus {
        ProxyStatus {
            rpc_requests: self.rpc_requests.load(Ordering::Relaxed),
            local_block_hits: self.local_block_hits.load(Ordering::Relaxed),
            pruned_block_requests: self.pruned_block_requests.load(Ordering::Relaxed),
            recovery_jobs: self.recovery_jobs.load(Ordering::Relaxed),
            blocks_recovered: self.blocks_recovered.load(Ordering::Relaxed),
            blocks_failed: self.blocks_failed.load(Ordering::Relaxed),
            bytes_downloaded: self.bytes_downloaded.load(Ordering::Relaxed),
            blocks_queued: self.queued.load(Ordering::Relaxed),
            blocks_fetching: self.fetching.load(Ordering::Relaxed),
            eligible_bitcoin_peers: self.eligible_peers.load(Ordering::Relaxed),
            last_fetch_at: nonzero(self.last_fetch_at.load(Ordering::Relaxed)),
            last_error_at: nonzero(self.last_error_at.load(Ordering::Relaxed)),
            last_error: self.last_error.lock().await.clone(),
        }
    }
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

struct App {
    client: reqwest::Client,
    upstream_url: String,
    rpc_user: String,
    rpc_pass: String,
    expected_auth: String,
    fetch_timeout: Duration,
    peer_timeout: Duration,
    fetch_slots: Semaphore,
    metrics: Metrics,
    recoveries: Mutex<HashMap<String, RecoveryFuture>>,
}

impl App {
    fn new(args: Args) -> Arc<Self> {
        let token = BASE64.encode(format!("{}:{}", args.rpc_user, args.rpc_pass));
        Arc::new(Self {
            client: reqwest::Client::new(),
            upstream_url: args.upstream_url.trim_end_matches('/').to_string(),
            rpc_user: args.rpc_user,
            rpc_pass: args.rpc_pass,
            expected_auth: format!("Basic {token}"),
            fetch_timeout: Duration::from_secs(args.fetch_timeout_secs),
            peer_timeout: Duration::from_secs(args.peer_timeout_secs),
            fetch_slots: Semaphore::new(args.max_fetches.max(1)),
            metrics: Metrics::default(),
            recoveries: Mutex::new(HashMap::new()),
        })
    }

    async fn upstream(&self, body: Bytes) -> Result<(StatusCode, Bytes), String> {
        let response = self
            .client
            .post(&self.upstream_url)
            .basic_auth(&self.rpc_user, Some(&self.rpc_pass))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("upstream request failed: {e}"))?;
        let status = response.status();
        let body = response.bytes().await.map_err(|e| format!("read upstream response: {e}"))?;
        Ok((status, body))
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "1.0",
                "id": "allod-bitcoind-proxy",
                "method": method,
                "params": params,
            }))
            .map_err(|e| e.to_string())?,
        );
        let (_, raw) = self.upstream(body).await?;
        let value: Value =
            serde_json::from_slice(&raw).map_err(|e| format!("invalid {method} response: {e}"))?;
        if !value["error"].is_null() {
            return Err(value["error"]["message"]
                .as_str()
                .unwrap_or("unknown bitcoind error")
                .to_string());
        }
        Ok(value["result"].clone())
    }

    async fn recovery(self: &Arc<Self>, hash: String) -> RecoveryResult {
        let mut jobs = self.recoveries.lock().await;
        if let Some(job) = jobs.get(&hash) {
            let job = job.clone();
            drop(jobs);
            return job.await;
        }

        self.metrics.recovery_jobs.fetch_add(1, Ordering::Relaxed);
        self.metrics.queued.fetch_add(1, Ordering::Relaxed);
        let app = Arc::clone(self);
        let recovery_hash = hash.clone();
        let job = async move {
            let result =
                tokio::time::timeout(app.fetch_timeout, app.fetch_pruned_block(&recovery_hash))
                    .await
                    .map_err(|_| Arc::new("timed out fetching pruned block".to_string()))
                    .and_then(|result| result.map_err(Arc::new));
            if let Err(error) = &result {
                app.metrics.blocks_failed.fetch_add(1, Ordering::Relaxed);
                app.metrics.last_error_at.store(unix_now(), Ordering::Relaxed);
                *app.metrics.last_error.lock().await = Some((**error).clone());
            }
            result
        }
        .boxed()
        .shared();
        jobs.insert(hash.clone(), job.clone());
        drop(jobs);

        let cleanup_app = Arc::clone(self);
        let cleanup_job = job.clone();
        tokio::spawn(async move {
            let _ = cleanup_job.await;
            cleanup_app.recoveries.lock().await.remove(&hash);
        });
        job.await
    }

    async fn fetch_pruned_block(&self, hash: &str) -> Result<(), String> {
        let permit = self.fetch_slots.acquire().await.map_err(|_| "proxy stopping".to_string())?;
        self.metrics.queued.fetch_sub(1, Ordering::Relaxed);
        self.metrics.fetching.fetch_add(1, Ordering::Relaxed);
        let result = self.fetch_pruned_block_inner(hash).await;
        self.metrics.fetching.fetch_sub(1, Ordering::Relaxed);
        drop(permit);
        result
    }

    async fn fetch_pruned_block_inner(&self, hash: &str) -> Result<(), String> {
        if let Ok(block) = self.rpc("getblock", json!([hash, 0])).await {
            self.record_recovered(&block).await;
            return Ok(());
        }

        let peers = self.rpc("getpeerinfo", json!([])).await?;
        let eligible: Vec<u64> = peers
            .as_array()
            .into_iter()
            .flatten()
            .filter(|peer| {
                let names = peer["servicesnames"].as_array();
                let has =
                    |name: &str| names.into_iter().flatten().any(|v| v.as_str() == Some(name));
                has("NETWORK") && (has("WITNESS") || has("WITNESS_RELAY"))
            })
            .filter_map(|peer| peer["id"].as_u64())
            .collect();
        self.metrics.eligible_peers.store(eligible.len(), Ordering::Relaxed);
        if eligible.is_empty() {
            return Err("no Bitcoin peer advertises NETWORK and WITNESS".into());
        }

        let mut errors = Vec::new();
        for peer_id in eligible {
            match self.rpc("getblockfrompeer", json!([hash, peer_id])).await {
                Ok(_) => {}
                Err(e) if e.contains("already downloaded") => {}
                Err(e) => {
                    errors.push(format!("peer {peer_id}: {e}"));
                    continue;
                }
            }
            let deadline = tokio::time::Instant::now() + self.peer_timeout;
            while tokio::time::Instant::now() < deadline {
                if let Ok(block) = self.rpc("getblock", json!([hash, 0])).await {
                    self.record_recovered(&block).await;
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            errors.push(format!("peer {peer_id}: no block before peer timeout"));
        }
        Err(format!("failed to recover block {hash}: {}", errors.join("; ")))
    }

    async fn record_recovered(&self, block: &Value) {
        let bytes = block.as_str().map(|hex| hex.len() / 2).unwrap_or(0) as u64;
        self.metrics.blocks_recovered.fetch_add(1, Ordering::Relaxed);
        self.metrics.bytes_downloaded.fetch_add(bytes, Ordering::Relaxed);
        self.metrics.last_fetch_at.store(unix_now(), Ordering::Relaxed);
        *self.metrics.last_error.lock().await = None;
    }
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", post(proxy_rpc))
        .route("/status", get(status))
        .route("/health", get(health))
        .with_state(app)
}

async fn status(State(app): State<Arc<App>>) -> Json<ProxyStatus> {
    Json(app.metrics.snapshot().await)
}

async fn health(State(app): State<Arc<App>>) -> impl IntoResponse {
    match app.rpc("getblockchaininfo", json!([])).await {
        Ok(_) => (StatusCode::OK, "ok"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable"),
    }
}

async fn proxy_rpc(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
    if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        != Some(app.expected_auth.as_str())
    {
        return response(StatusCode::UNAUTHORIZED, Bytes::from_static(b"unauthorized"));
    }
    app.metrics.rpc_requests.fetch_add(1, Ordering::Relaxed);

    let request: Option<Value> = serde_json::from_slice(&body).ok();
    let block_hash = request.as_ref().and_then(|request| {
        (request["method"].as_str() == Some("getblock"))
            .then(|| request["params"].get(0).and_then(Value::as_str).map(str::to_owned))
            .flatten()
    });

    let first = match app.upstream(body.clone()).await {
        Ok(value) => value,
        Err(error) => return rpc_error(request.as_ref(), &error),
    };
    if block_hash.is_none() || !is_pruned_error(&first.1) {
        if block_hash.is_some() && first.0.is_success() {
            app.metrics.local_block_hits.fetch_add(1, Ordering::Relaxed);
        }
        return response(first.0, first.1);
    }

    app.metrics.pruned_block_requests.fetch_add(1, Ordering::Relaxed);
    let hash = block_hash.expect("checked");
    if let Err(error) = app.recovery(hash).await {
        return rpc_error(request.as_ref(), error.as_str());
    }
    match app.upstream(body).await {
        Ok((status, body)) => response(status, body),
        Err(error) => rpc_error(request.as_ref(), &error),
    }
}

fn is_pruned_error(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
        .is_some_and(|message| message.contains("Block not available (pruned data)"))
}

fn rpc_error(request: Option<&Value>, message: &str) -> Response<Body> {
    let id = request.map(|v| v["id"].clone()).unwrap_or(Value::Null);
    let body = serde_json::to_vec(&json!({
        "result": null,
        "error": {"code": -1, "message": message},
        "id": id,
    }))
    .unwrap_or_default();
    response(StatusCode::INTERNAL_SERVER_ERROR, Bytes::from(body))
}

fn response(status: StatusCode, body: Bytes) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("valid response")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let listen = args.listen;
    let app = App::new(args);
    let listener = tokio::net::TcpListener::bind(listen).await.expect("bind proxy");
    tracing::info!(%listen, upstream = %app.upstream_url, "bitcoind proxy listening");
    axum::serve(listener, router(app)).await.expect("serve proxy");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::extract::State;
    use std::sync::atomic::AtomicBool;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct MockCore(Arc<AtomicBool>);

    async fn core(State(core): State<MockCore>, Json(request): Json<Value>) -> impl IntoResponse {
        let method = request["method"].as_str().unwrap_or_default();
        let result = match method {
            "getblock" if !core.0.load(Ordering::Relaxed) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "result": null,
                        "error": {"code": -1, "message": "Block not available (pruned data)"},
                        "id": request["id"],
                    })),
                );
            }
            "getblock" => Value::String("00".repeat(80)),
            "getpeerinfo" => json!([{"id": 7, "servicesnames": ["NETWORK", "WITNESS"]}]),
            "getblockfrompeer" => {
                core.0.store(true, Ordering::Relaxed);
                json!({})
            }
            "getblockchaininfo" => json!({"blocks": 1}),
            _ => Value::Null,
        };
        (StatusCode::OK, Json(json!({"result": result, "error": null, "id": request["id"]})))
    }

    #[tokio::test]
    async fn transparently_recovers_pruned_block() {
        let core_state = MockCore(Arc::new(AtomicBool::new(false)));
        let core_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let core_addr = core_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(core_listener, Router::new().route("/", post(core)).with_state(core_state))
                .await
                .unwrap();
        });
        let app = App::new(Args {
            listen: "127.0.0.1:0".parse().unwrap(),
            upstream_url: format!("http://{core_addr}"),
            rpc_user: "ldk".into(),
            rpc_pass: "secret".into(),
            max_fetches: 1,
            fetch_timeout_secs: 2,
            peer_timeout_secs: 1,
        });
        let auth = format!("Basic {}", BASE64.encode("ldk:secret"));
        let request = axum::http::Request::post("/")
            .header(header::AUTHORIZATION, auth)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"jsonrpc":"1.0","id":"ldk","method":"getblock","params":["abc",0]})
                    .to_string(),
            ))
            .unwrap();
        let response = router(Arc::clone(&app)).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["result"].as_str().unwrap().len(), 160);
        let status = app.metrics.snapshot().await;
        assert_eq!(status.pruned_block_requests, 1);
        assert_eq!(status.blocks_recovered, 1);
        assert_eq!(status.eligible_bitcoin_peers, 1);
    }
}
