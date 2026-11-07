//! Twelve Cash (twelve.cash): free BIP-353 names that point at a BOLT12 offer, on a
//! DNSSEC-signed domain run by a third party (SPEC §12.3). Free names are random
//! (`adjective.animal@twelve.cash`); custom names are bought on the website.

use std::time::Duration;

use serde_json::{json, Value};

/// The Twelve Cash domain Allod registers free names on.
pub const DOMAIN: &str = "twelve.cash";

#[derive(Debug, thiserror::Error)]
pub enum TwelveError {
    #[error("twelve.cash refused the request: {0}")]
    Refused(String),
    #[error("cannot reach twelve.cash: {0}")]
    Net(String),
    #[error("unexpected answer from twelve.cash: {0}")]
    Format(String),
}

#[derive(Debug, Clone)]
pub struct TwelveCash {
    http: reqwest::Client,
    base: String,
}

impl TwelveCash {
    /// `base` is `https://twelve.cash` (a mock in tests).
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        TwelveCash { http, base: base.trim_end_matches('/').to_string() }
    }

    /// `payCode.createRandomPayCode` (tRPC with superjson: the input goes in `json`).
    /// Returns the new user name; the TXT record holds `bitcoin:?lno=<offer>`.
    pub async fn create_random(&self, offer: &str) -> Result<String, TwelveError> {
        let res = self
            .http
            .post(format!("{}/api/trpc/payCode.createRandomPayCode", self.base))
            .json(&json!({"json": {"domain": DOMAIN, "lno": offer}}))
            .send()
            .await
            .map_err(|e| TwelveError::Net(e.to_string()))?;
        let status = res.status();
        let body: Value = res.json().await.map_err(|e| TwelveError::Format(e.to_string()))?;
        if !status.is_success() {
            let msg = body
                .pointer("/error/json/data/zodError/formErrors/0")
                .or_else(|| body.pointer("/error/json/message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(TwelveError::Refused(msg.chars().take(300).collect()));
        }
        let data = body.pointer("/result/data/json").unwrap_or(&Value::Null);
        match (
            data.get("userName").and_then(Value::as_str),
            data.get("status").and_then(Value::as_str),
        ) {
            (Some(name), Some("ACTIVE") | None) if !name.is_empty() => Ok(name.to_string()),
            _ => Err(TwelveError::Format(body.to_string().chars().take(300).collect())),
        }
    }
}

/// Splits `user@domain` and checks it is a Twelve Cash name.
pub fn parse_address(address: &str) -> Option<(String, String)> {
    let address = address.trim().trim_start_matches('₿').to_ascii_lowercase();
    let (user, domain) = address.split_once('@')?;
    let ok_user = !user.is_empty()
        && user.len() <= 64
        && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    (ok_user && matches!(domain, "twelve.cash" | "12cash.dev"))
        .then(|| (user.to_string(), domain.to_string()))
}

/// The BIP-353 TXT values published for `user@domain`, each with its strings joined.
pub async fn lookup(user: &str, domain: &str) -> Option<Vec<String>> {
    crate::wallet::bip353::plain_txt(&format!("{user}.user._bitcoin-payment.{domain}.")).await
}

/// Whether one of the published values pays `offer`.
pub fn points_at(values: &[String], offer: &str) -> bool {
    values.iter().any(|v| {
        v.to_ascii_lowercase().starts_with("bitcoin:")
            && v.split(['?', '&']).any(|p| p.eq_ignore_ascii_case(&format!("lno={offer}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(
            parse_address(" Brave.Penguin@twelve.cash ").unwrap(),
            ("brave.penguin".into(), "twelve.cash".into())
        );
        assert_eq!(parse_address("₿satoshi@12cash.dev").unwrap().0, "satoshi");
        assert!(parse_address("alice@example.com").is_none());
        assert!(parse_address("@twelve.cash").is_none());
        assert!(parse_address("a b@twelve.cash").is_none());
    }

    #[test]
    fn records_must_carry_the_offer() {
        let v = vec!["bitcoin:?lno=lno1abc&sp=sp1x".to_string()];
        assert!(points_at(&v, "lno1abc"));
        assert!(!points_at(&v, "lno1ab"));
        assert!(!points_at(&["lno=lno1abc".into()], "lno1abc"));
    }
}
