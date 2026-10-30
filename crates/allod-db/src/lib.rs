//! Database access for Allod. SQLite is the default; PostgreSQL is supported
//! with the same schema (SPEC §6.2).

mod auth;
mod config;

pub use auth::{ApiToken, AuditUsage, Owner, AUDIT_MAX_DATA_BYTES, AUDIT_MAX_ROWS};
pub use config::{keys as config_keys, ConfigEntry};

use std::str::FromStr;

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

/// Database errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The URL scheme is not `sqlite:` or `postgres:`.
    #[error("unsupported database url (expected sqlite:// or postgres://)")]
    UnsupportedUrl,
    /// Driver error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// Migration error.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A business constraint was violated (e.g. removing the last owner).
    #[error("{0}")]
    Constraint(String),
}

/// A connection pool for one of the supported databases.
#[derive(Debug, Clone)]
pub enum Db {
    /// SQLite pool.
    Sqlite(SqlitePool),
    /// PostgreSQL pool.
    Postgres(PgPool),
}

impl Db {
    /// Connects and runs pending migrations.
    pub async fn connect(url: &str) -> Result<Self, DbError> {
        let db = if url.starts_with("sqlite:") {
            let opts = SqliteConnectOptions::from_str(url)?
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .foreign_keys(true);
            Db::Sqlite(SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?)
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            Db::Postgres(PgPoolOptions::new().max_connections(8).connect(url).await?)
        } else {
            return Err(DbError::UnsupportedUrl);
        };
        db.migrate().await?;
        Ok(db)
    }

    async fn migrate(&self) -> Result<(), DbError> {
        match self {
            Db::Sqlite(pool) => sqlx::migrate!("./migrations/sqlite").run(pool).await?,
            Db::Postgres(pool) => sqlx::migrate!("./migrations/postgres").run(pool).await?,
        }
        tracing::debug!(backend = self.backend_name(), "database migrations applied");
        Ok(())
    }

    /// `"sqlite"` or `"postgres"`.
    pub fn backend_name(&self) -> &'static str {
        match self {
            Db::Sqlite(_) => "sqlite",
            Db::Postgres(_) => "postgres",
        }
    }

    /// Runs a trivial query; used by the health check.
    pub async fn ping(&self) -> Result<(), DbError> {
        match self {
            Db::Sqlite(pool) => sqlx::query("SELECT 1").execute(pool).await.map(|_| ())?,
            Db::Postgres(pool) => sqlx::query("SELECT 1").execute(pool).await.map(|_| ())?,
        }
        Ok(())
    }

    /// A pool on sqlx's `Any` driver for the same database, used by the wallet code
    /// so one set of SQL (with `$N` placeholders) serves SQLite and PostgreSQL.
    /// Call after [`Db::connect`] (which runs the migrations).
    pub async fn any_pool(url: &str) -> Result<sqlx::AnyPool, DbError> {
        use sqlx::Executor;
        sqlx::any::install_default_drivers();
        let sqlite = url.starts_with("sqlite:");
        let pool = sqlx::any::AnyPoolOptions::new()
            .max_connections(if sqlite { 4 } else { 8 })
            .after_connect(move |conn, _| {
                Box::pin(async move {
                    if sqlite {
                        conn.execute("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 10000;")
                            .await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(pool)
    }

    /// Closes the pool.
    pub async fn close(&self) {
        match self {
            Db::Sqlite(pool) => pool.close().await,
            Db::Postgres(pool) => pool.close().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_connects_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/allod.db", dir.path().display());
        let db = Db::connect(&url).await.unwrap();
        db.ping().await.unwrap();
        let Db::Sqlite(pool) = &db else { panic!("expected sqlite") };
        sqlx::query("INSERT INTO config (key, value) VALUES ('a', 'b')")
            .execute(pool)
            .await
            .unwrap();
        // Running migrations twice is a no-op.
        db.migrate().await.unwrap();
        db.close().await;
    }

    #[tokio::test]
    async fn rejects_unknown_scheme() {
        assert!(matches!(Db::connect("mysql://x").await, Err(DbError::UnsupportedUrl)));
    }
}
