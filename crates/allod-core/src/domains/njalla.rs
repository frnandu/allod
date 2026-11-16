//! Njalla DNS (njal.la): lets Allod keep the owner's records itself with an API token
//! (SPEC §12.1). JSON-RPC style: `POST https://njal.la/api/1/` with
//! `{"method", "params"}` and `Authorization: Njalla <token>`; methods `list-records`,
//! `add-record`, `remove-record` (as used by the lego ACME client).

use std::time::Duration;

use serde_json::{json, Value};

pub const DEFAULT_URL: &str = "https://njal.la/api/1/";

#[derive(Debug, thiserror::Error)]
pub enum NjallaError {
    #[error("Njalla refused the request: {0}")]
    Refused(String),
    #[error("cannot reach Njalla: {0}")]
    Net(String),
    #[error("unexpected answer from Njalla: {0}")]
    Format(String),
}

/// One DNS record at Njalla. `name` is relative to the domain (`@` for the apex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Njalla {
    http: reqwest::Client,
    url: String,
}

/// The error with its causes: reqwest's own text ("error sending request") hides them.
fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

fn id_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

impl Njalla {
    /// `url` is [`DEFAULT_URL`] (a mock in tests).
    pub fn new(url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Njalla { http, url: url.to_string() }
    }

    /// One API call; retried twice when Njalla cannot be reached (its IPv4 and IPv6
    /// addresses drop out for short spells).
    async fn call(&self, token: &str, method: &str, params: Value) -> Result<Value, NjallaError> {
        let mut attempt = 0;
        loop {
            match self.call_once(token, method, params.clone()).await {
                Err(NjallaError::Net(_)) if attempt < 2 => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
                }
                r => return r,
            }
        }
    }

    async fn call_once(
        &self,
        token: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, NjallaError> {
        let res = self
            .http
            .post(&self.url)
            .header("Authorization", format!("Njalla {token}"))
            .json(&json!({"method": method, "params": params}))
            .send()
            .await
            .map_err(|e| NjallaError::Net(chain(&e)))?;
        let status = res.status();
        let body: Value = allod_lnurl::bounded_response_json(res, allod_lnurl::JSON_RESPONSE_LIMIT)
            .await
            .map_err(|e| NjallaError::Format(e.to_string()))?;
        if let Some(err) = body.get("error").filter(|e| !e.is_null()) {
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(NjallaError::Refused(msg.chars().take(300).collect()));
        }
        if !status.is_success() {
            return Err(NjallaError::Refused(format!("HTTP {status}")));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Any API method (domain search, wallet, registration).
    pub async fn rpc(
        &self,
        token: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, NjallaError> {
        self.call(token, method, params).await
    }

    /// Checks a token: `list-domains` answers with the account's domains.
    pub async fn check(&self, token: &str) -> Result<Vec<String>, NjallaError> {
        let r = self.call(token, "list-domains", json!({})).await?;
        Ok(r.get("domains")
            .and_then(Value::as_array)
            .map(|d| {
                d.iter()
                    .filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `list-records {domain}`.
    pub async fn list(&self, token: &str, domain: &str) -> Result<Vec<DnsRecord>, NjallaError> {
        let r = self.call(token, "list-records", json!({"domain": domain})).await?;
        let records = r
            .get("records")
            .and_then(Value::as_array)
            .ok_or_else(|| NjallaError::Format(r.to_string().chars().take(300).collect()))?;
        Ok(records
            .iter()
            .map(|x| DnsRecord {
                id: id_of(x.get("id").unwrap_or(&Value::Null)),
                name: x.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                kind: x.get("type").and_then(Value::as_str).unwrap_or_default().to_string(),
                content: x.get("content").and_then(Value::as_str).unwrap_or_default().to_string(),
            })
            .collect())
    }

    /// `add-record {domain, type, name, content, ttl}`; returns the new record's id.
    pub async fn add(
        &self,
        token: &str,
        domain: &str,
        kind: &str,
        name: &str,
        content: &str,
        ttl: u32,
    ) -> Result<String, NjallaError> {
        let r = self
            .call(
                token,
                "add-record",
                json!({"domain": domain, "type": kind, "name": name, "content": content, "ttl": ttl}),
            )
            .await?;
        let id = id_of(r.get("id").unwrap_or(&Value::Null));
        if id.is_empty() {
            return Err(NjallaError::Format(r.to_string().chars().take(300).collect()));
        }
        Ok(id)
    }

    /// `remove-record {domain, id}`.
    pub async fn remove(&self, token: &str, domain: &str, id: &str) -> Result<(), NjallaError> {
        self.call(token, "remove-record", json!({"domain": domain, "id": id})).await.map(|_| ())
    }
}

/// The domain registered at Njalla for `name` (its last two labels, as lego does) and
/// `name`'s part below it (`@` for the domain itself).
pub fn split(name: &str) -> Option<(String, String)> {
    let labels: Vec<&str> = name.trim_end_matches('.').split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    let zone = labels[labels.len() - 2..].join(".");
    let sub = labels[..labels.len() - 2].join(".");
    Some((zone, if sub.is_empty() { "@".into() } else { sub }))
}

/// Joins a relative name below `sub` (`@` = the zone apex).
pub fn relative(name: &str, sub: &str) -> String {
    if sub == "@" {
        name.to_string()
    } else {
        format!("{name}.{sub}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(split("pay.fmar.dev"), Some(("fmar.dev".into(), "pay".into())));
        assert_eq!(split("fmar.dev"), Some(("fmar.dev".into(), "@".into())));
        assert_eq!(split("a.b.fmar.dev."), Some(("fmar.dev".into(), "a.b".into())));
        assert_eq!(split("localhost"), None);
        assert_eq!(relative("sats.user._bitcoin-payment", "pay"), "sats.user._bitcoin-payment.pay");
        assert_eq!(relative("sats.user._bitcoin-payment", "@"), "sats.user._bitcoin-payment");
    }
}
