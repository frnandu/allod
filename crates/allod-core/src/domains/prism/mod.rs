//! Prism splits (SPEC §12.1): an address forwards a percentage of what it receives to
//! other Lightning targets — an LNURL (or Lightning address), a BIP-353 name, or a BOLT12
//! offer. Percentages need not add up to 100: the remainder stays in the address's wallet,
//! and a split smaller than 1 sat is skipped. Payouts are recorded one row per split and
//! payment (migration 0020), so a failure can be retried later and the amounts that never
//! arrived stay visible. The background worker that pays them is in `worker.rs`.

pub mod worker;

use std::collections::HashSet;

use serde_json::{json, Value};

use super::{now, store, DomainError, DomainService};

/// A split never forwards less than this (1 sat).
pub const MIN_SPLIT_MSAT: i64 = 1_000;
/// Automatic attempts per payout before it waits for the owner ("Retry now").
pub const MAX_ATTEMPTS: i64 = 8;
/// Earnings older than this are not split when the daemon was down (a new split must not
/// pay for payments received before it existed).
pub const CATCH_UP_SECS: i64 = 86_400;

/// One configured split of an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub id: i64,
    pub position: i64,
    pub destination: String,
    /// `lnurl` | `bip353` | `bolt12`; `address` while a plain name is not checked yet.
    pub kind: String,
    pub percent_bps: i64,
    /// When the owner saved it: a payment received before that is not split by it.
    pub created_at: i64,
}

/// What one split did so far.
#[derive(Debug, Default, Clone)]
pub struct SplitStats {
    pub forwarded_msat: i64,
    pub failed_msat: i64,
    pub pending_msat: i64,
    pub pending_count: i64,
    pub failed_count: i64,
    pub skipped_msat: i64,
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
}

/// What an address's splits did so far.
#[derive(Debug, Default, Clone)]
pub struct PrismStats {
    pub forwarded_msat: i64,
    pub failed_msat: i64,
    pub pending_msat: i64,
    pub pending_count: i64,
    pub failed_count: i64,
    pub skipped_msat: i64,
    /// Payments received while splits were configured.
    pub payments: i64,
}

/// A payout row (one split of one received payment).
#[derive(Debug, Clone)]
pub struct Payout {
    pub id: i64,
    pub address_id: i64,
    pub incoming_tx_id: i64,
    pub destination: String,
    pub kind: String,
    pub amount_msat: i64,
    pub fee_msat: i64,
    pub percent_bps: i64,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub next_retry_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Payout {
    pub fn is_due(&self, t: i64) -> bool {
        match self.state.as_str() {
            "PENDING" => true,
            "FAILED" => self.next_retry_at.is_some_and(|at| at <= t),
            _ => false,
        }
    }
}

/// The canonical form of a BOLT12 offer: lowercase, with the `+` continuations and
/// whitespace a long offer is often wrapped with removed. `None`: not an offer.
///
/// BOLT12 offers are bech32 **without a checksum** (BOLT 12 uses `NoChecksum`), so the
/// ordinary [`bech32::decode`] rejects every real offer.
fn bolt12_offer(destination: &str) -> Option<String> {
    let joined: String =
        destination.trim().chars().filter(|c| *c != '+' && !c.is_whitespace()).collect();
    let lower = joined.to_ascii_lowercase();
    if !lower.starts_with("lno1") {
        return None;
    }
    let parsed =
        bech32::primitives::decode::CheckedHrpstring::new::<bech32::NoChecksum>(&lower).ok()?;
    if parsed.hrp().as_str() != "lno" || parsed.validate_segwit_padding().is_err() {
        return None;
    }
    Some(lower)
}

/// What a split destination is: `lnurl`, `bip353` or `bolt12`.
///
/// A plain `user@domain` is accepted as `lnurl` because whether it publishes a BIP-353
/// record is only known by asking; the wallet tries BIP-353 first. It is then stored as
/// `address` until the service checks it (see `checked_split_kind`). `₿user@domain` is a
/// BIP-353 name only.
pub fn destination_kind(destination: &str) -> Result<&'static str, DomainError> {
    let d = destination.trim();
    if d.is_empty() {
        return Err(DomainError::BadRequest("a split needs a destination".into()));
    }
    if d.starts_with('₿') {
        return if allod_lnurl::is_lnurl_target(d) {
            Ok("bip353")
        } else {
            Err(DomainError::BadRequest(format!("{d} is not a BIP-353 name")))
        };
    }
    let lower = d.to_ascii_lowercase();
    if lower.starts_with("lno1") {
        return match bolt12_offer(d) {
            Some(_) => Ok("bolt12"),
            None => Err(DomainError::BadRequest(format!("{d} is not a BOLT12 offer"))),
        };
    }
    if allod_lnurl::is_lnurl_target(d) {
        return Ok("lnurl");
    }
    Err(DomainError::BadRequest(format!(
        "{d} is not a Lightning address, an LNURL or a BOLT12 offer"
    )))
}

