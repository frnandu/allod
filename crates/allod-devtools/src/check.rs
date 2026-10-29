//! `allod-devtools check`: the checks behind `just doctor`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use allod_ln::{LdkServerBackend, LdkServerConfig};
use anyhow::{anyhow, bail, Context, Result};
use clap::Args;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// bitcoind JSON-RPC URL, e.g. http://bitcoind:18443
    #[arg(long, env = "BITCOIND_RPC_URL")]
    bitcoind_url: Option<String>,
    #[arg(long, env = "BITCOIND_RPC_USER", default_value = "")]
    bitcoind_user: String,
    #[arg(long, env = "BITCOIND_RPC_PASS", default_value = "")]
    bitcoind_pass: String,
    /// ldk-server node: NAME=HOST:PORT,CERT_FILE,MACAROON_FILE (repeatable).
    #[arg(long = "ldk", value_parser = parse_ldk)]
    ldk: Vec<LdkNode>,
    /// Minimum usable channels per node: NAME=N (repeatable).
    #[arg(long = "min-channels", value_parser = parse_min_channels)]
    min_channels: Vec<(String, usize)>,
    /// Maximum blocks an ldk-server node may lag behind bitcoind.
    #[arg(long, default_value_t = 2)]
    max_tip_lag: u64,
    /// Nostr relay URL (repeatable).
    #[arg(long = "relay")]
    relays: Vec<String>,
    /// Allod base URL, e.g. http://allod:8029
    #[arg(long, env = "ALLOD_URL")]
    allod_url: Option<String>,
    /// Require Allod to report ldk-server as healthy.
    #[arg(long)]
    allod_requires_ldk: bool,
    /// Keep retrying failed checks for this many seconds.
    #[arg(long, default_value_t = 0)]
    wait: u64,
}

#[derive(Debug, Clone)]
struct LdkNode {
    name: String,
    address: String,
    cert: PathBuf,
    macaroon: PathBuf,
}

fn parse_ldk(s: &str) -> Result<LdkNode, String> {
    let (name, rest) = s.split_once('=').ok_or("expected NAME=HOST:PORT,CERT,MACAROON")?;
    let parts: Vec<&str> = rest.split(',').collect();
    let [address, cert, macaroon] = parts.as_slice() else {
        return Err("expected NAME=HOST:PORT,CERT,MACAROON".into());
    };
    Ok(LdkNode {
        name: name.into(),
        address: (*address).into(),
        cert: cert.into(),
        macaroon: macaroon.into(),
    })
}

fn parse_min_channels(s: &str) -> Result<(String, usize), String> {
    let (name, n) = s.split_once('=').ok_or("expected NAME=N")?;
    Ok((name.into(), n.parse().map_err(|e| format!("{e}"))?))
}

struct Outcome {
    name: String,
    result: Result<String>,
}

