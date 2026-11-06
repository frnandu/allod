//! Minimal client for the deSEC API (desec.io): account signup, tokens, RRsets and the
//! dynDNS update endpoint (SPEC §12.1).

use std::net::IpAddr;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

/// Where the deSEC services live. Tests point these at a local mock.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// REST API base, without trailing slash (`https://desec.io/api/v1`).
    pub api: String,
    /// dynDNS update URL (`https://update.dedyn.io/`).
    pub update: String,
    /// Returns the caller's public IPv4 as plain text.
    pub checkip4: String,
    /// Returns the caller's public IPv6 as plain text.
    pub checkip6: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            api: "https://desec.io/api/v1".into(),
            update: "https://update.dedyn.io/".into(),
            checkip4: "https://checkipv4.dedyn.io/".into(),
            checkip6: "https://checkipv6.dedyn.io/".into(),
        }
    }
}

impl Endpoints {
    /// Every endpoint under one base URL (a local mock in tests and on regtest).
    pub fn under(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Endpoints {
            api: format!("{base}/api/v1"),
            update: format!("{base}/update/"),
            checkip4: format!("{base}/checkip4/"),
            checkip6: format!("{base}/checkip6/"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DesecError {
    /// The captcha solution was wrong or the captcha expired.
    #[error("the captcha answer was not accepted")]
    Captcha,
    /// deSEC refused the registration (abuse prevention).
    #[error("{0}")]
    Denied(String),
    /// The token or login is not valid (401/403).
    #[error("deSEC refused the credentials")]
    Unauthorized,
    /// Rate-limited; retry after this many seconds.
    #[error("deSEC asks to slow down (retry in {0} s)")]
    Throttled(u64),
    #[error("deSEC answered {0}: {1}")]
    Http(u16, String),
    #[error("cannot reach deSEC: {0}")]
    Net(String),
}

impl From<reqwest::Error> for DesecError {
    fn from(e: reqwest::Error) -> Self {
        DesecError::Net(e.to_string())
    }
}

/// A captcha to show to the owner.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Captcha {
    pub id: String,
    /// Base64 PNG.
    pub challenge: String,
}

#[derive(Debug, Clone)]
pub struct Desec {
    http: reqwest::Client,
    ep: Endpoints,
}

fn truncate(s: &str) -> String {
    s.chars().take(300).collect()
}

async fn check(res: reqwest::Response) -> Result<reqwest::Response, DesecError> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(DesecError::Unauthorized);
    }
    if status.as_u16() == 429 {
        let after = res
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        return Err(DesecError::Throttled(after));
    }
    let text = res.text().await.unwrap_or_default();
    if status.as_u16() == 400 {
        if let Ok(body) = serde_json::from_str::<Value>(&text) {
            if body.get("captcha").is_some() {
                return Err(DesecError::Captcha);
            }
            if body.get("code").and_then(Value::as_str) == Some("registration_denied") {
                let detail = body
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("deSEC refused the registration");
                return Err(DesecError::Denied(detail.to_string()));
            }
        }
    }
    Err(DesecError::Http(status.as_u16(), truncate(&text)))
}

impl Desec {
    pub fn new(ep: Endpoints) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Desec { http, ep }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.ep.api)
    }

    /// `POST /captcha/`
    pub async fn captcha(&self) -> Result<Captcha, DesecError> {
        let res = self.http.post(self.url("/captcha/")).json(&json!({})).send().await?;
        Ok(check(res).await?.json().await?)
    }

    /// `POST /auth/`: deSEC answers 202 and emails an activation link. The domain is
    /// created when the account is activated.
    pub async fn register(
        &self,
        email: &str,
        password: &str,
        captcha: (&str, &str),
        domain: &str,
    ) -> Result<(), DesecError> {
        let body = json!({
            "email": email,
            "password": password,
            "captcha": {"id": captcha.0, "solution": captcha.1},
            "domain": domain,
            "outreach_preference": false,
        });
        check(self.http.post(self.url("/auth/")).json(&body).send().await?).await?;
        Ok(())
    }

    /// `POST /auth/login/`: a short-lived login token, or `None` while the account is
    /// not activated (or the credentials are wrong).
    pub async fn login(&self, email: &str, password: &str) -> Result<Option<String>, DesecError> {
        let res = self
            .http
            .post(self.url("/auth/login/"))
            .json(&json!({"email": email, "password": password}))
            .send()
            .await?;
        match check(res).await {
            Ok(res) => {
                let body: Value = res.json().await?;
                Ok(body.get("token").and_then(Value::as_str).map(str::to_string))
            }
            Err(DesecError::Unauthorized) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `POST /auth/logout/`
    pub async fn logout(&self, login: &str) -> Result<(), DesecError> {
        let res = self
            .http
            .post(self.url("/auth/logout/"))
            .header("Authorization", auth(login))
            .send()
            .await?;
        check(res).await?;
        Ok(())
    }

    /// Creates a long-lived token that may write only `domain` (SPEC §12.1): no domain
    /// creation or deletion, no token management, no expiry, any source address.
    pub async fn create_restricted_token(
        &self,
        login: &str,
        domain: &str,
    ) -> Result<String, DesecError> {
        let res = self
            .http
            .post(self.url("/auth/tokens/"))
            .header("Authorization", auth(login))
            .json(&json!({
                "name": "allod",
                "perm_create_domain": false,
                "perm_delete_domain": false,
                "perm_manage_tokens": false,
                "max_age": null,
                "max_unused_period": null,
            }))
            .send()
            .await?;
        let body: Value = check(res).await?.json().await?;
        let id = body.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let secret = body
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| DesecError::Http(201, "no token in the answer".into()))?
            .to_string();
        let policies = format!("/auth/tokens/{id}/policies/rrsets/");
        for policy in [
            json!({"domain": null, "subname": null, "type": null, "perm_write": false}),
            json!({"domain": domain, "subname": null, "type": null, "perm_write": true}),
        ] {
            let res = self
                .http
                .post(self.url(&policies))
                .header("Authorization", auth(login))
                .json(&policy)
                .send()
                .await?;
            check(res).await?;
        }
        Ok(secret)
    }

    /// `GET /domains/{name}/`: true when the token can read the domain.
    pub async fn domain_exists(&self, token: &str, name: &str) -> Result<bool, DesecError> {
        let res = self
            .http
            .get(self.url(&format!("/domains/{name}/")))
            .header("Authorization", auth(token))
            .send()
            .await?;
        if res.status().as_u16() == 404 {
            return Ok(false);
        }
        check(res).await?;
        Ok(true)
    }

    /// `PUT /domains/{name}/rrsets/` (bulk). An RRset with `records: []` is deleted.
    pub async fn put_rrsets(
        &self,
        token: &str,
        name: &str,
        rrsets: &[Value],
    ) -> Result<(), DesecError> {
        if rrsets.is_empty() {
            return Ok(());
        }
        let res = self
            .http
            .put(self.url(&format!("/domains/{name}/rrsets/")))
            .header("Authorization", auth(token))
            .json(rrsets)
            .send()
            .await?;
        check(res).await?;
        Ok(())
    }

    async fn checkip(&self, url: &str) -> Result<IpAddr, DesecError> {
        let text = check(self.http.get(url).send().await?).await?.text().await?;
        text.trim()
            .parse()
            .map_err(|_| DesecError::Http(200, format!("not an address: {}", truncate(&text))))
    }

    /// This machine's public IPv4 address, as deSEC sees it.
    pub async fn public_ipv4(&self) -> Result<IpAddr, DesecError> {
        self.checkip(&self.ep.checkip4).await
    }

    /// This machine's public IPv6 address, as deSEC sees it.
    pub async fn public_ipv6(&self) -> Result<IpAddr, DesecError> {
        self.checkip(&self.ep.checkip6).await
    }

    /// dynDNS update of the apex A (and AAAA) record. `ipv6` is an address or
    /// `preserve`.
    pub async fn update_ip(
        &self,
        token: &str,
        name: &str,
        ipv4: &str,
        ipv6: &str,
    ) -> Result<(), DesecError> {
        let res = self
            .http
            .get(&self.ep.update)
            .basic_auth(name, Some(token))
            .query(&[("hostname", name), ("myipv4", ipv4), ("myipv6", ipv6)])
            .send()
            .await?;
        let text = check(res).await?.text().await?;
        if text.starts_with("good") || text.starts_with("nochg") {
            Ok(())
        } else {
            Err(DesecError::Http(200, truncate(&text)))
        }
    }
}

