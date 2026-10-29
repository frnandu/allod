//! LNURL-pay client (SPEC §1 item 6, §8.4 #14): resolve a Lightning address (LUD-16) or
//! an `lnurl1…` string (LUD-06) to pay parameters, then fetch an invoice for an amount,
//! with an optional comment (LUD-12).
//!
//! The caller must check the returned invoice (amount, and description hash when the
//! invoice carries one) with [`PayParams::check_invoice`] before paying it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LnurlError {
    #[error("not a Lightning address or LNURL")]
    Invalid,
    #[error("the LNURL service failed: {0}")]
    Service(String),
    #[error("amount must be between {min} and {max} msat")]
    Amount { min: u64, max: u64 },
    #[error("the comment is longer than the {0} characters this service accepts")]
    CommentTooLong(u64),
    #[error("the invoice from the LNURL service does not match the request: {0}")]
    BadInvoice(String),
}

/// What a payment target resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PayParams {
    /// The Lightning address or LNURL as given.
    pub target: String,
    pub callback: String,
    pub min_sendable_msat: u64,
    pub max_sendable_msat: u64,
    /// Raw LUD-06 metadata string (its SHA-256 is the invoice's description hash).
    pub metadata: String,
    /// `text/plain` entry of the metadata.
    pub description: Option<String>,
    /// LUD-12: maximum comment length, 0 when comments are not accepted.
    pub comment_allowed: u64,
    /// LUD-16 identifier (`text/identifier` or `text/email` metadata, or the address).
    pub identifier: Option<String>,
    /// NIP-57: the service accepts zaps.
    pub allows_nostr: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayResponse {
    tag: Option<String>,
    callback: Option<String>,
    min_sendable: Option<u64>,
    max_sendable: Option<u64>,
    metadata: Option<String>,
    #[serde(default)]
    comment_allowed: u64,
    #[serde(default)]
    allows_nostr: bool,
    status: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct InvoiceResponse {
    pr: Option<String>,
    status: Option<String>,
    reason: Option<String>,
}

/// LNURL-pay client.
#[derive(Clone)]
pub struct LnurlClient {
    http: reqwest::Client,
    /// Allow `http://` (tests and regtest only).
    allow_http: bool,
}

/// Is `s` something [`LnurlClient::resolve`] understands?
pub fn is_lnurl_target(s: &str) -> bool {
    to_url(s, true).is_ok()
}

/// The HTTPS URL behind a Lightning address or `lnurl1…` string.
fn to_url(input: &str, allow_http: bool) -> Result<String, LnurlError> {
    let s = input.trim();
    let s = s.strip_prefix('₿').unwrap_or(s);
    let s = s.strip_prefix("lightning:").or_else(|| s.strip_prefix("LIGHTNING:")).unwrap_or(s);
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("lnurl1") {
        let (hrp, data) = bech32::decode(&lower).map_err(|_| LnurlError::Invalid)?;
        if hrp.as_str() != "lnurl" {
            return Err(LnurlError::Invalid);
        }
        let url = String::from_utf8(data).map_err(|_| LnurlError::Invalid)?;
        let ok = url.starts_with("https://") || (allow_http && url.starts_with("http://"));
        return if ok { Ok(url) } else { Err(LnurlError::Invalid) };
    }
    // LUD-17 schemes.
    for (scheme, proto) in [("lnurlp://", "https://")] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            return Ok(format!("{proto}{rest}"));
        }
    }
    let (user, domain) = lower.split_once('@').ok_or(LnurlError::Invalid)?;
    let user_ok =
        !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c));
    let domain_ok = domain.contains('.') || domain.contains(':') || domain == "localhost";
    let domain_ok =
        domain_ok && domain.chars().all(|c| c.is_ascii_alphanumeric() || "-.:".contains(c));
    if !user_ok || !domain_ok {
        return Err(LnurlError::Invalid);
    }
    let scheme = if allow_http && (domain.starts_with("localhost") || domain.starts_with("127.")) {
        "http"
    } else {
        "https"
    };
    Ok(format!("{scheme}://{domain}/.well-known/lnurlp/{user}"))
}

