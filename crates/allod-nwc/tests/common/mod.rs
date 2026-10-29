//! Shared harness: a real Nostr client talks to the NWC service through an in-process
//! relay; the Lightning node is the mock backend.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use allod_core::apps::NewApp;
use allod_core::{AppService, Wallet, WalletConfig};
use allod_keys::{NostrKeys, RecoveryPhrase};
use allod_ln::mock::MockBackend;
use allod_ln::LightningBackend;
use allod_nwc::NwcService;
use nostr::nips::{nip04, nip44};
use nostr::prelude::*;
use nostr_relay_builder::builder::RateLimit;
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostr_sdk::{Client, RelayPoolNotification};
use serde_json::{json, Value};

const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

pub struct H {
    pub relay_url: String,
    pub apps: Arc<AppService>,
    pub node: Arc<MockBackend>,
    pub wallet: Arc<Wallet>,
    pub pool: sqlx::AnyPool,
    pub nwc: Arc<NwcService>,
    _relay: LocalRelay,
    _dir: tempfile::TempDir,
}

pub async fn harness() -> H {
    // Public relays allow ~60 events a minute; the tests send far more than one wallet
    // normally would, so the test relay allows more.
    let relay = LocalRelay::new(
        RelayBuilder::default().rate_limit(RateLimit { max_reqs: 500, notes_per_minute: 10_000 }),
    );
    relay.run().await.unwrap();
    let relay_url = relay.url().await.to_string();
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/n.db", dir.path().display());
    allod_db::Db::connect(&url).await.unwrap();
    let pool = allod_db::Db::any_pool(&url).await.unwrap();
    let node = Arc::new(MockBackend::new());
    let backend = node.clone() as Arc<dyn LightningBackend>;
    let wallet = Wallet::new(
        pool.clone(),
        backend.clone(),
        WalletConfig {
            settle_safety_blocks: 3,
            sync_timeout: Duration::from_millis(300),
            reconcile_interval: Duration::from_secs(3600),
            poll_interval: Duration::from_millis(20),
        },
    );
    wallet.reconcile().await.unwrap();
    wallet.start();
    let keys = Arc::new(NostrKeys::from_phrase(&RecoveryPhrase::parse(PHRASE).unwrap()).unwrap());
    let pool_copy = pool.clone();
    let apps = AppService::new(pool, keys, vec![relay_url.clone()], vec![]);
    let nwc = NwcService::new(apps.clone(), wallet.clone(), backend);
    nwc.start();
    H { relay_url, apps, node, wallet, pool: pool_copy, nwc: nwc.clone(), _relay: relay, _dir: dir }
}

/// A NWC client for one connection.
pub struct C {
    pub client: Client,
    pub keys: Keys,
    pub wallet: PublicKey,
    pub inbox: std::sync::Arc<std::sync::Mutex<Vec<(u16, Value)>>>,
    pub all_kinds: std::sync::Arc<std::sync::Mutex<Vec<u16>>>,
}

impl C {
    pub fn new(client: Client, keys: Keys, wallet: PublicKey) -> C {
        let c =
            C { client, keys, wallet, inbox: Default::default(), all_kinds: Default::default() };
        let (mut n, inbox, kinds, keys, wallet) = (
            c.client.notifications(),
            c.inbox.clone(),
            c.all_kinds.clone(),
            c.keys.clone(),
            c.wallet,
        );
        tokio::spawn(async move {
            while let Ok(msg) = n.recv().await {
                if let RelayPoolNotification::Event { event, .. } = msg {
                    let plain = match event.kind.as_u16() {
                        23197 => nip44::decrypt(keys.secret_key(), &wallet, &event.content).ok(),
                        23196 => nip04::decrypt(keys.secret_key(), &wallet, &event.content).ok(),
                        _ => None,
                    };
                    if let Some(p) = plain {
                        kinds.lock().unwrap().push(event.kind.as_u16());
                        if let Ok(v) = serde_json::from_str(&p) {
                            inbox.lock().unwrap().push((event.kind.as_u16(), v));
                        }
                    }
                }
            }
        });
        c
    }
}

