//! Transport and request pipeline (SPEC §8.1–8.2).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use allod_core::apps::{effective_methods, extensions, method_scope, NewApp, IMPLEMENTED_METHODS};
use allod_core::wallet::{budget::Renewal, outbox};
use allod_core::{App, AppService, Wallet};
use allod_ln::LightningBackend;
use nostr::nips::{nip04, nip44};
use nostr::prelude::*;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::{nwc_error_from_app, Code, NwcError};
use crate::handlers;

const MAX_GLOBAL: usize = 64;
const MAX_PER_APP: usize = 4;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENT_TAGS: usize = 32;
const INGRESS_BURST: f64 = 120.0;
const INGRESS_WINDOW_SECS: f64 = 10.0;
const RATE_BURST: f64 = 30.0;
const RATE_WINDOW_SECS: f64 = 10.0;
const STALE_SECS: u64 = 6 * 3600;
const FUTURE_SECS: u64 = 600;
const SUBSCRIPTION_CHUNK: usize = 500;
const INFO_REPUBLISH: Duration = Duration::from_secs(24 * 3600);
/// Request log retention: older entries are deleted hourly, except each connection's
/// newest few. 7 days is far beyond the 6-hour replay window that dedupe needs.
const REQUEST_LOG_DAYS: i64 = 7;
const REQUEST_LOG_KEEP_PER_APP: i64 = 50;