fn auth(token: &str) -> String {
    format!("Token {token}")
}

/// A TXT RRset value in presentation format: quoted chunks of at most 255 bytes, since
/// a BOLT12 offer is often longer than one DNS character-string.
pub fn txt_record(value: &str) -> String {
    value
        .as_bytes()
        .chunks(255)
        .map(|c| format!("\"{}\"", String::from_utf8_lossy(c)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One TXT RRset for the bulk endpoint; `None` deletes it.
pub fn txt_rrset(subname: &str, value: Option<&str>) -> Value {
    let records: Vec<String> = value.map(txt_record).into_iter().collect();
    json!({"subname": subname, "type": "TXT", "ttl": 3600, "records": records})
}

/// Deletes the apex address RRsets of the given types.
pub fn delete_rrset(kind: &str) -> Value {
    json!({"subname": "", "type": kind, "ttl": 3600, "records": []})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_txt_values_are_split_into_strings() {
        let offer = format!("bitcoin:?lno={}", "q".repeat(400));
        let r = txt_record(&offer);
        let parts: Vec<&str> = r.split("\" \"").collect();
        assert_eq!(parts.len(), 2);
        assert!(r.starts_with("\"bitcoin:?lno=") && r.ends_with('"'));
        assert_eq!(r.replace("\" \"", "").trim_matches('"'), offer);
    }

    #[test]
    fn deleting_an_rrset_sends_no_records() {
        assert_eq!(txt_rrset("a.user._bitcoin-payment", None)["records"], json!([]));
        assert_eq!(txt_rrset("a", Some("x"))["records"], json!(["\"x\""]));
    }
}
