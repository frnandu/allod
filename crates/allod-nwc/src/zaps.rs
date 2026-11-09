//! NIP-57 zap receipts (SPEC §12): when an invoice made for a zap (LNURL callback with
//! `nostr`) is paid, Allod signs the kind 9735 receipt with its zap key and sends it to
//! the relays the zap request names. The receipt's event id is kept in the transaction's
//! metadata (`zap_receipt`), so a restart or a relay outage only delays it: unsent
//! receipts of the last 3 days are retried every 10 minutes.

use std::sync::Arc;
use std::time::Duration;

use allod_core::wallet::{Tx, TxFilter};
use allod_core::{AppService, Wallet, WalletEvent};
use allod_lnurl::server;
use nostr::prelude::*;
use nostr_sdk::prelude::*;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

const RETRY_EVERY: Duration = Duration::from_secs(600);
const RETRY_WITHIN_SECS: i64 = 3 * 86_400;

/// Runs forever.
pub async fn worker(wallet: Arc<Wallet>, apps: Arc<AppService>) {
    let keys = match apps.zap_key().map(|k| Keys::parse(k.expose_secret_hex().as_str())) {
        Ok(Ok(k)) => k,
        _ => {
            tracing::warn!("zaps: no zap key; receipts are not published");
            return;
        }
    };
    let mut events = wallet.subscribe();
    let mut retry = tokio::time::interval(RETRY_EVERY);
    loop {
        tokio::select! {
            _ = retry.tick() => catch_up(&wallet, &keys).await,
            e = events.recv() => match e {
                Ok(WalletEvent::TransactionUpdated { public_id, state }) if state == "SETTLED" => {
                    if let Ok(tx) = wallet.transaction(&public_id).await {
                        send_receipt(&wallet, &keys, &tx).await;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => catch_up(&wallet, &keys).await,
                Err(RecvError::Closed) => return,
            },
        }
    }
}

/// Paid zaps of the last days whose receipt was not sent yet.
async fn catch_up(wallet: &Wallet, keys: &Keys) {
    let filter = TxFilter {
        direction: Some("incoming".into()),
        state: Some("SETTLED".into()),
        limit: 200,
        ..Default::default()
    };
    let Ok((txs, _)) = wallet.transactions(&filter).await else { return };
    let since = now() - RETRY_WITHIN_SECS;
    for tx in txs.iter().filter(|t| t.settled_at.unwrap_or(t.updated_at) >= since) {
        send_receipt(wallet, keys, tx).await;
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// The zap request of a paid zap that has no receipt yet.
fn pending_zap(tx: &Tx) -> Option<(Value, String)> {
    if tx.direction != "incoming" || tx.state != "SETTLED" {
        return None;
    }
    let meta: Value = serde_json::from_str(tx.metadata.as_deref()?).ok()?;
    if meta.get("zap_receipt").is_some() {
        return None;
    }
    let raw = meta.get("zap_request")?.as_str()?.to_string();
    Some((meta, raw))
}

async fn send_receipt(wallet: &Wallet, keys: &Keys, tx: &Tx) {
    let Some((mut meta, raw)) = pending_zap(tx) else { return };
    let Some(bolt11) = tx.payment_request.as_deref() else { return };
    let paid_at = tx.settled_at.unwrap_or(tx.updated_at).max(0) as u64;
    let receipt = match server::zap_receipt(keys, &raw, bolt11, tx.preimage.as_deref(), paid_at) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(tx = %tx.public_id, error = %e, "zaps: cannot build the receipt");
            return;
        }
    };
    let relays = server::zap_relays(&raw);
    if relays.is_empty() || !publish(&relays, &receipt).await {
        tracing::info!(tx = %tx.public_id, "zaps: receipt not accepted by any relay yet");
        return;
    }
    meta["zap_receipt"] = Value::from(receipt.id.to_hex());
    if let Err(e) = wallet.set_metadata(&tx.public_id, &meta.to_string()).await {
        tracing::warn!(tx = %tx.public_id, error = %e, "zaps: cannot record the receipt");
    } else {
        tracing::info!(tx = %tx.public_id, event = %receipt.id, "zaps: receipt published");
    }
}

/// Sends to the zap request's relays with a short-lived client; true when one accepted.
async fn publish(relays: &[String], event: &Event) -> bool {
    let client = Client::builder().build();
    for r in relays {
        let _ = client.add_relay(r.as_str()).await;
    }
    client.connect().await;
    let _ = client.try_connect().timeout(Duration::from_secs(5)).await;
    let ok =
        client.send_event(event).to(relays.to_vec()).await.is_ok_and(|o| !o.success.is_empty());
    client.disconnect().await;
    ok
}
