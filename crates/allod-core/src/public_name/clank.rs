//! clank.money: paid BIP-353 names (`₿name@clank.money`) on a DNSSEC-signed domain run
//! by a third party (SPEC §12.4). The owner picks the name; registration is paid over
//! L402 and returns a management token that can later point the name at another
//! `bitcoin:` URI.

use std::time::Duration;

use serde_json::{json, Value};

/// The domain clank.money issues names under.
pub const DOMAIN: &str = "clank.money";

#[derive(Debug, thiserror::Error)]
pub enum ClankError {
    #[error("{0}@clank.money is already taken")]
    Taken(String),
    #[error("clank.money refused the request: {0}")]
    Refused(String),
    #[error("cannot reach clank.money: {0}")]
    Net(String),
    #[error("unexpected answer from clank.money: {0}")]
    Format(String),
}

/// An L402 payment request: pay `invoice`, then send `macaroon:preimage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    pub macaroon: String,
    pub invoice: String,
    pub amount_sats: Option<u64>,
}

/// What a registration attempt led to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Payment needed.
    Pay(Challenge),
    /// Registered. `dns_pending`: paid, but the record is not published yet (202).
    Done { token: String, dns_pending: bool },
}

#[derive(Debug, Clone)]
pub struct Clank {
    http: reqwest::Client,
    base: String,
}

/// clank.money's username rule: 3–32 of `a-z 0-9 -`, no leading or trailing hyphen.
pub fn valid_username(u: &str) -> bool {
    (3..=32).contains(&u.len())
        && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !u.starts_with('-')
        && !u.ends_with('-')
}

fn text(v: &Value) -> String {
    v.to_string().chars().take(300).collect()
}

/// A field anywhere in the first two levels of the answer (top level or one wrapper).
fn find<'a>(body: &'a Value, key: &str) -> Option<&'a Value> {
    body.get(key).or_else(|| body.as_object()?.values().find_map(|v| v.get(key)))
}

fn find_str(body: &Value, key: &str) -> Option<String> {
    find(body, key).and_then(Value::as_str).map(str::to_string)
}

/// `L402 macaroon="…", invoice="…"` (also the older `LSAT` scheme).
fn parse_www_authenticate(h: &str) -> Option<(String, String)> {
    let rest = h.trim().strip_prefix("L402").or_else(|| h.trim().strip_prefix("LSAT"))?;
    let mut mac = None;
    let mut inv = None;
    for part in rest.split(',') {
        let (k, v) = part.trim().split_once('=')?;
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "macaroon" => mac = Some(v),
            "invoice" => inv = Some(v),
            _ => {}
        }
    }
    Some((mac?, inv?))
}

impl Clank {
    /// `base` is `https://clank.money` (a mock in tests).
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Clank { http, base: base.trim_end_matches('/').to_string() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/registrations{path}", self.base)
    }

    /// Whether `username` looks free: `Some(true)` on 404, `Some(false)` when it exists,
    /// `None` when clank.money does not say.
    pub async fn available(&self, username: &str) -> Option<bool> {
        let res = self.http.get(self.url(&format!("/{username}"))).send().await.ok()?;
        match res.status().as_u16() {
            404 => Some(true),
            200 => Some(false),
            _ => None,
        }
    }

    /// `POST /api/v1/registrations`. Without `l402` (`macaroon:preimage`) it asks for
    /// the price; with it, it registers.
    pub async fn register(
        &self,
        username: &str,
        bip321_uri: &str,
        l402: Option<&str>,
    ) -> Result<Step, ClankError> {
        let mut req = self
            .http
            .post(self.url(""))
            .json(&json!({"username": username, "bip321Uri": bip321_uri}));
        if let Some(cred) = l402 {
            req = req.header("Authorization", format!("L402 {cred}"));
        }
        let res = req.send().await.map_err(|e| ClankError::Net(e.to_string()))?;
        let status = res.status().as_u16();
        let header = res
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body: Value = res.json().await.unwrap_or(Value::Null);
        match status {
            200..=202 => {
                let token = find_str(&body, "managementToken")
                    .ok_or_else(|| ClankError::Format(text(&body)))?;
                Ok(Step::Done { token, dns_pending: status == 202 })
            }
            402 => {
                let (mac_h, inv_h) = header.as_deref().and_then(parse_www_authenticate).unzip();
                let macaroon = find_str(&body, "macaroon").or(mac_h);
                let invoice = find_str(&body, "invoice").or(inv_h);
                match (macaroon, invoice) {
                    (Some(macaroon), Some(invoice)) => Ok(Step::Pay(Challenge {
                        macaroon,
                        invoice,
                        amount_sats: find(&body, "amountSats").and_then(Value::as_u64),
                    })),
                    _ => Err(ClankError::Format(text(&body))),
                }
            }
            409 => Err(ClankError::Taken(username.to_string())),
            _ => {
                let msg = body
                    .pointer("/error/message")
                    .or_else(|| body.pointer("/error/code"))
                    .or_else(|| body.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("HTTP {status}"));
                Err(ClankError::Refused(msg.chars().take(300).collect()))
            }
        }
    }

    /// `PATCH /api/v1/registrations/{username}`: points the name at another URI.
    pub async fn update(
        &self,
        username: &str,
        token: &str,
        bip321_uri: &str,
    ) -> Result<(), ClankError> {
        let res = self
            .http
            .patch(self.url(&format!("/{username}")))
            .bearer_auth(token)
            .json(&json!({"bip321Uri": bip321_uri}))
            .send()
            .await
            .map_err(|e| ClankError::Net(e.to_string()))?;
        if res.status().is_success() {
            return Ok(());
        }
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Err(ClankError::Refused(format!("HTTP {status}: {}", text(&body))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        for ok in ["abc", "my-node", "a1b2c3", &"x".repeat(32)] {
            assert!(valid_username(ok), "{ok}");
        }
        for bad in ["ab", "-abc", "abc-", "ABC", "a.b", "a_b", &"x".repeat(33)] {
            assert!(!valid_username(bad), "{bad}");
        }
    }

    #[test]
    fn l402_header() {
        assert_eq!(
            parse_www_authenticate(r#"L402 macaroon="AGIAJEem", invoice="lnbc10n1x""#),
            Some(("AGIAJEem".into(), "lnbc10n1x".into()))
        );
        assert!(parse_www_authenticate("Bearer x").is_none());
    }

    #[test]
    fn fields_at_top_level_or_wrapped() {
        let b = json!({"data": {"managementToken": "t"}});
        assert_eq!(find_str(&b, "managementToken").as_deref(), Some("t"));
        let b = json!({"macaroon": "m"});
        assert_eq!(find_str(&b, "macaroon").as_deref(), Some("m"));
    }
}
