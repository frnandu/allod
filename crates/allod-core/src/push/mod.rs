//! Device notifications over Web Push (SPEC §8.9): the owner's browser subscriptions,
//! endpoints that NWC connections register (NWC-XX `register_push`), the push outbox and
//! the worker that delivers it.

pub mod webpush;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use reqwest::{StatusCode, Url};
use serde::Serialize;
use sqlx::{AnyPool, Row};

use crate::wallet::{outbox, Wallet};
pub use webpush::{PushCryptoError, VapidKey};

/// Subscriptions one owner may register.
pub const MAX_PER_OWNER: i64 = 20;
/// Endpoints one NWC connection may register.
pub const MAX_PER_APP: i64 = 10;
/// Largest plaintext that fits one push message: 4096 bytes minus the `aes128gcm`
/// header (86), the record delimiter (1) and the tag (16).
pub const MAX_PLAINTEXT: usize = 4096 - 86 - 1 - 16;
/// Longest label kept.
const MAX_LABEL: usize = 64;
/// Longest description sent in a payload.
const MAX_DESCRIPTION: usize = 120;
/// How long a push service keeps an undelivered message (seconds).
const TTL_SECS: u32 = 86_400;
/// Abuse limits (SPEC §8.9): a registered endpoint must not turn Allod into a tool
/// against other services. Pushes to one host share a budget, each subscription has its
/// own, excess waits in the queue instead of being sent.
const HOST_BURST: f64 = 30.0;
const HOST_PER_SEC: f64 = 5.0;
const SUB_BURST: f64 = 30.0;
const SUB_PER_SEC: f64 = 0.5;
/// Pending pushes kept per subscription; later notifications are not queued.
pub const MAX_QUEUED: i64 = 500;
/// Consecutive failed deliveries after which a subscription is removed.
pub const MAX_FAILURES: i64 = 50;
/// `sub` claim when Allod has no https public URL.
const DEFAULT_SUBJECT: &str = "https://github.com/frnandu/allod";

/// Push errors surfaced to the API.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("{0}")]
    BadRequest(String),
    #[error("subscription not found")]
    NotFound,
    #[error("{0}")]
    QuotaExceeded(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// A stored subscription.
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: i64,
    /// Set for the owner's own devices.
    pub owner_pubkey: Option<String>,
    /// Set for endpoints a NWC connection registered.
    pub app_id: Option<i64>,
    /// The connection's name, for display.
    pub app_name: Option<String>,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
    pub events: Vec<String>,
    pub hide_amounts: bool,
    pub failures: i64,
    pub last_error: Option<String>,
    pub last_success_at: Option<i64>,
    pub created_at: i64,
}

/// What the API shows: never the endpoint path or the keys.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionView {
    pub id: i64,
    pub label: String,
    /// The connection that registered this endpoint, if a connection did.
    pub app_id: Option<i64>,
    pub app_name: Option<String>,
    pub endpoint_host: String,
    pub events: Vec<String>,
    pub hide_amounts: bool,
    pub created_at: i64,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub failures: i64,
}

impl Subscription {
    pub fn view(&self) -> SubscriptionView {
        SubscriptionView {
            id: self.id,
            label: self.label.clone(),
            app_id: self.app_id,
            app_name: self.app_name.clone(),
            endpoint_host: Url::parse(&self.endpoint)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default(),
            events: self.events.clone(),
            hide_amounts: self.hide_amounts,
            created_at: self.created_at,
            last_success_at: self.last_success_at,
            last_error: self.last_error.clone(),
            failures: self.failures,
        }
    }
}

/// A browser registering (or re-registering) its subscription.
#[derive(Debug, Clone, Default)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: Option<String>,
    pub events: Option<Vec<String>>,
    pub hide_amounts: Option<bool>,
}

/// Changes to a subscription's preferences.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionPatch {
    pub label: Option<String>,
    pub events: Option<Vec<String>>,
    pub hide_amounts: Option<bool>,
}

/// Result of one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The push service accepted the message.
    Sent,
    /// The subscription no longer exists (404/410).
    Gone,
    /// The push service refused this message for good.
    Rejected(String),
    /// Try again later.
    Retry(String),
}

/// What `POST .../test` reports.
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub delivered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Owns the VAPID key, the subscriptions and the delivery worker.
pub struct PushService {
    pool: AnyPool,
    vapid: VapidKey,
    subject: String,
    http: reqwest::Client,
    wallet: Arc<Wallet>,
    /// Tests only: accept http and local endpoints.
    allow_local: AtomicBool,
    /// Token buckets per endpoint host and per subscription.
    host_buckets: StdMutex<HashMap<String, Bucket>>,
    sub_buckets: StdMutex<HashMap<i64, Bucket>>,
}

/// A token bucket.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    /// Refills, then returns how long until one token is available (zero: now).
    fn wait(&mut self, burst: f64, per_sec: f64, now: Instant) -> Duration {
        self.tokens =
            (self.tokens + now.duration_since(self.at).as_secs_f64() * per_sec).min(burst);
        self.at = now;
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / per_sec)
        }
    }
}

/// Takes one token from both buckets, or none: returns the wait when either is empty.
fn take_both(
    hosts: &mut HashMap<String, Bucket>,
    subs: &mut HashMap<i64, Bucket>,
    host: &str,
    sub: i64,
    now: Instant,
) -> Option<Duration> {
    let fresh = |burst: f64| Bucket { tokens: burst, at: now };
    let h = hosts.entry(host.to_string()).or_insert_with(|| fresh(HOST_BURST));
    let wait_h = h.wait(HOST_BURST, HOST_PER_SEC, now);
    let s = subs.entry(sub).or_insert_with(|| fresh(SUB_BURST));
    let wait_s = s.wait(SUB_BURST, SUB_PER_SEC, now);
    let wait = wait_h.max(wait_s);
    if wait > Duration::ZERO {
        return Some(wait);
    }
    h.tokens -= 1.0;
    s.tokens -= 1.0;
    None
}

/// Resolves endpoint hosts to public addresses only (SPEC §8.9). Checking at connection
/// time covers names that point at private addresses, Docker service names and DNS
/// rebinding; IP literals never reach a resolver and are checked by [`check_endpoint`].
struct PublicResolver;

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let public: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public_ip(a.ip()))
                .collect();
            if public.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

