//! ZEUS Pay (zeuspay.com): a free LNURL Lightning address on ZEUS's domain whose invoices
//! come from this node over NWC (SPEC §12.5). ZEUS's server answers LNURL and asks the
//! node for each invoice, so the node's IP stays private. The node proves it owns the
//! address by signing a server challenge with its node key, as the ZEUS app does
//! (`stores/LightningAddressStore.ts`); there is no published API documentation.

use std::time::Duration;

use serde_json::{json, Value};

/// ZEUS Pay's API host.
pub const DEFAULT_URL: &str = "https://zeuspay.com";
/// Domains ZEUS Pay offers for addresses.
pub const DOMAINS: [&str; 2] = ["zeuspay.com", "zeusnuts.com"];

#[derive(Debug, thiserror::Error)]
pub enum ZeusError {
    #[error("ZEUS Pay refused the request: {0}")]
    Refused(String),
    #[error("cannot reach ZEUS Pay: {0}")]
    Net(String),
    #[error("unexpected answer from ZEUS Pay: {0}")]
    Format(String),
}

/// A signed challenge: proves the request comes from the node with `pubkey`.
#[derive(Debug, Clone)]
pub struct Auth {
    pub pubkey: String,
    pub message: String,
    pub signature: String,
}

/// The address ZEUS Pay holds for this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub handle: String,
    pub domain: String,
}

/// ZEUS issues 64 random bytes as lowercase hex; anything else is not signed, so the
/// node key never signs text a server made up (same rule as the ZEUS app).
pub fn valid_challenge(c: &str) -> bool {
    c.len() == 128 && c.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// ZEUS Pay handles: lowercase letters, digits, '.', '_' and '-'. The server has the
/// final say; this only stops obvious typos before a signed request is sent.
pub fn valid_handle(h: &str) -> bool {
    (1..=64).contains(&h.len())
        && h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// What `status` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub address: Option<Address>,
    /// UNIX seconds; `None` without a subscription.
    pub plus_expires_at: Option<i64>,
    /// Price of one year of ZEUS Pay+.
    pub plus_fee_sats: Option<u64>,
}

/// Provider order identity and invoice returned for one ZEUS Pay+ purchase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlusOrder {
    pub id: String,
    pub bolt11: String,
}