impl H {
    pub async fn connect(&self, scopes: &[&str], budget: Option<i64>, isolated: bool) -> (C, i64) {
        let (app, uri) = self
            .apps
            .create(
                NewApp {
                    name: "test".into(),
                    scopes: scopes.iter().map(|s| s.to_string()).collect(),
                    max_amount_msat: budget,
                    budget_renewal: Some("monthly".into()),
                    isolated,
                    ..Default::default()
                },
                None,
                Timestamp::now().as_secs() as i64,
            )
            .await
            .unwrap();
        let uri = uri.unwrap();
        let secret = uri.split("secret=").nth(1).unwrap();
        let keys = Keys::parse(secret).unwrap();
        assert_eq!(keys.public_key().to_hex(), app.app_pubkey);
        let client = Client::builder().build();
        client.add_relay(self.relay_url.as_str()).await.unwrap();
        client.connect().await;
        let wallet = PublicKey::from_hex(app.wallet_pubkey.as_deref().unwrap()).unwrap();
        client
            .subscribe(
                Filter::new()
                    .kinds([
                        Kind::WalletConnectResponse,
                        Kind::Custom(23197),
                        Kind::WalletConnectNotification,
                    ])
                    .pubkey(keys.public_key()),
                None,
            )
            .await
            .unwrap();
        // Let the service resubscribe after the change (1 s debounce).
        tokio::time::sleep(Duration::from_millis(1600)).await;
        (C::new(client, keys, wallet), app.id)
    }
}

impl C {
    pub async fn send_raw(&self, body: Value, nip04_mode: bool, extra: Vec<Tag>) -> Event {
        let text = body.to_string();
        let mut tags = vec![Tag::public_key(self.wallet)];
        let content = if nip04_mode {
            nip04::encrypt(self.keys.secret_key(), &self.wallet, &text).unwrap()
        } else {
            tags.push(Tag::custom(TagKind::custom("encryption"), ["nip44_v2"]));
            nip44::encrypt(self.keys.secret_key(), &self.wallet, &text, nip44::Version::V2).unwrap()
        };
        tags.extend(extra);
        let event = EventBuilder::new(Kind::WalletConnectRequest, content)
            .tags(tags)
            .sign_with_keys(&self.keys)
            .unwrap();
        self.client.send_event(&event).await.unwrap();
        event
    }

    pub async fn wait_response(
        &self,
        request: EventId,
        nip04_mode: bool,
        timeout: Duration,
    ) -> Option<Value> {
        let mut n = self.client.notifications();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(tokio::time::Instant::now())?;
            match tokio::time::timeout(left, n.recv()).await {
                Ok(Ok(RelayPoolNotification::Event { event, .. }))
                    if event.kind == Kind::WalletConnectResponse
                        && event.tags.event_ids().any(|e| *e == request) =>
                {
                    let plain = if nip04_mode {
                        nip04::decrypt(self.keys.secret_key(), &self.wallet, &event.content)
                            .unwrap()
                    } else {
                        nip44::decrypt(self.keys.secret_key(), &self.wallet, &event.content)
                            .unwrap()
                    };
                    return Some(serde_json::from_str(&plain).unwrap());
                }
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    continue
                }
                _ => return None,
            }
        }
    }

    /// Sends a request and waits for the answer; backs off and retries on `RATE_LIMITED`,
    /// like a well-behaved client.
    pub async fn call(&self, method: &str, params: Value) -> Value {
        for _ in 0..20 {
            let e = self.send_raw(json!({"method": method, "params": params}), false, vec![]).await;
            let r = self
                .wait_response(e.id, false, Duration::from_secs(10))
                .await
                .unwrap_or_else(|| panic!("no response to {method}"));
            if r["error"]["code"] != "RATE_LIMITED" {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        panic!("{method}: still rate limited")
    }
}

impl C {
    /// Waits for a new kind 23197 (NIP-44) notification of `kind`. Notifications are
    /// ephemeral events, so they are collected live from connect time.
    pub async fn wait_notification(&self, kind: &str, timeout: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let mut inbox = self.inbox.lock().unwrap();
                if let Some(i) =
                    inbox.iter().position(|(k, v)| *k == 23197 && v["notification_type"] == kind)
                {
                    return Some(inbox.remove(i).1["notification"].clone());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Kinds of every notification received so far (23196 = NIP-04, 23197 = NIP-44).
    pub fn notification_kinds(&self) -> Vec<u16> {
        self.all_kinds.lock().unwrap().clone()
    }
}
