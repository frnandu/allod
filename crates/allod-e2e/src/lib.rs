//! Harness for the e2e tests (SPEC §16.4). The tests run against a regtest stack that is
//! already up (`just up`) and are `#[ignore]`d, so `cargo test --workspace` skips them.
//!
//! Environment:
//! - `ALLOD_E2E_URL`: Allod base URL (default `http://127.0.0.1:18029`).
//! - `ALLOD_E2E_PROJECT`: Compose project, for restarts (default `allod-regtest`).
//! - `ALLOD_E2E_DEV_DIR`: the regtest `deploy/.dev` directory (default: found from the crate).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use allod_ln::types::LnStatus;
use allod_ln::{LdkServerBackend, LdkServerConfig, LightningBackend};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures_util::StreamExt;
use nostr::prelude::*;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Secret key of the fixed regtest owner (`deploy/regtest.env`, public by design).
pub const DEV_OWNER_SECRET_HEX: &str =
    "9a2fb0b5e2cca568a75ae871559e357aed59005cf30ff7053f01b10709c04f0a";

const RPC_URL: &str = "http://127.0.0.1:18443";
const RPC_USER: &str = "ldk";
const RPC_PASS: &str = "ldk-regtest";

pub struct Stack {
    pub http: reqwest::Client,
    pub base: String,
    pub project: String,
    pub keys: Keys,
    token: tokio::sync::RwLock<String>,
    pub alice: LdkServerBackend,
    pub bob: LdkServerBackend,
    pub carol: LdkServerBackend,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ldk(dev: &std::path::Path, name: &str, port: u16) -> Result<LdkServerBackend> {
    let dir = dev.join(name);
    let config = LdkServerConfig::from_files(
        format!("localhost:{port}"),
        &dir.join("tls.crt"),
        &dir.join("admin.macaroon"),
    )
    .with_context(|| format!("credentials of {name} in {}", dir.display()))?;
    Ok(LdkServerBackend::new(&config)?)
}

impl Stack {
    pub async fn connect() -> Result<Self> {
        let base =
            std::env::var("ALLOD_E2E_URL").unwrap_or_else(|_| "http://127.0.0.1:18029".into());
        let dev = std::env::var("ALLOD_E2E_DEV_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root().join("deploy/.dev"));
        let stack = Stack {
            http: reqwest::Client::builder().timeout(Duration::from_secs(90)).build()?,
            base: base.trim_end_matches('/').to_string(),
            project: std::env::var("ALLOD_E2E_PROJECT").unwrap_or_else(|_| "allod-regtest".into()),
            keys: Keys::parse(DEV_OWNER_SECRET_HEX)?,
            token: tokio::sync::RwLock::new(String::new()),
            alice: ldk(&dev, "alice", 3536)?,
            bob: ldk(&dev, "bob", 3537)?,
            carol: ldk(&dev, "carol", 3538)?,
        };
        stack.wait_ready(Duration::from_secs(180)).await?;
        stack.login().await?;
        Ok(stack)
    }

    /// Waits until Allod answers and its Lightning node is reachable.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if let Ok(r) = self.http.get(format!("{}/api/health", self.base)).send().await {
                if let Ok(v) = r.json::<Value>().await {
                    if v["status"] == "ok" {
                        return Ok(());
                    }
                }
            }
            if start.elapsed() > timeout {
                bail!("Allod at {} is not ready after {timeout:?}", self.base);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub fn nip98(&self, url: &str, method: &str, body: &[u8]) -> Result<String> {
        nip98_header(&self.keys, url, method, body)
    }

    /// Nostr login (NIP-98) with the dev owner key; stores the session token.
    pub async fn login(&self) -> Result<String> {
        let token = login(&self.http, &self.base, &self.keys).await?;
        *self.token.write().await = token.clone();
        Ok(token)
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let token = self.token.read().await.clone();
        let mut req = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        let v = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok((status, v))
    }

    /// Calls the API and fails unless the status is 2xx.
    pub async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, v) = self.call(method.clone(), path, body).await?;
        if !status.is_success() {
            bail!("{method} {path}: {status} {v}");
        }
        Ok(v)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.ok(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.ok(Method::POST, path, Some(body)).await
    }

