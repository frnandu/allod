//! Transport and request pipeline (SPEC §8.1–8.2).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use allod_core::apps::{extensions, method_scope, NewApp, IMPLEMENTED_METHODS};
use allod_core::push::{nwc_payload, NewSubscription};
use allod_core::wallet::{budget::Renewal, outbox};
use allod_core::{App, AppService, PushError, PushService, Wallet};
use allod_ln::LightningBackend;
use nostr::nips::{nip04, nip44};
use nostr::prelude::*;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::{nwc_error_from_app, Code, NwcError};
use crate::handlers;

const MAX_INGRESS: usize = 64;
const MAX_INGRESS_PER_APP: usize = 8;
const MAX_CHEAP_GLOBAL: usize = 48;
const MAX_CHEAP_PER_APP: usize = 4;
const MAX_CHEAP_QUEUED_PER_APP: usize = 8;
const MAX_EXPENSIVE_GLOBAL: usize = 16;
const MAX_EXPENSIVE_PER_APP: usize = 2;
const MAX_EXPENSIVE_QUEUED_PER_APP: usize = 4;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENT_TAGS: usize = 32;
const INGRESS_BURST: f64 = 120.0;
const INGRESS_WINDOW_SECS: f64 = 10.0;
const RATE_BURST: f64 = 30.0;
const RATE_WINDOW_SECS: f64 = 10.0;
const STALE_SECS: u64 = 6 * 3600;
const FUTURE_SECS: u64 = 600;
/// Wallet pubkeys per REQ. Each pubkey gets its own filter: Alby's relays reject a
/// filter with more than one `#p` value ("filter must have exactly one author, p tag,
/// or e tag"). strfry relays accept at most 200 filters per REQ by default.
const SUBSCRIPTION_CHUNK: usize = 100;
const INFO_REPUBLISH: Duration = Duration::from_secs(24 * 3600);
/// Request log retention: older entries are deleted hourly, except each connection's
/// newest few. 7 days is far beyond the 6-hour replay window that dedupe needs.
const REQUEST_LOG_DAYS: i64 = 7;
const REQUEST_LOG_KEEP_PER_APP: i64 = 50;

/// Methods that move money, create payment targets or delegate access: dropped after 6 h.
fn is_stale_sensitive(method: &str) -> bool {
    is_expensive_method(method)
}

