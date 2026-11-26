//! NIP-57 zap receipts (SPEC §12): when an invoice made for a zap (LNURL callback with
//! `nostr`) is paid, Allod signs the kind 9735 receipt with its zap key and sends it to
//! the relays the zap request names. The receipt's event id is kept in the transaction's
//! metadata (`zap_receipt`), so a restart or a relay outage only delays it: unsent
//! receipts of the last 3 days are retried every 10 minutes.
//!
//! Also publishes the NIP-B1 zap (kind 9736) of a paid BOLT12 zap this wallet sent: an
//! anonymous one by itself, a public one once its payer signed it.

use std::sync::Arc;
use std::time::Duration;

use allod_core::wallet::{Tx, TxFilter};
use allod_core::{AppService, Wallet, WalletEvent};
use allod_lnurl::{server, zap};
use nostr::prelude::*;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

const RETRY_EVERY: Duration = Duration::from_secs(600);
const RETRY_WITHIN_SECS: i64 = 3 * 86_400;

/// Runs forever.
pub async fn worker(wallet: Arc<Wallet>, apps: Arc<AppService>) {
    let secret = apps.zap_key().ok().map(|k| k.expose_secret_hex());
    let keys = match secret.as_ref().map(|s| Keys::parse(s.as_str())) {
        Some(Ok(k)) => k,
        _ => {
            tracing::warn!("zaps: no zap key; receipts are not published");
            return;
        }
    };
    let secret = secret.map(|s| s.to_string()).unwrap_or_default();
    let mut events = wallet.subscribe();
    let mut retry = tokio::time::interval(RETRY_EVERY);
    loop {
        tokio::select! {
            _ = retry.tick() => catch_up(&wallet, &keys, &secret).await,
            e = events.recv() => match e {
                Ok(WalletEvent::TransactionUpdated { public_id, state }) if state == "SETTLED" => {
                    if let Ok(tx) = wallet.transaction(&public_id).await {
                        send_receipt(&wallet, &keys, &tx).await;
                        send_anonymous(&wallet, &secret, &tx).await;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => catch_up(&wallet, &keys, &secret).await,
                Err(RecvError::Closed) => return,
            },
        }
    }
}

/// Paid zaps of the last days whose receipt was not sent yet.
async fn catch_up(wallet: &Wallet, keys: &Keys, secret: &str) {
    let filter = TxFilter { state: Some("SETTLED".into()), limit: 200, ..Default::default() };
    let Ok((txs, _)) = wallet.transactions(&filter).await else { return };
    let since = now() - RETRY_WITHIN_SECS;
    for tx in txs.iter().filter(|t| t.settled_at.unwrap_or(t.updated_at) >= since) {
        send_receipt(wallet, keys, tx).await;
        send_anonymous(wallet, secret, tx).await;
    }
}

/// Where the BOLT12 zap of an outgoing payment stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bolt12Zap {
    /// Not a BOLT12 zap this wallet sent.
    NotAZap,
    /// The payment has not settled.
    Waiting,
    /// Paid, but the node gave no payer proof (yet).
    NoProof,
    /// The payer has to sign this.
    Sign(zap::Template),
    /// No relay took it.
    NotAccepted,
    Published(String),
}

/// Publishes the zap of a paid BOLT12 zap: signed here when it is anonymous, else the
/// `signed` event of the payer (without one, answers what to sign).
pub async fn bolt12_zap(
    wallet: &Wallet,
    secret: &str,
    public_id: &str,
    signed: Option<&str>,
) -> Result<Bolt12Zap, String> {
    let tx = wallet.transaction(public_id).await.map_err(|e| e.to_string())?;
    let mut meta: Value =
        tx.metadata.as_deref().and_then(|m| serde_json::from_str(m).ok()).unwrap_or_default();
    let intent = meta
        .get("zap_request")
        .and_then(Value::as_str)
        .filter(|_| tx.direction == "outgoing" && tx.payment_type == "bolt12")
        .and_then(|raw| zap::check_intent(raw, tx.amount_msat.max(0) as u64).ok())
        .filter(|i| tx.payer_note.as_deref() == Some(zap::payer_note(&i.event).as_str()));
    let Some(intent) = intent else { return Ok(Bolt12Zap::NotAZap) };
    if let Some(id) = meta.get("zap_receipt").and_then(Value::as_str) {
        return Ok(Bolt12Zap::Published(id.to_string()));
    }
    if tx.state != "SETTLED" {
        return Ok(Bolt12Zap::Waiting);
    }
    let proof = match wallet.ensure_payer_proof(public_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(Bolt12Zap::NoProof),
        Err(e) => {
            tracing::warn!(tx = %public_id, error = %e, "zaps: no payer proof");
            return Ok(Bolt12Zap::NoProof);
        }
    };
    let anonymous = zap::anonymous_keys(secret, &intent.zap_id)?;
    let is_anonymous = anonymous.public_key() == intent.event.pubkey;
    let paid_at = tx.settled_at.unwrap_or(tx.updated_at).max(0) as u64;
    let want = zap::zap_template(&intent, &proof, !is_anonymous, paid_at);
    let event = match signed {
        _ if is_anonymous => want.sign(&anonymous)?,
        Some(s) => zap::check_signed_zap(s, &intent, &want)?,
        None => return Ok(Bolt12Zap::Sign(want)),
    };
    let relays: Vec<String> = meta
        .get("zap_relays")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
        .unwrap_or_default();
    let relays: Vec<String> = relays.into_iter().take(server::MAX_ZAP_RELAYS).collect();
    if relays.is_empty() || !publish(&relays, &event).await {
        return Ok(Bolt12Zap::NotAccepted);
    }
    meta["zap_receipt"] = Value::from(event.id.to_hex());
    wallet.set_metadata(public_id, &meta.to_string()).await.map_err(|e| e.to_string())?;
    tracing::info!(tx = %public_id, event = %event.id, "zaps: BOLT12 zap published");
    Ok(Bolt12Zap::Published(event.id.to_hex()))
}

/// Publishes an anonymous BOLT12 zap that is still missing; public ones wait for their
/// payer's signature.
async fn send_anonymous(wallet: &Wallet, secret: &str, tx: &Tx) {
    let unsent = tx.direction == "outgoing"
        && tx.state == "SETTLED"
        && tx.payer_note.as_deref().is_some_and(|n| n.starts_with(zap::BOLT12_NOTE_PREFIX))
        && !tx.metadata.as_deref().is_some_and(|m| m.contains("\"zap_receipt\""));
    if unsent {
        if let Err(e) = bolt12_zap(wallet, secret, &tx.public_id, None).await {
            tracing::warn!(tx = %tx.public_id, error = %e, "zaps: cannot publish the BOLT12 zap");
        }
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
    let client = crate::relay_egress::client(false);
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