    /// Polls a transaction until `pred` holds.
    pub async fn wait_tx(
        &self,
        public_id: &str,
        timeout: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let start = Instant::now();
        loop {
            let tx = self.get(&format!("/api/transactions/{public_id}")).await?;
            if pred(&tx) {
                return Ok(tx);
            }
            if start.elapsed() > timeout {
                bail!("transaction {public_id} did not reach the expected state: {tx}");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    pub async fn wait_state(&self, public_id: &str, state: &str) -> Result<Value> {
        self.wait_tx(public_id, Duration::from_secs(60), |t| t["state"] == state).await
    }

    /// Transactions whose payment hash is `hash`.
    pub async fn by_hash(&self, hash: &str) -> Result<Vec<Value>> {
        let v = self.get(&format!("/api/transactions?q={hash}&include_unpaid=true")).await?;
        Ok(v["items"].as_array().cloned().unwrap_or_default())
    }

    pub async fn assert_ledger_ok(&self) -> Result<()> {
        let v = self.get("/api/ledger/check").await?;
        if v["ok"] != true {
            bail!("ledger check failed: {v}");
        }
        Ok(())
    }

    // ---------------------------------------------------------------- infrastructure

    async fn rpc(&self, method: &str, params: Value, wallet: Option<&str>) -> Result<Value> {
        let url = match wallet {
            Some(w) => format!("{RPC_URL}/wallet/{w}"),
            None => RPC_URL.to_string(),
        };
        let v: Value = self
            .http
            .post(url)
            .basic_auth(RPC_USER, Some(RPC_PASS))
            .json(&json!({"jsonrpc": "1.0", "id": "e2e", "method": method, "params": params}))
            .send()
            .await?
            .json()
            .await?;
        if !v["error"].is_null() {
            bail!("bitcoind {method}: {}", v["error"]);
        }
        Ok(v["result"].clone())
    }

    pub async fn mine(&self, n: u32) -> Result<()> {
        let addr = self.rpc("getnewaddress", json!([]), Some("miner")).await?;
        self.rpc("generatetoaddress", json!([n, addr]), None).await?;
        Ok(())
    }

    /// Restarts a Compose service of the stack (`docker restart`).
    pub fn restart(&self, service: &str) -> Result<()> {
        let name = format!("{}-{service}-1", self.project);
        let out = Command::new("docker").args(["restart", "-t", "5", &name]).output()?;
        if !out.status.success() {
            bail!("docker restart {name}: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }

    /// Waits until a payment sent by `node` leaves the pending state.
    pub async fn wait_ln(
        &self,
        node: &LdkServerBackend,
        id: &str,
        timeout: Duration,
    ) -> Result<LnStatus> {
        let start = Instant::now();
        loop {
            if let Some(p) = node.payment(id).await? {
                if p.status != LnStatus::Pending {
                    return Ok(p.status);
                }
            }
            if start.elapsed() > timeout {
                bail!("payment {id} is still pending");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

/// A random 32-byte preimage and its SHA-256 payment hash, both hex.
pub fn preimage() -> (String, String) {
    let p: [u8; 32] = rand::random();
    (hex::encode(p), hex::encode(Sha256::digest(p)))
}

/// Status of the payment with this payment hash on `node`, if any. Inbound payments have
/// their own ids in ldk-server, so this scans the payment list instead of `payment(id)`.
pub async fn status_by_hash(node: &LdkServerBackend, hash: &str) -> Result<Option<LnStatus>> {
    let mut page = None;
    loop {
        let (items, next) = node.list_payments(page).await?;
        if let Some(p) = items.iter().find(|p| p.kind.payment_hash() == Some(hash)) {
            return Ok(Some(p.status));
        }
        match next {
            Some(n) => page = Some(n),
            None => return Ok(None),
        }
    }
}

/// A NWC client for one connection (SPEC §16.4 scenarios 3–4). The pairing URI names the
/// relays as Allod sees them inside Docker; `relay_map` rewrites them for the host.
pub struct NwcClient {
    client: nostr_sdk::client::Client,
    keys: Keys,
    wallet: PublicKey,
    /// NIP-44 notifications received so far (ephemeral events: collected live).
    inbox: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

/// Relays as seen from the host.
pub fn host_relay(url: &str) -> String {
    url.replace("relay2:8080", "127.0.0.1:7001").replace("relay:8080", "127.0.0.1:7000")
}

impl NwcClient {
    pub async fn from_uri(uri: &str) -> Result<Self> {
        let rest = uri.strip_prefix("nostr+walletconnect://").context("not a NWC URI")?;
        let (wallet, query) = rest.split_once('?').context("no query")?;
        let mut relays = Vec::new();
        let mut secret = None;
        for kv in query.split('&') {
            match kv.split_once('=') {
                Some(("relay", v)) => relays.push(host_relay(&percent_decode(v))),
                Some(("secret", v)) => secret = Some(v.to_string()),
                _ => {}
            }
        }
        let keys = Keys::parse(&secret.context("no secret")?)?;
        let client = nostr_sdk::client::Client::builder().build();
        for r in &relays {
            client.add_relay(r.as_str()).await?;
        }
        client.connect().await;
        client
            .subscribe(
                Filter::new()
                    .kinds([Kind::WalletConnectResponse, Kind::Custom(23197)])
                    .pubkey(keys.public_key()),
            )
            .await?;
        let wallet = PublicKey::from_hex(wallet)?;
        let inbox: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let (mut n, sink, k) = (client.notifications(), inbox.clone(), keys.clone());
        tokio::spawn(async move {
            while let Some(msg) = n.next().await {
                if let nostr_sdk::client::ClientNotification::Event { event, .. } = msg {
                    if event.kind == Kind::Custom(23197) {
                        if let Ok(p) =
                            nostr::nips::nip44::decrypt(k.secret_key(), &wallet, &event.content)
                        {
                            if let Ok(v) = serde_json::from_str(&p) {
                                sink.lock().unwrap_or_else(|e| e.into_inner()).push(v);
                            }
                        }
                    }
                }
            }
        });
        Ok(NwcClient { client, keys, wallet, inbox })
    }

    /// Waits for a notification of `kind` whose `notification` satisfies `pred`.
    pub async fn wait_notification(
        &self,
        kind: &str,
        timeout: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(i) = inbox
                    .iter()
                    .position(|v| v["notification_type"] == kind && pred(&v["notification"]))
                {
                    return Ok(inbox.remove(i)["notification"].clone());
                }
            }
            if Instant::now() >= deadline {
                bail!("no {kind} notification within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Sends a NIP-44 request and waits for its response.
    pub async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        use nostr::nips::nip44;
        let body = json!({"method": method, "params": params}).to_string();
        let content =
            nip44::encrypt(self.keys.secret_key(), &self.wallet, body, nip44::Version::V2)?;
        let event = EventBuilder::new(Kind::WalletConnectRequest, content)
            .tags([Tag::public_key(self.wallet), Tag::custom("encryption", ["nip44_v2"])])
            .finalize(&self.keys)?;
        let mut n = self.client.notifications();
        self.client.send_event(&event).await?;
        let deadline = Instant::now() + timeout;
        loop {
            let left =
                deadline.checked_duration_since(Instant::now()).context("no NWC response")?;
            match tokio::time::timeout(left, n.next()).await {
                Ok(Some(nostr_sdk::client::ClientNotification::Event { event: e, .. }))
                    if e.kind == Kind::WalletConnectResponse
                        && e.tags.event_ids().any(|id| id == event.id) =>
                {
                    let plain = nip44::decrypt(self.keys.secret_key(), &self.wallet, &e.content)?;
                    return Ok(serde_json::from_str(&plain)?);
                }
                Ok(Some(_)) => {}
                Ok(None) => bail!("notification stream closed"),
                Err(_) => bail!("no NWC response to {method} within {timeout:?}"),
            }
        }
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// NIP-98 `Authorization` header value.
pub fn nip98_header(keys: &Keys, url: &str, method: &str, body: &[u8]) -> Result<String> {
    let mut tags = vec![Tag::parse(["u", url])?, Tag::parse(["method", method])?];
    if !body.is_empty() {
        tags.push(Tag::parse(["payload", &hex::encode(Sha256::digest(body))])?);
    }
    let event = EventBuilder::new(Kind::HttpAuth, "").tags(tags).finalize(keys)?;
    Ok(format!("Nostr {}", B64.encode(event.as_json())))
}

/// Signs in to Allod at `base` as an owner; returns the session token.
pub async fn login(http: &reqwest::Client, base: &str, keys: &Keys) -> Result<String> {
    let c: Value = http.get(format!("{base}/api/auth/challenge")).send().await?.json().await?;
    let body = json!({"challenge": c["challenge"]}).to_string();
    let url = format!("{base}/api/auth/login");
    let resp = http
        .post(&url)
        .header("authorization", nip98_header(keys, &url, "POST", body.as_bytes())?)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    if status != StatusCode::OK {
        bail!("login failed: {status} {v}");
    }
    Ok(v["token"].as_str().context("token")?.to_string())
}
