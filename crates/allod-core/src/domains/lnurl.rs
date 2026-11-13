//! LNURL-pay for Lightning addresses on served domains (SPEC §12): who an address pays
//! (Main or an account), its payRequest, the callback's invoice and LUD-21 verify. The
//! HTTP routes are in `allod-api`; zap receipts are published by `allod-nwc`.

use std::collections::HashMap;

use allod_lnurl::server;
use serde_json::{json, Value};

use super::{store, DomainService};
use crate::wallet::InvoiceRequest;

/// How long an LNURL invoice stays payable.
const INVOICE_EXPIRY_SECS: u32 = 3_600;

/// An address that answers: `username@domain`, paying into `app_id` (`None`: Main).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LnurlTarget {
    /// The `ln_addresses` row, for the address's prism splits.
    pub address_id: i64,
    pub app_id: Option<i64>,
    pub username: String,
    pub domain: String,
}

impl LnurlTarget {
    pub fn address(&self) -> String {
        format!("{}@{}", self.username, self.domain)
    }

    fn metadata(&self) -> String {
        server::metadata(&self.address(), &format!("Sats for {}", self.address()))
    }
}

/// An LNURL failure: the HTTP status and the reason in the LNURL error body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LnurlFailure {
    pub status: u16,
    pub reason: String,
}

impl LnurlFailure {
    fn new(status: u16, reason: impl Into<String>) -> Self {
        LnurlFailure { status, reason: reason.into() }
    }
    pub fn body(&self) -> Value {
        server::error(&self.reason)
    }
}

/// The domain of a `Host` header: lowercase, without port or trailing dot.
fn host_domain(host: &str) -> String {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    match h.rsplit_once(':') {
        Some((d, p)) if p.chars().all(|c| c.is_ascii_digit()) => d.to_string(),
        _ => h,
    }
}

impl DomainService {
    /// Who `username` at `host` pays, if that address exists on a served domain.
    pub async fn lnurl_target(&self, host: &str, username: &str) -> Option<LnurlTarget> {
        let d = store::domain_by_name(&self.pool, &host_domain(host)).await.ok()??;
        if !d.served() {
            return None;
        }
        let username = username.to_ascii_lowercase();
        let a = store::addresses(&self.pool, Some(d.id))
            .await
            .ok()?
            .into_iter()
            .find(|a| a.username == username && a.lnurl)?;
        Some(LnurlTarget { address_id: a.id, app_id: a.app_id, username, domain: d.name })
    }

    fn base_url(&self, domain: &str) -> String {
        let scheme = if self.cfg.lnurl_http { "http" } else { "https" };
        format!("{scheme}://{domain}")
    }

    /// The payRequest at `/.well-known/lnurlp/{username}` (LUD-16).
    pub async fn lnurl_pay_request(
        &self,
        host: &str,
        username: &str,
    ) -> Result<Value, LnurlFailure> {
        let t = self
            .lnurl_target(host, username)
            .await
            .ok_or_else(|| LnurlFailure::new(404, "no such Lightning address"))?;
        let callback = format!("{}/lnurlp/{}/callback", self.base_url(&t.domain), t.username);
        let zap = self.apps.get().and_then(|a| a.zap_key().ok()).map(|k| k.public_hex);
        Ok(server::pay_request(&callback, &t.metadata(), zap.as_deref()))
    }

    /// The callback: checks the request and answers `{pr, routes, verify}` with an
    /// invoice owned by the address's account.
    pub async fn lnurl_callback(
        &self,
        host: &str,
        username: &str,
        q: &HashMap<String, String>,
    ) -> Result<Value, LnurlFailure> {
        let t = self
            .lnurl_target(host, username)
            .await
            .ok_or_else(|| LnurlFailure::new(404, "no such Lightning address"))?;
        let c = server::parse_callback(q).map_err(|e| LnurlFailure::new(400, e))?;
        let wallet =
            self.wallet.get().ok_or_else(|| LnurlFailure::new(503, "the wallet is starting"))?;
        let metadata = t.metadata();
        // What the invoice records, so the prism worker knows which address was paid.
        let mut record = c.record();
        if let Some(m) = record.as_object_mut() {
            m.insert("lightning_address".into(), json!(t.address()));
            m.insert("ln_address_id".into(), json!(t.address_id));
        }
        let tx = wallet
            .create_invoice(InvoiceRequest {
                app_id: t.app_id,
                ln_address_id: Some(t.address_id),
                amount_msat: Some(c.amount_msat),
                description: c.note().unwrap_or_else(|| format!("Payment to {}", t.address())),
                description_hash: Some(c.description_hash(&metadata)),
                expiry_secs: Some(INVOICE_EXPIRY_SECS),
                metadata: Some(record.to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| {
                tracing::warn!(address = %t.address(), error = %e, "LNURL: no invoice");
                LnurlFailure::new(500, "could not create an invoice")
            })?;
        let pr = tx.payment_request.unwrap_or_default();
        let hash = tx.payment_hash.unwrap_or_default();
        Ok(json!({
            "pr": pr,
            "routes": [],
            "verify": format!("{}/lnurlp/{}/verify/{hash}", self.base_url(&t.domain), t.username),
        }))
    }

    /// LUD-21: whether the address's invoice with `payment_hash` is paid.
    pub async fn lnurl_verify(
        &self,
        host: &str,
        username: &str,
        payment_hash: &str,
    ) -> Result<Value, LnurlFailure> {
        let not_found = || LnurlFailure::new(404, "Not found");
        let t = self.lnurl_target(host, username).await.ok_or_else(not_found)?;
        let wallet =
            self.wallet.get().ok_or_else(|| LnurlFailure::new(503, "the wallet is starting"))?;
        let tx = wallet
            .incoming_by_hash(payment_hash)
            .await
            .ok()
            .flatten()
            .filter(|tx| tx.app_id == t.app_id && tx.payment_type == "bolt11")
            .ok_or_else(not_found)?;
        let settled = tx.state == "SETTLED";
        Ok(json!({
            "status": "OK",
            "settled": settled,
            "preimage": if settled { tx.preimage.map(Value::from).unwrap_or(Value::Null) } else { Value::Null },
            "pr": tx.payment_request,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::host_domain;

    #[test]
    fn host_domains() {
        assert_eq!(host_domain("Pay.Example.com"), "pay.example.com");
        assert_eq!(host_domain("pay.example.com:443"), "pay.example.com");
        assert_eq!(host_domain("pay.example.com."), "pay.example.com");
    }
}
