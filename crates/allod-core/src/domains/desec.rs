//! deSEC DNS (desec.io): lets Allod keep the owner's records itself with an API token
//! (SPEC §12.1). REST: `https://desec.io/api/v1/` with `Authorization: Token <token>`.
//! Records come in sets (one name and type, several values): Allod adds or removes one
//! value by rewriting its set (`PATCH domains/{zone}/rrsets/`, an empty set deletes it).
//! deSEC signs every zone (DNSSEC); `GET domains/{zone}/` gives the DS values for the
//! registrar.

use std::time::Duration;

use serde_json::{json, Value};

use super::njalla::DnsRecord;

pub const DEFAULT_URL: &str = "https://desec.io/api/v1/";

/// TXT values are sent as quoted strings of at most 255 bytes each.
const TXT_CHUNK: usize = 255;

#[derive(Debug, thiserror::Error)]
pub enum DesecError {
    #[error("deSEC refused the request: {0}")]
    Refused(String),
    #[error("cannot reach deSEC: {0}")]
    Net(String),
    #[error("unexpected answer from deSEC: {0}")]
    Format(String),
}

#[derive(Debug, Clone)]
pub struct Desec {
    http: reqwest::Client,
    url: String,
}

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

/// A value as deSEC stores it: TXT quoted and split into 255-byte strings.
pub fn encode(kind: &str, content: &str) -> String {
    if kind != "TXT" {
        return content.to_string();
    }
    let escaped = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let bytes = content.as_bytes();
    if bytes.is_empty() {
        return "\"\"".into();
    }
    // Chunks on char boundaries (the content is ASCII in practice).
    let mut parts = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + TXT_CHUNK).min(bytes.len());
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(format!("\"{}\"", escaped(&content[start..end])));
        start = end;
    }
    parts.join(" ")
}

/// The value back from deSEC's form: TXT strings unquoted and joined.
pub fn decode(kind: &str, value: &str) -> String {
    if kind != "TXT" {
        return value.to_string();
    }
    let mut out = String::new();
    let mut chars = value.chars();
    let mut inside = false;
    while let Some(c) = chars.next() {
        match (inside, c) {
            (false, '"') => inside = true,
            (true, '"') => inside = false,
            (true, '\\') => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            (true, c) => out.push(c),
            (false, _) => {}
        }
    }
    out
}

/// Record ids are synthetic: `TYPE|name|content` (deSEC has no per-value id).
fn record_id(kind: &str, name: &str, content: &str) -> String {
    format!("{kind}|{name}|{content}")
}

fn parse_id(id: &str) -> Option<(&str, &str, &str)> {
    let mut it = id.splitn(3, '|');
    Some((it.next()?, it.next()?, it.next()?))
}

/// deSEC's subname for Allod's relative name (`@`: the zone apex).
fn subname(name: &str) -> &str {
    if name == "@" {
        ""
    } else {
        name
    }
}

impl Desec {
    /// `url` is [`DEFAULT_URL`] (a mock in tests).
    pub fn new(url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let url = if url.ends_with('/') { url.to_string() } else { format!("{url}/") };
        Desec { http, url }
    }

