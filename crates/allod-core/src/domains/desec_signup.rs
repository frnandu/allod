//! A deSEC account made by Allod (SPEC §12.1): the owner solves deSEC's captcha; Allod
//! registers its own `npub…@uid.ovh` address (the gateway account key's Nostr mailbox,
//! as for LNVPS) with a random password, follows the activation link from the mailbox,
//! logs in, creates an API token and adds deSEC as a DNS provider.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{mailbox, now, provider_secret, store, DomainError, DomainService};

/// Config key of the sign-up's progress.
const SIGNUP_KEY: &str = "domains.desec_signup";
/// Secret file of the account's password.
const PASSWORD_SECRET: &str = "desec_password";
/// deSEC's activation links expire after 12 hours.
const LINK_SECS: i64 = 12 * 3600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Signup {
    pub email: String,
    /// `activating` (waiting for the mail), `done`, `failed`, or `removed` (its provider
    /// was removed; the account and password remain for reconnecting).
    pub state: String,
    pub since: i64,
    pub error: Option<String>,
    pub provider_id: Option<i64>,
}

impl DomainService {
    /// The mailbox's relay lists (tests: nothing to publish).
    pub(super) async fn announce_mailbox(&self, keys: &nostr::prelude::Keys) -> Result<(), String> {
        #[cfg(test)]
        if self.test_mail.lock().unwrap().is_some() {
            return Ok(());
        }
        mailbox::publish_relay_lists(keys).await
    }

    /// The mailbox's messages since `since` (tests: the ones set by the test).
    pub(super) async fn read_mail(
        &self,
        keys: &nostr::prelude::Keys,
        since: i64,
    ) -> Result<Vec<String>, String> {
        #[cfg(test)]
        if let Some(m) = self.test_mail.lock().unwrap().clone() {
            return Ok(m);
        }
        mailbox::fetch(keys, since).await
    }

    async fn signup(&self) -> Option<Signup> {
        let e = self.db.config_get(SIGNUP_KEY).await.ok()??;
        serde_json::from_str(&e.value).ok()
    }

