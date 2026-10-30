//! Owners, API tokens and the audit log (SPEC §11.1, §6.2).

use crate::{Db, DbError};

/// A Nostr key allowed to log in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// x-only public key, lowercase hex.
    pub pubkey: String,
    /// Free-text label.
    pub label: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// A stored API token (the token itself is never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiToken {
    /// Row id.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// "full" or "readonly".
    pub permission: String,
    /// SHA-256 of the token, hex.
    pub token_hash: String,
    /// Unix seconds, if the token expires.
    pub expires_at: Option<i64>,
    /// Unix seconds of the last use.
    pub last_used_at: Option<i64>,
    /// Unix seconds.
    pub created_at: i64,
}

type TokenRow = (i64, String, String, String, Option<i64>, Option<i64>, i64);

/// Audit retention and approximate JSON payload budget.
pub const AUDIT_MAX_ROWS: i64 = 100_000;
pub const AUDIT_MAX_DATA_BYTES: i64 = 32 * 1024 * 1024;
pub const AUDIT_RETENTION_SECS: i64 = 30 * 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditUsage {
    pub rows: i64,
    pub data_bytes: i64,
}

fn token_from_row(r: TokenRow) -> ApiToken {
    ApiToken {
        id: r.0,
        name: r.1,
        permission: r.2,
        token_hash: r.3,
        expires_at: r.4,
        last_used_at: r.5,
        created_at: r.6,
    }
}

/// Runs the same query text on whichever pool is active.
macro_rules! on_pool {
    ($db:expr, |$pool:ident| $body:expr) => {
        match $db {
            Db::Sqlite($pool) => $body,
            Db::Postgres($pool) => $body,
        }
    };
}

impl Db {
    /// All owners, oldest first.
    pub async fn owners(&self) -> Result<Vec<Owner>, DbError> {
        const Q: &str = "SELECT pubkey, label, created_at FROM owners ORDER BY created_at, pubkey";
        let rows: Vec<(String, String, i64)> =
            on_pool!(self, |pool| sqlx::query_as(Q).fetch_all(pool).await?);
        Ok(rows
            .into_iter()
            .map(|(pubkey, label, created_at)| Owner { pubkey, label, created_at })
            .collect())
    }

    /// True when `pubkey` is an owner.
    pub async fn is_owner(&self, pubkey: &str) -> Result<bool, DbError> {
        const Q: &str = "SELECT COUNT(*) FROM owners WHERE pubkey = $1";
        let (n,): (i64,) =
            on_pool!(self, |pool| sqlx::query_as(Q).bind(pubkey).fetch_one(pool).await?);
        Ok(n > 0)
    }

    /// Number of owners.
    pub async fn owner_count(&self) -> Result<i64, DbError> {
        const Q: &str = "SELECT COUNT(*) FROM owners";
        let (n,): (i64,) = on_pool!(self, |pool| sqlx::query_as(Q).fetch_one(pool).await?);
        Ok(n)
    }

    /// Adds an owner. Returns false if it already existed.
    pub async fn add_owner(&self, pubkey: &str, label: &str, now: i64) -> Result<bool, DbError> {
        const Q: &str = "INSERT INTO owners (pubkey, label, created_at) VALUES ($1, $2, $3) \
                         ON CONFLICT (pubkey) DO NOTHING";
        let affected = on_pool!(self, |pool| sqlx::query(Q)
            .bind(pubkey)
            .bind(label)
            .bind(now)
            .execute(pool)
            .await?
            .rows_affected());
        Ok(affected > 0)
    }

    /// Removes an owner unless it is the last one. Returns `Ok(false)` if it did not
    /// exist and an error if it is the last owner.
    pub async fn remove_owner(&self, pubkey: &str) -> Result<bool, DbError> {
        // Single statement, so two concurrent removals cannot delete the last two owners.
        const Q: &str = "DELETE FROM owners WHERE pubkey = $1 \
                         AND (SELECT COUNT(*) FROM owners) > 1";
        if !self.is_owner(pubkey).await? {
            return Ok(false);
        }
        let affected =
            on_pool!(self, |pool| sqlx::query(Q).bind(pubkey).execute(pool).await?.rows_affected());
        if affected == 0 {
            return Err(DbError::Constraint("cannot remove the last owner".into()));
        }
        Ok(true)
    }

    /// Stores a new API token hash; returns the row id.
    pub async fn create_api_token(
        &self,
        name: &str,
        permission: &str,
        token_hash: &str,
        expires_at: Option<i64>,
        now: i64,
    ) -> Result<i64, DbError> {
        const Q: &str =
            "INSERT INTO api_tokens (name, permission, token_hash, expires_at, created_at) \
                         VALUES ($1, $2, $3, $4, $5) RETURNING id";
        let (id,): (i64,) = on_pool!(self, |pool| sqlx::query_as(Q)
            .bind(name)
            .bind(permission)
            .bind(token_hash)
            .bind(expires_at)
            .bind(now)
            .fetch_one(pool)
            .await?);
        Ok(id)
    }