    /// One request; retried on network errors and when deSEC asks to slow down (429).
    async fn call(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>, DesecError> {
        let mut attempt = 0u64;
        loop {
            let mut req = self.http.request(method.clone(), format!("{}{path}", self.url));
            // Account calls before a token exists go without one.
            if !token.is_empty() {
                req = req.header("Authorization", format!("Token {token}"));
            }
            if let Some(b) = &body {
                req = req.json(b);
            }
            let res = match req.send().await {
                Ok(r) => r,
                Err(e) if attempt < 2 => {
                    attempt += 1;
                    tracing::debug!(error = %chain(&e), "deSEC: retrying");
                    tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
                    continue;
                }
                Err(e) => return Err(DesecError::Net(chain(&e))),
            };
            let status = res.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
                attempt += 1;
                let wait = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2)
                    .min(15);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let text = res.text().await.map_err(|e| DesecError::Net(chain(&e)))?;
            if !status.is_success() {
                let detail = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or(text);
                return Err(DesecError::Refused(format!(
                    "HTTP {status}: {}",
                    detail.chars().take(300).collect::<String>()
                )));
            }
            if text.trim().is_empty() {
                return Ok(Some(Value::Null));
            }
            return serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| DesecError::Format(e.to_string()));
        }
    }

    /// Checks a token: the domains it can manage.
    pub async fn check(&self, token: &str) -> Result<Vec<String>, DesecError> {
        let r = self.call(token, reqwest::Method::GET, "domains/", None).await?;
        let list = r
            .as_ref()
            .and_then(Value::as_array)
            .ok_or_else(|| DesecError::Refused("the token cannot list domains".into()))?;
        Ok(list
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str).map(str::to_string))
            .collect())
    }

    /// The deSEC domain holding `name` (the longest that ends it) and `name`'s part below
    /// it (`@` for the domain itself).
    pub async fn zone_of(&self, token: &str, name: &str) -> Result<(String, String), DesecError> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        let zone = self
            .check(token)
            .await?
            .into_iter()
            .filter(|z| name == *z || name.ends_with(&format!(".{z}")))
            .max_by_key(String::len)
            .ok_or_else(|| {
                DesecError::Refused(format!("{name} is not a domain of this deSEC account"))
            })?;
        let sub = name.strip_suffix(&zone).unwrap_or("").trim_end_matches('.').to_string();
        Ok((zone, if sub.is_empty() { "@".into() } else { sub }))
    }

    /// Every value of the zone, one record each (`name`: `@` for the apex).
    pub async fn list(&self, token: &str, zone: &str) -> Result<Vec<DnsRecord>, DesecError> {
        let r = self
            .call(token, reqwest::Method::GET, &format!("domains/{zone}/rrsets/"), None)
            .await?
            .ok_or_else(|| DesecError::Refused(format!("no domain {zone} in this account")))?;
        let sets = r.as_array().ok_or_else(|| DesecError::Format("rrsets is not a list".into()))?;
        let mut out = Vec::new();
        for s in sets {
            let kind = s["type"].as_str().unwrap_or_default().to_string();
            let sub = s["subname"].as_str().unwrap_or_default();
            let name = if sub.is_empty() { "@".to_string() } else { sub.to_string() };
            for v in s["records"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let content = decode(&kind, v);
                out.push(DnsRecord {
                    id: record_id(&kind, &name, &content),
                    name: name.clone(),
                    kind: kind.clone(),
                    content,
                });
            }
        }
        Ok(out)
    }

    /// The values of one set, as deSEC stores them, and its TTL.
    async fn rrset(
        &self,
        token: &str,
        zone: &str,
        kind: &str,
        name: &str,
    ) -> Result<(Vec<String>, Option<u64>), DesecError> {
        let sub = if name == "@" { "@" } else { name };
        let r = self
            .call(
                token,
                reqwest::Method::GET,
                &format!("domains/{zone}/rrsets/{sub}/{kind}/"),
                None,
            )
            .await?;
        Ok(match r {
            Some(s) => (
                s["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                s["ttl"].as_u64(),
            ),
            None => (Vec::new(), None),
        })
    }

    async fn write(
        &self,
        token: &str,
        zone: &str,
        kind: &str,
        name: &str,
        records: Vec<String>,
        ttl: u64,
    ) -> Result<(), DesecError> {
        let body =
            json!([{"subname": subname(name), "type": kind, "ttl": ttl, "records": records}]);
        self.call(token, reqwest::Method::PATCH, &format!("domains/{zone}/rrsets/"), Some(body))
            .await
            .map(|_| ())
    }

    /// Adds one value to its set; returns its record id.
    pub async fn add(
        &self,
        token: &str,
        zone: &str,
        kind: &str,
        name: &str,
        content: &str,
        ttl: u32,
    ) -> Result<String, DesecError> {
        let (mut records, old_ttl) = self.rrset(token, zone, kind, name).await?;
        let value = encode(kind, content);
        if !records.iter().any(|r| decode(kind, r) == content) {
            records.push(value);
            // deSEC's minimum TTL is 3600 on most accounts; keep a longer one already set.
            let ttl = old_ttl.unwrap_or(0).max(u64::from(ttl)).max(3600);
            self.write(token, zone, kind, name, records, ttl).await?;
        }
        Ok(record_id(kind, name, content))
    }

    /// Removes one value from its set (the set goes when it is empty).
    pub async fn remove(&self, token: &str, zone: &str, id: &str) -> Result<(), DesecError> {
        let (kind, name, content) =
            parse_id(id).ok_or_else(|| DesecError::Format(format!("bad record id {id}")))?;
        let (records, ttl) = self.rrset(token, zone, kind, name).await?;
        if records.is_empty() {
            return Ok(());
        }
        let keep: Vec<String> =
            records.iter().filter(|r| decode(kind, r) != content).cloned().collect();
        if keep.len() == records.len() {
            return Ok(());
        }
        self.write(token, zone, kind, name, keep, ttl.unwrap_or(3600)).await
    }

    // ------------------------------------------------------------- account

    /// `POST captcha/`: `(id, base64 PNG)` for the owner to solve.
    pub async fn captcha(&self) -> Result<(String, String), DesecError> {
        let r = self
            .call("", reqwest::Method::POST, "captcha/", Some(json!({})))
            .await?
            .unwrap_or(Value::Null);
        match (r["id"].as_str(), r["challenge"].as_str()) {
            (Some(id), Some(png)) => Ok((id.to_string(), png.to_string())),
            _ => Err(DesecError::Format("no captcha in the answer".into())),
        }
    }

    /// `POST auth/`: deSEC answers 202 and mails an activation link.
    pub async fn register(
        &self,
        email: &str,
        password: &str,
        captcha_id: &str,
        solution: &str,
    ) -> Result<(), DesecError> {
        let body = json!({
            "email": email,
            "password": password,
            "captcha": {"id": captcha_id, "solution": solution},
            "outreach_preference": false,
        });
        self.call("", reqwest::Method::POST, "auth/", Some(body)).await.map(|_| ())
    }

    /// `POST v/activate-account/{code}/`: the link of the activation mail (the captcha
    /// was solved at registration).
    pub async fn activate(&self, code: &str) -> Result<(), DesecError> {
        let path = format!("v/activate-account/{code}/");
        match self.call("", reqwest::Method::POST, &path, Some(json!({}))).await? {
            Some(_) => Ok(()),
            None => Err(DesecError::Refused("the activation link is not valid (expired?)".into())),
        }
    }

    /// `POST auth/login/`: a short-lived login token.
    pub async fn login(&self, email: &str, password: &str) -> Result<String, DesecError> {
        let r = self
            .call(
                "",
                reqwest::Method::POST,
                "auth/login/",
                Some(json!({"email": email, "password": password})),
            )
            .await?
            .unwrap_or(Value::Null);
        r["token"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| DesecError::Format("no token in the login answer".into()))
    }

    /// `POST auth/tokens/`: a token for Allod that may also create domains.
    pub async fn create_token(&self, login: &str, name: &str) -> Result<String, DesecError> {
        let body = json!({"name": name, "perm_create_domain": true});
        let r = self
            .call(login, reqwest::Method::POST, "auth/tokens/", Some(body))
            .await?
            .unwrap_or(Value::Null);
        r["token"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| DesecError::Format("no token in the answer".into()))
    }

    /// `POST auth/logout/`: ends the login token.
    pub async fn logout(&self, login: &str) {
        let _ = self.call(login, reqwest::Method::POST, "auth/logout/", None).await;
    }

    /// `POST domains/ {name}`: creates the zone (deSEC signs it at once).
    pub async fn create_domain(&self, token: &str, name: &str) -> Result<(), DesecError> {
        self.call(token, reqwest::Method::POST, "domains/", Some(json!({"name": name})))
            .await
            .map(|_| ())
    }

    /// The zone's DS values for the registrar, SHA-256 (digest type 2) first.
    pub async fn ds(&self, token: &str, zone: &str) -> Result<Vec<String>, DesecError> {
        let r = self
            .call(token, reqwest::Method::GET, &format!("domains/{zone}/"), None)
            .await?
            .unwrap_or(Value::Null);
        let mut ds: Vec<String> = r["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|k| k["managed"].as_bool() != Some(false))
            .flat_map(|k| k["ds"].as_array().cloned().unwrap_or_default())
            .filter_map(|d| d.as_str().map(str::to_string))
            .collect();
        ds.sort_by_key(|d| d.split_whitespace().nth(2) != Some("2"));
        ds.dedup();
        Ok(ds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_values_round_trip_in_255_byte_strings() {
        let offer = format!("bitcoin:?lno={}", "x".repeat(600));
        let enc = encode("TXT", &offer);
        assert!(enc.starts_with("\"bitcoin:?lno=") && enc.matches("\" \"").count() == 2, "{enc}");
        assert!(enc.split("\" \"").all(|p| p.trim_matches('"').len() <= 255));
        assert_eq!(decode("TXT", &enc), offer);
        assert_eq!(decode("TXT", "\"a \\\"b\\\"\""), "a \"b\"");
        assert_eq!(encode("A", "1.2.3.4"), "1.2.3.4");
    }

    #[test]
    fn ids() {
        let id = record_id("TXT", "sats.user._bitcoin-payment", "bitcoin:?lno=a|b");
        assert_eq!(parse_id(&id), Some(("TXT", "sats.user._bitcoin-payment", "bitcoin:?lno=a|b")));
        assert_eq!(subname("@"), "");
    }
}
