//! Key/value settings (`config` table, SPEC §6.2). Encrypted values are sealed by
//! `allod-keys` before they reach this module; the database never sees plaintext
//! secrets and never decrypts.

use crate::{Db, DbError};

/// One stored setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    /// Setting name.
    pub key: String,
    /// Plain value, or a sealed `v1:` value when `encrypted` is true.
    pub value: String,
    /// Whether `value` is sealed.
    pub encrypted: bool,
}

/// Well-known keys.
pub mod keys {
    /// Argon2id parameters of the vault (`VaultParams::encode`).
    pub const VAULT_PARAMS: &str = "vault.params";
    /// Hex salt of the vault.
    pub const VAULT_SALT: &str = "vault.salt";
    /// Sealed check value that verifies the unlock code.
    pub const VAULT_CHECK: &str = "vault.check";
    /// Sealed recovery phrase.
    pub const RECOVERY_PHRASE: &str = "secret.recovery_phrase";
    /// Sealed ldk-server macaroon used by Allod.
    pub const LDK_SERVER_MACAROON: &str = "secret.ldk_server_macaroon";
    /// ldk-server TLS certificate (PEM, not secret).
    pub const LDK_SERVER_CERT: &str = "ldk_server.cert_pem";
    /// ldk-server address.
    pub const LDK_SERVER_ADDRESS: &str = "ldk_server.address";
    /// "true" once setup finished.
    pub const SETUP_COMPLETED: &str = "setup.completed";
    /// Display currency (ISO 4217) or "none".
    pub const FIAT_CURRENCY: &str = "fiat.currency";
    /// Rate source: yadio | mempool | coingecko.
    pub const FIAT_SOURCE: &str = "fiat.source";
}

impl Db {
    /// Reads one setting.
    pub async fn config_get(&self, key: &str) -> Result<Option<ConfigEntry>, DbError> {
        let row: Option<(String, String, bool)> = match self {
            Db::Sqlite(pool) => {
                sqlx::query_as("SELECT key, value, encrypted FROM config WHERE key = $1")
                    .bind(key)
                    .fetch_optional(pool)
                    .await?
            }
            Db::Postgres(pool) => {
                sqlx::query_as("SELECT key, value, encrypted FROM config WHERE key = $1")
                    .bind(key)
                    .fetch_optional(pool)
                    .await?
            }
        };
        Ok(row.map(|(key, value, encrypted)| ConfigEntry { key, value, encrypted }))
    }

    /// Inserts or replaces one setting.
    pub async fn config_set(&self, key: &str, value: &str, encrypted: bool) -> Result<(), DbError> {
        self.config_set_many(&[ConfigEntry {
            key: key.to_string(),
            value: value.to_string(),
            encrypted,
        }])
        .await
    }

    /// Inserts or replaces several settings in one transaction (all or nothing).
    /// Used at setup and when re-encrypting every secret after an unlock code change.
    pub async fn config_set_many(&self, entries: &[ConfigEntry]) -> Result<(), DbError> {
        const UPSERT: &str = "INSERT INTO config (key, value, encrypted, updated_at) \
             VALUES ($1, $2, $3, CURRENT_TIMESTAMP) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, \
             encrypted = excluded.encrypted, updated_at = CURRENT_TIMESTAMP";
        match self {
            Db::Sqlite(pool) => {
                let mut tx = pool.begin().await?;
                for e in entries {
                    sqlx::query(UPSERT)
                        .bind(&e.key)
                        .bind(&e.value)
                        .bind(e.encrypted)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
            }
            Db::Postgres(pool) => {
                let mut tx = pool.begin().await?;
                for e in entries {
                    sqlx::query(UPSERT)
                        .bind(&e.key)
                        .bind(&e.value)
                        .bind(e.encrypted)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
            }
        }
        Ok(())
    }

    /// All encrypted settings (for re-encryption after an unlock code change).
    pub async fn config_encrypted_entries(&self) -> Result<Vec<ConfigEntry>, DbError> {
        const SELECT: &str =
            "SELECT key, value, encrypted FROM config WHERE encrypted = TRUE ORDER BY key";
        let rows: Vec<(String, String, bool)> = match self {
            Db::Sqlite(pool) => sqlx::query_as(SELECT).fetch_all(pool).await?,
            Db::Postgres(pool) => sqlx::query_as(SELECT).fetch_all(pool).await?,
        };
        Ok(rows
            .into_iter()
            .map(|(key, value, encrypted)| ConfigEntry { key, value, encrypted })
            .collect())
    }

    /// Deletes one setting. Returns true if it existed.
    pub async fn config_delete(&self, key: &str) -> Result<bool, DbError> {
        let affected = match self {
            Db::Sqlite(pool) => sqlx::query("DELETE FROM config WHERE key = $1")
                .bind(key)
                .execute(pool)
                .await?
                .rows_affected(),
            Db::Postgres(pool) => sqlx::query("DELETE FROM config WHERE key = $1")
                .bind(key)
                .execute(pool)
                .await?
                .rows_affected(),
        };
        Ok(affected > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}/c.db", dir.path().display())).await.unwrap();
        (db, dir)
    }

    #[tokio::test]
    async fn set_get_overwrite_delete() {
        let (db, _dir) = db().await;
        assert!(db.config_get("a").await.unwrap().is_none());
        db.config_set("a", "1", false).await.unwrap();
        db.config_set("a", "v1:xyz", true).await.unwrap();
        let e = db.config_get("a").await.unwrap().unwrap();
        assert_eq!((e.value.as_str(), e.encrypted), ("v1:xyz", true));
        assert!(db.config_delete("a").await.unwrap());
        assert!(!db.config_delete("a").await.unwrap());
    }

    #[tokio::test]
    async fn set_many_is_atomic_and_lists_encrypted() {
        let (db, _dir) = db().await;
        let entries = vec![
            ConfigEntry { key: "s1".into(), value: "v1:a".into(), encrypted: true },
            ConfigEntry { key: "p1".into(), value: "plain".into(), encrypted: false },
            ConfigEntry { key: "s2".into(), value: "v1:b".into(), encrypted: true },
        ];
        db.config_set_many(&entries).await.unwrap();
        let enc = db.config_encrypted_entries().await.unwrap();
        assert_eq!(enc.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
    }
}
