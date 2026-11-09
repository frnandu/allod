//! Rows of the domain tables (migration 0013) and their queries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::any::AnyRow;
use sqlx::{AnyPool, Row};

use super::{now, DomainError};

/// A record Allod created at a DNS provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedRecord {
    pub id: String,
    pub zone: String,
    pub kind: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct DnsProvider {
    pub id: i64,
    pub kind: String,
    pub label: String,
    pub created_at: i64,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Domain {
    pub id: i64,
    pub name: String,
    pub env: bool,
    pub dns_provider_id: Option<i64>,
    pub gateway_id: Option<i64>,
    pub direct: bool,
    /// `"TYPE name@zone"` → record.
    pub managed: BTreeMap<String, ManagedRecord>,
    pub synced_at: Option<i64>,
    pub error: Option<String>,
    pub reach_state: Option<String>,
    pub reach_checked_at: Option<i64>,
    pub reach_detail: Option<String>,
    pub created_at: i64,
    pub changed_at: i64,
}

impl Domain {
    /// Served over HTTPS (certificate, LNURL).
    pub fn served(&self) -> bool {
        self.gateway_id.is_some() || self.direct
    }
}

#[derive(Debug, Clone)]
pub struct Gateway {
    pub id: i64,
    pub label: String,
    /// `manual` | `lnvps`.
    pub provider: String,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    /// `dedicated` | `shared`.
    pub mode: String,
    pub tunnel_index: i64,
    /// `setup` | `active` | `offline` | `expired` | `error` | `ordering` (LNVPS, bought
    /// but not set up yet).
    pub state: String,
    pub provider_ref: Option<String>,
    pub provider_data: Option<String>,
    pub expires_at: Option<i64>,
    pub auto_renew: bool,
    pub renew_max_sats: Option<i64>,
    pub last_handshake_at: Option<i64>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct Address {
    pub id: i64,
    pub domain_id: i64,
    pub username: String,
    /// `None`: Main.
    pub app_id: Option<i64>,
    pub offer_id: Option<String>,
    pub verified: bool,
    pub verified_at: Option<i64>,
    pub created_at: i64,
}

/// Booleans are selected as integers: the Any driver cannot decode SQLite's BOOLEAN.
fn b(r: &AnyRow, col: &str) -> Result<bool, sqlx::Error> {
    Ok(r.try_get::<i64, _>(col)? != 0)
}

const DOMAIN_COLS: &str =
    "SELECT id, name, CAST(env AS INTEGER) AS env, dns_provider_id, gateway_id, \
    CAST(direct AS INTEGER) AS direct, managed, synced_at, error, reach_state, reach_checked_at, \
    reach_detail, created_at, changed_at FROM domains";
const GATEWAY_COLS: &str = "SELECT id, label, provider, ipv4, ipv6, mode, \
    CAST(tunnel_index AS BIGINT) AS tunnel_index, state, provider_ref, provider_data, expires_at, \
    CAST(auto_renew AS INTEGER) AS auto_renew, renew_max_sats, last_handshake_at, checked_at, error, \
    created_at FROM gateways";
const ADDRESS_COLS: &str = "SELECT id, domain_id, username, app_id, offer_id, \
    CAST(verified AS INTEGER) AS verified, verified_at, created_at FROM ln_addresses";

fn provider_row(r: &AnyRow) -> Result<DnsProvider, sqlx::Error> {
    Ok(DnsProvider {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        label: r.try_get("label")?,
        created_at: r.try_get("created_at")?,
        checked_at: r.try_get("checked_at")?,
        error: r.try_get("error")?,
    })
}

fn domain_row(r: &AnyRow) -> Result<Domain, sqlx::Error> {
    let managed: String = r.try_get("managed")?;
    Ok(Domain {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        env: b(r, "env")?,
        dns_provider_id: r.try_get("dns_provider_id")?,
        gateway_id: r.try_get("gateway_id")?,
        direct: b(r, "direct")?,
        managed: serde_json::from_str(&managed).unwrap_or_default(),
        synced_at: r.try_get("synced_at")?,
        error: r.try_get("error")?,
        reach_state: r.try_get("reach_state")?,
        reach_checked_at: r.try_get("reach_checked_at")?,
        reach_detail: r.try_get("reach_detail")?,
        created_at: r.try_get("created_at")?,
        changed_at: r.try_get("changed_at")?,
    })
}

fn gateway_row(r: &AnyRow) -> Result<Gateway, sqlx::Error> {
    Ok(Gateway {
        id: r.try_get("id")?,
        label: r.try_get("label")?,
        provider: r.try_get("provider")?,
        ipv4: r.try_get("ipv4")?,
        ipv6: r.try_get("ipv6")?,
        mode: r.try_get("mode")?,
        tunnel_index: r.try_get("tunnel_index")?,
        state: r.try_get("state")?,
        provider_ref: r.try_get("provider_ref")?,
        provider_data: r.try_get("provider_data")?,
        expires_at: r.try_get("expires_at")?,
        auto_renew: b(r, "auto_renew")?,
        renew_max_sats: r.try_get("renew_max_sats")?,
        last_handshake_at: r.try_get("last_handshake_at")?,
        checked_at: r.try_get("checked_at")?,
        error: r.try_get("error")?,
        created_at: r.try_get("created_at")?,
    })
}

fn address_row(r: &AnyRow) -> Result<Address, sqlx::Error> {
    Ok(Address {
        id: r.try_get("id")?,
        domain_id: r.try_get("domain_id")?,
        username: r.try_get("username")?,
        app_id: r.try_get("app_id")?,
        offer_id: r.try_get("offer_id")?,
        verified: b(r, "verified")?,
        verified_at: r.try_get("verified_at")?,
        created_at: r.try_get("created_at")?,
    })
}

fn rows<T>(
    v: Vec<AnyRow>,
    f: fn(&AnyRow) -> Result<T, sqlx::Error>,
) -> Result<Vec<T>, DomainError> {
    v.iter().map(f).collect::<Result<_, _>>().map_err(DomainError::from)
}

// ---------------------------------------------------------------- providers

pub async fn insert_provider(pool: &AnyPool, kind: &str, label: &str) -> Result<i64, DomainError> {
    Ok(sqlx::query(
        "INSERT INTO dns_providers (kind, label, created_at) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(kind)
    .bind(label)
    .bind(now())
    .fetch_one(pool)
    .await?
    .try_get("id")?)
}

pub async fn list_providers(pool: &AnyPool) -> Result<Vec<DnsProvider>, DomainError> {
    rows(
        sqlx::query("SELECT * FROM dns_providers ORDER BY id").fetch_all(pool).await?,
        provider_row,
    )
}

pub async fn provider(pool: &AnyPool, id: i64) -> Result<Option<DnsProvider>, DomainError> {
    let r = sqlx::query("SELECT * FROM dns_providers WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(r.as_ref().map(provider_row).transpose()?)
}

pub async fn set_provider_status(
    pool: &AnyPool,
    id: i64,
    error: Option<&str>,
) -> Result<(), DomainError> {
    sqlx::query("UPDATE dns_providers SET checked_at = $1, error = $2 WHERE id = $3")
        .bind(now())
        .bind(error)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- domains

pub async fn insert_domain(
    pool: &AnyPool,
    name: &str,
    dns_provider_id: Option<i64>,
    gateway_id: Option<i64>,
    direct: bool,
    env: bool,
) -> Result<i64, DomainError> {
    let t = now();
    Ok(sqlx::query(
        "INSERT INTO domains (name, env, dns_provider_id, gateway_id, direct, created_at, changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING id",
    )
    .bind(name)
    .bind(env)
    .bind(dns_provider_id)
    .bind(gateway_id)
    .bind(direct)
    .bind(t)
    .fetch_one(pool)
    .await?
    .try_get("id")?)
}

pub async fn list_domains(pool: &AnyPool) -> Result<Vec<Domain>, DomainError> {
    rows(sqlx::query(&format!("{DOMAIN_COLS} ORDER BY id")).fetch_all(pool).await?, domain_row)
}

pub async fn domain(pool: &AnyPool, id: i64) -> Result<Option<Domain>, DomainError> {
    let r =
        sqlx::query(&format!("{DOMAIN_COLS} WHERE id = $1")).bind(id).fetch_optional(pool).await?;
    Ok(r.as_ref().map(domain_row).transpose()?)
}

pub async fn domain_by_name(pool: &AnyPool, name: &str) -> Result<Option<Domain>, DomainError> {
    let q = format!("{DOMAIN_COLS} WHERE name = $1");
    let r = sqlx::query(&q).bind(name).fetch_optional(pool).await?;
    Ok(r.as_ref().map(domain_row).transpose()?)
}

/// Stores a sync's outcome: the records Allod keeps and the error, if any.
pub async fn set_sync(
    pool: &AnyPool,
    id: i64,
    managed: &BTreeMap<String, ManagedRecord>,
    error: Option<&str>,
) -> Result<(), DomainError> {
    sqlx::query("UPDATE domains SET managed = $1, synced_at = $2, error = $3 WHERE id = $4")
        .bind(serde_json::to_string(managed).unwrap_or_else(|_| "{}".into()))
        .bind(now())
        .bind(error)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_reach(
    pool: &AnyPool,
    id: i64,
    state: Option<&str>,
    detail: Option<&str>,
) -> Result<(), DomainError> {
    sqlx::query(
        "UPDATE domains SET reach_state = $1, reach_detail = $2, reach_checked_at = $3 WHERE id = $4",
    )
    .bind(state)
    .bind(detail)
    .bind(now())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- gateways

pub async fn insert_gateway(
    pool: &AnyPool,
    label: &str,
    provider: &str,
    ipv4: Option<&str>,
    mode: &str,
    tunnel_index: i64,
) -> Result<i64, DomainError> {
    Ok(sqlx::query(
        "INSERT INTO gateways (label, provider, ipv4, mode, tunnel_index, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(label)
    .bind(provider)
    .bind(ipv4)
    .bind(mode)
    .bind(tunnel_index)
    .bind(now())
    .fetch_one(pool)
    .await?
    .try_get("id")?)
}

pub async fn list_gateways(pool: &AnyPool) -> Result<Vec<Gateway>, DomainError> {
    rows(sqlx::query(&format!("{GATEWAY_COLS} ORDER BY id")).fetch_all(pool).await?, gateway_row)
}

pub async fn gateway(pool: &AnyPool, id: i64) -> Result<Option<Gateway>, DomainError> {
    let r =
        sqlx::query(&format!("{GATEWAY_COLS} WHERE id = $1")).bind(id).fetch_optional(pool).await?;
    Ok(r.as_ref().map(gateway_row).transpose()?)
}

/// The lowest free tunnel index (1–250).
pub async fn free_tunnel_index(pool: &AnyPool) -> Result<i64, DomainError> {
    let used: Vec<i64> = list_gateways(pool).await?.iter().map(|g| g.tunnel_index).collect();
    (1..=250)
        .find(|i| !used.contains(i))
        .ok_or_else(|| DomainError::Conflict("too many gateways".into()))
}

// ---------------------------------------------------------------- addresses

pub async fn insert_address(
    pool: &AnyPool,
    domain_id: i64,
    username: &str,
    app_id: Option<i64>,
) -> Result<i64, DomainError> {
    Ok(sqlx::query(
        "INSERT INTO ln_addresses (domain_id, username, app_id, created_at) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(domain_id)
    .bind(username)
    .bind(app_id)
    .bind(now())
    .fetch_one(pool)
    .await?
    .try_get("id")?)
}

/// Addresses, of one domain or all.
pub async fn addresses(
    pool: &AnyPool,
    domain_id: Option<i64>,
) -> Result<Vec<Address>, DomainError> {
    let r = match domain_id {
        Some(d) => {
            let q = format!("{ADDRESS_COLS} WHERE domain_id = $1 ORDER BY id");
            sqlx::query(&q).bind(d).fetch_all(pool).await?
        }
        None => sqlx::query(&format!("{ADDRESS_COLS} ORDER BY id")).fetch_all(pool).await?,
    };
    rows(r, address_row)
}

pub async fn address(pool: &AnyPool, id: i64) -> Result<Option<Address>, DomainError> {
    let q = format!("{ADDRESS_COLS} WHERE id = $1");
    let r = sqlx::query(&q).bind(id).fetch_optional(pool).await?;
    Ok(r.as_ref().map(address_row).transpose()?)
}

pub async fn set_offer(pool: &AnyPool, id: i64, offer_id: &str) -> Result<(), DomainError> {
    sqlx::query("UPDATE ln_addresses SET offer_id = $1 WHERE id = $2")
        .bind(offer_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_verified(pool: &AnyPool, id: i64, verified: bool) -> Result<(), DomainError> {
    sqlx::query(
        "UPDATE ln_addresses SET verified = $1, \
         verified_at = CASE WHEN $1 THEN COALESCE(verified_at, $2) ELSE NULL END WHERE id = $3",
    )
    .bind(verified)
    .bind(now())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