impl std::fmt::Debug for PushService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushService").field("vapid", &self.vapid).finish_non_exhaustive()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Checks a push endpoint (SPEC §8.9): https, and not a local or private address.
pub fn check_endpoint(endpoint: &str) -> Result<Url, String> {
    let url = Url::parse(endpoint).map_err(|_| "endpoint is not a URL".to_string())?;
    if url.scheme() != "https" {
        return Err("endpoint must use https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("endpoint must not carry credentials".into());
    }
    let host = url.host_str().ok_or("endpoint has no host")?.to_ascii_lowercase();
    let bare = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err("endpoint must be a public address".into());
        }
        return Ok(url);
    }
    // Single-label names (`bitcoind`, `postgres`) are local, e.g. Docker services.
    if !bare.contains('.')
        || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|s| bare.ends_with(s))
    {
        return Err("endpoint must be a public host".into());
    }
    Ok(url)
}

/// Whether an address is globally reachable unicast. IPv6 forms that embed an IPv4
/// address (mapped, compatible, NAT64, 6to4) are judged by that address.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || o[0] == 0
                || o[0] >= 240 // 240.0.0.0/4 reserved
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)) // 192.0.0.0/24 IETF
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| {
                Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
            };
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            if seg[..6] == [0; 6] && !v6.is_loopback() && !v6.is_unspecified() {
                // ::a.b.c.d (IPv4-compatible)
                return is_public_ip(IpAddr::V4(embedded(seg[6], seg[7])));
            }
            if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                // 64:ff9b::/96 NAT64
                return is_public_ip(IpAddr::V4(embedded(seg[6], seg[7])));
            }
            if seg[0] == 0x2002 {
                // 2002::/16 6to4
                return is_public_ip(IpAddr::V4(embedded(seg[1], seg[2])));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || (seg[0] == 0x2001 && seg[1] == 0) // Teredo (hides an IPv4 address)
                || (seg[0] == 0x100 && seg[1..4] == [0, 0, 0])) // 100::/64 discard
        }
    }
}

fn check_events(events: &[String]) -> Result<Vec<String>, PushError> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        if !outbox::TYPES.contains(&e.as_str()) {
            return Err(PushError::BadRequest(format!("unknown event type {e}")));
        }
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    Ok(out)
}

fn clean_label(label: &str) -> String {
    label.trim().chars().take(MAX_LABEL).collect()
}

/// Subject for the VAPID JWT: the public URL when it is https.
pub fn vapid_subject(public_url: Option<&str>) -> String {
    public_url
        .filter(|u| u.starts_with("https://"))
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_SUBJECT.to_string())
}

const COLUMNS: &str = "s.id, s.owner_pubkey, s.endpoint, s.p256dh, s.auth, s.label, s.events, \
     CAST(s.hide_amounts AS INTEGER), s.failures, s.last_error, s.last_success_at, s.created_at, \
     s.app_id, a.name";
const FROM: &str = "push_subscriptions s LEFT JOIN apps a ON a.id = s.app_id";

fn row_to_sub(r: &sqlx::any::AnyRow) -> Subscription {
    let events: String = r.get(6);
    Subscription {
        id: r.get(0),
        owner_pubkey: r.get(1),
        endpoint: r.get(2),
        p256dh: r.get(3),
        auth: r.get(4),
        label: r.get(5),
        events: events.split(',').filter(|e| !e.is_empty()).map(str::to_string).collect(),
        hide_amounts: r.get::<i64, _>(7) != 0,
        failures: r.get(8),
        last_error: r.get(9),
        last_success_at: r.get(10),
        created_at: r.get(11),
        app_id: r.get(12),
        app_name: r.get(13),
    }
}

impl PushService {
    /// `push_key` is the secret of `m/1237'/4'/0'` ([`allod_keys::NostrKeys::push_key`]).
    pub fn new(
        pool: AnyPool,
        push_key: &[u8; 32],
        wallet: Arc<Wallet>,
        public_url: Option<&str>,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            // A proxy would resolve names itself, around the public-address check.
            .no_proxy()
            .dns_resolver(Arc::new(PublicResolver))
            .build()
            .expect("reqwest client");
        Arc::new(PushService {
            pool,
            vapid: VapidKey::derive(push_key),
            subject: vapid_subject(public_url),
            http,
            wallet,
            allow_local: AtomicBool::new(false),
            host_buckets: StdMutex::new(HashMap::new()),
            sub_buckets: StdMutex::new(HashMap::new()),
        })
    }

    /// Takes a send token for `sub`, or returns how long to wait.
    fn throttle(&self, sub: &Subscription) -> Option<Duration> {
        let host = Url::parse(&sub.endpoint)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        let mut hosts = self.host_buckets.lock().expect("host buckets");
        let mut subs = self.sub_buckets.lock().expect("subscription buckets");
        take_both(&mut hosts, &mut subs, &host, sub.id, Instant::now())
    }

    /// Tests only: accept `http://` and local endpoints (a test push service).
    #[doc(hidden)]
    pub fn allow_local_endpoints(&self) {
        self.allow_local.store(true, Ordering::Relaxed);
    }

    fn check(&self, endpoint: &str) -> Result<Url, String> {
        if self.allow_local.load(Ordering::Relaxed) {
            return Url::parse(endpoint).map_err(|_| "endpoint is not a URL".to_string());
        }
        check_endpoint(endpoint)
    }

    /// The browser's `applicationServerKey`.
    pub fn public_key(&self) -> String {
        self.vapid.public_key()
    }

    /// Every subscription, oldest first.
    pub async fn list(&self) -> Result<Vec<Subscription>, PushError> {
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM {FROM} ORDER BY s.id"))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_to_sub).collect())
    }

