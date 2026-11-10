//! Pays the recorded prism splits and keeps their state: one payout per split and
//! received payment. A failed payout is retried with backoff; the amount and the reason
//! stay on the row, so the owner sees how much never arrived.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;

use super::{now, DomainService, Payout};
use crate::wallet::PayRequest;
use crate::{Wallet, WalletEvent};

/// How often pending and failed payouts are looked at.
const SWEEP: Duration = Duration::from_secs(60);
/// Payouts dispatched per pass.
const DUE_LIMIT: i64 = 20;
/// "Paying" payouts checked per pass (left by a crash or a lost event).
const PAYING_LIMIT: i64 = 50;

/// The metadata key of a payout's outgoing payment: it makes the attempt findable after a
/// crash, before its transaction id was written back. The value is a string, so the search
/// pattern can end at its closing quote and never match a longer id.
pub(crate) const MARKER: &str = "prism_payout";

fn marker(payout_id: i64) -> String {
    serde_json::json!({ MARKER: payout_id.to_string() }).to_string()
}

impl DomainService {
    /// Starts the worker (called from [`DomainService::start`]).
    pub(crate) fn spawn_prism(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move { this.prism_worker().await });
    }

    async fn prism_worker(self: Arc<Self>) {
        // The wallet starts once the Lightning node answers; splits wait for it.
        let wallet = loop {
            if let Some(w) = self.wallet.get() {
                break w.clone();
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        };
        tracing::info!("prism splits started");
        let mut events = wallet.subscribe();
        let mut sweep = tokio::time::interval(SWEEP);
        sweep.tick().await;
        // Payments received while the daemon was down, then anything left over.
        self.catch_up().await;
        self.prism_sweep(&wallet).await;
        loop {
            tokio::select! {
                _ = sweep.tick() => self.prism_sweep(&wallet).await,
                _ = self.prism_wake.notified() => self.prism_sweep(&wallet).await,
                e = events.recv() => match e {
                    Ok(WalletEvent::TransactionUpdated { public_id, state }) if state == "SETTLED" => {
                        if self.prism_payment(&wallet, &public_id).await {
                            self.dispatch(&wallet).await;
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        tracing::warn!("prism splits lagged behind wallet events; catching up");
                        self.catch_up().await;
                        self.prism_sweep(&wallet).await;
                    }
                    Err(RecvError::Closed) => return,
                },
            }
        }
    }

    /// One pass: settle "paying" rows against their outgoing payment, then pay what is due.
    pub(crate) async fn prism_sweep(&self, wallet: &Arc<Wallet>) {
        self.reconcile_paying(wallet).await;
        self.dispatch(wallet).await;
    }

    /// Splits payments the daemon missed while it was down.
    pub(crate) async fn catch_up(&self) {
        match self.unsettled_splits(now()).await {
            Ok(list) => {
                for (tx_id, address_id, amount, settled_at) in list {
                    if let Err(e) = self.split_payment(address_id, tx_id, amount, settled_at).await
                    {
                        tracing::warn!(error = %e, "prism: cannot split a received payment");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "prism: cannot look for missed payments"),
        }
    }

    /// Records the splits of a settled payment. Answers whether anything was recorded.
    pub(crate) async fn prism_payment(&self, wallet: &Arc<Wallet>, public_id: &str) -> bool {
        let tx = match wallet.transaction(public_id).await {
            Ok(tx) => tx,
            Err(_) => return false,
        };
        if tx.direction != "incoming" || tx.state != "SETTLED" {
            return false;
        }
        let address =
            match self.address_of_payment(tx.metadata.as_deref(), tx.offer_id.as_deref()).await {
                Ok(Some(a)) => a,
                Ok(None) => return false,
                Err(e) => {
                    tracing::warn!(error = %e, "prism: cannot find the address of a payment");
                    return false;
                }
            };
        let settled_at = tx.settled_at.unwrap_or(tx.updated_at);
        match self.split_payment(address.id, tx.id, tx.amount_msat, settled_at).await {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!(error = %e, "prism: cannot record splits");
                false
            }
        }
    }

    /// Settles the rows a worker left in "paying": a crash or a restart must not pay twice
    /// and must not lose a payment that went through.
    async fn reconcile_paying(&self, wallet: &Arc<Wallet>) {
        let paying = match self.paying_payouts(PAYING_LIMIT).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "prism: cannot list paying payouts");
                return;
            }
        };
        for p in paying {
            let tx = match wallet.outgoing_by_marker(MARKER, &p.id.to_string()).await {
                Ok(tx) => tx,
                Err(e) => {
                    tracing::warn!(error = %e, "prism: cannot look for a payout payment");
                    continue;
                }
            };
            let t = now();
            match tx {
                None => {
                    // The outgoing transaction is written before the node is asked, so no
                    // payment was made: it is safe to try again.
                    let _ =
                        self.payout_failed(p.id, "the payment was not sent", p.attempts, t).await;
                }
                Some(tx) => match tx.state.as_str() {
                    "SETTLED" => {
                        let _ = self
                            .payout_paid(
                                p.id,
                                tx.amount_msat,
                                tx.fee_msat,
                                tx.payment_hash.as_deref(),
                                Some(tx.id),
                                t,
                            )
                            .await;
                    }
                    "FAILED" | "CANCELED" | "EXPIRED" => {
                        let reason =
                            tx.failure_reason.clone().unwrap_or_else(|| "payment failed".into());
                        let _ = self.payout_failed(p.id, &reason, p.attempts, t).await;
                    }
                    _ => {}
                },
            }
        }
    }

    /// Pays every due payout once, one at a time.
    async fn dispatch(&self, wallet: &Arc<Wallet>) {
        let due = match self.due_payouts(now(), DUE_LIMIT).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "prism: cannot list due payouts");
                return;
            }
        };
        for p in due {
            if !self.claim_payout(p.id, now()).await.unwrap_or(false) {
                continue;
            }
            self.pay_once(wallet, &p).await;
        }
    }

    /// One attempt at one split.
    async fn pay_once(&self, wallet: &Arc<Wallet>, p: &Payout) {
        // `claim_payout` already counted this attempt; `p` is the row before it.
        let attempts = p.attempts + 1;
        let app_id = match super::store::address(&self.pool, p.address_id).await {
            Ok(Some(a)) => a.app_id,
            Ok(None) => {
                // The address is gone: the split cannot mean anything any more.
                let _ = self
                    .payout_state(
                        p.id,
                        "CANCELED",
                        Some("the address was removed"),
                        attempts,
                        now(),
                    )
                    .await;
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "prism: cannot read the address of a payout");
                let _ = self
                    .payout_failed(p.id, "the address could not be read", attempts, now())
                    .await;
                return;
            }
        };
        let request = PayRequest {
            app_id,
            payment: p.destination.clone(),
            amount_msat: u64::try_from(p.amount_msat).ok(),
            max_fee_msat: None,
            payer_note: None,
            metadata: Some(marker(p.id)),
        };
        let result = wallet.pay(request).await;
        let tx = match wallet.outgoing_by_marker(MARKER, &p.id.to_string()).await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(error = %e, "prism: cannot read a payout's payment");
                None
            }
        };
        let t = now();
        let Some(tx) = tx else {
            // No outgoing transaction: nothing left this wallet.
            let error = match result {
                Ok(_) => "the payment was not recorded".to_string(),
                Err(e) => e.to_string(),
            };
            let _ = self.payout_failed(p.id, &error, attempts, t).await;
            return;
        };
        match tx.state.as_str() {
            "SETTLED" => {
                let _ = self
                    .payout_paid(
                        p.id,
                        tx.amount_msat,
                        tx.fee_msat,
                        tx.payment_hash.as_deref(),
                        Some(tx.id),
                        t,
                    )
                    .await;
                tracing::info!(
                    payout = p.id,
                    destination = %p.destination,
                    amount_msat = tx.amount_msat,
                    "prism split forwarded"
                );
            }
            "FAILED" | "CANCELED" | "EXPIRED" => {
                let reason = tx
                    .failure_reason
                    .clone()
                    .or_else(|| result.err().map(|e| e.to_string()))
                    .unwrap_or_else(|| "payment failed".into());
                let _ = self.payout_failed(p.id, &reason, attempts, t).await;
                tracing::warn!(payout = p.id, destination = %p.destination, %reason, "prism split failed");
            }
            // Still on its way: the wallet's reconciler and the next sweep settle it.
            _ => {}
        }
    }
}
