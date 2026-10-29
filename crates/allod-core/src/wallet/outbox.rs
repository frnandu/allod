//! Notification outbox (SPEC §8.5): rows are written in the same DB transaction as the
//! state change; the NWC service publishes them and retries.

use sqlx::{AnyConnection, Row};

use super::WalletError;

pub const PAYMENT_RECEIVED: &str = "payment_received";
pub const PAYMENT_SENT: &str = "payment_sent";
pub const HOLD_ACCEPTED: &str = "hold_invoice_accepted";
/// Every notification type Allod sends.
pub const TYPES: &[&str] = &[PAYMENT_RECEIVED, PAYMENT_SENT, HOLD_ACCEPTED];
/// Publish attempts before a notification is given up (`dead`).
pub const MAX_ATTEMPTS: i64 = 10;

/// Queues the notification for a transaction's new state, for every connection that
/// may see it (§8.5): isolated apps get only their own rows; non-isolated apps get Main's
/// rows and their own.
pub async fn enqueue(conn: &mut AnyConnection, tx_id: i64, now: i64) -> Result<(), WalletError> {
    let Some(row) = sqlx::query(
        "SELECT direction, state, CAST(hold AS INTEGER), app_id FROM transactions WHERE id = $1",
    )
    .bind(tx_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    let direction: String = row.get(0);
    let state: String = row.get(1);
    let hold: i64 = row.get(2);
    let app_id: Option<i64> = row.get(3);
    let kind = match (direction.as_str(), state.as_str()) {
        ("incoming", "SETTLED") => PAYMENT_RECEIVED,
        ("outgoing", "SETTLED") => PAYMENT_SENT,
        ("incoming", "ACCEPTED") if hold != 0 => HOLD_ACCEPTED,
        _ => return Ok(()),
    };
    // -1 never matches an app id, so Main's rows reach only non-isolated apps.
    let owner = app_id.unwrap_or(-1);
    sqlx::query(
        "INSERT INTO notifications_outbox (app_id, kind, transaction_id, next_attempt_at, created_at) \
         SELECT a.id, $1, $2, $3, $3 FROM apps a \
         JOIN app_permissions p ON p.app_id = a.id AND p.scope = 'notifications' \
         WHERE a.kind = 'connection' AND (a.expires_at IS NULL OR a.expires_at > $3) \
         AND (a.id = $4 OR (a.isolated = FALSE AND $5 = 1)) \
         ON CONFLICT DO NOTHING",
    )
    .bind(kind)
    .bind(tx_id)
    .bind(now)
    .bind(owner)
    .bind(i64::from(app_id.is_none()))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A notification ready to publish.
#[derive(Debug, Clone)]
pub struct Due {
    pub id: i64,
    pub app_id: i64,
    pub kind: String,
    pub transaction_id: i64,
    pub attempts: i64,
}

pub async fn due(conn: &mut AnyConnection, now: i64, limit: i64) -> Result<Vec<Due>, WalletError> {
    Ok(sqlx::query(
        "SELECT id, app_id, kind, transaction_id, attempts FROM notifications_outbox \
         WHERE state = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= $1) ORDER BY id LIMIT $2",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| Due {
        id: r.get(0),
        app_id: r.get(1),
        kind: r.get(2),
        transaction_id: r.get(3),
        attempts: r.get(4),
    })
    .collect())
}

pub async fn mark_sent(conn: &mut AnyConnection, id: i64) -> Result<(), WalletError> {
    sqlx::query(
        "UPDATE notifications_outbox SET state = 'sent', attempts = attempts + 1 WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Schedules a retry with exponential backoff, or gives up after `MAX_ATTEMPTS`.
pub async fn mark_failed(conn: &mut AnyConnection, d: &Due, now: i64) -> Result<(), WalletError> {
    let attempts = d.attempts + 1;
    let (state, next) = if attempts >= MAX_ATTEMPTS {
        ("dead", None)
    } else {
        ("pending", Some(now + (5i64 << attempts.min(12))))
    };
    sqlx::query("UPDATE notifications_outbox SET state = $1, attempts = $2, next_attempt_at = $3 WHERE id = $4")
        .bind(state)
        .bind(attempts)
        .bind(next)
        .bind(d.id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