    /// All API tokens, newest first.
    pub async fn api_tokens(&self) -> Result<Vec<ApiToken>, DbError> {
        const Q: &str = "SELECT id, name, permission, token_hash, expires_at, last_used_at, \
                         created_at FROM api_tokens ORDER BY id DESC";
        let rows: Vec<TokenRow> = on_pool!(self, |pool| sqlx::query_as(Q).fetch_all(pool).await?);
        Ok(rows.into_iter().map(token_from_row).collect())
    }

    /// Looks a token up by hash and records its use.
    pub async fn use_api_token(
        &self,
        token_hash: &str,
        now: i64,
    ) -> Result<Option<ApiToken>, DbError> {
        const Q: &str = "UPDATE api_tokens SET last_used_at = $2 WHERE token_hash = $1 \
                         RETURNING id, name, permission, token_hash, expires_at, last_used_at, \
                         created_at";
        let row: Option<TokenRow> = on_pool!(self, |pool| sqlx::query_as(Q)
            .bind(token_hash)
            .bind(now)
            .fetch_optional(pool)
            .await?);
        Ok(row.map(token_from_row))
    }

    /// Deletes a token. Returns true if it existed.
    pub async fn delete_api_token(&self, id: i64) -> Result<bool, DbError> {
        const Q: &str = "DELETE FROM api_tokens WHERE id = $1";
        let affected =
            on_pool!(self, |pool| sqlx::query(Q).bind(id).execute(pool).await?.rows_affected());
        Ok(affected > 0)
    }

    /// Appends an audit log record. `data` is a JSON object.
    pub async fn audit(
        &self,
        kind: &str,
        app_id: Option<i64>,
        data: &str,
        now: i64,
    ) -> Result<(), DbError> {
        const Q: &str =
            "INSERT INTO audit_log (kind, app_id, data, created_at) VALUES ($1, $2, $3, $4)";
        on_pool!(self, |pool| sqlx::query(Q)
            .bind(kind)
            .bind(app_id)
            .bind(data)
            .bind(now)
            .execute(pool)
            .await
            .map(|_| ())?);
        Ok(())
    }

    /// Audit table size before pruning. `data_bytes` counts stored JSON text.
    pub async fn audit_usage(&self) -> Result<AuditUsage, DbError> {
        let (rows, data_bytes): (i64, i64) =
            match self {
                Db::Sqlite(pool) => sqlx::query_as(
                    "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(data AS BLOB))), 0) FROM audit_log",
                )
                .fetch_one(pool)
                .await?,
                Db::Postgres(pool) => {
                    sqlx::query_as(
                        "SELECT COUNT(*), COALESCE(SUM(OCTET_LENGTH(data)), 0) FROM audit_log",
                    )
                    .fetch_one(pool)
                    .await?
                }
            };
        Ok(AuditUsage { rows, data_bytes })
    }

    /// Deletes old rows and oldest rows beyond count or payload budget in batches.
    pub async fn prune_audit_log(&self, now: i64) -> Result<(AuditUsage, u64), DbError> {
        self.prune_audit_log_with_limits(now, AUDIT_MAX_ROWS, AUDIT_MAX_DATA_BYTES).await
    }

    async fn prune_audit_log_with_limits(
        &self,
        now: i64,
        max_rows: i64,
        max_data_bytes: i64,
    ) -> Result<(AuditUsage, u64), DbError> {
        let before = self.audit_usage().await?;
        const ROW_CUTOFF: &str = "SELECT COALESCE((\
            SELECT id FROM audit_log ORDER BY id DESC LIMIT 1 OFFSET $1), 0)";
        let (row_cutoff,): (i64,) = on_pool!(self, |pool| sqlx::query_as(ROW_CUTOFF)
            .bind(max_rows)
            .fetch_one(pool)
            .await?);
        let (byte_cutoff,): (i64,) = match self {
            Db::Sqlite(pool) => {
                sqlx::query_as(
                    "SELECT COALESCE(MAX(id), 0) FROM (\
                SELECT id, SUM(size) OVER (ORDER BY id DESC) AS used \
                FROM (SELECT id, LENGTH(CAST(data AS BLOB)) AS size \
                      FROM audit_log ORDER BY id DESC LIMIT $1) recent\
                ) ranked WHERE used > $2",
                )
                .bind(max_rows)
                .bind(max_data_bytes)
                .fetch_one(pool)
                .await?
            }
            Db::Postgres(pool) => {
                sqlx::query_as(
                    "SELECT COALESCE(MAX(id), 0) FROM (\
                SELECT id, SUM(size) OVER (ORDER BY id DESC) AS used \
                FROM (SELECT id, OCTET_LENGTH(data) AS size \
                      FROM audit_log ORDER BY id DESC LIMIT $1) recent\
                ) ranked WHERE used > $2",
                )
                .bind(max_rows)
                .bind(max_data_bytes)
                .fetch_one(pool)
                .await?
            }
        };
        let cutoff = row_cutoff.max(byte_cutoff);
        const DELETE: &str = "DELETE FROM audit_log WHERE id IN (\
             SELECT id FROM audit_log WHERE created_at < $1 OR id <= $2 \
             ORDER BY id LIMIT 5000)";
        let mut deleted = 0;
        loop {
            let n = on_pool!(self, |pool| sqlx::query(DELETE)
                .bind(now.saturating_sub(AUDIT_RETENTION_SECS))
                .bind(cutoff)
                .execute(pool)
                .await?
                .rows_affected());
            deleted += n;
            if n < 5000 {
                break;
            }
        }
        Ok((before, deleted))
    }

