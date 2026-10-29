//! Transaction and offer rows (SPEC §6.2).

use serde::Serialize;
use sqlx::any::AnyRow;
use sqlx::{AnyConnection, FromRow, Row};

use super::WalletError;

pub const TX_COLUMNS: &str = "id, public_id, app_id, direction, payment_type, state, amount_msat, \
    fee_msat, fee_reserve_msat, max_fee_msat, ln_payment_id, payment_request, payment_hash, preimage, \
    offer_id, offer, payer_note, quantity, payer_proof, description, description_hash, metadata, \
    CAST(self_payment AS INTEGER) AS self_payment, CAST(hold AS INTEGER) AS hold, settle_deadline, claimable_amount_msat, counterpart_tx_id, failure_reason, \
    labels, created_at, updated_at, expires_at, settled_at";

/// One payment record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tx {
    #[serde(skip)]
    pub id: i64,
    pub public_id: String,
    pub app_id: Option<i64>,
    /// incoming | outgoing
    pub direction: String,
    /// bolt11 | bolt12 | internal
    pub payment_type: String,
    /// PENDING | ACCEPTED | SETTLED | FAILED | EXPIRED | CANCELED
    pub state: String,
    pub amount_msat: i64,
    pub fee_msat: i64,
    pub fee_reserve_msat: i64,
    pub max_fee_msat: Option<i64>,
    #[serde(skip)]
    pub ln_payment_id: Option<String>,
    pub payment_request: Option<String>,
    pub payment_hash: Option<String>,
    pub preimage: Option<String>,
    pub offer_id: Option<String>,
    pub offer: Option<String>,
    pub payer_note: Option<String>,
    pub quantity: Option<i64>,
    pub payer_proof: Option<String>,
    pub description: String,
    pub description_hash: Option<String>,
    pub metadata: Option<String>,
    pub self_payment: bool,
    pub hold: bool,
    pub settle_deadline: Option<i64>,
    pub claimable_amount_msat: Option<i64>,
    #[serde(skip)]
    pub counterpart_tx_id: Option<i64>,
    pub failure_reason: Option<String>,
    /// JSON array of strings, stored as text.
    #[serde(serialize_with = "labels_as_array")]
    pub labels: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
    pub settled_at: Option<i64>,
    /// Who the payment came from or went to, as far as the payment tells (derived).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<super::counterparty::Counterparty>,
}

fn labels_as_array<S: serde::Serializer>(labels: &str, s: S) -> Result<S::Ok, S::Error> {
    let v: Vec<String> = serde_json::from_str(labels).unwrap_or_default();
    serde::Serialize::serialize(&v, s)
}

// The Any driver cannot decode SQLite BOOLEAN columns, so they are selected as
// integers (see TX_COLUMNS) and converted here.
impl<'r> FromRow<'r, AnyRow> for Tx {
    fn from_row(r: &'r AnyRow) -> Result<Self, sqlx::Error> {
        let direction: String = r.try_get("direction")?;
        let description: String = r.try_get("description")?;
        let metadata: Option<String> = r.try_get("metadata")?;
        let payer_note: Option<String> = r.try_get("payer_note")?;
        let counterparty = super::counterparty::of(
            &direction,
            &description,
            metadata.as_deref(),
            payer_note.as_deref(),
        );
        Ok(Tx {
            counterparty,
            id: r.try_get("id")?,
            public_id: r.try_get("public_id")?,
            app_id: r.try_get("app_id")?,
            direction,
            payment_type: r.try_get("payment_type")?,
            state: r.try_get("state")?,
            amount_msat: r.try_get("amount_msat")?,
            fee_msat: r.try_get("fee_msat")?,
            fee_reserve_msat: r.try_get("fee_reserve_msat")?,
            max_fee_msat: r.try_get("max_fee_msat")?,
            ln_payment_id: r.try_get("ln_payment_id")?,
            payment_request: r.try_get("payment_request")?,
            payment_hash: r.try_get("payment_hash")?,
            preimage: r.try_get("preimage")?,
            offer_id: r.try_get("offer_id")?,
            offer: r.try_get("offer")?,
            payer_note,
            quantity: r.try_get("quantity")?,
            payer_proof: r.try_get("payer_proof")?,
            description,
            description_hash: r.try_get("description_hash")?,
            metadata,
            self_payment: r.try_get::<i64, _>("self_payment")? != 0,
            hold: r.try_get::<i64, _>("hold")? != 0,
            settle_deadline: r.try_get("settle_deadline")?,
            claimable_amount_msat: r.try_get("claimable_amount_msat")?,
            counterpart_tx_id: r.try_get("counterpart_tx_id")?,
            failure_reason: r.try_get("failure_reason")?,
            labels: r.try_get("labels")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
            expires_at: r.try_get("expires_at")?,
            settled_at: r.try_get("settled_at")?,
        })
    }
}

/// Fields for a new transaction.
#[derive(Debug, Clone, Default)]
pub struct NewTx {
    pub app_id: Option<i64>,
    pub direction: &'static str,
    pub payment_type: &'static str,
    pub state: &'static str,
    pub amount_msat: i64,
    pub fee_reserve_msat: i64,
    pub max_fee_msat: Option<i64>,
    pub ln_payment_id: Option<String>,
    pub payment_request: Option<String>,
    pub payment_hash: Option<String>,
    pub preimage: Option<String>,
    pub offer_id: Option<String>,
    pub offer: Option<String>,
    pub payer_note: Option<String>,
    pub quantity: Option<i64>,
    pub description: String,
    pub description_hash: Option<String>,
    pub metadata: Option<String>,
    pub self_payment: bool,
    pub hold: bool,
    pub expires_at: Option<i64>,
    pub settled_at: Option<i64>,
}