/// Methods that move money, create payment targets or delegate access: dropped after 6 h.
fn is_stale_sensitive(method: &str) -> bool {
    method.starts_with("pay")
        || method.starts_with("multi_pay")
        || method.starts_with("make_")
        || matches!(
            method,
            "receive" | "settle_hold_invoice" | "cancel_hold_invoice" | "create_connection"
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encryption {
    Nip44,
    Nip04,
}

fn now_secs() -> u64 {
    Timestamp::now().as_secs()
}

/// A handler's answer: one response, or one per item (`multi_pay_invoice`).
enum Reply {
    One(handlers::HandlerResult),
    Many(Result<Vec<(String, handlers::HandlerResult)>, NwcError>),
}

/// The NWC wallet service.
pub struct NwcService {
    apps: Arc<AppService>,
    wallet: Arc<Wallet>,
    node: Arc<dyn LightningBackend>,
    client: Client,
    global: Arc<Semaphore>,
    /// Refreshed before subscriptions so unknown authors never reach the database.
    known_pairs: StdMutex<HashMap<(PublicKey, PublicKey), i64>>,
    per_app: StdMutex<HashMap<i64, Arc<Semaphore>>>,
    ingress_buckets: StdMutex<HashMap<i64, (f64, Instant)>>,
    buckets: StdMutex<HashMap<i64, (f64, Instant)>>,
    last_seen: AtomicU64,
    connected_relays: tokio::sync::Mutex<HashSet<String>>,
    /// Info event content (methods and tags) last published per app; unchanged ones are
    /// not republished on every connection change, only by the daily refresh.
    published_info: StdMutex<HashMap<i64, String>>,
    subscriptions: tokio::sync::Mutex<Vec<SubscriptionId>>,
    subscription_gen: AtomicU64,
}

impl NwcService {
    pub fn new(
        apps: Arc<AppService>,
        wallet: Arc<Wallet>,
        node: Arc<dyn LightningBackend>,
    ) -> Arc<Self> {
        Arc::new(NwcService {
            apps,
            wallet,
            node,
            client: Client::builder().build(),
            global: Arc::new(Semaphore::new(MAX_GLOBAL)),
            known_pairs: StdMutex::new(HashMap::new()),
            per_app: StdMutex::new(HashMap::new()),
            ingress_buckets: StdMutex::new(HashMap::new()),
            buckets: StdMutex::new(HashMap::new()),
            last_seen: AtomicU64::new(0),
            connected_relays: tokio::sync::Mutex::new(HashSet::new()),
            published_info: StdMutex::new(HashMap::new()),
            subscriptions: tokio::sync::Mutex::new(Vec::new()),
            subscription_gen: AtomicU64::new(0),
        })
    }

    /// Connects to the relays, publishes info events, subscribes, and handles requests
    /// until the process stops.
    pub fn start(self: &Arc<Self>) {
        let worker = self.clone();
        tokio::spawn(async move { worker.notification_worker().await });
        let pruner = self.clone();
        tokio::spawn(async move { pruner.prune_request_log_hourly().await });
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.refresh().await {
                tracing::warn!(error = %e, "NWC: initial refresh failed");
            }
            let mut changes = me.apps.watch();
            let mut notifications = me.client.notifications();
            let mut republish = tokio::time::interval(INFO_REPUBLISH);
            republish.tick().await;
            loop {
                tokio::select! {
                    n = notifications.next() => match n {
                        Some(ClientNotification::Event { event, .. }) => {
                            if event.kind == Kind::WalletConnectRequest {
                                if !me.accepts_shape(&event) {
                                    continue;
                                }
                                if let Ok(permit) = me.global.clone().try_acquire_owned() {
                                    let me = me.clone();
                                    tokio::spawn(async move { me.handle_event_admitted(*event, permit).await });
                                }
                            }
                        }
                        Some(ClientNotification::Shutdown) | None => break,
                        Some(_) => {}
                    },
                    r = changes.changed() => {
                        if r.is_err() { break; }
                        // Debounce bursts of changes (SPEC §8.1).
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        changes.borrow_and_update();
                        if let Err(e) = me.refresh().await {
                            tracing::warn!(error = %e, "NWC: refresh failed");
                        }
                    }
                    _ = republish.tick() => {
                        me.published_info.lock().unwrap_or_else(|p| p.into_inner()).clear();
                        if let Err(e) = me.refresh().await {
                            tracing::warn!(error = %e, "NWC: republish failed");
                        }
                    }
                }
            }
            tracing::info!("NWC service stopped");
        });
    }

    /// Adds relays, republishes info events and replaces the subscriptions.
    pub async fn refresh(&self) -> Result<(), String> {
        let apps = self.apps.list().await.map_err(|e| e.to_string())?;
        let now = now_secs() as i64;
        let live: Vec<&App> =
            apps.iter().filter(|a| !a.is_expired(now) && a.wallet_pubkey.is_some()).collect();
        let pairs = live
            .iter()
            .filter_map(|a| {
                Some((
                    (
                        PublicKey::from_hex(&a.app_pubkey).ok()?,
                        PublicKey::from_hex(a.wallet_pubkey.as_deref()?).ok()?,
                    ),
                    a.id,
                ))
            })
            .collect();
        *self.known_pairs.lock().unwrap_or_else(|p| p.into_inner()) = pairs;

        let mut wanted: HashSet<String> = self.apps.relays().iter().cloned().collect();
        for a in &live {
            wanted.extend(self.apps.app_relays(a));
        }
        let mut added = false;
        {
            let mut connected = self.connected_relays.lock().await;
            for r in wanted.difference(&connected.clone()) {
                match self.client.add_relay(r.as_str()).await {
                    Ok(_) => {
                        let _ = self.client.connect_relay(r.as_str()).await;
                        connected.insert(r.clone());
                        added = true;
                    }
                    Err(e) => tracing::warn!(relay = %r, error = %e, "NWC: cannot add relay"),
                }
            }
        }
        // Wait for new relay connections only; known ones reconnect on their own.
        if added {
            let _ = self.client.try_connect().timeout(Duration::from_secs(5)).await;
        }

        // Subscribe before publishing info events: requests are ephemeral events, so a
        // new connection's first request must not arrive before the subscription.
        // Subscriptions: since now − 6 h on first connect, then last seen − 60 s.
        let seen = self.last_seen.load(Ordering::Relaxed);
        let since = if seen == 0 { now_secs() - STALE_SECS } else { seen.saturating_sub(60) };
        let mut keys: Vec<PublicKey> = Vec::new();
        for a in &live {
            if let Ok(pk) = PublicKey::from_hex(a.wallet_pubkey.as_deref().unwrap_or_default()) {
                keys.push(pk);
            }
        }
        // A new generation of subscription IDs each time: nostr-sdk refuses to register an ID
        // twice, so reusing them would keep stale filters (e.g. without newly added wallets).
        // The old subscriptions are dropped only after the new ones are in place.
        let gen = self.subscription_gen.fetch_add(1, Ordering::Relaxed);
        let chunks: Vec<Vec<PublicKey>> =
            keys.chunks(SUBSCRIPTION_CHUNK).map(|c| c.to_vec()).collect();
        let mut fresh: Vec<SubscriptionId> = Vec::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            let filter = Filter::new()
                .kind(Kind::WalletConnectRequest)
                .pubkeys(chunk.clone())
                .since(Timestamp::from(since));
            let id = SubscriptionId::new(format!("allod-nwc-{gen}-{i}"));
            match self.client.subscribe(filter).with_id(id.clone()).await {
                Ok(_) => fresh.push(id),
                Err(e) => tracing::warn!(error = %e, "NWC: subscribe failed"),
            }
        }
        let stale = std::mem::replace(&mut *self.subscriptions.lock().await, fresh);
        for id in stale {
            let _ = self.client.unsubscribe(&id).await;
        }
        let bolt12 = self.node.capabilities().bolt12;
        for a in &live {
            if let Err(e) = self.publish_info(a, bolt12).await {
                tracing::warn!(app = a.id, error = %e, "NWC: info event not published");
            }
        }

        tracing::info!(connections = live.len(), relays = wanted.len(), "NWC: subscribed");
        Ok(())
    }

    fn wallet_keys(&self, app: &App) -> Result<Keys, NwcError> {
        let kp = self.apps.wallet_key(app.id).map_err(nwc_error_from_app)?;
        Keys::parse(kp.expose_secret_hex().as_str())
            .map_err(|e| NwcError::new(Code::Internal, e.to_string()))
    }

    /// Kind 13194 info event, signed by the app's wallet key (SPEC §8.1).
    async fn publish_info(&self, app: &App, bolt12: bool) -> Result<(), String> {
        let keys = self.wallet_keys(app).map_err(|e| e.message)?;
        let methods = effective_methods(&app.scopes, bolt12);
        let notif = app.has_scope("notifications");
        let mut tags = vec![Tag::custom("encryption", ["nip44_v2 nip04"])];
        if notif {
            tags.push(Tag::custom("notifications", [outbox::TYPES.join(" ")]));
        }
        tags.push(Tag::custom("extensions", [extensions(&methods, notif).join(" ")]));
        let content = methods.join(" ");
        let fingerprint = format!("{content}|{notif}|{:?}", self.apps.app_relays(app));
        if self.published_info.lock().unwrap_or_else(|p| p.into_inner()).get(&app.id)
            == Some(&fingerprint)
        {
            return Ok(());
        }
        let event = EventBuilder::new(Kind::WalletConnectInfo, content)
            .tags(tags)
            .finalize(&keys)
            .map_err(|e| e.to_string())?;
        if !self.publish_with_backoff(&self.apps.app_relays(app), &event).await {
            return Err("no relay accepted the info event".into());
        }
        self.published_info.lock().unwrap_or_else(|p| p.into_inner()).insert(app.id, fingerprint);
        Ok(())
    }

    /// Publishes to the relays; retries with exponential backoff (about 30 s in total)
    /// while every relay refuses, e.g. because of relay rate limits.
    async fn publish_with_backoff(&self, relays: &[String], event: &Event) -> bool {
        for attempt in 0..6u32 {
            match self.client.send_event(event).to(relays.to_vec()).await {
                Ok(out) if !out.success.is_empty() => return true,
                Ok(out) => {
                    tracing::debug!(failed = ?out.failed, attempt, "NWC: relays refused an event")
                }
                Err(e) => tracing::debug!(error = %e, attempt, "NWC: publish failed"),
            }
            tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
        }
        false
    }

    fn take_token(&self, app_id: i64) -> bool {
        let mut b = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let (tokens, last) = b.entry(app_id).or_insert((RATE_BURST, now));
        let refill = now.duration_since(*last).as_secs_f64() * RATE_BURST / RATE_WINDOW_SECS;
        *tokens = (*tokens + refill).min(RATE_BURST);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn take_ingress_token(&self, app_id: i64) -> bool {
        let mut buckets = self.ingress_buckets.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let (tokens, last) = buckets.entry(app_id).or_insert((INGRESS_BURST, now));
        let refill = now.duration_since(*last).as_secs_f64() * INGRESS_BURST / INGRESS_WINDOW_SECS;
        *tokens = (*tokens + refill).min(INGRESS_BURST);
        *last = now;
        if *tokens < 1.0 {
            return false;
        }
        *tokens -= 1.0;
        true
    }

    fn accepts_shape(&self, event: &Event) -> bool {
        if event.kind != Kind::WalletConnectRequest || event.tags.len() > MAX_EVENT_TAGS {
            return false;
        }
        let size = event
            .tags
            .iter()
            .flat_map(|tag| tag.as_slice())
            .fold(event.content.len(), |size, part| size.saturating_add(part.len()));
        if size > MAX_EVENT_BYTES {
            return false;
        }
        let Some(wallet) = event.tags.public_keys().next() else { return false };
        self.known_pairs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&(event.pubkey, wallet))
    }

    fn app_semaphore(&self, app_id: i64) -> Arc<Semaphore> {
        let mut m = self.per_app.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(app_id).or_insert_with(|| Arc::new(Semaphore::new(MAX_PER_APP))).clone()
    }

    /// One request through the pipeline (SPEC §8.2).
    pub async fn handle_event(self: Arc<Self>, event: Event) {
        if !self.accepts_shape(&event) {
            return;
        }
        let Ok(permit) = self.global.clone().try_acquire_owned() else { return };
        self.handle_event_admitted(event, permit).await;
    }

    async fn handle_event_admitted(self: Arc<Self>, event: Event, _permit: OwnedSemaphorePermit) {
        let now = now_secs();
        // 1. Signature and timestamp; kind and size were checked at ingress.
        if event.verify().is_err() || event.created_at.as_secs() > now + FUTURE_SECS {
            return;
        }
        let Some(wallet_pubkey) = event.tags.public_keys().next() else { return };
        let p_tag = Some(wallet_pubkey.to_hex());
        let app_id = self
            .known_pairs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(event.pubkey, wallet_pubkey))
            .copied();
        let Some(app_id) = app_id else { return };
        if !self.take_ingress_token(app_id) {
            return;
        }
        self.last_seen.fetch_max(event.created_at.as_secs(), Ordering::Relaxed);

        // 2. App lookup (before dedupe, so no unknown author gets a log row).
        let app = match self.apps.by_app_pubkey(&event.pubkey.to_hex()).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "NWC: app lookup failed");
                return;
            }
        };
        let app = app.filter(|a| a.wallet_pubkey.is_some() && a.wallet_pubkey == p_tag);

        // A connection can be revoked between the ingress snapshot and this lookup.
        let Some(app) = app else { return };

        // 3. Dedupe.
        let req_id =
            match self.apps.record_request(&event.id.to_hex(), Some(app.id), now as i64).await {
                Ok(Some(id)) => id,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "NWC: cannot record request");
                    return;
                }
            };
        let _app_permit = self.app_semaphore(app.id).acquire_owned().await;
        let keys = match self.wallet_keys(&app) {
            Ok(k) => k,
            Err(_) => return,
        };

        // 4. Encryption.
        let enc =
            match event.tags.iter().find(|t| t.kind() == "encryption").and_then(|t| t.content()) {
                None => Encryption::Nip04,
                Some(s) if s.split_whitespace().any(|v| v == "nip44_v2") => Encryption::Nip44,
                Some(s) if s.split_whitespace().any(|v| v == "nip04") => Encryption::Nip04,
                Some(_) => {
                    let err = NwcError::new(Code::UnsupportedEncryption, "use nip44_v2 or nip04");
                    self.reply(&app, &keys, &event, req_id, Encryption::Nip44, "", Err(err)).await;
                    return;
                }
            };
        let plain = match enc {
            Encryption::Nip44 => {
                nip44::decrypt(keys.secret_key(), &event.pubkey, &event.content).ok()
            }
            Encryption::Nip04 => {
                nip04::decrypt(keys.secret_key(), &event.pubkey, &event.content).ok()
            }
        };
        let Some(plain) = plain else {
            let err = NwcError::new(Code::BadRequest, "cannot decrypt the request");
            self.reply(&app, &keys, &event, req_id, Encryption::Nip44, "", Err(err)).await;
            return;
        };

        let _ = self
            .apps
            .set_encryption(app.id, if enc == Encryption::Nip44 { "nip44" } else { "nip04" })
            .await;

        // 5. Parse.
        let body: Value = serde_json::from_str(&plain).unwrap_or(Value::Null);
        let method = body.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = body.get("params").cloned().unwrap_or(json!({}));
        let _ = self.apps.update_request(req_id, Some(&method), "executing", now as i64).await;
        if method.is_empty() {
            let err = NwcError::new(Code::BadRequest, "method is required");
            self.reply(&app, &keys, &event, req_id, enc, "", Err(err)).await;
            return;
        }
        if !IMPLEMENTED_METHODS.contains(&method.as_str()) {
            let err = NwcError::new(Code::NotImplemented, format!("{method} is not supported"));
            self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
            return;
        }
        // 6. Staleness (before expiration validation, so old replay is always dropped).
        if is_stale_sensitive(&method) && event.created_at.as_secs() + STALE_SECS < now {
            let _ = self.apps.update_request(req_id, None, "error", now as i64).await;
            return;
        }
        // 7. NIP-40 expiration: drop.
        if let Some(exp) = event.tags.expiration() {
            if exp.as_secs() <= now {
                let _ = self.apps.update_request(req_id, None, "error", now as i64).await;
                return;
            }
        }
        if method_scope(&method) == Some("superuser") {
            match event.tags.expiration() {
                None => {
                    let err = NwcError::new(
                        Code::BadRequest,
                        "privileged requests require a NIP-40 expiration tag",
                    );
                    self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
                    return;
                }
                Some(exp)
                    if exp.as_secs() > event.created_at.as_secs().saturating_add(600)
                        || exp.as_secs() > now.saturating_add(600) =>
                {
                    let err = NwcError::new(
                        Code::BadRequest,
                        "privileged request expiration must be within 10 minutes",
                    );
                    self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
                    return;
                }
                Some(_) => {}
            }
        }
        // 8. Rate limit before any permission-denial audit write.
        if !self.take_token(app.id) {
            let err = NwcError::new(Code::RateLimited, "too many requests");
            self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
            return;
        }
        // 9. Expiry and scope.
        if app.is_expired(now as i64) {
            let err = NwcError::new(Code::Expired, "this connection has expired");
            self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
            return;
        }
        if let Some(scope) = method_scope(&method) {
            if !app.has_scope(scope) {
                self.apps
                    .audit(
                        "nwc_permission_denied",
                        Some(app.id),
                        json!({"method": method}),
                        now as i64,
                    )
                    .await;
                let err = NwcError::new(Code::Restricted, format!("{method} is not permitted"));
                self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
                return;
            }
        }
        // 10. Dispatch (panics become INTERNAL).
        let _ = self.apps.touch(app.id, now as i64).await;
        let result = {
            let (me, app, method, params) =
                (self.clone(), app.clone(), method.clone(), params.clone());
            match tokio::spawn(async move { me.dispatch(&app, &method, &params).await }).await {
                Ok(r) => r,
                Err(_) => Reply::One(Err(NwcError::new(Code::Internal, "internal error"))),
            }
        };
        // 11. Reply (one event per item for multi methods, tagged `d`).
        match result {
            Reply::One(r) => self.reply(&app, &keys, &event, req_id, enc, &method, r).await,
            Reply::Many(Err(e)) => {
                self.reply(&app, &keys, &event, req_id, enc, &method, Err(e)).await
            }
            Reply::Many(Ok(items)) => {
                for (d, r) in items {
                    self.reply_tagged(&app, &keys, &event, req_id, enc, &method, r, Some(&d)).await;
                }
            }
        }
    }

    async fn dispatch(&self, app: &App, method: &str, params: &Value) -> Reply {
        let w = self.wallet.as_ref();
        let node = self.node.as_ref();
        let r = match method {
            "get_info" => handlers::get_info(app, node, node.capabilities().bolt12).await,
            "get_balance" => handlers::get_balance(app, w).await,
            "get_budget" => handlers::get_budget(app, w).await,
            "pay_invoice" => handlers::pay_invoice(app, w, params).await,
            "multi_pay_invoice" => {
                return Reply::Many(handlers::multi_pay_invoice(app, w, node, params).await)
            }
            "pay" => handlers::pay(app, w, params).await,
            "make_invoice" => handlers::make_invoice(app, w, params).await,
            "make_hold_invoice" => handlers::make_hold_invoice(app, w, params).await,
            "settle_hold_invoice" => handlers::settle_hold_invoice(app, w, params).await,
            "cancel_hold_invoice" => handlers::cancel_hold_invoice(app, w, params).await,
            "receive" => handlers::receive(app, w, params).await,
            "make_offer" => handlers::make_offer(app, w, params).await,
            "lookup_invoice" => handlers::lookup_invoice(app, w, params).await,
            "lookup_payment" => handlers::lookup_payment(app, w, params).await,
            "list_transactions" => handlers::list_transactions(app, w, params).await,
            "sign_message" if app.isolated => Err(NwcError::new(
                Code::Restricted,
                "sign_message is not available to isolated connections",
            )),
            "sign_message" => handlers::sign_message(node, params).await,
            "create_connection" => self.create_connection(app, params).await,
            other => Err(NwcError::new(Code::NotImplemented, format!("{other} is not supported"))),
        };
        Reply::One(r)
    }

    /// `create_connection` (SPEC §8.4 #13): a caller cannot hand out more than it has.
    async fn create_connection(&self, caller: &App, params: &Value) -> handlers::HandlerResult {
        let bad = |m: &str| NwcError::new(Code::BadRequest, m);
        let pubkey = params
            .get("pubkey")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("pubkey is required"))?;
        let name =
            params.get("name").and_then(Value::as_str).ok_or_else(|| bad("name is required"))?;
        let methods = params
            .get("request_methods")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("request_methods is required"))?;
        let mut scopes: Vec<String> = Vec::new();
        for m in methods {
            let m = m.as_str().ok_or_else(|| bad("request_methods must be strings"))?;
            if m == "create_connection" {
                return Err(NwcError::new(Code::Restricted, "create_connection cannot be granted"));
            }
            let scope = match m {
                "get_info" => Some("get_info"),
                "get_budget" => None,
                other => Some(
                    method_scope(other).ok_or_else(|| bad(&format!("unknown method {other}")))?,
                ),
            };
            if let Some(s) = scope {
                if !scopes.iter().any(|x| x == s) {
                    scopes.push(s.into());
                }
            }
        }
        if methods.is_empty() {
            return Err(bad("request_methods must not be empty"));
        }
        let types =
            params.get("notification_types").and_then(Value::as_array).cloned().unwrap_or_default();
        for t in &types {
            if !t.as_str().is_some_and(|t| outbox::TYPES.contains(&t)) {
                return Err(bad(&format!("unsupported notification type {t}")));
            }
        }
        if !types.is_empty() {
            scopes.push("notifications".into());
        }
        let isolated = params.get("isolated").and_then(Value::as_bool).unwrap_or(false);
        if caller.isolated && !isolated {
            return Err(NwcError::new(
                Code::Restricted,
                "an isolated connection can only create isolated connections",
            ));
        }
        let max_amount = params.get("max_amount").and_then(Value::as_i64);
        if max_amount.is_some_and(|m| m < 0) {
            return Err(bad("max_amount must not be negative"));
        }
        let now = now_secs() as i64;
        if let Some(b) =
            self.wallet.budget(caller.id).await.map_err(crate::error::nwc_error_from)?
        {
            let remaining = (b.total_budget - b.used_budget).max(0);
            match max_amount {
                Some(m) if m > 0 && m <= remaining => {}
                _ => {
                    return Err(NwcError::new(
                        Code::QuotaExceeded,
                        format!("max_amount is required and must not exceed the remaining budget ({remaining} msat)"),
                    ))
                }
            }
        }
        let requested_renewal =
            params.get("budget_renewal").and_then(Value::as_str).unwrap_or(&caller.budget_renewal);
        let renewal =
            Renewal::parse(requested_renewal).ok_or_else(|| bad("invalid budget_renewal"))?;
        let caller_renewal = Renewal::parse(&caller.budget_renewal)
            .ok_or_else(|| bad("invalid caller budget_renewal"))?;
        let frequency = |r| match r {
            Renewal::Daily => 0,
            Renewal::Weekly => 1,
            Renewal::Monthly => 2,
            Renewal::Yearly => 3,
            Renewal::Never => 4,
        };
        if caller.max_amount_msat > 0 && frequency(renewal) < frequency(caller_renewal) {
            return Err(NwcError::new(
                Code::Restricted,
                "child budget cannot renew more often than caller budget",
            ));
        }
        let requested_expiry = params.get("expires_at").and_then(Value::as_i64);
        if caller.expires_at.is_some_and(|end| requested_expiry.is_some_and(|e| e > end)) {
            return Err(NwcError::new(Code::Restricted, "child expiry exceeds caller expiry"));
        }
        let expires_at = requested_expiry.or(caller.expires_at);
        let (app, _) = self
            .apps
            .create(
                NewApp {
                    name: name.into(),
                    pubkey: Some(pubkey.into()),
                    scopes,
                    max_amount_msat: max_amount,
                    budget_renewal: Some(renewal.as_str().into()),
                    expires_at,
                    isolated,
                    metadata: params.get("metadata").cloned().filter(|m| !m.is_null()),
                    ..Default::default()
                },
                Some(caller.id),
                now,
            )
            .await
            .map_err(nwc_error_from_app)?;
        // Publish the new info event and subscribe before answering (SPEC §8.4 #13).
        if let Err(e) = self.refresh().await {
            tracing::warn!(error = %e, "NWC: refresh after create_connection failed");
        }
        Ok(json!({"wallet_pubkey": app.wallet_pubkey}))
    }

    /// Deletes old NWC request events every hour (first run a minute after start).
    async fn prune_request_log_hourly(self: Arc<Self>) {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let before = now_secs() as i64 - REQUEST_LOG_DAYS * 86_400;
            match self.apps.prune_requests(before, REQUEST_LOG_KEEP_PER_APP).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(deleted = n, "NWC: pruned old request log entries"),
                Err(e) => tracing::warn!(error = %e, "NWC: request log pruning failed"),
            }
        }
    }

    /// Publishes queued notifications (SPEC §8.5): kind 23197 (NIP-44) always, kind 23196
    /// (NIP-04) for apps that use NIP-04 or never sent a request.
    async fn notification_worker(self: Arc<Self>) {
        let mut events = self.wallet.subscribe();
        loop {
            match self.wallet.outbox_due(50).await {
                Ok(due) => {
                    for d in due {
                        let sent = self.publish_notification(&d).await;
                        if let Err(e) = self.wallet.outbox_done(&d, sent).await {
                            tracing::warn!(error = %e, "NWC: outbox update failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "NWC: outbox read failed"),
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        }
    }

    async fn publish_notification(&self, d: &outbox::Due) -> bool {
        let Ok(app) = self.apps.get(d.app_id).await else { return false };
        let Ok(tx) = self.wallet.transaction_by_id(d.transaction_id).await else { return false };
        let Ok(keys) = self.wallet_keys(&app) else { return false };
        let Ok(client) = PublicKey::from_hex(&app.app_pubkey) else { return false };
        let mut n = handlers::payment_record(&tx);
        if d.kind == outbox::HOLD_ACCEPTED {
            n["settle_deadline"] = json!(tx.settle_deadline);
        }
        let text = json!({"notification_type": d.kind, "notification": n}).to_string();
        let relays = self.apps.app_relays(&app);
        let mut ok = false;
        if let Ok(c) = nip44::encrypt(keys.secret_key(), &client, &text, nip44::Version::V2) {
            if let Ok(ev) = EventBuilder::new(Kind::Custom(23197), c)
                .tags([Tag::public_key(client), Tag::custom("encryption", ["nip44_v2"])])
                .finalize(&keys)
            {
                ok |= self
                    .client
                    .send_event(&ev)
                    .to(relays.clone())
                    .await
                    .is_ok_and(|o| !o.success.is_empty());
            }
        }
        if self.apps.wants_nip04(app.id).await.unwrap_or(true) {
            if let Ok(c) = nip04::encrypt(keys.secret_key(), &client, &text) {
                if let Ok(ev) = EventBuilder::new(Kind::WalletConnectNotification, c)
                    .tags([Tag::public_key(client)])
                    .finalize(&keys)
                {
                    ok |= self
                        .client
                        .send_event(&ev)
                        .to(relays)
                        .await
                        .is_ok_and(|o| !o.success.is_empty());
                }
            }
        }
        ok
    }

    /// Encrypts, signs and publishes a kind 23195 response; retries 3 times.
    #[allow(clippy::too_many_arguments)]
    async fn reply(
        &self,
        app: &App,
        keys: &Keys,
        request: &Event,
        req_id: i64,
        enc: Encryption,
        method: &str,
        result: handlers::HandlerResult,
    ) {
        self.reply_tagged(app, keys, request, req_id, enc, method, result, None).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn reply_tagged(
        &self,
        app: &App,
        keys: &Keys,
        request: &Event,
        req_id: i64,
        enc: Encryption,
        method: &str,
        result: handlers::HandlerResult,
        d_tag: Option<&str>,
    ) {
        let ok = result.is_ok();
        let body = match result {
            Ok(r) => json!({"result_type": method, "result": r}),
            Err(e) => json!({"result_type": method, "error": e.to_json()}),
        };
        let text = body.to_string();
        let content = match enc {
            Encryption::Nip44 => {
                nip44::encrypt(keys.secret_key(), &request.pubkey, &text, nip44::Version::V2)
                    .map_err(|e| e.to_string())
            }
            Encryption::Nip04 => {
                nip04::encrypt(keys.secret_key(), &request.pubkey, &text).map_err(|e| e.to_string())
            }
        };
        let now = now_secs() as i64;
        let Ok(content) = content else {
            let _ = self.apps.update_request(req_id, None, "error", now).await;
            return;
        };
        let mut tags = vec![Tag::public_key(request.pubkey), Tag::event(request.id)];
        if let Some(d) = d_tag {
            tags.push(Tag::identifier(d));
        }
        if enc == Encryption::Nip44 {
            tags.push(Tag::custom("encryption", ["nip44_v2"]));
        }
        let event =
            match EventBuilder::new(Kind::WalletConnectResponse, content).tags(tags).finalize(keys)
            {
                Ok(e) => e,
                Err(_) => return,
            };
        let relays = self.apps.app_relays(app);
        let sent = self.publish_with_backoff(&relays, &event).await;
        let _ = self.apps.record_response(req_id, &event.id.to_hex(), sent, now).await;
        let state = if !sent {
            "error"
        } else if ok {
            "executed"
        } else {
            "error"
        };
        let _ = self.apps.update_request(req_id, None, state, now).await;
        if !sent {
            tracing::warn!(app = app.id, "NWC: response not delivered to any relay");
        }
    }
}
