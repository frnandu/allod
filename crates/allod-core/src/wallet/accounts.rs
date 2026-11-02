//! Accounts (SPEC §10): internal transfers between Main and accounts, ledger statements
//! and closing an account.

use serde::Serialize;
use sqlx::{AnyConnection, Row};

use super::store::{self, NewTx, Tx};
use super::{i, ledger, now, Wallet, WalletError};

/// One posting of a ledger account, with the balance after it (SPEC §6.3 statements).
#[derive(Debug, Clone, Serialize)]
pub struct StatementLine {
    pub posting_id: i64,
    /// Journal entry kind: payment_reserve | payment_settle | payment_release |
    /// receive_settle | transfer | reconcile | reversal
    pub entry_kind: String,
    pub memo: String,
    /// Signed: positive credits the account.
    pub amount_msat: i64,
    /// Account balance after this posting.
    pub balance_msat: i64,
    pub created_at: i64,
    /// The payment the entry belongs to, if any.
    pub transaction_id: Option<String>,
    pub description: Option<String>,
    pub direction: Option<String>,
    pub payment_type: Option<String>,
}

/// A page of a statement, newest first.
#[derive(Debug, Clone, Serialize)]
pub struct Statement {
    pub items: Vec<StatementLine>,
    pub total: i64,
    /// Current balance of the account.
    pub balance_msat: i64,
}

/// Filters for [`Wallet::statement`]. Times are unix seconds, `until` exclusive.
#[derive(Debug, Clone, Default)]
pub struct StatementQuery {
    pub from: Option<i64>,
    pub until: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

/// `closed_at` of an app, `NotFound` if it does not exist.
async fn closed_at(conn: &mut AnyConnection, app_id: i64) -> Result<Option<i64>, WalletError> {
    sqlx::query("SELECT closed_at FROM apps WHERE id = $1")
        .bind(app_id)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| r.get::<Option<i64>, _>(0))
        .ok_or(WalletError::NotFound)
}

impl Wallet {
    /// Moves funds between Main and accounts without touching the network (SPEC §6.7).
    pub async fn transfer(
        &self,
        from: Option<i64>,
        to: Option<i64>,
        amount_msat: u64,
        description: String,
    ) -> Result<Tx, WalletError> {
        if amount_msat == 0 || from == to {
            return Err(WalletError::BadRequest(
                "transfer needs a positive amount and two different accounts".into(),
            ));
        }
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        if let Some(id) = from {
            closed_at(&mut dbtx, id).await?;
        }
        if let Some(id) = to {
            if closed_at(&mut dbtx, id).await?.is_some() {
                return Err(WalletError::BadRequest("this account is closed".into()));
            }
        }
        let (out, inc) =
            self.transfer_in(&mut dbtx, from, to, i(amount_msat), description, now()).await?;
        dbtx.commit().await?;
        self.notify(&out);
        self.notify(&inc);
        Ok(out)
    }

    /// Writes a settled internal pair and its `transfer` entry inside `dbtx`. The caller
    /// holds the write lock.
    async fn transfer_in(
        &self,
        dbtx: &mut AnyConnection,
        from: Option<i64>,
        to: Option<i64>,
        a: i64,
        description: String,
        t: i64,
    ) -> Result<(Tx, Tx), WalletError> {
        let payer = Self::wallet_code(dbtx, from).await?;
        let receiver = Self::wallet_code(dbtx, to).await?;
        if payer == receiver {
            return Err(WalletError::BadRequest("both sides use the same balance".into()));
        }
        if ledger::balance_locked(dbtx, &payer, self.postgres()).await? < a {
            return Err(WalletError::InsufficientBalance);
        }
        let base = NewTx {
            payment_type: "internal",
            state: "SETTLED",
            amount_msat: a,
            description,
            self_payment: true,
            settled_at: Some(t),
            ..Default::default()
        };
        let out =
            store::insert(dbtx, &NewTx { app_id: from, direction: "outgoing", ..base.clone() }, t)
                .await?;
        let inc =
            store::insert(dbtx, &NewTx { app_id: to, direction: "incoming", ..base }, t).await?;
        for (a, b) in [(out.id, inc.id), (inc.id, out.id)] {
            sqlx::query("UPDATE transactions SET counterpart_tx_id = $1 WHERE id = $2")
                .bind(b)
                .bind(a)
                .execute(&mut *dbtx)
                .await?;
        }
        ledger::post(
            dbtx,
            "transfer",
            Some(out.id),
            "transfer",
            &[(&payer, -a), (&receiver, a)],
            t,
        )
        .await?;
        let out = store::by_id(dbtx, out.id).await?.ok_or(WalletError::NotFound)?;
        let inc = store::by_id(dbtx, inc.id).await?.ok_or(WalletError::NotFound)?;
        Ok((out, inc))
    }