pub async fn run(args: CheckArgs) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(args.wait);
    loop {
        let outcomes = run_once(&args).await;
        let failed = outcomes.iter().filter(|o| o.result.is_err()).count();
        if failed == 0 || Instant::now() >= deadline {
            for o in &outcomes {
                match &o.result {
                    Ok(detail) => println!("[ ok ] {:<22} {detail}", o.name),
                    Err(e) => println!("[FAIL] {:<22} {e:#}", o.name),
                }
            }
            if failed > 0 {
                bail!("{failed} check(s) failed");
            }
            println!("all {} checks passed", outcomes.len());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn run_once(args: &CheckArgs) -> Vec<Outcome> {
    let mut out = Vec::new();
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("client");

    let mut tip: Option<u64> = None;
    if let Some(url) = &args.bitcoind_url {
        let result = bitcoind_info(&http, url, &args.bitcoind_user, &args.bitcoind_pass).await;
        if let Ok(info) = &result {
            tip = info["blocks"].as_u64();
        }
        out.push(Outcome {
            name: "bitcoind".into(),
            result: result.map(|i| {
                format!(
                    "chain={} blocks={} ibd={} pruned={}",
                    i["chain"].as_str().unwrap_or("?"),
                    i["blocks"],
                    i["initialblockdownload"],
                    i["pruned"]
                )
            }),
        });
    }

    for node in &args.ldk {
        let min = args.min_channels.iter().find(|(n, _)| n == &node.name).map(|(_, m)| *m);
        out.push(Outcome {
            name: format!("ldk-server {}", node.name),
            result: check_ldk(node, tip, args.max_tip_lag, min).await,
        });
    }

    for relay in &args.relays {
        out.push(Outcome { name: format!("relay {relay}"), result: check_relay(relay).await });
    }

    if let Some(url) = &args.allod_url {
        out.push(Outcome {
            name: "allod".into(),
            result: check_allod(&http, url, args.allod_requires_ldk).await,
        });
    }
    out
}

async fn bitcoind_info(http: &reqwest::Client, url: &str, user: &str, pass: &str) -> Result<Value> {
    let body =
        json!({"jsonrpc": "1.0", "id": "doctor", "method": "getblockchaininfo", "params": []});
    let resp: Value =
        http.post(url).basic_auth(user, Some(pass)).json(&body).send().await?.json().await?;
    if !resp["error"].is_null() {
        bail!("rpc error: {}", resp["error"]);
    }
    Ok(resp["result"].clone())
}

async fn check_ldk(
    node: &LdkNode,
    tip: Option<u64>,
    max_lag: u64,
    min_channels: Option<usize>,
) -> Result<String> {
    let config = LdkServerConfig::from_files(node.address.clone(), &node.cert, &node.macaroon)?;
    let backend = LdkServerBackend::new(&config)?;
    let info = backend.node_info().await?;
    let height = u64::from(info.summary.block_height);
    if let Some(tip) = tip {
        if tip.saturating_sub(height) > max_lag {
            bail!("node at height {height}, bitcoind at {tip}");
        }
    }
    let channels = backend.channel_counts().await?;
    if let Some(min) = min_channels {
        if channels.usable < min {
            bail!("{} usable channel(s), expected at least {min}", channels.usable);
        }
    }
    let pubkey = info.summary.pubkey;
    Ok(format!(
        "{}… height={height} channels={}/{} usable",
        pubkey.get(..16).unwrap_or(&pubkey),
        channels.usable,
        channels.total
    ))
}

async fn check_relay(url: &str) -> Result<String> {
    let start = Instant::now();
    let fut = async {
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.context("connect")?;
        let req = json!(["REQ", "doctor", {"kinds": [13194], "limit": 1}]).to_string();
        ws.send(Message::Text(req.into())).await?;
        while let Some(msg) = ws.next().await {
            if let Message::Text(text) = msg? {
                let v: Value = serde_json::from_str(&text)?;
                if v[0] == "EOSE" {
                    let _ = ws.close(None).await;
                    return Ok(());
                }
            }
        }
        Err(anyhow!("connection closed before EOSE"))
    };
    tokio::time::timeout(Duration::from_secs(10), fut).await.context("timeout")??;
    Ok(format!("EOSE in {} ms", start.elapsed().as_millis()))
}

async fn check_allod(http: &reqwest::Client, base: &str, requires_ldk: bool) -> Result<String> {
    let url = format!("{}/api/health", base.trim_end_matches('/'));
    let health: allod_types::HealthResponse = http.get(&url).send().await?.json().await?;
    if health.status == allod_types::HealthStatus::Down {
        bail!("status down: {:?}", health.db.detail);
    }
    if requires_ldk && !health.ldk_server.ok {
        bail!("ldk_server not ok: {}", health.ldk_server.detail.unwrap_or_default());
    }
    Ok(format!(
        "status={:?} db={} ldk_server={}",
        health.status,
        health.db.ok,
        health.node.map(|n| format!("height {}", n.block_height)).unwrap_or_else(|| "n/a".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_spec() {
        let n = parse_ldk("alice=ldk:3536,/s/tls.crt,/s/admin.macaroon").unwrap();
        assert_eq!(n.name, "alice");
        assert_eq!(n.address, "ldk:3536");
        assert!(parse_ldk("alice=ldk:3536").is_err());
        assert_eq!(parse_min_channels("bob=2").unwrap(), ("bob".into(), 2));
    }
}
