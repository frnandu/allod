//! A Njalla account made by Allod (SPEC §12.1). Njalla has no API for accounts, so Allod
//! uses its website as a browser would: it signs up with the gateway account key's own
//! address (`npub…@uid.ovh`, the same Nostr mailbox as LNVPS and deSEC) and a random
//! password, opens the confirmation link from the mailbox (which also logs the session
//! in), and adds an API token through the site's own JSON-RPC call (`add-token` on
//! `/api/1/` with the session cookie and `X-CSRFToken`). No captcha is involved.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{mailbox, now, provider_secret, store, DomainError, DomainService};

/// Config key of the sign-up's progress.
const SIGNUP_KEY: &str = "domains.njalla_signup";
/// Secret file of the account's password.
const PASSWORD_SECRET: &str = "njalla_password";
/// Give up waiting for the confirmation mail after this long.
const WAIT_SECS: i64 = 12 * 3600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NjallaSignup {
    pub email: String,
    /// `activating` (waiting for the mail), `done`, `failed`, or `removed` (its provider
    /// was removed; the account and password remain for reconnecting).
    pub state: String,
    pub since: i64,
    pub error: Option<String>,
    pub provider_id: Option<i64>,
}

/// A browser-like session on Njalla's website (cookies kept).
struct Web {
    http: reqwest::Client,
    jar: std::sync::Arc<reqwest::cookie::Jar>,
    base: String,
}

impl Web {
    fn new(base: &str) -> Result<Self, String> {
        let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
        let http = reqwest::Client::builder()
            .cookie_provider(jar.clone())
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Mozilla/5.0 (compatible; allod/", env!("CARGO_PKG_VERSION"), ")"))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Web { http, jar, base: base.trim_end_matches('/').to_string() })
    }

    /// A cookie of the session (Njalla's scripts send `csrftoken` as `X-CSRFToken`).
    fn cookie(&self, name: &str) -> Option<String> {
        use reqwest::cookie::CookieStore;
        let url = reqwest::Url::parse(&self.base).ok()?;
        let header = self.jar.cookies(&url)?;
        header.to_str().ok()?.split(';').find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, path: &str) -> Result<String, String> {
        let r = self.http.get(self.url(path)).send().await.map_err(|e| format!("Njalla: {e}"))?;
        r.text().await.map_err(|e| e.to_string())
    }