    pub async fn get(&self, id: i64) -> Result<Subscription, PushError> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM {FROM} WHERE s.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| row_to_sub(&r))
            .ok_or(PushError::NotFound)
    }

    /// Registers a browser, or refreshes its keys when the endpoint is already known.
    /// Preferences change only when given, so a re-registration keeps them.
    pub async fn upsert(
        &self,
        owner: &str,
        new: NewSubscription,
    ) -> Result<Subscription, PushError> {
        self.check(&new.endpoint).map_err(PushError::BadRequest)?;
        if new.endpoint.len() > 2048 {
            return Err(PushError::BadRequest("endpoint is too long".into()));
        }
        webpush::check_keys(&new.p256dh, &new.auth)
            .map_err(|e| PushError::BadRequest(e.to_string()))?;
        let events = new.events.as_deref().map(check_events).transpose()?;
        let t = now();
        let existing: Option<(i64, Option<i64>)> =
            sqlx::query("SELECT id, app_id FROM push_subscriptions WHERE endpoint = $1")
                .bind(&new.endpoint)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| (r.get(0), r.get(1)));
        let id = match existing {
            Some((_, Some(_))) => {
                return Err(PushError::BadRequest(
                    "this endpoint is registered by a wallet connection".into(),
                ))
            }
            Some((id, None)) => {
                sqlx::query(
                    "UPDATE push_subscriptions SET owner_pubkey = $1, p256dh = $2, auth = $3, \
                     failures = 0, last_error = NULL, updated_at = $4 WHERE id = $5",
                )
                .bind(owner)
                .bind(&new.p256dh)
                .bind(&new.auth)
                .bind(t)
                .bind(id)
                .execute(&self.pool)
                .await?;
                self.patch(
                    id,
                    SubscriptionPatch { label: new.label, events, hide_amounts: new.hide_amounts },
                )
                .await?;
                id
            }
            None => {
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM push_subscriptions WHERE owner_pubkey = $1",
                )
                .bind(owner)
                .fetch_one(&self.pool)
                .await?;
                if count >= MAX_PER_OWNER {
                    return Err(PushError::BadRequest(format!(
                        "at most {MAX_PER_OWNER} devices can receive notifications; remove one first"
                    )));
                }
                let events =
                    events.unwrap_or_else(|| outbox::TYPES.iter().map(|s| s.to_string()).collect());
                sqlx::query_scalar(
                    "INSERT INTO push_subscriptions (owner_pubkey, endpoint, p256dh, auth, label, \
                     events, hide_amounts, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) RETURNING id",
                )
                .bind(owner)
                .bind(&new.endpoint)
                .bind(&new.p256dh)
                .bind(&new.auth)
                .bind(clean_label(new.label.as_deref().unwrap_or_default()))
                .bind(events.join(","))
                .bind(new.hide_amounts.unwrap_or(false))
                .bind(t)
                .fetch_one(&self.pool)
                .await?
            }
        };
        self.get(id).await
    }

    /// `register_push` (NWC-XX): registers or refreshes an endpoint for one connection.
    /// `allowed` are the notification types the connection receives; requested types
    /// outside it are ignored. Returns the types that will be pushed.
    pub async fn register_app(
        &self,
        app_id: i64,
        new: NewSubscription,
        allowed: &[&str],
    ) -> Result<Vec<String>, PushError> {
        self.check(&new.endpoint).map_err(PushError::BadRequest)?;
        if new.endpoint.len() > 2048 {
            return Err(PushError::BadRequest("endpoint is too long".into()));
        }
        webpush::check_keys(&new.p256dh, &new.auth)
            .map_err(|e| PushError::BadRequest(e.to_string()))?;
        let mut types: Vec<String> = Vec::new();
        for t in allowed {
            let wanted = new.events.as_ref().is_none_or(|e| e.iter().any(|x| x == t));
            if wanted {
                types.push(t.to_string());
            }
        }
        let t = now();
        let existing: Option<(i64, Option<i64>)> =
            sqlx::query("SELECT id, app_id FROM push_subscriptions WHERE endpoint = $1")
                .bind(&new.endpoint)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| (r.get(0), r.get(1)));
        match existing {
            Some((id, Some(owner))) if owner == app_id => {
                sqlx::query(
                    "UPDATE push_subscriptions SET p256dh = $1, auth = $2, events = $3, \
                     failures = 0, last_error = NULL, updated_at = $4 WHERE id = $5",
                )
                .bind(&new.p256dh)
                .bind(&new.auth)
                .bind(types.join(","))
                .bind(t)
                .bind(id)
                .execute(&self.pool)
                .await?;
                if let Some(label) = new.label.as_deref() {
                    sqlx::query("UPDATE push_subscriptions SET label = $1 WHERE id = $2")
                        .bind(clean_label(label))
                        .bind(id)
                        .execute(&self.pool)
                        .await?;
                }
            }
            Some(_) => {
                return Err(PushError::BadRequest("this endpoint is already registered".into()))
            }
            None => {
                let count: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions WHERE app_id = $1")
                        .bind(app_id)
                        .fetch_one(&self.pool)
                        .await?;
                if count >= MAX_PER_APP {
                    return Err(PushError::QuotaExceeded(format!(
                        "at most {MAX_PER_APP} push endpoints per connection"
                    )));
                }
                sqlx::query(
                    "INSERT INTO push_subscriptions (app_id, endpoint, p256dh, auth, label, events, \
                     created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
                )
                .bind(app_id)
                .bind(&new.endpoint)
                .bind(&new.p256dh)
                .bind(&new.auth)
                .bind(clean_label(new.label.as_deref().unwrap_or_default()))
                .bind(types.join(","))
                .bind(t)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(types)
    }

    /// `unregister_push` (NWC-XX): removes one of the connection's endpoints. Unknown
    /// endpoints are not an error.
    pub async fn unregister_app(&self, app_id: i64, endpoint: &str) -> Result<(), PushError> {
        sqlx::query("DELETE FROM push_subscriptions WHERE app_id = $1 AND endpoint = $2")
            .bind(app_id)
            .bind(endpoint)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Queues `payload` (see [`nwc_payload`]) for every endpoint of the connection that
    /// wants `kind`. Called when the NWC notification is published; repeated calls for
    /// the same transaction queue nothing new.
    pub async fn enqueue_for_app(
        &self,
        app_id: i64,
        kind: &str,
        transaction_id: i64,
        payload: &str,
    ) -> Result<u64, PushError> {
        let done = sqlx::query(
            "INSERT INTO push_outbox (subscription_id, kind, transaction_id, payload, \
             next_attempt_at, created_at) \
             SELECT s.id, $1, $2, $3, $4, $4 FROM push_subscriptions s \
             WHERE s.app_id = $5 AND (',' || s.events || ',') LIKE ('%,' || CAST($1 AS TEXT) || ',%') \
             AND (SELECT COUNT(*) FROM push_outbox q WHERE q.subscription_id = s.id \
                  AND q.state = 'pending') < $6 \
             ON CONFLICT DO NOTHING",
        )
        .bind(kind)
        .bind(transaction_id)
        .bind(payload)
        .bind(now())
        .bind(app_id)
        .bind(MAX_QUEUED)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Changes preferences; fields left out stay as they are.
    pub async fn patch(
        &self,
        id: i64,
        patch: SubscriptionPatch,
    ) -> Result<Subscription, PushError> {
        let sub = self.get(id).await?;
        let events = match patch.events {
            Some(e) => check_events(&e)?,
            None => sub.events,
        };
        sqlx::query(
            "UPDATE push_subscriptions SET label = $1, events = $2, hide_amounts = $3, \
             updated_at = $4 WHERE id = $5",
        )
        .bind(patch.label.as_deref().map(clean_label).unwrap_or(sub.label))
        .bind(events.join(","))
        .bind(patch.hide_amounts.unwrap_or(sub.hide_amounts))
        .bind(now())
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.get(id).await
    }

    /// Removes a subscription and its queued pushes.
    pub async fn delete(&self, id: i64) -> Result<(), PushError> {
        let done = sqlx::query("DELETE FROM push_subscriptions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(PushError::NotFound);
        }
        Ok(())
    }

    /// Sends a test notification at once, outside the outbox.
    pub async fn test(&self, id: i64) -> Result<TestResult, PushError> {
        let sub = self.get(id).await?;
        if let Some(wait) = self.throttle(&sub) {
            return Ok(TestResult {
                delivered: false,
                status: None,
                error: Some(format!("too many pushes; try again in {} s", wait.as_secs() + 1)),
            });
        }
        let payload = serde_json::json!({
            "v": 1, "type": "test", "transaction_id": null, "account": null,
            "amount_msat": null, "description": null, "created_at": now(),
        });
        let (delivery, status) = self.deliver(&sub, &payload.to_string(), "normal").await;
        self.record(&sub, &delivery).await?;
        Ok(match delivery {
            Delivery::Sent => TestResult { delivered: true, status, error: None },
            Delivery::Gone => TestResult {
                delivered: false,
                status,
                error: Some("the browser removed this subscription; it was deleted".into()),
            },
            Delivery::Rejected(e) | Delivery::Retry(e) => {
                TestResult { delivered: false, status, error: Some(e) }
            }
        })
    }

    /// Encrypts and posts one payload. Returns the outcome and the HTTP status, if any.
    pub async fn deliver(
        &self,
        sub: &Subscription,
        payload: &str,
        urgency: &str,
    ) -> (Delivery, Option<u16>) {
        let url = match self.check(&sub.endpoint) {
            Ok(u) => u,
            Err(e) => return (Delivery::Rejected(e), None),
        };
        let body = match webpush::encrypt(&sub.p256dh, &sub.auth, payload.as_bytes()) {
            Ok(b) => b,
            Err(e) => return (Delivery::Rejected(e.to_string()), None),
        };
        let audience = url.origin().ascii_serialization();
        let auth = self.vapid.authorization(&audience, &self.subject, now());
        let resp = self
            .http
            .post(url)
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("TTL", TTL_SECS.to_string())
            .header("Urgency", urgency)
            .header("Authorization", auth)
            .body(body)
            .send()
            .await;
        match resp {
            // Only the status is kept: response bodies and connection errors from
            // arbitrary URLs must not reach the API (SPEC §8.9).
            Ok(r) => (classify(r.status()), Some(r.status().as_u16())),
            Err(e) => {
                tracing::debug!(error = %e, id = sub.id, "push: delivery failed");
                let what = if e.is_timeout() { "timed out" } else { "unreachable" };
                (Delivery::Retry(format!("push service {what}")), None)
            }
        }
    }

    /// Updates the subscription's health after a delivery.
    async fn record(&self, sub: &Subscription, delivery: &Delivery) -> Result<(), PushError> {
        match delivery {
            Delivery::Sent => {
                sqlx::query(
                    "UPDATE push_subscriptions SET failures = 0, last_error = NULL, \
                     last_success_at = $1 WHERE id = $2",
                )
                .bind(now())
                .bind(sub.id)
                .execute(&self.pool)
                .await?;
            }
            Delivery::Gone => {
                tracing::info!(id = sub.id, "push subscription expired; removed");
                sqlx::query("DELETE FROM push_subscriptions WHERE id = $1")
                    .bind(sub.id)
                    .execute(&self.pool)
                    .await?;
            }
            Delivery::Rejected(e) | Delivery::Retry(e) => {
                sqlx::query(
                    "UPDATE push_subscriptions SET failures = failures + 1, last_error = $1 \
                     WHERE id = $2",
                )
                .bind(e)
                .bind(sub.id)
                .execute(&self.pool)
                .await?;
                let dropped =
                    sqlx::query("DELETE FROM push_subscriptions WHERE id = $1 AND failures >= $2")
                        .bind(sub.id)
                        .bind(MAX_FAILURES)
                        .execute(&self.pool)
                        .await?;
                if dropped.rows_affected() > 0 {
                    tracing::warn!(id = sub.id, "push subscription kept failing; removed");
                }
            }
        }
        Ok(())
    }

    /// Starts the worker that delivers the push outbox.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { me.worker().await });
    }

    async fn worker(self: Arc<Self>) {
        let mut events = self.wallet.subscribe();
        loop {
            if let Err(e) = self.run_due(50).await {
                tracing::warn!(error = %e, "push: outbox run failed");
            }
            // Wake on wallet activity, or poll for retries.
            let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        }
    }

    /// Delivers up to `limit` due rows. Returns how many were attempted.
    pub async fn run_due(&self, limit: i64) -> Result<usize, PushError> {
        let t = now();
        let rows = sqlx::query(
            "SELECT id, subscription_id, kind, transaction_id, attempts, payload FROM push_outbox \
             WHERE state = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= $1) \
             ORDER BY id LIMIT $2",
        )
        .bind(t)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let n = rows.len();
        for r in rows {
            let (id, sub_id, kind, tx_id, attempts): (i64, i64, String, i64, i64) =
                (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
            let stored: Option<String> = r.get(5);
            let sub = match self.get(sub_id).await {
                Ok(s) => s,
                Err(PushError::NotFound) => continue,
                Err(e) => return Err(e),
            };
            if let Some(wait) = self.throttle(&sub) {
                // Over the limit: wait in the queue, without using up an attempt.
                sqlx::query("UPDATE push_outbox SET next_attempt_at = $1 WHERE id = $2")
                    .bind(now() + wait.as_secs() as i64 + 1)
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
                continue;
            }
            // Connection endpoints carry the NWC payload built at publish time.
            let payload = match stored {
                Some(p) => Ok(p),
                None => self.payload(&sub, &kind, tx_id).await,
            };
            let delivery = match payload {
                Ok(payload) => {
                    let urgency = if kind == outbox::PAYMENT_SENT { "normal" } else { "high" };
                    self.deliver(&sub, &payload, urgency).await.0
                }
                Err(e) => Delivery::Retry(format!("payload: {e}")),
            };
            self.finish(id, attempts, &delivery).await?;
            self.record(&sub, &delivery).await?;
        }
        Ok(n)
    }

    async fn finish(&self, id: i64, attempts: i64, delivery: &Delivery) -> Result<(), PushError> {
        let attempts = attempts + 1;
        let (state, next) = match delivery {
            Delivery::Sent => ("sent", None),
            // The subscription and this row are deleted by `record`.
            Delivery::Gone => return Ok(()),
            Delivery::Rejected(_) => ("dead", None),
            Delivery::Retry(_) if attempts >= outbox::MAX_ATTEMPTS => ("dead", None),
            Delivery::Retry(_) => ("pending", Some(now() + (5i64 << attempts.min(12)))),
        };
        sqlx::query(
            "UPDATE push_outbox SET state = $1, attempts = $2, next_attempt_at = $3 WHERE id = $4",
        )
        .bind(state)
        .bind(attempts)
        .bind(next)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The JSON a device receives for one transaction (SPEC §8.9).
    async fn payload(&self, sub: &Subscription, kind: &str, tx_id: i64) -> Result<String, String> {
        let tx = self.wallet.transaction_by_id(tx_id).await.map_err(|e| e.to_string())?;
        let account: Option<String> = match tx.app_id {
            Some(app_id) => sqlx::query_scalar("SELECT name FROM apps WHERE id = $1")
                .bind(app_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| e.to_string())?,
            None => None,
        };
        let description = [tx.description.as_str(), tx.payer_note.as_deref().unwrap_or_default()]
            .into_iter()
            .map(str::trim)
            .find(|d| !d.is_empty())
            .map(|d| d.chars().take(MAX_DESCRIPTION).collect::<String>());
        let (amount, description) =
            if sub.hide_amounts { (None, None) } else { (Some(tx.amount_msat), description) };
        Ok(serde_json::json!({
            "v": 1,
            "type": kind,
            "transaction_id": tx.public_id,
            "account": account,
            "amount_msat": amount,
            "description": description,
            "created_at": tx.created_at,
        })
        .to_string())
    }
}

/// The push payload for a NWC connection (NWC-XX): the NWC-02 notification payload plus
/// the id of the published notification event. When it would not fit one push message,
/// `notification` keeps only `type`, `state`, `payment_hash` and `amount`, and
/// `truncated: true` is added.
pub fn nwc_payload(event_id: &str, kind: &str, notification: &serde_json::Value) -> String {
    let full = serde_json::json!({
        "event_id": event_id,
        "notification_type": kind,
        "notification": notification,
    })
    .to_string();
    if full.len() <= MAX_PLAINTEXT {
        return full;
    }
    let mut short = serde_json::Map::new();
    for k in ["type", "state", "payment_hash", "amount"] {
        if let Some(v) = notification.get(k) {
            short.insert(k.to_string(), v.clone());
        }
    }
    serde_json::json!({
        "event_id": event_id,
        "notification_type": kind,
        "notification": short,
        "truncated": true,
    })
    .to_string()
}

/// Maps a push service response to an outcome (SPEC §8.9).
pub fn classify(status: StatusCode) -> Delivery {
    let reason = || format!("push service answered {}", status.as_u16());
    match status.as_u16() {
        200..=299 => Delivery::Sent,
        404 | 410 => Delivery::Gone,
        400 | 401 | 403 | 413 => Delivery::Rejected(reason()),
        _ => Delivery::Retry(reason()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rules() {
        assert!(check_endpoint("https://fcm.googleapis.com/fcm/send/abc").is_ok());
        assert!(check_endpoint("https://updates.push.services.mozilla.com/wpush/v2/x").is_ok());
        assert!(check_endpoint("https://8.8.8.8/push").is_ok());
        for bad in [
            "http://fcm.googleapis.com/x",
            "https://localhost/x",
            "https://foo.localhost/x",
            "https://printer.local/x",
            "https://svc.internal/x",
            "https://127.0.0.1/x",
            "https://10.1.2.3/x",
            "https://192.168.1.10/x",
            "https://172.16.0.1/x",
            "https://169.254.1.1/x",
            "https://100.64.0.1/x",
            "https://0.0.0.0/x",
            "https://[::1]/x",
            "https://[fd00::1]/x",
            "https://[fe80::1]/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://user:pw@push.example/x",
            "not a url",
            // Docker service names and other local names.
            "https://bitcoind:8332/x",
            "https://postgres/x",
            "https://router.lan/x",
            // Reserved, multicast and IPv6 forms that embed a private IPv4 address.
            "https://224.0.0.1/x",
            "https://198.18.0.1/x",
            "https://240.0.0.1/x",
            "https://255.255.255.255/x",
            "https://[ff02::1]/x",
            "https://[2001:db8::1]/x",
            "https://[::127.0.0.1]/x",
            "https://[64:ff9b::a00:1]/x",
            "https://[2002:c0a8:101::1]/x",
            "https://[2001:0:4136:e378::1]/x",
        ] {
            assert!(check_endpoint(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn embedded_public_ipv4_stays_allowed() {
        assert!(check_endpoint("https://[64:ff9b::808:808]/x").is_ok()); // NAT64 of 8.8.8.8
        assert!(check_endpoint("https://[2002:808:808::1]/x").is_ok()); // 6to4 of 8.8.8.8
        assert!(check_endpoint("https://[2606:4700::1111]/x").is_ok());
    }

    #[tokio::test]
    async fn resolver_refuses_names_with_only_local_addresses() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;
        let err = match PublicResolver
            .resolve(reqwest::dns::Name::from_str("localhost").unwrap())
            .await
        {
            Ok(_) => panic!("localhost must not resolve"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("no public address"), "{err}");
    }

    #[test]
    fn rate_limits_share_a_host_and_refill() {
        let (mut hosts, mut subs) = (HashMap::new(), HashMap::new());
        let t0 = Instant::now();
        // One subscription: its burst, then it waits about 2 s per push.
        for _ in 0..SUB_BURST as usize {
            assert!(take_both(&mut hosts, &mut subs, "push.example", 1, t0).is_none());
        }
        let wait = take_both(&mut hosts, &mut subs, "push.example", 1, t0).unwrap();
        assert!(wait > Duration::from_secs(1) && wait <= Duration::from_secs(2), "{wait:?}");
        assert!(take_both(&mut hosts, &mut subs, "push.example", 1, t0 + Duration::from_secs(2))
            .is_none());
        // Many subscriptions on one host share the host budget.
        let (mut hosts, mut subs) = (HashMap::new(), HashMap::new());
        let mut sent = 0;
        for sub in 0..100 {
            if take_both(&mut hosts, &mut subs, "victim.example", sub, t0).is_none() {
                sent += 1;
            }
        }
        assert_eq!(sent, HOST_BURST as usize);
        // Another host is not affected.
        assert!(take_both(&mut hosts, &mut subs, "other.example", 1000, t0).is_none());
    }

    #[test]
    fn response_classes() {
        assert_eq!(classify(StatusCode::CREATED), Delivery::Sent);
        assert_eq!(classify(StatusCode::GONE), Delivery::Gone);
        assert_eq!(classify(StatusCode::NOT_FOUND), Delivery::Gone);
        assert!(
            matches!(classify(StatusCode::FORBIDDEN), Delivery::Rejected(e) if e == "push service answered 403")
        );
        assert!(matches!(classify(StatusCode::PAYLOAD_TOO_LARGE), Delivery::Rejected(_)));
        assert!(matches!(classify(StatusCode::TOO_MANY_REQUESTS), Delivery::Retry(_)));
        assert!(matches!(classify(StatusCode::BAD_GATEWAY), Delivery::Retry(_)));
    }

    use std::time::Duration;

    use allod_ln::mock::MockBackend;
    use allod_ln::LightningBackend;
    use axum::http::HeaderMap;
    use base64::Engine;
    use tokio::sync::mpsc;

    use crate::wallet::{InvoiceRequest, WalletConfig};

    struct T {
        push: Arc<PushService>,
        wallet: Arc<Wallet>,
        node: Arc<MockBackend>,
        pool: AnyPool,
        _dir: tempfile::TempDir,
    }

    async fn setup() -> T {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/p.db", dir.path().display());
        allod_db::Db::connect(&url).await.unwrap();
        let pool = allod_db::Db::any_pool(&url).await.unwrap();
        let node = Arc::new(MockBackend::new());
        let wallet = Wallet::new(
            pool.clone(),
            node.clone() as Arc<dyn LightningBackend>,
            WalletConfig {
                sync_timeout: Duration::from_millis(300),
                poll_interval: Duration::from_millis(20),
                ..Default::default()
            },
        );
        wallet.reconcile().await.unwrap();
        wallet.start();
        sqlx::query("INSERT INTO owners (pubkey, label, created_at) VALUES ('owner', '', 0)")
            .execute(&pool)
            .await
            .unwrap();
        let push = PushService::new(pool.clone(), &[9; 32], wallet.clone(), None);
        push.allow_local_endpoints();
        T { push, wallet, node, pool, _dir: dir }
    }

    /// A push service on localhost that answers `status` and hands over each request.
    async fn push_server(status: u16) -> (String, mpsc::UnboundedReceiver<(HeaderMap, Vec<u8>)>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/push/{id}",
            axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send((headers, body.to_vec()));
                    axum::http::StatusCode::from_u16(status).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/push/device1"), rx)
    }

    fn browser_keys() -> (p256::SecretKey, String, [u8; 16]) {
        let ua = p256::SecretKey::random(&mut rand::rngs::OsRng);
        let p256dh = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            p256::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(&ua.public_key(), false)
                .as_bytes(),
        );
        (ua, p256dh, [3; 16])
    }

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    #[tokio::test]
    async fn settled_payment_reaches_the_device_encrypted() {
        let t = setup().await;
        let (endpoint, mut rx) = push_server(201).await;
        let (ua, p256dh, auth) = browser_keys();
        let sub = t
            .push
            .upsert(
                "owner",
                NewSubscription {
                    endpoint: endpoint.clone(),
                    p256dh: p256dh.clone(),
                    auth: b64(&auth),
                    label: Some("laptop".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(sub.events, outbox::TYPES);
        let acct: i64 = sqlx::query_scalar(
            "INSERT INTO apps (name, kind, app_pubkey, isolated, created_at, updated_at) \
             VALUES ('kid', 'account', 'pk-kid', TRUE, 0, 0) RETURNING id",
        )
        .fetch_one(&t.pool)
        .await
        .unwrap();
        let tx = t
            .wallet
            .create_invoice(InvoiceRequest {
                app_id: Some(acct),
                amount_msat: Some(21_000),
                description: "pocket money".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 21_000);
        let mut delivered = 0;
        for _ in 0..100 {
            delivered += t.push.run_due(10).await.unwrap();
            if delivered > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(delivered, 1);
        let (headers, body) = rx.recv().await.unwrap();
        assert_eq!(headers["content-encoding"], "aes128gcm");
        assert_eq!(headers["ttl"], "86400");
        assert_eq!(headers["urgency"], "high");
        let authz = headers["authorization"].to_str().unwrap();
        assert!(authz.starts_with("vapid t="));
        assert!(authz.ends_with(&format!(", k={}", t.push.public_key())));
        let payload: serde_json::Value =
            serde_json::from_slice(&webpush::decrypt(&ua, &auth, &body).unwrap()).unwrap();
        assert_eq!(payload["type"], "payment_received");
        assert_eq!(payload["amount_msat"], 21_000);
        assert_eq!(payload["account"], "kid");
        assert_eq!(payload["description"], "pocket money");
        assert_eq!(payload["transaction_id"], tx.public_id);
        let sub = t.push.get(sub.id).await.unwrap();
        assert!(sub.last_success_at.is_some());
        let state: String =
            sqlx::query_scalar("SELECT state FROM push_outbox").fetch_one(&t.pool).await.unwrap();
        assert_eq!(state, "sent");

        // The same browser registering again keeps its preferences.
        t.push
            .patch(
                sub.id,
                SubscriptionPatch {
                    events: Some(vec![outbox::PAYMENT_SENT.into()]),
                    hide_amounts: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let again = t
            .push
            .upsert(
                "owner",
                NewSubscription { endpoint, p256dh, auth: b64(&auth), ..Default::default() },
            )
            .await
            .unwrap();
        assert_eq!(again.id, sub.id);
        assert_eq!(again.events, [outbox::PAYMENT_SENT]);
        assert!(again.hide_amounts);
        assert_eq!(again.label, "laptop");
    }

    #[tokio::test]
    async fn hidden_amounts_and_unsubscribed_types_and_gone_devices() {
        let t = setup().await;
        let (endpoint, mut rx) = push_server(410).await;
        let (ua, p256dh, auth) = browser_keys();
        let sub = t
            .push
            .upsert(
                "owner",
                NewSubscription {
                    endpoint,
                    p256dh,
                    auth: b64(&auth),
                    hide_amounts: Some(true),
                    events: Some(vec![outbox::PAYMENT_RECEIVED.into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let tx = t
            .wallet
            .create_invoice(InvoiceRequest {
                amount_msat: Some(5_000),
                description: "secret".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 5_000);
        let mut n = 0;
        for _ in 0..100 {
            n += t.push.run_due(10).await.unwrap();
            if n > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (_, body) = rx.recv().await.unwrap();
        let payload: serde_json::Value =
            serde_json::from_slice(&webpush::decrypt(&ua, &auth, &body).unwrap()).unwrap();
        assert_eq!(payload["amount_msat"], serde_json::Value::Null);
        assert_eq!(payload["description"], serde_json::Value::Null);
        assert_eq!(payload["account"], serde_json::Value::Null);
        // 410: the browser dropped the subscription, so Allod forgets it and its queue.
        assert!(matches!(t.push.get(sub.id).await, Err(PushError::NotFound)));
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_outbox")
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn validation_and_limits() {
        let t = setup().await;
        let (_, p256dh, auth) = browser_keys();
        let new = |endpoint: &str| NewSubscription {
            endpoint: endpoint.into(),
            p256dh: p256dh.clone(),
            auth: b64(&auth),
            ..Default::default()
        };
        let strict = PushService::new(t.pool.clone(), &[9; 32], t.wallet.clone(), None);
        assert!(matches!(
            strict.upsert("owner", new("http://127.0.0.1/push/x")).await,
            Err(PushError::BadRequest(_))
        ));
        let mut bad = new("https://push.example/x");
        bad.events = Some(vec!["everything".into()]);
        assert!(matches!(t.push.upsert("owner", bad).await, Err(PushError::BadRequest(_))));
        let mut bad = new("https://push.example/x");
        bad.p256dh = "AAAA".into();
        assert!(matches!(t.push.upsert("owner", bad).await, Err(PushError::BadRequest(_))));
        for i in 0..MAX_PER_OWNER {
            t.push.upsert("owner", new(&format!("https://push.example/{i}"))).await.unwrap();
        }
        assert!(matches!(
            t.push.upsert("owner", new("https://push.example/one-more")).await,
            Err(PushError::BadRequest(_))
        ));
        // Re-registering a known endpoint is not a new device.
        t.push.upsert("owner", new("https://push.example/0")).await.unwrap();
        // Removing the owner removes their devices.
        sqlx::query("DELETE FROM owners WHERE pubkey = 'owner'").execute(&t.pool).await.unwrap();
        assert!(t.push.list().await.unwrap().is_empty());
    }

    async fn add_connection(t: &T, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO apps (name, kind, app_pubkey, created_at, updated_at) \
             VALUES ($1, 'connection', $2, 0, 0) RETURNING id",
        )
        .bind(name)
        .bind(format!("pk-{name}"))
        .fetch_one(&t.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn connection_endpoints_scope_types_quota_and_conflicts() {
        let t = setup().await;
        let app = add_connection(&t, "wallet-app").await;
        let other = add_connection(&t, "other-app").await;
        let (_, p256dh, auth) = browser_keys();
        let new = |endpoint: &str, events: Option<Vec<&str>>| NewSubscription {
            endpoint: endpoint.into(),
            p256dh: p256dh.clone(),
            auth: b64(&auth),
            events: events.map(|e| e.into_iter().map(str::to_string).collect()),
            ..Default::default()
        };
        let all = outbox::TYPES;
        // Unknown and unauthorized types are ignored; none given means all.
        let types = t
            .push
            .register_app(
                app,
                new("https://up.example/a", Some(vec!["payment_sent", "bogus"])),
                all,
            )
            .await
            .unwrap();
        assert_eq!(types, ["payment_sent"]);
        let types = t.push.register_app(app, new("https://up.example/a", None), all).await.unwrap();
        assert_eq!(types, all);
        let subs = t.push.list().await.unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].app_id, Some(app));
        assert_eq!(subs[0].app_name.as_deref(), Some("wallet-app"));
        assert_eq!(subs[0].owner_pubkey, None);

        // An endpoint belongs to one owner or one connection.
        assert!(matches!(
            t.push.register_app(other, new("https://up.example/a", None), all).await,
            Err(PushError::BadRequest(_))
        ));
        assert!(matches!(
            t.push.upsert("owner", new("https://up.example/a", None)).await,
            Err(PushError::BadRequest(_))
        ));
        t.push.upsert("owner", new("https://up.example/mine", None)).await.unwrap();
        assert!(matches!(
            t.push.register_app(app, new("https://up.example/mine", None), all).await,
            Err(PushError::BadRequest(_))
        ));

        for i in 1..MAX_PER_APP {
            t.push
                .register_app(app, new(&format!("https://up.example/{i}"), None), all)
                .await
                .unwrap();
        }
        assert!(matches!(
            t.push.register_app(app, new("https://up.example/too-many", None), all).await,
            Err(PushError::QuotaExceeded(_))
        ));
        // Unregistering is idempotent and limited to the caller's endpoints.
        t.push.unregister_app(other, "https://up.example/a").await.unwrap();
        t.push.unregister_app(app, "https://up.example/unknown").await.unwrap();
        t.push.unregister_app(app, "https://up.example/a").await.unwrap();
        assert_eq!(t.push.list().await.unwrap().len(), MAX_PER_APP as usize);
        // Deleting the connection deletes its endpoints.
        sqlx::query("DELETE FROM apps WHERE id = $1").bind(app).execute(&t.pool).await.unwrap();
        let left = t.push.list().await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].owner_pubkey.as_deref(), Some("owner"));
    }

    #[tokio::test]
    async fn connection_pushes_carry_the_nwc_payload() {
        let t = setup().await;
        let app = add_connection(&t, "wallet-app").await;
        let (endpoint, mut rx) = push_server(201).await;
        let (ua, p256dh, auth) = browser_keys();
        t.push
            .register_app(
                app,
                NewSubscription { endpoint, p256dh, auth: b64(&auth), ..Default::default() },
                outbox::TYPES,
            )
            .await
            .unwrap();
        let tx = t
            .wallet
            .create_invoice(InvoiceRequest {
                amount_msat: Some(1_000),
                description: "coffee".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 1_000);
        let id = loop {
            let row: Option<(i64, String)> =
                sqlx::query("SELECT id, state FROM transactions WHERE public_id = $1")
                    .bind(&tx.public_id)
                    .fetch_optional(&t.pool)
                    .await
                    .unwrap()
                    .map(|r| (r.get(0), r.get(1)));
            if let Some((id, _)) = row.filter(|(_, s)| s == "SETTLED") {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        // The wallet queues owner devices only; connection endpoints wait for the NWC
        // worker, which knows the published event.
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_outbox")
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(queued, 0);

        let n = serde_json::json!({"type": "incoming", "payment_hash": "ab", "amount": 1000});
        let payload = nwc_payload("evt1", outbox::PAYMENT_RECEIVED, &n);
        assert_eq!(
            t.push.enqueue_for_app(app, outbox::PAYMENT_RECEIVED, id, &payload).await.unwrap(),
            1
        );
        // A retry of the same notification queues nothing new.
        let again = nwc_payload("evt2", outbox::PAYMENT_RECEIVED, &n);
        assert_eq!(
            t.push.enqueue_for_app(app, outbox::PAYMENT_RECEIVED, id, &again).await.unwrap(),
            0
        );
        assert_eq!(t.push.run_due(10).await.unwrap(), 1);
        let (headers, body) = rx.recv().await.unwrap();
        assert_eq!(headers["urgency"], "high");
        let plain = webpush::decrypt(&ua, &auth, &body).unwrap();
        assert_eq!(String::from_utf8(plain).unwrap(), payload);
    }

    #[tokio::test]
    async fn queue_cap_and_failing_endpoints_are_dropped() {
        let t = setup().await;
        let app = add_connection(&t, "noisy").await;
        let (_, p256dh, auth) = browser_keys();
        t.push
            .register_app(
                app,
                NewSubscription {
                    endpoint: "https://victim.example/x".into(),
                    p256dh,
                    auth: b64(&auth),
                    ..Default::default()
                },
                outbox::TYPES,
            )
            .await
            .unwrap();
        let sub = t.push.list().await.unwrap().remove(0);
        // Fill the queue up to the cap with real transactions.
        let tx_id: i64 = sqlx::query_scalar(
            "INSERT INTO transactions (public_id, direction, payment_type, state, amount_msat, \
             created_at, updated_at) VALUES ('t-cap', 'incoming', 'bolt11', 'SETTLED', 1, 0, 0) \
             RETURNING id",
        )
        .fetch_one(&t.pool)
        .await
        .unwrap();
        for i in 0..MAX_QUEUED {
            sqlx::query(
                "INSERT INTO push_outbox (subscription_id, kind, transaction_id, payload, \
                 next_attempt_at, created_at) VALUES ($1, $2, $3, '{}', $4, 0)",
            )
            .bind(sub.id)
            .bind(format!("k{i}"))
            .bind(tx_id)
            .bind(i64::MAX)
            .execute(&t.pool)
            .await
            .unwrap();
        }
        let n = t.push.enqueue_for_app(app, outbox::PAYMENT_RECEIVED, tx_id, "{}").await.unwrap();
        assert_eq!(n, 0, "a full queue takes nothing more");

        // An endpoint that keeps failing is removed, so Allod stops sending to it.
        for _ in 0..MAX_FAILURES - 1 {
            t.push
                .record(&sub, &Delivery::Retry("push service answered 503".into()))
                .await
                .unwrap();
        }
        assert!(t.push.get(sub.id).await.is_ok());
        t.push.record(&sub, &Delivery::Retry("push service answered 503".into())).await.unwrap();
        assert!(matches!(t.push.get(sub.id).await, Err(PushError::NotFound)));
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_outbox")
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn nwc_payload_is_cut_to_fit_one_message() {
        let small = serde_json::json!({"type": "incoming", "amount": 5, "payment_hash": "ab"});
        let v: serde_json::Value =
            serde_json::from_str(&nwc_payload("e", "payment_received", &small)).unwrap();
        assert_eq!(v["event_id"], "e");
        assert_eq!(v["notification"], small);
        assert!(v.get("truncated").is_none());

        let big = serde_json::json!({
            "type": "incoming", "state": "settled", "payment_hash": "ab", "amount": 5,
            "invoice": "lnbc".repeat(800), "metadata": {"comment": "x".repeat(2000)},
        });
        let p = nwc_payload("e", "payment_received", &big);
        assert!(p.len() <= MAX_PLAINTEXT);
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["truncated"], true);
        assert_eq!(
            v["notification"],
            serde_json::json!({"type": "incoming", "state": "settled", "payment_hash": "ab", "amount": 5})
        );
    }

    #[test]
    fn subject_uses_https_public_url() {
        assert_eq!(vapid_subject(Some("https://allod.example/")), "https://allod.example");
        assert_eq!(vapid_subject(Some("http://allod.example")), DEFAULT_SUBJECT);
        assert_eq!(vapid_subject(None), DEFAULT_SUBJECT);
    }
}