pub fn new_public_id() -> String {
    ulid::Ulid::new().to_string()
}

pub async fn insert(conn: &mut AnyConnection, t: &NewTx, now: i64) -> Result<Tx, WalletError> {
    let public_id = new_public_id();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO transactions (public_id, app_id, direction, payment_type, state, amount_msat, \
         fee_reserve_msat, max_fee_msat, ln_payment_id, payment_request, payment_hash, preimage, offer_id, \
         offer, payer_note, quantity, description, description_hash, metadata, self_payment, hold, \
         created_at, updated_at, expires_at, settled_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
         $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $22, $23, $24) RETURNING id",
    )
    .bind(&public_id)
    .bind(t.app_id)
    .bind(t.direction)
    .bind(t.payment_type)
    .bind(t.state)
    .bind(t.amount_msat)
    .bind(t.fee_reserve_msat)
    .bind(t.max_fee_msat)
    .bind(&t.ln_payment_id)
    .bind(&t.payment_request)
    .bind(&t.payment_hash)
    .bind(&t.preimage)
    .bind(&t.offer_id)
    .bind(&t.offer)
    .bind(&t.payer_note)
    .bind(t.quantity)
    .bind(&t.description)
    .bind(&t.description_hash)
    .bind(&t.metadata)
    .bind(t.self_payment)
    .bind(t.hold)
    .bind(now)
    .bind(t.expires_at)
    .bind(t.settled_at)
    .fetch_one(&mut *conn)
    .await?;
    if t.state == "SETTLED" {
        super::outbox::enqueue(conn, id, now).await?;
    }
    by_id(conn, id)
        .await?
        .ok_or_else(|| WalletError::Internal("inserted transaction vanished".into()))
}

pub async fn by_id(conn: &mut AnyConnection, id: i64) -> Result<Option<Tx>, WalletError> {
    Ok(sqlx::query_as::<_, Tx>(&format!("SELECT {TX_COLUMNS} FROM transactions WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?)
}

pub async fn by_public_id(
    conn: &mut AnyConnection,
    public_id: &str,
) -> Result<Option<Tx>, WalletError> {
    Ok(sqlx::query_as::<_, Tx>(&format!(
        "SELECT {TX_COLUMNS} FROM transactions WHERE public_id = $1"
    ))
    .bind(public_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Newest non-self transaction in `direction` for a backend payment id or hash.
pub async fn by_ln(
    conn: &mut AnyConnection,
    direction: &str,
    ln_payment_id: &str,
    payment_hash: Option<&str>,
) -> Result<Option<Tx>, WalletError> {
    Ok(sqlx::query_as::<_, Tx>(&format!(
        "SELECT {TX_COLUMNS} FROM transactions WHERE direction = $1 AND self_payment = FALSE \
         AND (ln_payment_id = $2 OR payment_hash = $3) ORDER BY id DESC LIMIT 1"
    ))
    .bind(direction)
    .bind(ln_payment_id)
    .bind(payment_hash.unwrap_or("-"))
    .fetch_optional(&mut *conn)
    .await?)
}

/// Moves a transaction between states; returns false when it was not in `from`.
pub async fn transition(
    conn: &mut AnyConnection,
    id: i64,
    from: &[&str],
    to: &str,
    now: i64,
) -> Result<bool, WalletError> {
    let list = from.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
    let settled = if to == "SETTLED" { Some(now) } else { None };
    let n = sqlx::query(&format!(
        "UPDATE transactions SET state = $1, updated_at = $2, settled_at = COALESCE($3, settled_at) \
         WHERE id = $4 AND state IN ({list})"
    ))
    .bind(to)
    .bind(now)
    .bind(settled)
    .bind(id)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if n == 1 && (to == "SETTLED" || to == "ACCEPTED") {
        super::outbox::enqueue(conn, id, now).await?;
    }
    Ok(n == 1)
}

/// A stored offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OfferRow {
    #[serde(skip)]
    pub id: i64,
    pub offer_id: String,
    pub offer: String,
    pub app_id: Option<i64>,
    pub amount_msat: Option<i64>,
    pub description: Option<String>,
    pub single_use: bool,
    pub metadata: Option<String>,
    pub state: String,
    pub expires_at: Option<i64>,
    pub created_at: i64,
}

pub const OFFER_COLUMNS: &str = "id, offer_id, offer, app_id, amount_msat, description, \
    CAST(single_use AS INTEGER) AS single_use, metadata, state, expires_at, created_at";

impl<'r> FromRow<'r, AnyRow> for OfferRow {
    fn from_row(r: &'r AnyRow) -> Result<Self, sqlx::Error> {
        Ok(OfferRow {
            id: r.try_get("id")?,
            offer_id: r.try_get("offer_id")?,
            offer: r.try_get("offer")?,
            app_id: r.try_get("app_id")?,
            amount_msat: r.try_get("amount_msat")?,
            description: r.try_get("description")?,
            single_use: r.try_get::<i64, _>("single_use")? != 0,
            metadata: r.try_get("metadata")?,
            state: r.try_get("state")?,
            expires_at: r.try_get("expires_at")?,
            created_at: r.try_get("created_at")?,
        })
    }
}

pub async fn offer_by_id(
    conn: &mut AnyConnection,
    offer_id: &str,
) -> Result<Option<OfferRow>, WalletError> {
    Ok(sqlx::query_as::<_, OfferRow>(&format!(
        "SELECT {OFFER_COLUMNS} FROM offers WHERE offer_id = $1"
    ))
    .bind(offer_id)
    .fetch_optional(&mut *conn)
    .await?)
}
