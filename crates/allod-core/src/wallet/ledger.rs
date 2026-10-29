//! Double-entry ledger (SPEC §6.3). Append-only: entries are never updated or deleted.

use sqlx::{AnyConnection, Row};

use super::WalletError;

pub const MAIN: &str = "wallet:main";
pub const PENDING_OUT: &str = "pending:out";
pub const EXTERNAL: &str = "external:lightning";
pub const FEES: &str = "fees:routing";
pub const EQUITY: &str = "equity:node";

pub fn app_wallet(app_id: i64) -> String {
    format!("wallet:app:{app_id}")
}

fn kind_of(code: &str) -> &'static str {
    match code.split(':').next() {
        Some("wallet") => "wallet",
        Some("pending") => "pending",
        Some("external") => "external",
        Some("fees") => "fees",
        _ => "equity",
    }
}

/// Account id for `code`, created on first use.
pub async fn account(conn: &mut AnyConnection, code: &str, now: i64) -> Result<i64, WalletError> {
    if let Some(row) = sqlx::query("SELECT id FROM ledger_accounts WHERE code = $1")
        .bind(code)
        .fetch_optional(&mut *conn)
        .await?
    {
        return Ok(row.get::<i64, _>(0));
    }
    let app_id: Option<i64> = code.strip_prefix("wallet:app:").and_then(|s| s.parse().ok());
    sqlx::query(
        "INSERT INTO ledger_accounts (code, kind, app_id, created_at) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (code) DO NOTHING",
    )
    .bind(code)
    .bind(kind_of(code))
    .bind(app_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    let row = sqlx::query("SELECT id FROM ledger_accounts WHERE code = $1")
        .bind(code)
        .fetch_one(&mut *conn)
        .await?;
    Ok(row.get::<i64, _>(0))
}

/// Posts one journal entry. Postings must sum to zero. Returns false (and changes
/// nothing) when an entry of this kind already exists for the transaction, which makes
/// replayed events idempotent.
pub async fn post(
    conn: &mut AnyConnection,
    kind: &str,
    transaction_id: Option<i64>,
    memo: &str,
    postings: &[(&str, i64)],
    now: i64,
) -> Result<bool, WalletError> {
    let sum: i64 = postings.iter().map(|(_, a)| a).sum();
    if sum != 0 {
        return Err(WalletError::Internal(format!("unbalanced journal entry {kind}: sum {sum}")));
    }
    let entry: Option<i64> = sqlx::query(
        "INSERT INTO journal_entries (kind, transaction_id, memo, created_at) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (transaction_id, kind) DO NOTHING RETURNING id",
    )
    .bind(kind)
    .bind(transaction_id)
    .bind(memo)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| r.get::<i64, _>(0));
    let Some(entry) = entry else { return Ok(false) };
    for (code, amount) in postings {
        if *amount == 0 {
            continue;
        }
        let acct = account(conn, code, now).await?;
        let pid: i64 = sqlx::query(
            "INSERT INTO postings (entry_id, account_id, amount_msat, created_at) VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(entry)
        .bind(acct)
        .bind(*amount)
        .bind(now)
        .fetch_one(&mut *conn)
        .await?
        .get(0);
        sqlx::query(
            "INSERT INTO ledger_balances (account_id, balance_msat, last_posting_id) VALUES ($1, $2, $3) \
             ON CONFLICT (account_id) DO UPDATE SET balance_msat = ledger_balances.balance_msat + $2, \
             last_posting_id = $3",
        )
        .bind(acct)
        .bind(*amount)
        .bind(pid)
        .execute(&mut *conn)
        .await?;
    }
    Ok(true)
}

/// Balance of one account (0 if it has no postings).
pub async fn balance(conn: &mut AnyConnection, code: &str) -> Result<i64, WalletError> {
    let row = sqlx::query(
        "SELECT b.balance_msat FROM ledger_balances b JOIN ledger_accounts a ON a.id = b.account_id \
         WHERE a.code = $1",
    )
    .bind(code)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| r.get::<i64, _>(0)).unwrap_or(0))
}

/// Σ of all accounts of one kind.
pub async fn kind_total(
    conn: &mut AnyConnection,
    kind: &str,
    exclude: Option<&str>,
) -> Result<i64, WalletError> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(b.balance_msat), 0) FROM ledger_balances b JOIN ledger_accounts a \
         ON a.id = b.account_id WHERE a.kind = $1 AND a.code <> $2",
    )
    .bind(kind)
    .bind(exclude.unwrap_or(""))
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.get::<i64, _>(0))
}

/// Invariant check (SPEC §6.3): every entry sums to 0, cached balances equal the
/// postings, no app wallet is negative, and `pending:out` equals in-flight payments.
pub async fn check(conn: &mut AnyConnection) -> Result<Vec<String>, WalletError> {
    let mut problems = Vec::new();
    let unbalanced: i64 = sqlx::query(
        "SELECT COUNT(*) FROM (SELECT entry_id FROM postings GROUP BY entry_id HAVING SUM(amount_msat) <> 0) x",
    )
    .fetch_one(&mut *conn)
    .await?
    .get(0);
    if unbalanced > 0 {
        problems.push(format!("{unbalanced} unbalanced journal entries"));
    }
    let drift: i64 = sqlx::query(
        "SELECT COUNT(*) FROM ledger_balances b WHERE b.balance_msat <> \
         (SELECT COALESCE(SUM(p.amount_msat), 0) FROM postings p WHERE p.account_id = b.account_id)",
    )
    .fetch_one(&mut *conn)
    .await?
    .get(0);
    if drift > 0 {
        problems.push(format!("{drift} cached balances differ from postings"));
    }
    let negative: i64 = sqlx::query(
        "SELECT COUNT(*) FROM ledger_balances b JOIN ledger_accounts a ON a.id = b.account_id \
         WHERE a.code LIKE 'wallet:app:%' AND b.balance_msat < 0",
    )
    .fetch_one(&mut *conn)
    .await?
    .get(0);
    if negative > 0 {
        problems.push(format!("{negative} account wallets are negative"));
    }
    let in_flight: i64 = sqlx::query(
        "SELECT COALESCE(SUM(amount_msat + fee_reserve_msat), 0) FROM transactions \
         WHERE direction = 'outgoing' AND state = 'PENDING'",
    )
    .fetch_one(&mut *conn)
    .await?
    .get(0);
    let pending = balance(conn, PENDING_OUT).await?;
    if pending != in_flight {
        problems.push(format!("pending:out is {pending} but in-flight payments total {in_flight}"));
    }
    Ok(problems)
}