/// A time as ZEUS sends it: an ISO 8601 / RFC 3339 string, or a number of seconds or
/// milliseconds. Returns UNIX seconds.
pub fn parse_time(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    let s = v.as_str()?.trim();
    if let Ok(n) = s.parse::<i64>() {
        return parse_time(&Value::from(n));
    }
    // YYYY-MM-DD[THH:MM[:SS[.fff]]][Z|±HH:MM]
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if s.get(4..5)? != "-"
        || s.get(7..8)? != "-"
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
    {
        return None;
    }
    let (mut h, mut mi, mut se) = (0, 0, 0);
    let mut rest = s.get(10..).unwrap_or("");
    if let Some(t) = rest.strip_prefix(['T', ' ']) {
        h = t.get(0..2)?.parse().ok()?;
        mi = t.get(3..5)?.parse().ok()?;
        rest = t.get(5..).unwrap_or("");
        if let Some(sec) = rest.strip_prefix(':') {
            se = sec.get(0..2)?.parse().ok()?;
            rest = sec.get(2..).unwrap_or("");
        }
        if let Some(frac) = rest.strip_prefix('.') {
            rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
        }
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (o.get(1..3)?.parse::<i64>().ok()? * 3600
                + o.get(4..6)?.parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - offset)
}

#[derive(Debug, Clone)]
pub struct ZeusPay {
    http: reqwest::Client,
    base: String,
}

fn text(v: &Value) -> String {
    v.to_string().chars().take(300).collect()
}

impl ZeusPay {
    /// `base` is [`DEFAULT_URL`] (a mock in tests).
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        ZeusPay { http, base: base.trim_end_matches('/').to_string() }
    }

    /// Whether this client talks to the real ZEUS Pay (not a test stand-in).
    pub fn is_default(&self) -> bool {
        self.base == DEFAULT_URL
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, ZeusError> {
        self.post_at(&format!("/api/lnurl/{path}"), body).await
    }

    async fn post_at(&self, path: &str, body: Value) -> Result<Value, ZeusError> {
        let res = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| ZeusError::Net(e.to_string()))?;
        let status = res.status();
        let body: Value = res.json().await.map_err(|e| ZeusError::Format(e.to_string()))?;
        if !status.is_success() || body.get("success") == Some(&Value::Bool(false)) {
            let msg = body
                .get("error")
                .map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| text(e)))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(ZeusError::Refused(msg.chars().take(300).collect()));
        }
        Ok(body)
    }

    /// `POST /api/lnurl/auth {pubkey}` → the challenge to sign.
    pub async fn challenge(&self, pubkey: &str) -> Result<String, ZeusError> {
        let body = self.post("auth", json!({"pubkey": pubkey})).await?;
        let c = body.get("verification").and_then(Value::as_str).unwrap_or_default();
        if !valid_challenge(c) {
            return Err(ZeusError::Format("the challenge is not 64 bytes of hex".into()));
        }
        Ok(c.to_string())
    }

    fn signed(auth: &Auth, extra: Value) -> Value {
        let mut v =
            json!({"pubkey": auth.pubkey, "message": auth.message, "signature": auth.signature});
        if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
            o.extend(e);
        }
        v
    }

    fn address(body: &Value) -> Result<Address, ZeusError> {
        match (
            body.get("handle").and_then(Value::as_str),
            body.get("domain").and_then(Value::as_str),
        ) {
            (Some(h), d) if !h.is_empty() => Ok(Address {
                handle: h.to_string(),
                domain: d.filter(|d| !d.is_empty()).unwrap_or(DOMAINS[0]).to_string(),
            }),
            _ => Err(ZeusError::Format(text(body))),
        }
    }

    /// `POST /api/lnurl/create` with `address_type: "nwc"`: ZEUS Pay requests invoices
    /// from `nwc` from now on.
    pub async fn create_nwc(&self, auth: &Auth, nwc: &str) -> Result<Address, ZeusError> {
        let body = self
            .post("create", Self::signed(auth, json!({"nwc_string": nwc, "address_type": "nwc"})))
            .await?;
        Self::address(&body)
    }

    /// `POST /api/lnurl/update {updates: {handle, domain}}`: renames the address.
    pub async fn rename(
        &self,
        auth: &Auth,
        handle: &str,
        domain: &str,
    ) -> Result<Address, ZeusError> {
        let body = self
            .post(
                "update",
                Self::signed(auth, json!({"updates": {"handle": handle, "domain": domain}})),
            )
            .await?;
        Ok(Self::address(&body)
            .unwrap_or_else(|_| Address { handle: handle.into(), domain: domain.into() }))
    }

    /// `POST /api/lnurl/status`: the address this node has, if any, and its ZEUS Pay+
    /// subscription (needed to choose the name or domain).
    pub async fn status(&self, auth: &Auth) -> Result<Status, ZeusError> {
        let body = self.post("status", Self::signed(auth, json!({}))).await?;
        Ok(Status {
            address: Self::address(&body).ok(),
            plus_expires_at: body.get("plusExpiresAt").and_then(parse_time),
            plus_fee_sats: body.get("plusAnnualFeeSats").and_then(|v| {
                v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            }),
        })
    }

    /// `POST /api/plus/order`: a BOLT11 invoice for one year of ZEUS Pay+ (or one more
    /// year when it is active).
    pub async fn plus_order(&self, auth: &Auth) -> Result<PlusOrder, ZeusError> {
        let body = self.post_at("/api/plus/order", Self::signed(auth, json!({}))).await?;
        let id = body
            .get("orderId")
            .or_else(|| body.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ZeusError::Format(text(&body)))?;
        let bolt11 = body
            .get("bolt11")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .map(str::to_string)
            .ok_or_else(|| ZeusError::Format(text(&body)))?;
        Ok(PlusOrder { id: id.into(), bolt11 })
    }

    /// `POST /api/lnurl/delete`: gives the address up.
    pub async fn delete(&self, auth: &Auth) -> Result<(), ZeusError> {
        self.post("delete", Self::signed(auth, json!({}))).await.map(|_| ())
    }

    /// Whether `https://{domain}/.well-known/lnurlp/{handle}` answers an LNURL-pay
    /// request (with a stand-in, its base URL instead of the domain).
    pub async fn answers(&self, a: &Address) -> bool {
        let base =
            if self.is_default() { format!("https://{}", a.domain) } else { self.base.clone() };
        let Ok(res) = self.http.get(format!("{base}/.well-known/lnurlp/{}", a.handle)).send().await
        else {
            return false;
        };
        res.status().is_success()
            && res
                .json::<Value>()
                .await
                .is_ok_and(|v| v.get("tag").and_then(Value::as_str) == Some("payRequest"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time(&json!("1970-01-01T00:00:00Z")), Some(0));
        assert_eq!(parse_time(&json!("2026-11-08T12:30:00.123Z")), Some(1_794_141_000));
        assert_eq!(parse_time(&json!("2026-11-08T13:30:00+01:00")), Some(1_794_141_000));
        assert_eq!(parse_time(&json!("2026-11-08")), Some(1_794_096_000));
        assert_eq!(parse_time(&json!(1_794_141_000)), Some(1_794_141_000));
        assert_eq!(parse_time(&json!(1_794_141_000_000_i64)), Some(1_794_141_000), "milliseconds");
        assert_eq!(parse_time(&json!("next year")), None);
        assert_eq!(parse_time(&json!(null)), None);
    }

    #[test]
    fn challenges_and_handles() {
        assert!(valid_challenge(&"ab".repeat(64)));
        assert!(!valid_challenge(&"AB".repeat(64)), "lowercase only");
        assert!(!valid_challenge("Lightning Signed Message: send me your coins"));
        assert!(!valid_challenge(&"ab".repeat(63)));
        assert!(valid_handle("my.node_1-x"));
        assert!(!valid_handle("") && !valid_handle("Bad") && !valid_handle("a b"));
    }
}