    /// Ledger postings of an account (`None` = Main) with a running balance, newest
    /// first (SPEC §6.3, `GET /api/accounts/:id/statement`).
    pub async fn statement(
        &self,
        app_id: Option<i64>,
        q: &StatementQuery,
    ) -> Result<Statement, WalletError> {
        let mut c = self.conn().await?;
        let code = Self::wallet_code(&mut c, app_id).await?;
        let balance_msat = ledger::balance(&mut c, &code).await?;
        let Some(account) = sqlx::query("SELECT id FROM ledger_accounts WHERE code = $1")
            .bind(&code)
            .fetch_optional(&mut *c)
            .await?
            .map(|r| r.get::<i64, _>(0))
        else {
            return Ok(Statement { items: Vec::new(), total: 0, balance_msat });
        };
        let from = q.from.unwrap_or(0);
        let until = q.until.unwrap_or(i64::MAX);
        let total: i64 = sqlx::query(
            "SELECT COUNT(*) FROM postings WHERE account_id = $1 AND created_at >= $2 AND created_at < $3",
        )
        .bind(account)
        .bind(from)
        .bind(until)
        .fetch_one(&mut *c)
        .await?
        .get(0);
        let rows = sqlx::query(
            "SELECT * FROM (\
               SELECT p.id, p.amount_msat, p.created_at, e.kind, e.memo, t.public_id, t.description, \
                 t.direction, t.payment_type, \
                 CAST(SUM(p.amount_msat) OVER (ORDER BY p.id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
                   AS BIGINT) AS running \
               FROM postings p JOIN journal_entries e ON e.id = p.entry_id \
               LEFT JOIN transactions t ON t.id = e.transaction_id \
               WHERE p.account_id = $1\
             ) s WHERE created_at >= $2 AND created_at < $3 ORDER BY id DESC LIMIT $4 OFFSET $5",
        )
        .bind(account)
        .bind(from)
        .bind(until)
        .bind(q.limit.clamp(1, 500))
        .bind(q.offset.max(0))
        .fetch_all(&mut *c)
        .await?;
        let items = rows
            .iter()
            .map(|r| {
                Ok(StatementLine {
                    posting_id: r.try_get("id")?,
                    amount_msat: r.try_get("amount_msat")?,
                    created_at: r.try_get("created_at")?,
                    entry_kind: r.try_get("kind")?,
                    memo: r.try_get("memo")?,
                    transaction_id: r.try_get("public_id")?,
                    description: r.try_get("description")?,
                    direction: r.try_get("direction")?,
                    payment_type: r.try_get("payment_type")?,
                    balance_msat: r.try_get("running")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        Ok(Statement { items, total, balance_msat })
    }

    /// Closes an account (SPEC §10). With `sweep`, its balance first moves to Main in the
    /// same DB transaction; without it, the account must be empty. Refused while one of
    /// its payments is in flight or a hold invoice is accepted. Its active offers are
    /// disabled. Returns the sweep transfer, if there was one.
    pub async fn close_account(&self, app_id: i64, sweep: bool) -> Result<Option<Tx>, WalletError> {
        let _g = self.write_lock.lock().await;
        let mut dbtx = self.begin().await?;
        let row = sqlx::query("SELECT kind, closed_at FROM apps WHERE id = $1")
            .bind(app_id)
            .fetch_optional(&mut *dbtx)
            .await?
            .ok_or(WalletError::NotFound)?;
        if row.get::<String, _>(0) != "account" {
            return Err(WalletError::NotFound);
        }
        if row.get::<Option<i64>, _>(1).is_some() {
            return Err(WalletError::BadRequest("this account is already closed".into()));
        }
        let busy: i64 = sqlx::query(
            "SELECT COUNT(*) FROM transactions WHERE app_id = $1 AND \
             ((direction = 'outgoing' AND state = 'PENDING') OR state IN ('ACCEPTED', 'CANCELING'))",
        )
        .bind(app_id)
        .fetch_one(&mut *dbtx)
        .await?
        .get(0);
        if busy > 0 {
            return Err(WalletError::BadRequest(
                "this account has payments in flight; close it once they finish".into(),
            ));
        }
        let code = ledger::app_wallet(app_id);
        let balance = ledger::balance_locked(&mut dbtx, &code, self.postgres()).await?;
        if balance < 0 {
            return Err(WalletError::Internal(format!("{code} is negative")));
        }
        let t = now();
        let swept = if balance > 0 {
            if !sweep {
                return Err(WalletError::BadRequest(
                    "this account still holds funds; withdraw them or sweep them to Main".into(),
                ));
            }
            let (out, inc) = self
                .transfer_in(&mut dbtx, Some(app_id), None, balance, "Account closed".into(), t)
                .await?;
            Some((out, inc))
        } else {
            None
        };
        sqlx::query("UPDATE apps SET closed_at = $1, updated_at = $1 WHERE id = $2")
            .bind(t)
            .bind(app_id)
            .execute(&mut *dbtx)
            .await?;
        sqlx::query("UPDATE offers SET state = 'disabled' WHERE app_id = $1 AND state = 'active'")
            .bind(app_id)
            .execute(&mut *dbtx)
            .await?;
        dbtx.commit().await?;
        Ok(swept.map(|(out, inc)| {
            self.notify(&out);
            self.notify(&inc);
            out
        }))
    }
}
