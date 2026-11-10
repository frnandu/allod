//! NIP-05 names on domains served by this Allod instance.

use nostr::prelude::*;
use serde::Serialize;
use sqlx::Row;

use super::{now, store, DomainError, DomainService};

#[derive(Debug, Serialize)]
pub struct Name {
    pub id: i64,
    pub account_id: Option<i64>,
    pub domain_id: i64,
    pub domain: String,
    pub username: String,
    pub pubkey: String,
    pub npub: String,
}

fn host_domain(host: &str) -> String {
    host.trim().split(':').next().unwrap_or_default().trim_end_matches('.').to_ascii_lowercase()
}

impl DomainService {
    pub async fn nip05_list(&self) -> Result<Vec<Name>, DomainError> {
        let rows = sqlx::query(
            "SELECT n.id, n.account_id, n.domain_id, d.name AS domain, n.username, n.pubkey \
             FROM nip05_names n JOIN domains d ON d.id = n.domain_id ORDER BY d.name, n.username",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                let pubkey: String = r.try_get("pubkey")?;
                let npub = PublicKey::from_hex(&pubkey)
                    .map_err(|e| DomainError::Internal(e.to_string()))?
                    .to_bech32()
                    .map_err(|e| DomainError::Internal(e.to_string()))?;
                Ok(Name {
                    id: r.try_get("id")?,
                    account_id: r.try_get("account_id")?,
                    domain_id: r.try_get("domain_id")?,
                    domain: r.try_get("domain")?,
                    username: r.try_get("username")?,
                    pubkey,
                    npub,
                })
            })
            .collect()
    }

    pub async fn nip05_add(
        &self,
        domain_id: i64,
        username: &str,
        npub: &str,
    ) -> Result<(), DomainError> {
        self.nip05_add_for_account(domain_id, username, npub, None).await
    }

    async fn check_nip05_account(&self, account_id: i64) -> Result<(), DomainError> {
        let account = self
            .apps()?
            .get(account_id)
            .await
            .map_err(|_| DomainError::NotFound("account".into()))?;
        if !account.is_account() || account.closed_at.is_some() {
            return Err(DomainError::BadRequest("choose an open account".into()));
        }
        Ok(())
    }

    pub async fn nip05_add_for_account(
        &self,
        domain_id: i64,
        username: &str,
        npub: &str,
        account_id: Option<i64>,
    ) -> Result<(), DomainError> {
        let domain = store::domain(&self.pool, domain_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("domain".into()))?;
        if !domain.served() {
            return Err(DomainError::BadRequest("serve this domain over HTTPS first".into()));
        }
        let username = if username.trim() == "_" {
            "_".to_string()
        } else {
            crate::apps::validate_username(username)
                .map_err(|e| DomainError::BadRequest(e.to_string()))?
        };
        let key = PublicKey::from_bech32(npub.trim())
            .map_err(|_| DomainError::BadRequest("enter a valid npub".into()))?;
        if let Some(id) = account_id {
            self.check_nip05_account(id).await?;
        }
        sqlx::query(
            "INSERT INTO nip05_names (domain_id, username, pubkey, account_id, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(domain_id)
        .bind(username)
        .bind(key.to_hex())
        .bind(account_id)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn nip05_set_account(
        &self,
        id: i64,
        account_id: Option<i64>,
    ) -> Result<(), DomainError> {
        self.nip05_update(id, None, None, None, Some(account_id)).await
    }

    pub async fn nip05_update(
        &self,
        id: i64,
        domain_id: Option<i64>,
        username: Option<&str>,
        npub: Option<&str>,
        account_id: Option<Option<i64>>,
    ) -> Result<(), DomainError> {
        let current = self
            .nip05_list()
            .await?
            .into_iter()
            .find(|n| n.id == id)
            .ok_or_else(|| DomainError::NotFound("NIP-05 name".into()))?;
        let domain_id = domain_id.unwrap_or(current.domain_id);
        let domain = store::domain(&self.pool, domain_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("domain".into()))?;
        if !domain.served() {
            return Err(DomainError::BadRequest("serve this domain over HTTPS first".into()));
        }
        let username = match username {
            Some(value) if value.trim() == "_" => "_".to_string(),
            Some(value) => crate::apps::validate_username(value)
                .map_err(|e| DomainError::BadRequest(e.to_string()))?,
            None => current.username,
        };
        let pubkey = match npub {
            Some(value) => PublicKey::from_bech32(value.trim())
                .map_err(|_| DomainError::BadRequest("enter a valid npub".into()))?
                .to_hex(),
            None => current.pubkey,
        };
        let account_id = account_id.unwrap_or(current.account_id);
        if account_id != current.account_id {
            if let Some(account_id) = account_id {
                self.check_nip05_account(account_id).await?;
            }
        }
        let changed = sqlx::query(
            "UPDATE nip05_names SET domain_id = $1, username = $2, pubkey = $3, account_id = $4 WHERE id = $5",
        )
            .bind(domain_id)
            .bind(username)
            .bind(pubkey)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(DomainError::NotFound("NIP-05 name".into()));
        }
        Ok(())
    }

    pub async fn nip05_remove(&self, id: i64) -> Result<(), DomainError> {
        let n = sqlx::query("DELETE FROM nip05_names WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(DomainError::NotFound("NIP-05 name".into()));
        }
        Ok(())
    }

    /// None means this host is not one of our served domains. Only the requested name is
    /// returned, so the public endpoint cannot enumerate a domain's names.
    pub async fn nip05_names(
        &self,
        host: &str,
        username: &str,
    ) -> Result<Option<serde_json::Value>, DomainError> {
        let Some(d) = store::domain_by_name(&self.pool, &host_domain(host)).await? else {
            return Ok(None);
        };
        if !d.served() {
            return Ok(None);
        }
        let pubkey: Option<String> = sqlx::query_scalar(
            "SELECT pubkey FROM nip05_names WHERE domain_id = $1 AND username = $2",
        )
        .bind(d.id)
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        let mut names = serde_json::Map::new();
        if let Some(pubkey) = pubkey {
            names.insert(username.to_string(), serde_json::Value::String(pubkey));
        }
        Ok(Some(serde_json::json!({"names": names})))
    }
}
