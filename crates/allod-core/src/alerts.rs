//! Owner alerts (SPEC §8.9): what needs the owner's attention and is not a payment, such
//! as a gateway about to expire. An alert is raised once per `key` (raising it again is
//! a no-op), shown in the app until dismissed or resolved, and queued for every owner
//! device subscription; the push worker delivers it with the same encryption, retries and
//! limits as payment pushes.

use serde::Serialize;
use sqlx::{AnyPool, Row};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Alert {
    pub id: i64,
    pub key: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// Where the app opens it (a path like `/settings/gateways`).
    pub url: Option<String>,
    pub created_at: i64,
}

/// A new alert, or nothing when one with this key exists (shown or dismissed).
pub async fn raise(
    pool: &AnyPool,
    key: &str,
    kind: &str,
    title: &str,
    body: &str,
    url: Option<&str>,
) -> Result<Option<i64>, sqlx::Error> {
    let t = now();
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO alerts (key, kind, title, body, url, created_at) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (key) DO NOTHING RETURNING id",
    )
    .bind(key)
    .bind(kind)
    .bind(title)
    .bind(body)
    .bind(url)
    .bind(t)
    .fetch_optional(pool)
    .await?;
    let Some(id) = id else { return Ok(None) };
    // Every owner device, whatever payment events it chose.
    sqlx::query(
        "INSERT INTO alert_outbox (subscription_id, alert_id, created_at) \
         SELECT id, $1, $2 FROM push_subscriptions WHERE owner_pubkey IS NOT NULL",
    )
    .bind(id)
    .bind(t)
    .execute(pool)
    .await?;
    tracing::info!(%key, %title, "alert raised");
    Ok(Some(id))
}

/// The alerts shown now (not dismissed), newest first.
pub async fn active(pool: &AnyPool) -> Result<Vec<Alert>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, key, kind, title, body, url, created_at FROM alerts \
         WHERE dismissed_at IS NULL ORDER BY id DESC LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            Ok(Alert {
                id: r.try_get("id")?,
                key: r.try_get("key")?,
                kind: r.try_get("kind")?,
                title: r.try_get("title")?,
                body: r.try_get("body")?,
                url: r.try_get("url")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect()
}

/// The owner dismissed it.
pub async fn dismiss(pool: &AnyPool, id: i64) -> Result<bool, sqlx::Error> {
    let r =
        sqlx::query("UPDATE alerts SET dismissed_at = $1 WHERE id = $2 AND dismissed_at IS NULL")
            .bind(now())
            .bind(id)
            .execute(pool)
            .await?;
    Ok(r.rows_affected() > 0)
}

/// The push a device receives for an alert.
pub fn payload(a: &Alert) -> String {
    serde_json::json!({
        "v": 1,
        "type": "alert",
        "kind": a.kind,
        "alert_id": a.id,
        "title": a.title,
        "body": a.body,
        "url": a.url,
        "created_at": a.created_at,
    })
    .to_string()
}

/// One alert by id (for delivery).
pub async fn get(pool: &AnyPool, id: i64) -> Result<Option<Alert>, sqlx::Error> {
    let r =
        sqlx::query("SELECT id, key, kind, title, body, url, created_at FROM alerts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    r.map(|r| {
        Ok(Alert {
            id: r.try_get("id")?,
            key: r.try_get("key")?,
            kind: r.try_get("kind")?,
            title: r.try_get("title")?,
            body: r.try_get("body")?,
            url: r.try_get("url")?,
            created_at: r.try_get("created_at")?,
        })
    })
    .transpose()
}

/// Clears the shown alerts whose key starts with `prefix` but not with `keep` (a
/// gateway renewed: the alerts about its old end date go).
pub async fn resolve_except(pool: &AnyPool, prefix: &str, keep: &str) -> Result<u64, sqlx::Error> {
    let mut n = 0;
    for a in active(pool).await? {
        if a.key.starts_with(prefix) && !a.key.starts_with(keep) {
            sqlx::query("UPDATE alerts SET dismissed_at = $1 WHERE id = $2")
                .bind(now())
                .bind(a.id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM alert_outbox WHERE alert_id = $1 AND state = 'pending'")
                .bind(a.id)
                .execute(pool)
                .await?;
            n += 1;
        }
    }
    Ok(n)
}