    async fn save_signup(&self, s: &Signup) -> Result<(), DomainError> {
        self.db
            .config_set(SIGNUP_KEY, &serde_json::to_string(s).unwrap_or_default(), false)
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    /// The sign-up as the UI shows it (no password).
    pub(super) async fn desec_signup_view(&self) -> Value {
        // Its provider removed before Allod tracked that: the account can be reconnected.
        if let Some(mut s) = self.signup().await.filter(|s| s.state == "done") {
            if let Some(id) = s.provider_id {
                if matches!(store::provider(&self.pool, id).await, Ok(None)) {
                    s.state = "removed".into();
                    s.provider_id = None;
                    let _ = self.save_signup(&s).await;
                }
            }
        }
        match self.signup().await {
            Some(s) => json!({
                "email": s.email, "state": s.state, "since": s.since,
                "error": s.error, "provider_id": s.provider_id,
            }),
            None => Value::Null,
        }
    }

    /// A captcha from deSEC for the owner: `{id, challenge}` (base64 PNG).
    pub async fn desec_captcha(&self) -> Result<Value, DomainError> {
        let (id, png) =
            self.desec.captcha().await.map_err(|e| DomainError::Upstream(e.to_string()))?;
        Ok(json!({"id": id, "challenge": png}))
    }

    /// Registers the account with the owner's captcha answer; the rest follows in the
    /// background (activation mail, token, provider).
    pub async fn desec_signup(&self, captcha_id: &str, solution: &str) -> Result<(), DomainError> {
        if let Some(s) = self.signup().await {
            if s.state == "activating" && now() - s.since < LINK_SECS {
                return Err(DomainError::Conflict(
                    "a deSEC account is being activated already".into(),
                ));
            }
            if s.state == "done" {
                return Err(DomainError::Conflict("Allod made a deSEC account already".into()));
            }
            // Its provider was removed: the account may still exist; a new token for it.
            if s.state == "removed" && self.desec_reconnect(&s.email).await? {
                return Ok(());
            }
        }
        let solution = solution.trim();
        if solution.is_empty() {
            return Err(DomainError::BadRequest("type the characters of the picture".into()));
        }
        let keys = self.lnvps_keys()?;
        let email = mailbox::address(&keys).map_err(DomainError::Internal)?;
        self.announce_mailbox(&keys).await.map_err(DomainError::Upstream)?;
        let password = {
            use rand::RngCore;
            let mut b = [0u8; 24];
            rand::rngs::OsRng.fill_bytes(&mut b);
            hex::encode(b)
        };
        self.secrets.write(PASSWORD_SECRET, &password)?;
        self.desec.register(&email, &password, captcha_id, solution).await.map_err(|e| {
            let e = e.to_string();
            if e.to_ascii_lowercase().contains("captcha") {
                DomainError::BadRequest("the captcha answer was not accepted: try a new one".into())
            } else {
                DomainError::Upstream(e)
            }
        })?;
        tracing::info!(%email, "deSEC: account registered, waiting for the activation mail");
        // A minute back: the mail may be stamped a little before our clock.
        let s =
            Signup { email, state: "activating".into(), since: now() - 60, ..Default::default() };
        self.save_signup(&s).await?;
        self.trigger();
        Ok(())
    }

    /// Whether a sign-up waits for its mail (checked every few seconds).
    pub(super) async fn desec_signup_active(&self) -> bool {
        self.signup().await.is_some_and(|s| s.state == "activating")
    }

    /// Advances a sign-up: activation mail → activate → token → provider.
    pub(super) async fn desec_signup_upkeep(&self) {
        let Some(mut s) = self.signup().await else { return };
        if s.state != "activating" {
            return;
        }
        let result = self.desec_signup_step(&mut s).await;
        match result {
            Ok(false) => {
                if now() - s.since > LINK_SECS {
                    s.state = "failed".into();
                    s.error = Some("no activation mail from deSEC in 12 hours: start again".into());
                } else {
                    return;
                }
            }
            Ok(true) => {}
            Err(e) => {
                tracing::warn!(error = %e, "deSEC sign-up");
                if s.error.as_deref() == Some(e.as_str()) {
                    return;
                }
                s.error = Some(e);
            }
        }
        let _ = self.save_signup(&s).await;
    }

    /// True once done.
    async fn desec_signup_step(&self, s: &mut Signup) -> Result<bool, String> {
        let keys = self.lnvps_keys().map_err(|e| e.to_string())?;
        let mail = self.read_mail(&keys, s.since).await?;
        let Some(code) = mail.iter().find_map(|m| mailbox::desec_activation(m)) else {
            tracing::debug!(messages = mail.len(), "deSEC: no activation mail yet");
            return Ok(false);
        };
        // Activated before (a restart after this step): logging in works either way.
        if let Err(e) = self.desec.activate(&code).await {
            tracing::info!(error = %e, "deSEC: activation link not accepted (already used?)");
        } else {
            tracing::info!("deSEC: account activated through the uid.ovh mailbox");
        }
        let password = self
            .secrets
            .read(PASSWORD_SECRET)
            .map_err(|e| e.to_string())?
            .ok_or("the deSEC password is missing")?;
        let login = self.desec.login(&s.email, &password).await.map_err(|e| e.to_string())?;
        let token = self.desec.create_token(&login, "Allod").await.map_err(|e| e.to_string());
        self.desec.logout(&login).await;
        let token = token?;
        let id = store::insert_provider(&self.pool, "desec", "deSEC")
            .await
            .map_err(|e| e.to_string())?;
        self.secrets.write(&provider_secret(id), &token).map_err(|e| e.to_string())?;
        tracing::info!(provider = id, "deSEC: account ready, added as a DNS provider");
        s.state = "done".into();
        s.error = None;
        s.provider_id = Some(id);
        self.trigger();
        Ok(true)
    }

    /// Logs in to the account Allod made before and adds it again as a provider with a
    /// new token. False when the account is gone (deleted at desec.io).
    async fn desec_reconnect(&self, email: &str) -> Result<bool, DomainError> {
        let Some(password) = self.secrets.read(PASSWORD_SECRET)?.map(|p| p.to_string()) else {
            return Ok(false);
        };
        let Ok(login) = self.desec.login(email, &password).await else { return Ok(false) };
        let token = self.desec.create_token(&login, "Allod").await;
        self.desec.logout(&login).await;
        let token = token.map_err(|e| DomainError::Upstream(e.to_string()))?;
        let id = store::insert_provider(&self.pool, "desec", "deSEC").await?;
        self.secrets.write(&provider_secret(id), &token)?;
        tracing::info!(provider = id, "deSEC: reconnected the account Allod made");
        let s = Signup {
            email: email.to_string(),
            state: "done".into(),
            since: now(),
            error: None,
            provider_id: Some(id),
        };
        self.save_signup(&s).await?;
        self.trigger();
        Ok(true)
    }

    /// A provider was removed: a sign-up that made it becomes `removed` (the account
    /// and its password stay, so it can be reconnected).
    pub(super) async fn signups_provider_removed(&self, id: i64) {
        if let Some(mut s) = self.signup().await.filter(|s| s.provider_id == Some(id)) {
            s.state = "removed".into();
            s.provider_id = None;
            let _ = self.save_signup(&s).await;
        }
        self.njalla_provider_removed(id).await;
    }

    /// The account's login for desec.io (the owner may also use deSEC's website).
    pub async fn desec_credentials(&self) -> Result<Value, DomainError> {
        let s = self.signup().await.ok_or_else(|| super::not_found("deSEC account"))?;
        let password = self.secrets.read(PASSWORD_SECRET)?.map(|p| p.to_string());
        Ok(json!({"email": s.email, "password": password}))
    }
}