fn is_expensive_method(method: &str) -> bool {
    method.starts_with("pay")
        || method.starts_with("multi_pay")
        || method.starts_with("make_")
        || matches!(
            method,
            "receive" | "settle_hold_invoice" | "cancel_hold_invoice" | "create_connection"
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkClass {
    Cheap,
    Expensive,
}

struct AppClassLimits {
    admitted: Arc<Semaphore>,
    running: Arc<Semaphore>,
}

struct ClassLimits {
    global: Arc<Semaphore>,
    running_per_app: usize,
    admitted_per_app: usize,
    apps: StdMutex<HashMap<i64, Arc<AppClassLimits>>>,
}

impl ClassLimits {
    fn new(global: usize, running_per_app: usize, queued_per_app: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            running_per_app,
            admitted_per_app: running_per_app + queued_per_app,
            apps: StdMutex::new(HashMap::new()),
        }
    }

    fn app(&self, app_id: i64) -> Arc<AppClassLimits> {
        let mut apps = self.apps.lock().unwrap_or_else(|p| p.into_inner());
        apps.entry(app_id)
            .or_insert_with(|| {
                Arc::new(AppClassLimits {
                    admitted: Arc::new(Semaphore::new(self.admitted_per_app)),
                    running: Arc::new(Semaphore::new(self.running_per_app)),
                })
            })
            .clone()
    }

    async fn acquire(&self, app_id: i64) -> Option<WorkPermit> {
        let app = self.app(app_id);
        let admitted = app.admitted.clone().try_acquire_owned().ok()?;
        // App queue comes first: no global execution permit is held while waiting here.
        let running = app.running.clone().acquire_owned().await.ok()?;
        let global = self.global.clone().acquire_owned().await.ok()?;
        Some(WorkPermit { _admitted: admitted, _running: running, _global: global })
    }
}

struct Scheduler {
    cheap: ClassLimits,
    expensive: ClassLimits,
}

impl Scheduler {
    fn new() -> Self {
        Self {
            cheap: ClassLimits::new(MAX_CHEAP_GLOBAL, MAX_CHEAP_PER_APP, MAX_CHEAP_QUEUED_PER_APP),
            expensive: ClassLimits::new(
                MAX_EXPENSIVE_GLOBAL,
                MAX_EXPENSIVE_PER_APP,
                MAX_EXPENSIVE_QUEUED_PER_APP,
            ),
        }
    }

    async fn acquire(&self, app_id: i64, class: WorkClass) -> Option<WorkPermit> {
        match class {
            WorkClass::Cheap => self.cheap.acquire(app_id).await,
            WorkClass::Expensive => self.expensive.acquire(app_id).await,
        }
    }
}

struct WorkPermit {
    _admitted: OwnedSemaphorePermit,
    _running: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

struct IngressPermit {
    _app: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
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
    ingress: Arc<Semaphore>,
    ingress_per_app: StdMutex<HashMap<i64, Arc<Semaphore>>>,
    /// Refreshed before subscriptions so unknown authors never reach the database.
    known_pairs: StdMutex<HashMap<(PublicKey, PublicKey), i64>>,
    scheduler: Scheduler,
    ingress_buckets: StdMutex<HashMap<i64, (f64, Instant)>>,
    buckets: StdMutex<HashMap<i64, (f64, Instant)>>,
    last_seen: AtomicU64,
    connected_relays: tokio::sync::Mutex<HashSet<String>>,
    /// Info event content (methods and tags) last published per app; unchanged ones are
    /// not republished on every connection change, only by the daily refresh.
    published_info: StdMutex<HashMap<i64, String>>,
    subscriptions: tokio::sync::Mutex<Vec<SubscriptionId>>,
    subscription_gen: AtomicU64,
    /// Device notifications (SPEC §8.9), for `register_push` and push delivery.
    push: OnceLock<Arc<PushService>>,
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
            ingress: Arc::new(Semaphore::new(MAX_INGRESS)),
            ingress_per_app: StdMutex::new(HashMap::new()),
            known_pairs: StdMutex::new(HashMap::new()),
            scheduler: Scheduler::new(),
            ingress_buckets: StdMutex::new(HashMap::new()),
            buckets: StdMutex::new(HashMap::new()),
            last_seen: AtomicU64::new(0),
            connected_relays: tokio::sync::Mutex::new(HashSet::new()),
            published_info: StdMutex::new(HashMap::new()),
            subscriptions: tokio::sync::Mutex::new(Vec::new()),
            subscription_gen: AtomicU64::new(0),
            push: OnceLock::new(),
        })
    }

    /// Enables `register_push` / `unregister_push` and push delivery of notifications.
    pub fn set_push(&self, push: Arc<PushService>) {
        let _ = self.push.set(push);
    }

    /// Connects to the relays, publishes info events, subscribes, and handles requests
    /// until the process stops.
    pub fn start(self: &Arc<Self>) {
        let worker = self.clone();
        tokio::spawn(async move { worker.notification_worker().await });
        tokio::spawn(crate::zaps::worker(self.wallet.clone(), self.apps.clone()));
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
                                if let Some(permit) = me.try_admit_ingress(&event) {
                                    let me = me.clone();
                                    tokio::spawn(async move { me.handle_event_admitted(*event, permit).await });
                                }
                            }
                        }
                        // A relay that refuses the subscription never delivers requests:
                        // make that visible instead of silently losing the relay.
                        Some(ClientNotification::Message { relay_url, message }) => {
                            if let RelayMessage::Closed { subscription_id, message } = *message {
                                if subscription_id.as_str().starts_with("allod-nwc-") {
                                    tracing::warn!(relay = %relay_url, reason = %message, "NWC: relay closed the subscription");
                                }
                            }
                        }
                        Some(ClientNotification::Shutdown) | None => break,
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
        let apps = self.apps.list_nwc().await.map_err(|e| e.to_string())?;
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
            let filters: Vec<Filter> = chunk
                .iter()
                .map(|pk| {
                    Filter::new()
                        .kind(Kind::WalletConnectRequest)
                        .pubkey(*pk)
                        .since(Timestamp::from(since))
                })
                .collect();
            let id = SubscriptionId::new(format!("allod-nwc-{gen}-{i}"));
            match self.client.subscribe(filters).with_id(id.clone()).await {
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
        let methods = app.effective_methods(bolt12);
        let types = app.effective_notification_types();
        let notif = !types.is_empty();
        let mut tags = vec![Tag::custom("encryption", ["nip44_v2 nip04"])];
        // NWC-08: an app that brought its own key knows no wallet key. It finds this one
        // by asking a relay for the info event that names it (Alby Hub does the same for
        // `create_connection`, which is how Alby Go serves `nostr+walletauth://` links).
        if let Ok(client) = PublicKey::from_hex(&app.app_pubkey) {
            tags.push(Tag::public_key(client));
        }
        // NWC-08: the client's own value proves the event answers its request, and the
        // `relay` tags say where this wallet listens (the client must use those).
        if let Some(state) = &app.auth_state {
            tags.push(Tag::custom("state", [state.as_str()]));
            for r in self.apps.client_relays(app) {
                tags.push(Tag::custom("relay", [r]));
            }
        }
        if notif {
            tags.push(Tag::custom("notifications", [types.join(" ")]));
        }
        tags.push(Tag::custom("extensions", [extensions(&methods, notif).join(" ")]));
        let content = methods.join(" ");
        let fingerprint = format!(
            "{content}|{}|{:?}|{:?}",
            types.join(" "),
            self.apps.app_relays(app),
            app.auth_state
        );
        let pending = app.auth_relays.clone().unwrap_or_default();
        let published = self.published_info.lock().unwrap_or_else(|p| p.into_inner()).get(&app.id)
            == Some(&fingerprint);
        if published && pending.is_empty() {
            return Ok(());
        }
        let event = EventBuilder::new(Kind::WalletConnectInfo, content)
            .tags(tags)
            .finalize(&keys)
            .map_err(|e| e.to_string())?;
        // The client waits on its own relays: those first.
        if !pending.is_empty() {
            self.publish_to_client_relays(app, &pending, &event).await;
        }
        if !published {
            if !self.publish_with_backoff(&self.apps.app_relays(app), &event).await {
                return Err("no relay accepted the info event".into());
            }
            self.published_info
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(app.id, fingerprint);
        }
        Ok(())
    }

    /// NWC-08: the info event goes once to the relays named in the client's request.
    /// Relays this wallet does not use are joined for the delivery only. A relay that
    /// cannot be reached is tried again on later refreshes, for ten minutes.
    async fn publish_to_client_relays(&self, app: &App, relays: &[String], event: &Event) {
        let mut joined: Vec<&String> = Vec::new();
        {
            let connected = self.connected_relays.lock().await;
            for r in relays.iter().filter(|r| !connected.contains(*r)) {
                // Write only: this wallet's subscriptions stay on its own relays.
                if self
                    .client
                    .add_relay(r.as_str())
                    .capabilities(RelayCapabilities::WRITE)
                    .await
                    .is_ok()
                {
                    let _ = self.client.connect_relay(r.as_str()).await;
                    joined.push(r);
                }
            }
        }
        if !joined.is_empty() {
            let _ = self.client.try_connect().timeout(Duration::from_secs(5)).await;
        }
        let mut delivered = false;
        for attempt in 0..3u32 {
            match self.client.send_event(event).to(relays.to_vec()).await {
                Ok(out) if !out.success.is_empty() => {
                    delivered = true;
                    break;
                }
                Ok(out) => tracing::debug!(failed = ?out.failed, "NWC: client relays refused"),
                Err(e) => tracing::debug!(error = %e, "NWC: client relays unreachable"),
            }
            tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
        }
        for r in joined {
            let _ = self.client.remove_relay(r.as_str()).force().await;
        }
        let expired = app.created_at + 600 < now_secs() as i64;
        if delivered || expired {
            if !delivered {
                tracing::warn!(app = app.id, "NWC: the client's relays never took the info event");
            }
            if let Err(e) = self.apps.clear_auth_relays(app.id).await {
                tracing::warn!(app = app.id, error = %e, "NWC: cannot clear client relays");
            }
        }
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

    fn accepts_shape(&self, event: &Event) -> Option<i64> {
        if event.kind != Kind::WalletConnectRequest || event.tags.len() > MAX_EVENT_TAGS {
            return None;
        }
        let size = event
            .tags
            .iter()
            .flat_map(|tag| tag.as_slice())
            .fold(event.content.len(), |size, part| size.saturating_add(part.len()));
        if size > MAX_EVENT_BYTES {
            return None;
        }
        let wallet = event.tags.public_keys().next()?;
        self.known_pairs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(event.pubkey, wallet))
            .copied()
    }

    fn try_admit_ingress(&self, event: &Event) -> Option<IngressPermit> {
        let app_id = self.accepts_shape(event)?;
        let app = {
            let mut apps = self.ingress_per_app.lock().unwrap_or_else(|p| p.into_inner());
            apps.entry(app_id)
                .or_insert_with(|| Arc::new(Semaphore::new(MAX_INGRESS_PER_APP)))
                .clone()
        };
        let app = app.try_acquire_owned().ok()?;
        let global = self.ingress.clone().try_acquire_owned().ok()?;
        Some(IngressPermit { _app: app, _global: global })
    }

    /// One request through the pipeline (SPEC §8.2).
    pub async fn handle_event(self: Arc<Self>, event: Event) {
        let Some(permit) = self.try_admit_ingress(&event) else { return };
        self.handle_event_admitted(event, permit).await;
    }

    async fn handle_event_admitted(self: Arc<Self>, event: Event, ingress_permit: IngressPermit) {
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
                    drop(ingress_permit);
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
            drop(ingress_permit);
            let err = NwcError::new(Code::BadRequest, "cannot decrypt the request");
            self.reply(&app, &keys, &event, req_id, Encryption::Nip44, "", Err(err)).await;
            return;
        };
        drop(ingress_permit);

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
        // The tag stays optional, also for `create_connection`: NIP-47 clients (Alby Go,
        // the Alby SDK) do not send one. Replay is closed by step 6 together with the
        // request log, which keeps every event id far longer than `STALE_SECS`.
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
            if !app.has_scope(scope) || !app.permits_method(&method) {
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
        let class =
            if is_expensive_method(&method) { WorkClass::Expensive } else { WorkClass::Cheap };
        let Some(work_permit) = self.scheduler.acquire(app.id, class).await else {
            let err = NwcError::new(Code::RateLimited, "this connection's request queue is full");
            self.reply(&app, &keys, &event, req_id, enc, &method, Err(err)).await;
            return;
        };
        let _ = self.apps.touch(app.id, now as i64).await;
        let result = {
            let (me, app, method, params) =
                (self.clone(), app.clone(), method.clone(), params.clone());
            match tokio::spawn(async move { me.dispatch(&app, &method, &params).await }).await {
                Ok(r) => r,
                Err(_) => Reply::One(Err(NwcError::new(Code::Internal, "internal error"))),
            }
        };
        drop(work_permit);
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
            "get_info" => {
                let key = self.push.get().map(|p| p.public_key());
                handlers::get_info(app, node, node.capabilities().bolt12, key).await
            }
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
            "register_push" => self.register_push(app, params).await,
            "unregister_push" => self.unregister_push(app, params).await,
            other => Err(NwcError::new(Code::NotImplemented, format!("{other} is not supported"))),
        };
        Reply::One(r)
    }

    fn push_service(&self) -> Result<&Arc<PushService>, NwcError> {
        self.push.get().ok_or_else(|| {
            NwcError::new(Code::NotImplemented, "push notifications are not available")
        })
    }

    /// `register_push` (NWC-XX, SPEC §8.4 #18): a push endpoint for this connection's
    /// notifications.
    async fn register_push(&self, app: &App, params: &Value) -> handlers::HandlerResult {
        let push = self.push_service()?;
        let field = |k: &str| {
            params
                .get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| NwcError::new(Code::BadRequest, format!("{k} is required")))
        };
        let types = match params.get("notification_types") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => Some(
                a.iter()
                    .map(|t| {
                        t.as_str().map(str::to_string).ok_or_else(|| {
                            NwcError::new(Code::BadRequest, "notification_types must be strings")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Some(_) => {
                return Err(NwcError::new(Code::BadRequest, "notification_types must be an array"))
            }
        };
        let new = NewSubscription {
            endpoint: field("endpoint")?,
            p256dh: field("p256dh")?,
            auth: field("auth")?,
            // Optional, shown to the wallet owner in the connection's device list.
            label: params.get("device_name").and_then(Value::as_str).map(str::to_string),
            events: types,
            ..Default::default()
        };
        let types = push
            .register_app(app.id, new, &app.effective_notification_types())
            .await
            .map_err(push_error)?;
        Ok(json!({"notification_types": types}))
    }

    /// `unregister_push` (NWC-XX, SPEC §8.4 #19).
    async fn unregister_push(&self, app: &App, params: &Value) -> handlers::HandlerResult {
        let push = self.push_service()?;
        let endpoint = params
            .get("endpoint")
            .and_then(Value::as_str)
            .ok_or_else(|| NwcError::new(Code::BadRequest, "endpoint is required"))?;
        push.unregister_app(app.id, endpoint).await.map_err(push_error)?;
        Ok(json!({}))
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
        // A closed account has no client any more; drop its notifications.
        if app.closed_at.is_some() {
            return true;
        }
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
        // The id pushed to the connection's endpoints: the NIP-44 event, else NIP-04.
        let mut event_id: Option<String> = None;
        if let Ok(c) = nip44::encrypt(keys.secret_key(), &client, &text, nip44::Version::V2) {
            if let Ok(ev) = EventBuilder::new(Kind::Custom(23197), c)
                .tags([Tag::public_key(client), Tag::custom("encryption", ["nip44_v2"])])
                .finalize(&keys)
            {
                event_id = Some(ev.id.to_hex());
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
                    event_id.get_or_insert_with(|| ev.id.to_hex());
                    ok |= self
                        .client
                        .send_event(&ev)
                        .to(relays)
                        .await
                        .is_ok_and(|o| !o.success.is_empty());
                }
            }
        }
        // Push endpoints of this connection (NWC-XX, SPEC §8.9). Queued once per
        // transaction: a retry after a failed relay publish queues nothing new.
        if let (Some(push), Some(id)) = (self.push.get(), event_id.as_deref()) {
            let payload = nwc_payload(id, &d.kind, &n);
            if let Err(e) = push.enqueue_for_app(app.id, &d.kind, d.transaction_id, &payload).await
            {
                tracing::warn!(error = %e, app = app.id, "NWC: push queue failed");
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

fn push_error(e: PushError) -> NwcError {
    match e {
        PushError::BadRequest(m) => NwcError::new(Code::BadRequest, m),
        PushError::QuotaExceeded(m) => NwcError::new(Code::QuotaExceeded, m),
        PushError::NotFound => NwcError::new(Code::NotFound, "push endpoint not found"),
        PushError::Db(e) => {
            tracing::error!(error = %e, "push error");
            NwcError::new(Code::Internal, "internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_queue_wait_does_not_hold_global_execution_slot() {
        let limits = Arc::new(ClassLimits::new(1, 1, 2));
        let app_one = limits.app(1);
        let held_running = app_one.running.clone().acquire_owned().await.unwrap();
        let queued_limits = limits.clone();
        let queued = tokio::spawn(async move { queued_limits.acquire(1).await });
        for _ in 0..100 {
            if app_one.admitted.available_permits() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(app_one.admitted.available_permits(), 2, "request entered app queue");
        assert_eq!(limits.global.available_permits(), 1, "queued app owns no global slot");

        let other = tokio::time::timeout(Duration::from_millis(100), limits.acquire(2))
            .await
            .expect("another app is scheduled")
            .expect("another app has queue capacity");
        drop(other);
        drop(held_running);
        assert!(queued.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn app_queues_are_bounded_and_cheap_work_has_separate_capacity() {
        let limits = ClassLimits::new(1, 1, 2);
        let app = limits.app(1);
        let full = app.admitted.clone().acquire_many_owned(3).await.unwrap();
        assert!(limits.acquire(1).await.is_none());
        drop(full);

        let scheduler = Scheduler::new();
        let mut expensive = Vec::new();
        for app_id in 0..(MAX_EXPENSIVE_GLOBAL / MAX_EXPENSIVE_PER_APP) as i64 {
            for _ in 0..MAX_EXPENSIVE_PER_APP {
                expensive
                    .push(scheduler.acquire(app_id, WorkClass::Expensive).await.expect("capacity"));
            }
        }
        assert_eq!(scheduler.expensive.global.available_permits(), 0);
        let cheap = tokio::time::timeout(
            Duration::from_millis(100),
            scheduler.acquire(999, WorkClass::Cheap),
        )
        .await
        .expect("cheap work is isolated")
        .expect("cheap queue has capacity");
        drop(cheap);
        drop(expensive);
    }
}