/// Checks and normalizes a whole split list: `(destination, percent in basis points)`.
/// Percentages add up to at most 100%; the remainder stays in the wallet.
pub fn normalize_splits(
    input: &[(String, i64)],
) -> Result<Vec<(String, &'static str, i64)>, DomainError> {
    if input.len() > 32 {
        return Err(DomainError::BadRequest("at most 32 splits per address".into()));
    }
    let mut out: Vec<(String, &'static str, i64)> = Vec::with_capacity(input.len());
    let mut total = 0i64;
    for (destination, percent_bps) in input {
        let destination = destination.trim().to_string();
        let mut kind = destination_kind(&destination)?;
        // A plain address may be either; saving checks which one it is.
        if kind == "lnurl" && is_plain_address(&destination) {
            kind = "address";
        }
        // Keep a long offer in one piece, as it was encoded.
        let destination = if kind == "bolt12" {
            bolt12_offer(&destination).unwrap_or(destination)
        } else {
            destination
        };
        if !(1..=10_000).contains(percent_bps) {
            return Err(DomainError::BadRequest(
                "each split must forward between 0.01% and 100%".into(),
            ));
        }
        if out.iter().any(|(d, _, _)| d.eq_ignore_ascii_case(&destination)) {
            return Err(DomainError::BadRequest(format!("{destination} is already a split")));
        }
        total += percent_bps;
        out.push((destination, kind, *percent_bps));
    }
    if total > 10_000 {
        return Err(DomainError::BadRequest(
            "the splits add up to more than 100%; the rest must stay in this wallet".into(),
        ));
    }
    Ok(out)
}

/// A split's share of a payment, rounded down; `None` when it is under 1 sat.
pub fn share(amount_msat: i64, percent_bps: i64) -> Option<i64> {
    let share = (i128::from(amount_msat) * i128::from(percent_bps) / 10_000) as i64;
    (share >= MIN_SPLIT_MSAT).then_some(share)
}

/// `user@domain` → (username, domain), lowercase.
fn split_address(address: &str) -> Option<(String, String)> {
    let (user, domain) = address.trim().trim_start_matches('₿').split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    Some((user.to_ascii_lowercase(), domain.trim_end_matches('.').to_ascii_lowercase()))
}

/// A bare `user@domain`: a BIP-353 name when one is signed, an LNURL address otherwise.
/// `₿name`, `lnurl1…` and `lnurlp://…` say which one they mean.
fn is_plain_address(destination: &str) -> bool {
    let d = destination.trim();
    let d =
        d.strip_prefix("lightning:").or_else(|| d.strip_prefix("LIGHTNING:")).unwrap_or(d).trim();
    !d.starts_with('₿') && split_address(d).is_some()
}

/// The HTTPS URL behind `lnurl1…` or `lnurlp://…`, for matching our own LNURL endpoint.
fn lnurl_url(destination: &str) -> Option<String> {
    if let Some(rest) = destination.strip_prefix("lnurlp://") {
        return Some(format!("https://{rest}"));
    }
    if destination.starts_with("lnurl1") {
        let (hrp, data) = bech32::decode(destination).ok()?;
        if hrp.as_str() != "lnurl" {
            return None;
        }
        return String::from_utf8(data).ok();
    }
    None
}

/// `(domain, username)` of an Allod LNURL endpoint (`https://{domain}/.well-known/lnurlp/{user}`).
fn lnurl_username(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let user = path.strip_prefix(".well-known/lnurlp/")?;
    if user.is_empty() || user.contains(['/', '?', '#']) {
        return None;
    }
    let host = host.split(':').next()?.trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    Some((host.to_ascii_lowercase(), user.to_ascii_lowercase()))
}

fn payout_row(r: &sqlx::any::AnyRow) -> Result<Payout, sqlx::Error> {
    use sqlx::Row;
    Ok(Payout {
        id: r.try_get("id")?,
        address_id: r.try_get("address_id")?,
        incoming_tx_id: r.try_get("incoming_tx_id")?,
        destination: r.try_get("destination")?,
        kind: r.try_get("kind")?,
        amount_msat: r.try_get("amount_msat")?,
        fee_msat: r.try_get("fee_msat")?,
        percent_bps: r.try_get("percent_bps")?,
        state: r.try_get("state")?,
        attempts: r.try_get("attempts")?,
        last_error: r.try_get("last_error")?,
        next_retry_at: r.try_get("next_retry_at")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const PAYOUT_COLS: &str = "id, address_id, incoming_tx_id, destination, kind, amount_msat, \
    fee_msat, percent_bps, state, CAST(attempts AS BIGINT) AS attempts, last_error, \
    next_retry_at, created_at, updated_at";

impl DomainService {
    /// What a destination is, checked against the address it names.
    ///
    /// A plain `user@domain` says nothing by itself: it is BIP-353 when a signed record
    /// exists and LNURL otherwise. `Wallet::decode` follows exactly what a payment does,
    /// so a name that answers neither is refused when the splits are saved instead of
    /// failing on every payout.
    async fn checked_split_kind(
        &self,
        destination: &str,
        kind: &'static str,
    ) -> Result<&'static str, DomainError> {
        if let Some(a) = self.own_destination(destination).await? {
            return match kind {
                "bip353" if !a.bip353 => Err(DomainError::BadRequest(format!(
                    "{destination} does not publish a BIP-353 record"
                ))),
                "lnurl" if !a.lnurl => {
                    Err(DomainError::BadRequest(format!("{destination} does not answer LNURL")))
                }
                "address" => Ok(if a.bip353 { "bip353" } else { "lnurl" }),
                k => Ok(k),
            };
        }
        // An offer is self-contained: its bech32 payload was checked already.
        if kind == "bolt12" {
            return Ok(kind);
        }
        // Without a wallet there is nobody to ask (only during startup): keep the split,
        // and a payout records whatever went wrong.
        let Some(wallet) = self.wallet.get() else {
            return Ok(if kind == "address" { "lnurl" } else { kind });
        };
        match wallet.decode(destination).await {
            // A signed record is a BIP-353 name, but it still has to name a Lightning
            // payment: an on-chain-only record cannot be a split.
            Ok(d) if d.bip353.is_some() && matches!(d.kind, "lnurl" | "bolt11" | "bolt12") => {
                Ok("bip353")
            }
            Ok(d) if d.kind == "lnurl" => Ok("lnurl"),
            Ok(d) => Err(DomainError::BadRequest(format!(
                "{destination} is {}, but a split needs a Lightning payment target",
                d.kind
            ))),
            Err(e) => Err(DomainError::BadRequest(format!(
                "{destination} cannot receive a Lightning payment: {e}"
            ))),
        }
    }

    /// What to show for a stored split. A plain address saved before it was checked is
    /// left as `address` ("BIP-353 or LNURL") rather than guessed; our own names are
    /// known from their settings.
    async fn display_split_kind(
        &self,
        destination: &str,
        kind: &str,
    ) -> Result<String, DomainError> {
        if kind != "address" || !is_plain_address(destination) {
            return Ok(kind.to_string());
        }
        let Some((user, domain)) = split_address(destination) else {
            return Ok(kind.to_string());
        };
        Ok(match self.own_address_at(&domain, &user).await? {
            Some(a) if a.bip353 => "bip353".into(),
            Some(_) => "lnurl".into(),
            None => kind.to_string(),
        })
    }

    /// Splits configured for an address, in display order.
    pub async fn prism_splits(&self, address_id: i64) -> Result<Vec<Split>, DomainError> {
        let rows = sqlx::query(
            "SELECT id, position, destination, kind, CAST(percent_bps AS BIGINT) AS percent_bps, \
             created_at FROM prism_splits WHERE address_id = $1 ORDER BY position",
        )
        .bind(address_id)
        .fetch_all(&self.pool)
        .await?;
        use sqlx::Row;
        let mut splits = rows
            .iter()
            .map(|r| {
                Ok(Split {
                    id: r.try_get("id")?,
                    position: r.try_get("position")?,
                    destination: r.try_get("destination")?,
                    kind: r.try_get("kind")?,
                    percent_bps: r.try_get("percent_bps")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        // Rows saved before a plain address said what it was keep their stored kind; show
        // what it actually resolves to now.
        for s in &mut splits {
            s.kind = self.display_split_kind(&s.destination, &s.kind).await?;
        }
        Ok(splits)
    }

    /// The address of ours a destination would pay, if it is one of them: its Lightning
    /// address or BIP-353 name (`user@domain`, `₿user@domain`), its LNURL (`lnurl1…`,
    /// `lnurlp://…`) or its BOLT12 offer (`lno1…`).
    async fn own_destination(
        &self,
        destination: &str,
    ) -> Result<Option<store::Address>, DomainError> {
        let d = destination.trim();
        let d = d
            .strip_prefix("lightning:")
            .or_else(|| d.strip_prefix("LIGHTNING:"))
            .unwrap_or(d)
            .trim();
        if let Some((user, domain)) = split_address(d) {
            return self.own_address_at(&domain, &user).await;
        }
        let lower = d.to_ascii_lowercase();
        if lower.starts_with("lno1") {
            let Some(offer) = bolt12_offer(d) else {
                return Ok(None);
            };
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT a.id FROM ln_addresses a JOIN offers o ON o.offer_id = a.offer_id \
                 WHERE lower(o.offer) = $1 LIMIT 1",
            )
            .bind(&offer)
            .fetch_optional(&self.pool)
            .await?;
            return match id {
                Some(id) => store::address(&self.pool, id).await,
                None => Ok(None),
            };
        }
        let Some(url) = lnurl_url(&lower) else {
            return Ok(None);
        };
        let Some((domain, user)) = lnurl_username(&url) else {
            return Ok(None);
        };
        self.own_address_at(&domain, &user).await
    }

    /// One of our addresses on `domain`, if it exists.
    async fn own_address_at(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<store::Address>, DomainError> {
        let Some(d) = store::domain_by_name(&self.pool, domain.trim_end_matches('.')).await? else {
            return Ok(None);
        };
        Ok(store::addresses(&self.pool, Some(d.id))
            .await?
            .into_iter()
            .find(|a| a.username.eq_ignore_ascii_case(username)))
    }

    /// Whether `from` forwards to `target`, following the splits of our own addresses.
    /// `proposed` stands in for the splits of one address (a list being saved).
    async fn forwards_to(
        &self,
        from: i64,
        target: i64,
        proposed: Option<(i64, &[String])>,
    ) -> Result<bool, DomainError> {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(id) = stack.pop() {
            if id == target {
                return Ok(true);
            }
            if !seen.insert(id) {
                continue;
            }
            let destinations: Vec<String> = match proposed {
                Some((saving, list)) if saving == id => list.to_vec(),
                _ => {
                    // Only destinations: this runs inside the loop check and must not
                    // re-enter kind resolution.
                    sqlx::query_scalar(
                        "SELECT destination FROM prism_splits WHERE address_id = $1 \
                         ORDER BY position",
                    )
                    .bind(id)
                    .fetch_all(&self.pool)
                    .await?
                }
            };
            for d in destinations {
                if let Some(a) = self.own_destination(&d).await? {
                    stack.push(a.id);
                }
            }
        }
        Ok(false)
    }

    /// Why paying `destination` would loop money back to this address, if it would.
    /// `None`: it is not one of our addresses, or does not lead back here.
    pub(crate) async fn loop_reason(
        &self,
        address_id: i64,
        destination: &str,
    ) -> Result<Option<String>, DomainError> {
        let Some(target) = self.own_destination(destination).await? else {
            return Ok(None);
        };
        if target.id == address_id {
            return Ok(Some(format!(
                "{destination} is this address: a split cannot forward to itself"
            )));
        }
        if self.forwards_to(target.id, address_id, None).await? {
            return Ok(Some(format!(
                "{destination} forwards back to this address; the payout was not sent to \
                 avoid a loop"
            )));
        }
        Ok(None)
    }

    /// Replaces an address's splits. Payouts already made are history and stay; payouts that
    /// never went out for a destination the owner removed are canceled, not paid or retried.
    pub async fn set_prism_splits(
        &self,
        address_id: i64,
        input: &[(String, i64)],
    ) -> Result<Vec<Split>, DomainError> {
        store::address(&self.pool, address_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("no such Lightning address".into()))?;
        let mut clean = normalize_splits(input)?;
        // Check what each destination really is: a plain name must answer BIP-353 or
        // LNURL, and one of ours must have the kind it is named by. An address that
        // cannot receive a Lightning payment is refused here, not on every payout.
        for (destination, kind, _) in &mut clean {
            *kind = self.checked_split_kind(destination, kind).await?;
        }
        // A split that pays one of our own addresses which forwards back here would split
        // its own payment again, forever. The worker checks this again before every payout
        // (a save racing another one, or an older configuration, must not loop either).
        let destinations: Vec<String> = clean.iter().map(|(d, _, _)| d.clone()).collect();
        for (destination, _, _) in &clean {
            let Some(target) = self.own_destination(destination).await? else {
                continue;
            };
            if target.id == address_id {
                return Err(DomainError::BadRequest(format!(
                    "{destination} is this address: a split cannot forward to itself"
                )));
            }
            if self.forwards_to(target.id, address_id, Some((address_id, &destinations))).await? {
                return Err(DomainError::BadRequest(format!(
                    "{destination} forwards back to this address through another prism; \
                     a split cannot loop"
                )));
            }
        }
        let t = now();
        let mut tx = write_tx(&self.pool).await?;
        let kept: Vec<&str> = clean.iter().map(|(d, _, _)| d.as_str()).collect();
        // Payouts that never went out are canceled with the split: one already on its way is
        // settled by the worker and recorded as it went, and the rest are history.
        let existing = sqlx::query(
            "SELECT DISTINCT destination FROM prism_payouts \
             WHERE address_id = $1 AND state IN ('PENDING', 'FAILED')",
        )
        .bind(address_id)
        .fetch_all(&mut *tx)
        .await?;
        use sqlx::Row;
        for row in &existing {
            let destination: String = row.try_get("destination")?;
            if !kept.contains(&destination.as_str()) {
                sqlx::query(
                    "UPDATE prism_payouts SET state = 'CANCELED', next_retry_at = NULL, \
                     last_error = 'the split was removed; this payout was not sent', \
                     updated_at = $1 WHERE address_id = $2 AND destination = $3 \
                     AND state IN ('PENDING', 'FAILED')",
                )
                .bind(t)
                .bind(address_id)
                .bind(&destination)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query("DELETE FROM prism_splits WHERE address_id = $1")
            .bind(address_id)
            .execute(&mut *tx)
            .await?;
        for (position, (destination, kind, percent_bps)) in clean.iter().enumerate() {
            sqlx::query(
                "INSERT INTO prism_splits (address_id, position, destination, kind, percent_bps, \
                 created_at) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(address_id)
            .bind(position as i64)
            .bind(destination)
            .bind(kind)
            .bind(percent_bps)
            .bind(t)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.prism_splits(address_id).await
    }

    /// Per-destination totals for one address.
    pub async fn prism_split_stats(
        &self,
        address_id: i64,
    ) -> Result<std::collections::HashMap<String, SplitStats>, DomainError> {
        let rows = sqlx::query(
            "SELECT destination, state, COUNT(*) AS n, CAST(SUM(amount_msat) AS BIGINT) AS msat \
             FROM prism_payouts WHERE address_id = $1 GROUP BY destination, state",
        )
        .bind(address_id)
        .fetch_all(&self.pool)
        .await?;
        use sqlx::Row;
        let mut out: std::collections::HashMap<String, SplitStats> = Default::default();
        for r in &rows {
            let destination: String = r.try_get("destination")?;
            let state: String = r.try_get("state")?;
            let n: i64 = r.try_get("n")?;
            let msat: i64 = r.try_get("msat")?;
            let s = out.entry(destination).or_default();
            match state.as_str() {
                "PAID" => s.forwarded_msat += msat,
                "FAILED" => {
                    s.failed_msat += msat;
                    s.failed_count += n;
                }
                "PENDING" | "PAYING" => {
                    s.pending_msat += msat;
                    s.pending_count += n;
                }
                "SKIPPED" => s.skipped_msat += msat,
                _ => {}
            }
        }
        let errors = sqlx::query(
            "SELECT destination, last_error, updated_at FROM prism_payouts \
             WHERE address_id = $1 AND state = 'FAILED' AND last_error IS NOT NULL \
             ORDER BY updated_at DESC",
        )
        .bind(address_id)
        .fetch_all(&self.pool)
        .await?;
        for r in &errors {
            let destination: String = r.try_get("destination")?;
            if let Some(s) = out.get_mut(&destination) {
                if s.last_error.is_none() {
                    s.last_error = r.try_get("last_error")?;
                    s.last_error_at = r.try_get("updated_at")?;
                }
            }
        }
        Ok(out)
    }

    /// Address-wide totals, for the "failed" line and the badge.
    pub async fn prism_stats(&self, address_id: i64) -> Result<PrismStats, DomainError> {
        let rows = sqlx::query(
            "SELECT state, COUNT(*) AS n, CAST(SUM(amount_msat) AS BIGINT) AS msat \
             FROM prism_payouts WHERE address_id = $1 GROUP BY state",
        )
        .bind(address_id)
        .fetch_all(&self.pool)
        .await?;
        use sqlx::Row;
        let mut stats = PrismStats::default();
        for r in &rows {
            let state: String = r.try_get("state")?;
            let n: i64 = r.try_get("n")?;
            let msat: i64 = r.try_get("msat")?;
            match state.as_str() {
                "PAID" => stats.forwarded_msat += msat,
                "FAILED" => {
                    stats.failed_msat += msat;
                    stats.failed_count += n;
                }
                "PENDING" | "PAYING" => {
                    stats.pending_msat += msat;
                    stats.pending_count += n;
                }
                "SKIPPED" => stats.skipped_msat += msat,
                _ => {}
            }
        }
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT incoming_tx_id) AS n FROM prism_payouts WHERE address_id = $1",
        )
        .bind(address_id)
        .fetch_one(&self.pool)
        .await?;
        stats.payments = row.try_get("n")?;
        Ok(stats)
    }

    /// The `prism` object of one address in the domains view.
    pub async fn prism_view(&self, address_id: i64) -> Result<Value, DomainError> {
        self.address_by_id(address_id).await?;
        let splits = self.prism_splits(address_id).await?;
        let stats = self.prism_split_stats(address_id).await?;
        let totals = self.prism_stats(address_id).await?;
        Ok(json!({
            "splits": splits
                .iter()
                .map(|s| {
                    let st = stats.get(&s.destination).cloned().unwrap_or_default();
                    let mut v = json!({
                        "id": s.id,
                        "destination": s.destination,
                        "kind": s.kind,
                        "position": s.position,
                        "percent_bps": s.percent_bps,
                        "forwarded_msat": st.forwarded_msat,
                        "failed_msat": st.failed_msat,
                        "pending_msat": st.pending_msat,
                        "pending_count": st.pending_count,
                        "failed_count": st.failed_count,
                        "skipped_msat": st.skipped_msat,
                    });
                    if let Some(e) = &st.last_error {
                        v["last_error"] = json!(e);
                        v["last_error_at"] = json!(st.last_error_at);
                    }
                    v
                })
                .collect::<Vec<_>>(),
            "split_bps": splits.iter().map(|s| s.percent_bps).sum::<i64>(),
            "stats": {
                "forwarded_msat": totals.forwarded_msat,
                "failed_msat": totals.failed_msat,
                "pending_msat": totals.pending_msat,
                "pending_count": totals.pending_count,
                "failed_count": totals.failed_count,
                "skipped_msat": totals.skipped_msat,
                "payments": totals.payments,
            },
        }))
    }

    /// Puts failed payouts of an address back in the queue ("Retry now").
    pub async fn prism_retry(&self, address_id: i64) -> Result<u64, DomainError> {
        store::address(&self.pool, address_id)
            .await?
            .ok_or_else(|| DomainError::NotFound("no such Lightning address".into()))?;
        let t = now();
        // Only destinations that are still splits: a payout whose split was removed is not
        // sent anywhere behind the owner's back.
        let res = sqlx::query(
            "UPDATE prism_payouts SET state = 'PENDING', attempts = 0, next_retry_at = $1, \
             updated_at = $1 WHERE address_id = $2 AND state = 'FAILED' \
             AND destination IN (SELECT destination FROM prism_splits WHERE address_id = $2)",
        )
        .bind(t)
        .bind(address_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The address a received payment belongs to, from what the invoice or offer recorded.
    pub(crate) async fn address_of_payment(
        &self,
        metadata: Option<&str>,
        offer_id: Option<&str>,
    ) -> Result<Option<super::Address>, DomainError> {
        let meta: Option<Value> = metadata.and_then(|m| serde_json::from_str(m).ok());
        if let Some(id) = meta.as_ref().and_then(|m| m.get("ln_address_id")).and_then(Value::as_i64)
        {
            if let Some(a) = store::address(&self.pool, id).await? {
                return Ok(Some(a));
            }
        }
        let named = meta
            .as_ref()
            .and_then(|m| m.get("lightning_address").or_else(|| m.get("bip353")))
            .and_then(Value::as_str);
        if let Some(address) = named {
            if let Some((username, domain)) = split_address(address) {
                if let Some(d) = store::domain_by_name(&self.pool, &domain).await? {
                    if let Some(a) = store::addresses(&self.pool, Some(d.id))
                        .await?
                        .into_iter()
                        .find(|a| a.username == username)
                    {
                        return Ok(Some(a));
                    }
                }
            }
        }
        if let Some(offer_id) = offer_id {
            let row = sqlx::query(
                "SELECT id FROM ln_addresses WHERE offer_id = $1 ORDER BY id DESC LIMIT 1",
            )
            .bind(offer_id)
            .fetch_optional(&self.pool)
            .await?;
            if let Some(r) = row {
                use sqlx::Row;
                let id: i64 = r.try_get("id")?;
                return store::address(&self.pool, id).await;
            }
        }
        Ok(None)
    }

    /// Records one payout per split for a received payment, then leaves them to the
    /// worker. Idempotent: the same payment is never split twice.
    pub(crate) async fn split_payment(
        &self,
        address_id: i64,
        incoming_tx_id: i64,
        amount_msat: i64,
        settled_at: i64,
    ) -> Result<usize, DomainError> {
        let splits = self.prism_splits(address_id).await?;
        if splits.is_empty() {
            return Ok(0);
        }
        let t = now();
        let mut created = 0;
        for s in splits {
            // A split added after the payment settled does not claim it retroactively.
            if settled_at < s.created_at {
                continue;
            }
            let (amount, state, error) = match share(amount_msat, s.percent_bps) {
                Some(a) => (a, "PENDING", None),
                None => (amount_msat * s.percent_bps / 10_000, "SKIPPED", Some("less than 1 sat")),
            };
            let res = sqlx::query(
                "INSERT INTO prism_payouts (address_id, incoming_tx_id, destination, kind, \
                 amount_msat, percent_bps, state, last_error, next_retry_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) \
                 ON CONFLICT (incoming_tx_id, destination) DO NOTHING",
            )
            .bind(address_id)
            .bind(incoming_tx_id)
            .bind(&s.destination)
            .bind(&s.kind)
            .bind(amount)
            .bind(s.percent_bps)
            .bind(state)
            .bind(error)
            .bind((state == "PENDING").then_some(t))
            .bind(t)
            .execute(&self.pool)
            .await?;
            created += res.rows_affected() as usize;
        }
        Ok(created)
    }

    /// Pending or due payouts, oldest first.
    pub(crate) async fn due_payouts(&self, t: i64, limit: i64) -> Result<Vec<Payout>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {PAYOUT_COLS} FROM prism_payouts p WHERE (p.state = 'PENDING' \
             OR (p.state = 'FAILED' AND p.next_retry_at IS NOT NULL AND p.next_retry_at <= $1)) \
             AND EXISTS (SELECT 1 FROM prism_splits s \
                         WHERE s.address_id = p.address_id AND s.destination = p.destination) \
             ORDER BY p.id LIMIT $2"
        ))
        .bind(t)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(payout_row).collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Payouts that claimed to be paying but whose worker is gone (a crash, or a restart).
    pub(crate) async fn paying_payouts(&self, limit: i64) -> Result<Vec<Payout>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {PAYOUT_COLS} FROM prism_payouts WHERE state = 'PAYING' ORDER BY id LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(payout_row).collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Incoming payments of the last day that have splits and no payout records yet (the
    /// daemon was down); the worker splits them.
    pub(crate) async fn unsettled_splits(
        &self,
        t: i64,
    ) -> Result<Vec<(i64, i64, i64, i64)>, DomainError> {
        // (incoming tx id, address id, amount msat, settled at)
        let rows = sqlx::query(
            "SELECT tx.id, tx.amount_msat, tx.settled_at, tx.metadata, tx.offer_id \
             FROM transactions tx \
             WHERE tx.direction = 'incoming' AND tx.state = 'SETTLED' \
               AND tx.settled_at >= $1 \
               AND NOT EXISTS (SELECT 1 FROM prism_payouts p WHERE p.incoming_tx_id = tx.id) \
             ORDER BY tx.id DESC LIMIT 200",
        )
        .bind(t - CATCH_UP_SECS)
        .fetch_all(&self.pool)
        .await?;
        use sqlx::Row;
        let mut out = Vec::new();
        for r in &rows {
            let metadata: Option<String> = r.try_get("metadata")?;
            let offer_id: Option<String> = r.try_get("offer_id")?;
            let Some(a) = self.address_of_payment(metadata.as_deref(), offer_id.as_deref()).await?
            else {
                continue;
            };
            let settled_at: Option<i64> = r.try_get("settled_at")?;
            out.push((r.try_get("id")?, a.id, r.try_get("amount_msat")?, settled_at.unwrap_or(0)));
        }
        Ok(out)
    }

    /// Takes a payout for this worker (only one attempt at a time).
    pub(crate) async fn claim_payout(&self, id: i64, t: i64) -> Result<bool, DomainError> {
        let res = sqlx::query(
            "UPDATE prism_payouts SET state = 'PAYING', attempts = attempts + 1, updated_at = $1 \
             WHERE id = $2 AND state IN ('PENDING', 'FAILED')",
        )
        .bind(t)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Marks a payout paid.
    pub(crate) async fn payout_paid(
        &self,
        id: i64,
        amount_msat: i64,
        fee_msat: i64,
        payment_hash: Option<&str>,
        out_tx_id: Option<i64>,
        t: i64,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE prism_payouts SET state = 'PAID', amount_msat = $1, fee_msat = $2, \
             payment_hash = $3, out_tx_id = $4, paid_at = $5, updated_at = $5, \
             next_retry_at = NULL, last_error = NULL WHERE id = $6",
        )
        .bind(amount_msat)
        .bind(fee_msat)
        .bind(payment_hash)
        .bind(out_tx_id)
        .bind(t)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Marks a payout failed and schedules the next attempt (or gives up).
    pub(crate) async fn payout_failed(
        &self,
        id: i64,
        error: &str,
        attempts: i64,
        t: i64,
    ) -> Result<(), DomainError> {
        let next = (attempts < MAX_ATTEMPTS).then(|| t + retry_delay(attempts));
        sqlx::query(
            "UPDATE prism_payouts SET state = 'FAILED', last_error = $1, next_retry_at = $2, \
             updated_at = $3 WHERE id = $4",
        )
        .bind(error.chars().take(500).collect::<String>())
        .bind(next)
        .bind(t)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Puts a payout that is still paying back in the queue, after its outgoing payment
    /// settled or failed.
    pub(crate) async fn payout_state(
        &self,
        id: i64,
        state: &str,
        error: Option<&str>,
        attempts: i64,
        t: i64,
    ) -> Result<(), DomainError> {
        if state == "FAILED" {
            return self.payout_failed(id, error.unwrap_or("payment failed"), attempts, t).await;
        }
        sqlx::query(
            "UPDATE prism_payouts SET state = $1, last_error = $2, next_retry_at = NULL, \
             updated_at = $3 WHERE id = $4",
        )
        .bind(state)
        .bind(error.map(|e| e.chars().take(500).collect::<String>()))
        .bind(t)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// A write transaction. On SQLite it takes the write lock up front (`BEGIN IMMEDIATE`): a
/// deferred transaction that reads first cannot upgrade while another connection writes,
/// and fails with "database is locked" instead of waiting for the busy timeout.
async fn write_tx(
    pool: &sqlx::AnyPool,
) -> Result<sqlx::Transaction<'static, sqlx::Any>, sqlx::Error> {
    if pool.connect_options().database_url.scheme() == "sqlite" {
        pool.begin_with("BEGIN IMMEDIATE").await
    } else {
        pool.begin().await
    }
}

/// Minutes to the next attempt: 1, 5, 15, 60, 240, 720, 1440.
fn retry_delay(attempts: i64) -> i64 {
    const MINUTES: [i64; 7] = [1, 5, 15, 60, 240, 720, 1_440];
    let i = attempts.saturating_sub(1).clamp(0, MINUTES.len() as i64 - 1) as usize;
    MINUTES[i] * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_are_checked() {
        assert_eq!(destination_kind("sats@example.com").unwrap(), "lnurl");
        assert_eq!(destination_kind("₿sats@example.com").unwrap(), "bip353");
        // LUD-01 test vector.
        let lnurl = "LNURL1DP68GURN8GHJ7UM9WFMXJCM99E3K7MF0V9CXJ0M385EKVCENXC6R2C35XVUKXEFCV5MKVV34X5EKZD3EV56NYD3HXQURZEPEXEJXXEPNXSCRVWFNV9NXZCN9XQ6XYEFHVGCXXCMYXYMNSERXFQ5FNS";
        assert_eq!(destination_kind(lnurl).unwrap(), "lnurl");
        assert!(destination_kind("sats@example.com@x").is_err());
        assert!(destination_kind("not a target").is_err());
        assert!(destination_kind("").is_err());
        // A BOLT12 offer is bech32 *without* a checksum (BOLT 12 uses `NoChecksum`), so it
        // must not be validated with `bech32::decode`.
        let bytes: Vec<u8> = (0..120u8).collect();
        let offer =
            bech32::encode::<bech32::NoChecksum>(bech32::Hrp::parse("lno").unwrap(), &bytes)
                .unwrap();
        assert_eq!(destination_kind(&offer).unwrap(), "bolt12");
        // A long offer wrapped with `+` continuations is accepted and kept in one piece.
        let wrapped = format!("{}+  {}", &offer[..120], &offer[120..]);
        assert_eq!(destination_kind(&wrapped).unwrap(), "bolt12");
        assert_eq!(normalize_splits(&[(wrapped, 1_000)]).unwrap()[0].0, offer);
        assert!(destination_kind("lno1notanoffer").is_err());
    }

    #[test]
    fn percentages_stay_at_or_below_hundred() {
        let ok =
            normalize_splits(&[("a@example.com".into(), 5_000), ("b@example.com".into(), 2_500)])
                .unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].1, "address", "a plain name is checked when the splits are saved");
        assert!(normalize_splits(&[
            ("a@example.com".into(), 6_000),
            ("b@example.com".into(), 5_000)
        ])
        .is_err());
        assert!(normalize_splits(&[("a@example.com".into(), 0)]).is_err());
        assert!(normalize_splits(&[("a@example.com".into(), 10_001)]).is_err());
        assert!(normalize_splits(&[
            ("a@example.com".into(), 1_000),
            ("A@example.com".into(), 1_000)
        ])
        .is_err());
    }

    #[test]
    fn shares_are_floored_and_skipped_under_one_sat() {
        assert_eq!(share(100_000, 5_000), Some(50_000));
        assert_eq!(share(10_000, 100), None, "1% of 10 sat is under 1 sat");
        assert_eq!(share(10_000_000, 1), Some(1_000), "0.01% of 10k sat is 1 sat");
        assert_eq!(share(9_999_000, 1), None);
        assert_eq!(share(1_000, 10_000), Some(1_000));
        assert_eq!(share(100_000, 3_333), Some(33_330));
    }

    #[test]
    fn retries_back_off() {
        assert_eq!(retry_delay(1), 60);
        assert_eq!(retry_delay(2), 300);
        assert_eq!(retry_delay(3), 900);
        assert_eq!(retry_delay(7), 86_400);
        assert_eq!(retry_delay(99), 86_400);
    }
}
