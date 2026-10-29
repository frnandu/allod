//! `nwc-uri` and `nwc`: create a test connection on a regtest Allod and call it over
//! Nostr Wallet Connect (SPEC §2.3.9, `just nwc-uri` / `just nwc`).

use std::time::Duration;

use allod_e2e::{login, NwcClient, DEV_OWNER_SECRET_HEX};
use anyhow::{bail, Context, Result};
use clap::Args;
use nostr::prelude::*;
use serde_json::{json, Value};

#[derive(Debug, Args)]
pub struct UriArgs {
    /// Allod base URL.
    #[arg(long, default_value = "http://127.0.0.1:18029")]
    pub url: String,
    /// Owner secret key (hex); defaults to the public regtest test owner.
    #[arg(long, env = "DEV_OWNER_SECRET_HEX")]
    pub owner_secret: Option<String>,
    #[arg(long, default_value = "devtools")]
    pub name: String,
    /// Comma-separated scopes.
    #[arg(
        long,
        default_value = "pay_invoice,get_balance,make_invoice,lookup_invoice,list_transactions,notifications"
    )]
    pub scopes: String,
    /// Budget in sats (0 = none).
    #[arg(long, default_value_t = 100_000)]
    pub budget_sats: i64,
    #[arg(long)]
    pub isolated: bool,
}

pub async fn uri(a: UriArgs) -> Result<()> {
    let http = reqwest::Client::new();
    let base = a.url.trim_end_matches('/');
    let keys = Keys::parse(a.owner_secret.as_deref().unwrap_or(DEV_OWNER_SECRET_HEX))?;
    let token = login(&http, base, &keys).await?;
    let scopes: Vec<&str> = a.scopes.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let resp = http
        .post(format!("{base}/api/apps"))
        .bearer_auth(token)
        .json(&json!({
            "name": a.name, "scopes": scopes, "isolated": a.isolated,
            "max_amount_msat": a.budget_sats * 1000, "budget_renewal": "monthly",
        }))
        .send()
        .await?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    if !status.is_success() {
        bail!("{status}: {v}");
    }
    println!("{}", v["pairing_uri"].as_str().context("no pairing_uri")?);
    Ok(())
}

#[derive(Debug, Args)]
pub struct CallArgs {
    /// `nostr+walletconnect://` URI.
    #[arg(long, env = "NWC_URI")]
    pub uri: String,
    /// NIP-47 method, e.g. get_balance.
    pub method: String,
    /// Params as JSON.
    #[arg(default_value = "{}")]
    pub params: String,
    /// Seconds to wait for the response.
    #[arg(long, default_value_t = 60)]
    pub timeout: u64,
}

pub async fn call(a: CallArgs) -> Result<()> {
    let params: Value = serde_json::from_str(&a.params).context("params must be JSON")?;
    let client = NwcClient::from_uri(&a.uri).await?;
    let r = client.call(&a.method, params, Duration::from_secs(a.timeout)).await?;
    println!("{}", serde_json::to_string_pretty(&r)?);
    Ok(())
}