    /// Latest audit records of one kind (all kinds when `kind` is None), newest first.
    pub async fn audit_entries(
        &self,
        kind: Option<&str>,
        limit: i64,
    ) -> Result<Vec<(i64, String, Option<i64>, String, i64)>, DbError> {
        const Q: &str = "SELECT id, kind, app_id, data, created_at FROM audit_log \
                         WHERE ($1 IS NULL OR kind = $1) ORDER BY id DESC LIMIT $2";
        let rows =
            on_pool!(self, |pool| sqlx::query_as(Q).bind(kind).bind(limit).fetch_all(pool).await?);
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}/a.db", dir.path().display())).await.unwrap();
        (db, dir)
    }

    #[tokio::test]
    async fn owners_cannot_drop_to_zero() {
        let (db, _d) = db().await;
        assert!(db.add_owner("aa", "first", 1).await.unwrap());
        assert!(!db.add_owner("aa", "dup", 2).await.unwrap());
        assert!(db.add_owner("bb", "second", 3).await.unwrap());
        assert!(db.is_owner("bb").await.unwrap());
        assert_eq!(db.owner_count().await.unwrap(), 2);
        assert!(db.remove_owner("aa").await.unwrap());
        assert!(matches!(db.remove_owner("bb").await, Err(DbError::Constraint(_))));
        assert!(!db.remove_owner("zz").await.unwrap());
        assert_eq!(db.owners().await.unwrap()[0].pubkey, "bb");
    }

    #[tokio::test]
    async fn api_tokens_lifecycle() {
        let (db, _d) = db().await;
        let id = db.create_api_token("ci", "readonly", "h1", Some(100), 10).await.unwrap();
        let used = db.use_api_token("h1", 20).await.unwrap().unwrap();
        assert_eq!(
            (used.id, used.last_used_at, used.permission.as_str()),
            (id, Some(20), "readonly")
        );
        assert!(db.use_api_token("nope", 20).await.unwrap().is_none());
        assert_eq!(db.api_tokens().await.unwrap().len(), 1);
        assert!(db.delete_api_token(id).await.unwrap());
        assert!(db.use_api_token("h1", 30).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn audit_log_append_and_filter() {
        let (db, _d) = db().await;
        db.audit("login", None, "{}", 1).await.unwrap();
        db.audit("owner_added", None, r#"{"pubkey":"aa"}"#, 2).await.unwrap();
        assert_eq!(db.audit_entries(None, 10).await.unwrap().len(), 2);
        let only = db.audit_entries(Some("login"), 10).await.unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].1, "login");
    }

    #[tokio::test]
    async fn audit_log_stays_within_row_and_payload_budget() {
        let (db, _dir) = db().await;
        for cycle in 0..3 {
            for i in 0..100 {
                db.audit(
                    "login_failed",
                    None,
                    &format!(r#"{{"attempt":{i},"cycle":{cycle}}}"#),
                    100,
                )
                .await
                .unwrap();
            }
            let (before, deleted) = db.prune_audit_log_with_limits(100, 5, 100).await.unwrap();
            assert!(before.rows >= 100);
            assert!(deleted >= 95);
            let after = db.audit_usage().await.unwrap();
            assert!(after.rows <= 5, "{after:?}");
            assert!(after.data_bytes <= 100, "{after:?}");
        }
        db.audit("old", None, "{}", 1).await.unwrap();
        db.audit("new", None, "{}", AUDIT_RETENTION_SECS + 2).await.unwrap();
        db.prune_audit_log(AUDIT_RETENTION_SECS + 2).await.unwrap();
        assert!(db.audit_entries(Some("old"), 10).await.unwrap().is_empty());
        assert_eq!(db.audit_entries(Some("new"), 10).await.unwrap().len(), 1);
    }
}
