//! App budgets (SPEC §6.4): usage per renewal period and the `QUOTA_EXCEEDED` check.

use serde::Serialize;
use sqlx::{AnyConnection, Row};

use super::WalletError;

/// Budget renewal period. Periods start at 00:00 UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    Daily,
    Weekly,
    Monthly,
    Yearly,
    Never,
}

impl Renewal {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "daily" => Renewal::Daily,
            "weekly" => Renewal::Weekly,
            "monthly" => Renewal::Monthly,
            "yearly" => Renewal::Yearly,
            "never" => Renewal::Never,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Renewal::Daily => "daily",
            Renewal::Weekly => "weekly",
            Renewal::Monthly => "monthly",
            Renewal::Yearly => "yearly",
            Renewal::Never => "never",
        }
    }

    /// Start of the period that contains `now` (unix seconds).
    pub fn period_start(self, now: i64) -> i64 {
        let day = now.div_euclid(86_400);
        match self {
            Renewal::Never => 0,
            Renewal::Daily => day * 86_400,
            // 1970-01-01 was a Thursday; Monday-based weeks.
            Renewal::Weekly => (day - (day + 3).rem_euclid(7)) * 86_400,
            Renewal::Monthly => {
                let (y, m, _) = civil_from_days(day);
                days_from_civil(y, m, 1) * 86_400
            }
            Renewal::Yearly => {
                let (y, _, _) = civil_from_days(day);
                days_from_civil(y, 1, 1) * 86_400
            }
        }
    }

    /// Start of the next period, or `None` for `never`.
    pub fn next_start(self, now: i64) -> Option<i64> {
        let start = self.period_start(now);
        match self {
            Renewal::Never => None,
            Renewal::Daily => Some(start + 86_400),
            Renewal::Weekly => Some(start + 7 * 86_400),
            Renewal::Monthly => {
                let (y, m, _) = civil_from_days(start / 86_400);
                let (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                Some(days_from_civil(y, m, 1) * 86_400)
            }
            Renewal::Yearly => {
                let (y, _, _) = civil_from_days(start / 86_400);
                Some(days_from_civil(y + 1, 1, 1) * 86_400)
            }
        }
    }
}

// Howard Hinnant's civil date algorithms (proleptic Gregorian, days since 1970-01-01).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// An app's budget as reported by NWC `get_budget` (msat).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Budget {
    pub used_budget: i64,
    pub total_budget: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renews_at: Option<i64>,
    pub renewal_period: String,
}

/// What the app and its descendants spent in the period starting at `since`:
/// amount + fee reserve while pending, amount + fee once settled.
pub async fn usage(c: &mut AnyConnection, app_id: i64, since: i64) -> Result<i64, WalletError> {
    let row = sqlx::query(
        "WITH RECURSIVE descendants(id) AS ( \
         SELECT id FROM apps WHERE id = $1 \
         UNION ALL SELECT a.id FROM apps a JOIN descendants d ON a.created_by_app_id = d.id \
         ) SELECT CAST(COALESCE(SUM(CASE WHEN state = 'PENDING' THEN amount_msat + fee_reserve_msat \
         ELSE amount_msat + fee_msat END), 0) AS BIGINT) FROM transactions \
         WHERE app_id IN (SELECT id FROM descendants) AND direction = 'outgoing' \
         AND state IN ('PENDING', 'SETTLED') AND created_at >= $2",
    )
    .bind(app_id)
    .bind(since)
    .fetch_one(&mut *c)
    .await?;
    Ok(row.get::<i64, _>(0))
}

/// The budget of `app_id`, or `None` when it has none (`max_amount_msat = 0`).
pub async fn get(
    c: &mut AnyConnection,
    app_id: i64,
    now: i64,
) -> Result<Option<Budget>, WalletError> {
    let row = sqlx::query("SELECT max_amount_msat, budget_renewal FROM apps WHERE id = $1")
        .bind(app_id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or(WalletError::NotFound)?;
    let max: i64 = row.get(0);
    if max <= 0 {
        return Ok(None);
    }
    let renewal = Renewal::parse(&row.get::<String, _>(1)).unwrap_or(Renewal::Never);
    let used = usage(c, app_id, renewal.period_start(now)).await?;
    Ok(Some(Budget {
        used_budget: used,
        total_budget: max,
        renews_at: renewal.next_start(now),
        renewal_period: renewal.as_str().into(),
    }))
}

/// Fails if spending `add` msat more would exceed any budget in the delegation chain.
/// Call it under the wallet write lock, in the transaction that reserves the funds.
pub async fn check(
    c: &mut AnyConnection,
    app_id: Option<i64>,
    add: i64,
    now: i64,
    postgres: bool,
) -> Result<(), WalletError> {
    let Some(mut id) = app_id else { return Ok(()) };
    loop {
        let query = if postgres {
            "SELECT created_by_app_id, expires_at FROM apps WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT created_by_app_id, expires_at FROM apps WHERE id = $1"
        };
        let row = sqlx::query(query)
            .bind(id)
            .fetch_optional(&mut *c)
            .await?
            .ok_or(WalletError::NotFound)?;
        let parent: Option<i64> = row.get(0);
        let expires_at: Option<i64> = row.get(1);
        if expires_at.is_some_and(|expires| expires <= now) {
            return Err(WalletError::QuotaExceeded);
        }
        if let Some(b) = get(c, id, now).await? {
            if b.used_budget.saturating_add(add) > b.total_budget {
                return Err(WalletError::QuotaExceeded);
            }
        }
        match parent {
            Some(parent) => id = parent,
            None => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-28 13:00:00 UTC, a Wednesday.
    const NOW: i64 = 1_793_192_400;

    #[test]
    fn period_starts_are_utc_calendar_boundaries() {
        assert_eq!(Renewal::Daily.period_start(NOW), 1_793_145_600); // 2026-10-28
        assert_eq!(Renewal::Weekly.period_start(NOW), 1_792_972_800); // Mon 2026-10-26
        assert_eq!(Renewal::Monthly.period_start(NOW), 1_790_812_800); // 2026-10-01
        assert_eq!(Renewal::Yearly.period_start(NOW), 1_767_225_600); // 2026-01-01
        assert_eq!(Renewal::Never.period_start(NOW), 0);
        assert_eq!(Renewal::Monthly.next_start(NOW), Some(1_793_491_200)); // 2026-11-01
        assert_eq!(Renewal::Yearly.next_start(NOW), Some(1_798_761_600)); // 2027-01-01
        assert_eq!(Renewal::Never.next_start(NOW), None);
        // December rolls into the next year.
        let dec = 1_796_083_200; // 2026-12-01
        assert_eq!(Renewal::Monthly.next_start(dec), Some(1_798_761_600));
    }

    #[test]
    fn civil_round_trip() {
        for day in [-800_000, -1, 0, 59, 60, 10_000, 20_754, 100_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
    }
}