fn metadata_entry(metadata: &str, kind: &str) -> Option<String> {
    let entries: Vec<Vec<serde_json::Value>> = serde_json::from_str(metadata).ok()?;
    entries
        .iter()
        .find(|e| e.first().and_then(|k| k.as_str()) == Some(kind))
        .and_then(|e| e.get(1))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

impl LnurlClient {
    pub fn new(allow_http: bool) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .expect("http client");
        LnurlClient { http, allow_http }
    }

    /// Fetches the pay parameters of a Lightning address or LNURL.
    pub async fn resolve(&self, target: &str) -> Result<PayParams, LnurlError> {
        let url = to_url(target, self.allow_http)?;
        let resp: PayResponse = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| LnurlError::Service(e.to_string()))?
            .json()
            .await
            .map_err(|e| LnurlError::Service(format!("bad response: {e}")))?;
        if resp.status.as_deref() == Some("ERROR") {
            return Err(LnurlError::Service(resp.reason.unwrap_or_default()));
        }
        if resp.tag.as_deref() != Some("payRequest") {
            return Err(LnurlError::Service("not an LNURL-pay service".into()));
        }
        let callback = resp.callback.ok_or_else(|| LnurlError::Service("no callback".into()))?;
        let ok = callback.starts_with("https://")
            || (self.allow_http && callback.starts_with("http://"));
        if !ok {
            return Err(LnurlError::Service("callback must use https".into()));
        }
        let metadata = resp.metadata.unwrap_or_else(|| "[]".into());
        let min = resp.min_sendable.unwrap_or(1).max(1);
        let max = resp.max_sendable.unwrap_or(min).max(min);
        let identifier = metadata_entry(&metadata, "text/identifier")
            .or_else(|| metadata_entry(&metadata, "text/email"))
            .or_else(|| {
                target
                    .contains('@')
                    .then(|| target.trim().trim_start_matches('₿').to_ascii_lowercase())
            });
        Ok(PayParams {
            target: target.trim().to_string(),
            callback,
            min_sendable_msat: min,
            max_sendable_msat: max,
            description: metadata_entry(&metadata, "text/plain"),
            metadata,
            comment_allowed: resp.comment_allowed,
            identifier,
            allows_nostr: resp.allows_nostr,
        })
    }

    /// Asks the service for an invoice of `amount_msat`.
    pub async fn invoice(
        &self,
        p: &PayParams,
        amount_msat: u64,
        comment: Option<&str>,
    ) -> Result<String, LnurlError> {
        if amount_msat < p.min_sendable_msat || amount_msat > p.max_sendable_msat {
            return Err(LnurlError::Amount { min: p.min_sendable_msat, max: p.max_sendable_msat });
        }
        let mut req = self.http.get(&p.callback).query(&[("amount", amount_msat.to_string())]);
        if let Some(c) = comment.filter(|c| !c.is_empty()) {
            if c.chars().count() as u64 > p.comment_allowed {
                return Err(LnurlError::CommentTooLong(p.comment_allowed));
            }
            req = req.query(&[("comment", c)]);
        }
        let resp: InvoiceResponse = req
            .send()
            .await
            .map_err(|e| LnurlError::Service(e.to_string()))?
            .json()
            .await
            .map_err(|e| LnurlError::Service(format!("bad response: {e}")))?;
        if resp.status.as_deref() == Some("ERROR") {
            return Err(LnurlError::Service(resp.reason.unwrap_or_default()));
        }
        resp.pr.ok_or_else(|| LnurlError::Service("no invoice in the response".into()))
    }
}

impl PayParams {
    /// LUD-06: the invoice must be for the requested amount, and its description hash
    /// (when present) must be the SHA-256 of the metadata.
    pub fn check_invoice(
        &self,
        amount_msat: u64,
        invoice_amount_msat: Option<u64>,
        invoice_description_hash: Option<&str>,
    ) -> Result<(), LnurlError> {
        if invoice_amount_msat != Some(amount_msat) {
            return Err(LnurlError::BadInvoice(format!(
                "amount {invoice_amount_msat:?} instead of {amount_msat}"
            )));
        }
        if let Some(h) = invoice_description_hash {
            let want = hex::encode(Sha256::digest(self.metadata.as_bytes()));
            if !h.eq_ignore_ascii_case(&want) {
                return Err(LnurlError::BadInvoice(
                    "description hash does not match the metadata".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(
            to_url("Alice@Example.com", false).unwrap(),
            "https://example.com/.well-known/lnurlp/alice"
        );
        assert_eq!(
            to_url("₿bob@example.com", false).unwrap(),
            "https://example.com/.well-known/lnurlp/bob"
        );
        assert!(to_url("not an address", false).is_err());
        assert!(to_url("a@b", false).is_err());
        assert_eq!(
            to_url("x@127.0.0.1:9", true).unwrap(),
            "http://127.0.0.1:9/.well-known/lnurlp/x"
        );
        assert_eq!(
            to_url("x@127.0.0.1:9", false).unwrap(),
            "https://127.0.0.1:9/.well-known/lnurlp/x"
        );
        // LUD-01 example
        let lnurl = "LNURL1DP68GURN8GHJ7UM9WFMXJCM99E3K7MF0V9CXJ0M385EKVCENXC6R2C35XVUKXEFCV5MKVV34X5EKZD3EV56NYD3HXQURZEPEXEJXXEPNXSCRVWFNV9NXZCN9XQ6XYEFHVGCXXCMYXYMNSERXFQ5FNS";
        assert_eq!(
            to_url(lnurl, false).unwrap(),
            "https://service.com/api?q=3fc3645b439ce8e7f2553a69e5267081d96dcd340693afabe04be7b0ccd178df"
        );
    }

    #[test]
    fn invoice_checks() {
        let p = PayParams {
            target: "a@b.c".into(),
            callback: "https://b.c/cb".into(),
            min_sendable_msat: 1000,
            max_sendable_msat: 10_000,
            metadata: r#"[["text/plain","hi"]]"#.into(),
            description: Some("hi".into()),
            comment_allowed: 0,
            identifier: None,
            allows_nostr: false,
        };
        let h = hex::encode(Sha256::digest(p.metadata.as_bytes()));
        assert!(p.check_invoice(5000, Some(5000), Some(&h)).is_ok());
        assert!(p.check_invoice(5000, Some(5000), None).is_ok());
        assert!(p.check_invoice(5000, Some(6000), Some(&h)).is_err());
        assert!(p.check_invoice(5000, Some(5000), Some(&"00".repeat(32))).is_err());
    }
}