    /// Posts a page's form (with its CSRF field) back to it.
    async fn post_form(
        &self,
        path: &str,
        page: &str,
        fields: &[(&str, &str)],
    ) -> Result<String, String> {
        let csrf = hidden(page, "csrfmiddlewaretoken").ok_or("Njalla's form has no CSRF field")?;
        let mut form: Vec<(&str, &str)> = vec![("csrfmiddlewaretoken", csrf.as_str())];
        form.extend_from_slice(fields);
        let r = self
            .http
            .post(self.url(path))
            .header("Referer", self.url(path))
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("Njalla: {e}"))?;
        let status = r.status();
        let body = r.text().await.map_err(|e| e.to_string())?;
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err("Njalla refused the form (permission denied)".into());
        }
        Ok(body)
    }

    /// One call of the site's JSON-RPC API with the logged-in session.
    async fn rpc(&self, csrf: &str, method: &str, params: Value) -> Result<Value, String> {
        let r = self
            .http
            .post(self.url("/api/1/"))
            .header("Referer", self.url("/settings/api/"))
            .header("X-CSRFToken", csrf)
            .json(&json!({"method": method, "params": params}))
            .send()
            .await
            .map_err(|e| format!("Njalla: {e}"))?;
        let body: Value = r.json().await.map_err(|e| format!("Njalla answered: {e}"))?;
        if let Some(e) = body.get("error").filter(|e| !e.is_null()) {
            return Err(format!(
                "Njalla refused {method}: {}",
                e.get("message").and_then(Value::as_str).unwrap_or("error")
            ));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// The value of a hidden input of a page.
fn hidden(page: &str, name: &str) -> Option<String> {
    let at = page.find(&format!("name=\"{name}\""))?;
    let tag_start = page[..at].rfind('<')?;
    let tag_end = at + page[at..].find('>')?;
    let tag = &page[tag_start..tag_end];
    let v = tag.find("value=\"")? + "value=\"".len();
    Some(tag[v..].split('"').next()?.to_string())
}

/// The visible text of a page, roughly (for its messages).
fn text(page: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in page.chars() {
        match c {
            '<' => inside = true,
            '>' => {
                inside = false;
                out.push(' ');
            }
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Njalla's login proof of work: the `cnonce` that makes
/// `sha256(email + password + cnonce)` start with the page's `nonce` (hex).
pub fn login_cnonce(email: &str, password: &str, nonce: &str) -> u64 {
    let nonce = nonce.to_ascii_lowercase();
    (0u64..)
        .find(|c| hex::encode(Sha256::digest(format!("{email}{password}{c}"))).starts_with(&nonce))
        .unwrap_or(0)
}

impl DomainService {
    /// Njalla's website (the API URL without `api/1/`).
    fn njalla_site(&self) -> String {
        let api = self.cfg.njalla_url.as_deref().unwrap_or(super::njalla::DEFAULT_URL);
        api.trim_end_matches('/').trim_end_matches("/api/1").to_string()
    }

    async fn njalla_signup_state(&self) -> Option<NjallaSignup> {
        let e = self.db.config_get(SIGNUP_KEY).await.ok()??;
        serde_json::from_str(&e.value).ok()
    }

    async fn save_njalla_signup(&self, s: &NjallaSignup) -> Result<(), DomainError> {
        self.db
            .config_set(SIGNUP_KEY, &serde_json::to_string(s).unwrap_or_default(), false)
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    pub(super) async fn njalla_signup_view(&self) -> Value {
        if let Some(id) = self
            .njalla_signup_state()
            .await
            .filter(|s| s.state == "done")
            .and_then(|s| s.provider_id)
        {
            if matches!(store::provider(&self.pool, id).await, Ok(None)) {
                self.njalla_provider_removed(id).await;
            }
        }
        match self.njalla_signup_state().await {
            Some(s) => json!({
                "email": s.email, "state": s.state, "since": s.since,
                "error": s.error, "provider_id": s.provider_id,
            }),
            None => Value::Null,
        }
    }

    /// Signs up at Njalla with Allod's own address; confirmation and the token follow in
    /// the background.
    pub async fn njalla_signup(&self) -> Result<(), DomainError> {
        if let Some(s) = self.njalla_signup_state().await {
            if s.state == "activating" && now() - s.since < WAIT_SECS {
                return Err(DomainError::Conflict(
                    "a Njalla account is being confirmed already".into(),
                ));
            }
            if s.state == "done" {
                return Err(DomainError::Conflict("Allod made a Njalla account already".into()));
            }
            // Its provider was removed: log in to the account again for a new token.
            if s.state == "removed" {
                if let Some(password) = self.secrets.read(PASSWORD_SECRET)?.map(|p| p.to_string()) {
                    let web = Web::new(&self.njalla_site()).map_err(DomainError::Internal)?;
                    if let Ok(id) = self.njalla_add_token(&web, &s.email, &password).await {
                        tracing::info!(provider = id, "Njalla: reconnected the account Allod made");
                        let done = NjallaSignup {
                            state: "done".into(),
                            provider_id: Some(id),
                            since: now(),
                            ..s
                        };
                        self.save_njalla_signup(&done).await?;
                        self.trigger();
                        return Ok(());
                    }
                }
            }
        }
        let keys = self.lnvps_keys()?;
        let email = mailbox::address(&keys).map_err(DomainError::Internal)?;
        // The mailbox must be findable before Njalla sends anything.
        self.announce_mailbox(&keys).await.map_err(DomainError::Upstream)?;
        let password = {
            use rand::RngCore;
            let mut b = [0u8; 24];
            rand::rngs::OsRng.fill_bytes(&mut b);
            hex::encode(b)
        };
        self.secrets.write(PASSWORD_SECRET, &password)?;
        let web = Web::new(&self.njalla_site()).map_err(DomainError::Internal)?;
        let page = web.get("/signup/").await.map_err(DomainError::Upstream)?;
        let since = now() - 60;
        let answer = web
            .post_form(
                "/signup/",
                &page,
                &[
                    ("xmpp", "0"),
                    ("email", &email),
                    ("password", &password),
                    ("password_confirm", &password),
                ],
            )
            .await
            .map_err(DomainError::Upstream)?;
        let said = text(&answer);
        if said.contains("already exists") {
            return Err(DomainError::Conflict(format!(
                "Njalla has an account for {email} already; add its token by hand"
            )));
        }
        if hidden(&answer, "password_confirm").is_some()
            || answer.contains("name=\"password_confirm\"")
        {
            return Err(DomainError::Upstream(format!(
                "Njalla did not accept the sign-up: {}",
                short(&said)
            )));
        }
        tracing::info!(%email, "Njalla: signed up, waiting for the confirmation mail");
        let s = NjallaSignup { email, state: "activating".into(), since, ..Default::default() };
        self.save_njalla_signup(&s).await?;
        self.trigger();
        Ok(())
    }

    pub(super) async fn njalla_signup_active(&self) -> bool {
        self.njalla_signup_state().await.is_some_and(|s| s.state == "activating")
    }

    /// Advances a sign-up: confirmation mail → logged-in session → token → provider.
    pub(super) async fn njalla_signup_upkeep(&self) {
        let Some(mut s) = self.njalla_signup_state().await else { return };
        if s.state != "activating" {
            return;
        }
        match self.njalla_signup_step(&mut s).await {
            Ok(false) => {
                if now() - s.since <= WAIT_SECS {
                    return;
                }
                s.state = "failed".into();
                s.error = Some("no confirmation mail from Njalla in 12 hours: start again".into());
            }
            Ok(true) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Njalla sign-up");
                if s.error.as_deref() == Some(e.as_str()) {
                    return;
                }
                s.error = Some(e);
            }
        }
        let _ = self.save_njalla_signup(&s).await;
    }

    async fn njalla_signup_step(&self, s: &mut NjallaSignup) -> Result<bool, String> {
        let keys = self.lnvps_keys().map_err(|e| e.to_string())?;
        let mail = self.read_mail(&keys, s.since).await?;
        let Some(link) = mail.iter().find_map(|m| mailbox::njalla_confirmation(m)) else {
            tracing::debug!(messages = mail.len(), "Njalla: no confirmation mail yet");
            return Ok(false);
        };
        let password = self
            .secrets
            .read(PASSWORD_SECRET)
            .map_err(|e| e.to_string())?
            .ok_or("the Njalla password is missing")?
            .to_string();
        let web = Web::new(&self.njalla_site())?;
        // The link confirms the address and logs this session in.
        let path = link.split_once("/a/").map(|(_, c)| format!("/a/{c}")).ok_or("bad link")?;
        web.get(&path).await?;
        tracing::info!("Njalla: address confirmed through the uid.ovh mailbox");
        let id = self.njalla_add_token(&web, &s.email, &password).await?;
        tracing::info!(provider = id, "Njalla: account ready, added as a DNS provider");
        s.state = "done".into();
        s.error = None;
        s.provider_id = Some(id);
        self.trigger();
        Ok(true)
    }

    /// With a session (logged in, or logged in here with the password): a new API token,
    /// checked, and the provider that uses it.
    async fn njalla_add_token(
        &self,
        web: &Web,
        email: &str,
        password: &str,
    ) -> Result<i64, String> {
        let mut page = web.get("/settings/api/").await?;
        if page.contains("name=\"cnonce\"") {
            // Not logged in (link used before): sign in, with the site's proof of work.
            let nonce = hidden(&page, "nonce").unwrap_or_default();
            let cnonce = login_cnonce(email, password, &nonce).to_string();
            web.post_form(
                "/signin/",
                &page,
                &[("email", email), ("password", password), ("nonce", &nonce), ("cnonce", &cnonce)],
            )
            .await?;
            page = web.get("/settings/api/").await?;
            if page.contains("name=\"cnonce\"") {
                return Err("cannot log in to Njalla".into());
            }
        }
        let csrf = web
            .cookie("csrftoken")
            .or_else(|| hidden(&page, "csrfmiddlewaretoken"))
            .or_else(|| cookie_csrf(&page))
            .ok_or("Njalla's settings page has no CSRF token")?;
        let r = web
            .rpc(
                &csrf,
                "add-token",
                json!({
                    "comment": "Allod", "from": [], "allowed_domains": [], "allowed_methods": [],
                    "allowed_prefixes": [], "allowed_servers": [], "allowed_types": [],
                }),
            )
            .await?;
        let token = r["key"].as_str().ok_or("Njalla returned no token")?.to_string();
        // The token works with the plain API before Allod relies on it.
        self.njalla.check(&token).await.map_err(|e| e.to_string())?;
        let id = store::insert_provider(&self.pool, "njalla", "Njalla")
            .await
            .map_err(|e| e.to_string())?;
        self.secrets.write(&provider_secret(id), &token).map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub(super) async fn njalla_provider_removed(&self, id: i64) {
        if let Some(mut s) = self.njalla_signup_state().await.filter(|s| s.provider_id == Some(id))
        {
            s.state = "removed".into();
            s.provider_id = None;
            let _ = self.save_njalla_signup(&s).await;
        }
    }

    /// The account's login for njal.la (the owner may also use Njalla's website).
    pub async fn njalla_credentials(&self) -> Result<Value, DomainError> {
        let s =
            self.njalla_signup_state().await.ok_or_else(|| super::not_found("Njalla account"))?;
        let password = self.secrets.read(PASSWORD_SECRET)?.map(|p| p.to_string());
        Ok(json!({"email": s.email, "password": password}))
    }
}

/// Some pages carry the CSRF token only in a script (`csrftoken`).
fn cookie_csrf(page: &str) -> Option<String> {
    let i = page.find("csrftoken")?;
    let rest = &page[i..];
    let q = rest.find(['\'', '"'])?;
    let v: String = rest[q + 1..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (v.len() >= 32).then_some(v)
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_and_proof_of_work() {
        let page = r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="abc123"><input type="hidden" name="nonce" value="badc"></form>"#;
        assert_eq!(hidden(page, "csrfmiddlewaretoken").as_deref(), Some("abc123"));
        assert_eq!(hidden(page, "nonce").as_deref(), Some("badc"));
        assert_eq!(hidden(page, "missing"), None);
        let c = login_cnonce("a@b.c", "pw", "ab");
        assert!(hex::encode(Sha256::digest(format!("a@b.cpw{c}"))).starts_with("ab"));
        assert_eq!(text("<p>Address <b>already</b> exists</p>"), "Address already exists");
    }
}
