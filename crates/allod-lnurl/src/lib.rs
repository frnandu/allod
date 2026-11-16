//! LNURL-pay client (SPEC §1 item 6, §8.4 #14): resolve a Lightning address (LUD-16) or
//! an `lnurl1…` string (LUD-06) to pay parameters, then fetch an invoice for an amount,
//! with an optional comment (LUD-12).
//!
//! The caller must check the returned invoice (amount, and description hash when the
//! invoice carries one) with [`PayParams::check_invoice`] before paying it.

pub mod server;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
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
    /// Allow `http://` and local destinations (tests and regtest only).
    allow_http: bool,
}

/// Resolves at connection time, then removes every non-public address. Reqwest connects
/// to one of these returned addresses without resolving the name again, preventing a DNS
/// rebinding between validation and connection.
#[derive(Debug)]
struct PublicResolver {
    allow_local: bool,
}

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let allow_local = self.allow_local;
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| allow_local || is_public_ip(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether an address is globally reachable unicast. IPv6 forms that embed an IPv4
/// address (mapped, compatible, NAT64, 6to4) are judged by that address.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0))
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| {
                Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
            };
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            if seg[..6] == [0; 6] && !v6.is_loopback() && !v6.is_unspecified() {
                return is_public_ip(IpAddr::V4(embedded(seg[6], seg[7])));
            }
            if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return is_public_ip(IpAddr::V4(embedded(seg[6], seg[7])));
            }
            if seg[0] == 0x2002 {
                return is_public_ip(IpAddr::V4(embedded(seg[1], seg[2])));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)
                || (seg[0] == 0x2001 && seg[1] == 0)
                || (seg[0] == 0x100 && seg[1..4] == [0, 0, 0]))
        }
    }
}

fn checked_url(input: &str, allow_local: bool) -> Result<Url, LnurlError> {
    let url = Url::parse(input).map_err(|_| LnurlError::Service("invalid URL".into()))?;
    if url.scheme() != "https" && !(allow_local && url.scheme() == "http") {
        return Err(LnurlError::Service("URL must use https".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LnurlError::Service("URL must not carry credentials".into()));
    }
    let host = url.host_str().ok_or_else(|| LnurlError::Service("URL has no host".into()))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if !allow_local && !is_public_ip(ip) {
            return Err(LnurlError::Service("URL must use a public address".into()));
        }
    } else if !allow_local
        && (!bare.contains('.')
            || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
                .iter()
                .any(|suffix| bare.to_ascii_lowercase().ends_with(suffix)))
    {
        return Err(LnurlError::Service("URL must use a public host".into()));
    }
    Ok(url)
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
        let redirect_allow_local = allow_http;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if !attempt.previous().is_empty() {
                    return attempt.error("too many LNURL redirects");
                }
                match checked_url(attempt.url().as_str(), redirect_allow_local) {
                    Ok(_) => attempt.follow(),
                    Err(error) => attempt.error(error.to_string()),
                }
            }))
            // A proxy would resolve names itself, bypassing PublicResolver.
            .no_proxy()
            .dns_resolver(Arc::new(PublicResolver { allow_local: allow_http }))
            .build()
            .expect("http client");
        LnurlClient { http, allow_http }
    }

    /// Fetches the pay parameters of a Lightning address or LNURL.
    pub async fn resolve(&self, target: &str) -> Result<PayParams, LnurlError> {
        let url = checked_url(&to_url(target, self.allow_http)?, self.allow_http)?;
        let resp: PayResponse = self
            .http
            .get(url)
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
        let callback = checked_url(&callback, self.allow_http)?.to_string();
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
        let callback = checked_url(&p.callback, self.allow_http)?;
        let mut req = self.http.get(callback).query(&[("amount", amount_msat.to_string())]);
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
    fn production_urls_reject_ssrf_destinations_and_credentials() {
        for bad in [
            "https://127.0.0.1/x",
            "https://10.0.0.1/x",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://bitcoind:8332/x",
            "https://router.lan/x",
            "https://user:password@example.com/x",
            "http://example.com/x",
        ] {
            assert!(checked_url(bad, false).is_err(), "{bad} should be refused");
        }
        assert!(checked_url("https://8.8.8.8/x", false).is_ok());
        assert!(checked_url("https://example.com/x", false).is_ok());
        assert!(checked_url("http://127.0.0.1:8080/x", true).is_ok());
    }

    #[tokio::test]
    async fn production_resolver_refuses_local_addresses() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;

        let error = match (PublicResolver { allow_local: false })
            .resolve(reqwest::dns::Name::from_str("localhost").unwrap())
            .await
        {
            Ok(_) => panic!("localhost must not resolve"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("no public address"), "{error}");
    }

    #[tokio::test]
    async fn invoice_revalidates_callback_before_request() {
        let params = PayParams {
            target: "alice@example.com".into(),
            callback: "https://127.0.0.1/invoice".into(),
            min_sendable_msat: 1,
            max_sendable_msat: 1_000,
            metadata: "[]".into(),
            description: None,
            comment_allowed: 0,
            identifier: None,
            allows_nostr: false,
        };
        let error = LnurlClient::new(false).invoice(&params, 1, None).await.unwrap_err();
        assert!(error.to_string().contains("public address"), "{error}");
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
