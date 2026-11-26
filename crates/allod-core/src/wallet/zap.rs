//! Paying a zap the owner (or a throwaway key) signed: NIP-57 over LNURL, or NIP-B1 over
//! BOLT12. The signed event is checked against the payment before any money moves and is
//! kept in the transaction's metadata (`nostr`, `zap_request`, `zap_relays`).

use allod_lnurl::{server, zap};
use nostr::prelude::*;
use serde_json::{json, Map, Value};

use super::bip321::{self, Instruction};
use super::{store, PayRequest, Tx, Wallet, WalletError};

impl Wallet {
    /// Pays `req` as the zap described by `signed`: a kind 9734 request for a Lightning
    /// address or LNURL, a kind 9737 intent for a BOLT12 offer. `relays` is where a
    /// BOLT12 zap is published once paid.
    pub async fn pay_zap(
        &self,
        mut req: PayRequest,
        signed: &str,
        relays: &[String],
    ) -> Result<Tx, WalletError> {
        let amount = req
            .amount_msat
            .filter(|a| *a > 0)
            .ok_or_else(|| WalletError::BadRequest("enter an amount to zap".into()))?;
        let event = Event::from_json(signed)
            .map_err(|_| WalletError::BadRequest("the zap is not a Nostr event".into()))?;
        let mut meta: Map<String, Value> =
            req.metadata.as_deref().and_then(|m| serde_json::from_str(m).ok()).unwrap_or_default();
        meta.insert("nostr".into(), serde_json::to_value(&event).unwrap_or_default());
        meta.insert("zap_request".into(), json!(signed));
        let parsed = bip321::parse(&req.payment)?;
        match event.kind.as_u16() {
            9734 => {
                server::check_zap_request(signed, amount).map_err(WalletError::BadRequest)?;
                // A zap goes through LNURL even if the name also has a BIP-353 record.
                let Instruction::Lnurl(target) = parsed.instruction else {
                    return Err(WalletError::BadRequest(
                        "a NIP-57 zap needs a Lightning address or LNURL".into(),
                    ));
                };
                req.payer_note = None;
                req.metadata = Some(Value::Object(meta).to_string());
                self.pay_lnurl(req, &target, Some(signed)).await
            }
            zap::BOLT12_INTENT_KIND => {
                let intent = zap::check_intent(signed, amount).map_err(WalletError::BadRequest)?;
                let Instruction::Bolt12(offer) = parsed.instruction else {
                    return Err(WalletError::BadRequest(
                        "a BOLT12 zap needs a BOLT12 offer".into(),
                    ));
                };
                if !intent.offers.contains(&zap::canonical_offer(&offer)) {
                    return Err(WalletError::BadRequest(
                        "the recipient has not announced this offer for zaps".into(),
                    ));
                }
                let capabilities = self.node.capabilities();
                if !capabilities.bolt12 || !capabilities.payer_proof {
                    return Err(WalletError::Unsupported(
                        "this node cannot prove BOLT12 payments, so it cannot zap an offer".into(),
                    ));
                }
                let decoded = self.node.decode_offer(&offer).await.map_err(super::bad_input)?;
                {
                    let mut c = self.conn().await?;
                    if store::offer_by_id(&mut c, &decoded.offer_id).await?.is_some() {
                        return Err(WalletError::BadRequest(
                            "your own offer cannot be zapped: there is no payment to prove".into(),
                        ));
                    }
                }
                meta.insert("zap_relays".into(), json!(relays));
                req.payer_note = Some(zap::payer_note(&intent.event));
                req.metadata = Some(Value::Object(meta).to_string());
                self.pay_bolt12(&req, &offer, None).await
            }
            _ => Err(WalletError::BadRequest("not a zap request or zap intent".into())),
        }
    }

    /// The payer proof of a settled BOLT12 payment, asking the node for it when it was
    /// not stored. `None` when the payment is not settled or cannot be proven.
    pub async fn ensure_payer_proof(&self, public_id: &str) -> Result<Option<String>, WalletError> {
        let tx = self.transaction(public_id).await?;
        if tx.payer_proof.is_some() {
            return Ok(tx.payer_proof);
        }
        if tx.direction != "outgoing" || tx.payment_type != "bolt12" || tx.state != "SETTLED" {
            return Ok(None);
        }
        let (Some(id), Some(preimage), Some(invoice)) =
            (&tx.ln_payment_id, &tx.preimage, &tx.payment_request)
        else {
            return Ok(None);
        };
        let zap = tx.payer_note.as_deref().is_some_and(|n| n.starts_with(zap::BOLT12_NOTE_PREFIX));
        let proof = self.node.bolt12_payer_proof(id, preimage, invoice, zap).await?;
        let _g = self.write_lock.lock().await;
        let mut c = self.conn().await?;
        sqlx::query("UPDATE transactions SET payer_proof = $1 WHERE public_id = $2")
            .bind(&proof)
            .bind(public_id)
            .execute(&mut *c)
            .await?;
        Ok(Some(proof))
    }
}
