//! NWC-XX push notifications (SPEC §8.4 #18–19, §8.9): a connection registers its own
//! Web Push / UnifiedPush endpoint and gets its notifications pushed, encrypted.

mod common;

use std::time::Duration;

use allod_core::push::webpush;
use axum::http::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use common::*;
use nostr::prelude::*;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const SECS: Duration = Duration::from_secs(5);

/// A push service on localhost that hands over each request body.
async fn push_server() -> (String, mpsc::UnboundedReceiver<(HeaderMap, Vec<u8>)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = axum::Router::new().route(
        "/up/{id}",
        axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let tx = tx.clone();
            async move {
                let _ = tx.send((headers, body.to_vec()));
                axum::http::StatusCode::CREATED
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/up/device"), rx)
}

struct Device {
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Device {
    fn new() -> Self {
        Device { secret: p256::SecretKey::random(&mut rand::rngs::OsRng), auth: rand::random() }
    }
    fn params(&self, endpoint: &str) -> Value {
        json!({
            "endpoint": endpoint,
            "p256dh": B64.encode(self.secret.public_key().to_encoded_point(false).as_bytes()),
            "auth": B64.encode(self.auth),
        })
    }
    fn open(&self, body: &[u8]) -> Value {
        serde_json::from_slice(&webpush::decrypt(&self.secret, &self.auth, body).unwrap()).unwrap()
    }
}

async fn recv(rx: &mut mpsc::UnboundedReceiver<(HeaderMap, Vec<u8>)>) -> Option<Vec<u8>> {
    tokio::time::timeout(SECS, rx.recv()).await.ok().flatten().map(|(_, b)| b)
}

#[tokio::test]
async fn connection_registers_an_endpoint_and_gets_its_notifications_pushed() {
    let h = harness().await;
    let (c, _) = h.connect(&["notifications", "make_invoice"], None, true).await;
    let (other, _) = h.connect(&["notifications", "make_invoice"], None, true).await;

    let info = c.call("get_info", json!({})).await;
    let methods = info["result"]["methods"].as_array().unwrap();
    assert!(methods.contains(&json!("register_push")));
    assert!(methods.contains(&json!("unregister_push")));
    assert_eq!(info["result"]["push_vapid_key"], h.push.public_key());

    let (endpoint, mut rx) = push_server().await;
    let device = Device::new();
    let mut params = device.params(&endpoint);
    params["notification_types"] = json!(["payment_received", "unknown_type"]);
    let r = c.call("register_push", params).await;
    assert_eq!(r["result"], json!({"notification_types": ["payment_received"]}), "{r}");

    // Another connection's payment is not pushed to this device.
    let inv = other.call("make_invoice", json!({"amount": 3_000})).await;
    h.node.pay_invoice(inv["result"]["payment_hash"].as_str().unwrap(), 3_000);
    other.wait_notification("payment_received", SECS).await.expect("other notified");

    // Its own payment is: the NWC-02 payload plus the published event's id.
    let inv = c.call("make_invoice", json!({"amount": 21_000, "description": "tip"})).await;
    let hash = inv["result"]["payment_hash"].as_str().unwrap().to_string();
    h.node.pay_invoice(&hash, 21_000);
    let relay_n = c.wait_notification("payment_received", SECS).await.expect("relay event");
    let pushed = device.open(&recv(&mut rx).await.expect("pushed"));
    assert_eq!(pushed["notification_type"], "payment_received");
    assert_eq!(pushed["notification"], relay_n);
    assert_eq!(pushed["notification"]["payment_hash"], hash);
    assert_eq!(pushed["notification"]["amount"], 21_000);
    assert!(pushed.get("truncated").is_none());
    // Notification events are ephemeral (relays do not store them), so the id only
    // lets the client drop the copy that also arrives by relay.
    assert!(EventId::from_hex(pushed["event_id"].as_str().unwrap()).is_ok());
    assert!(recv(&mut rx).await.is_none(), "exactly one push for that payment");

    // After unregister_push nothing more is pushed.
    let r = c.call("unregister_push", json!({"endpoint": endpoint})).await;
    assert_eq!(r["result"], json!({}));
    let r = c.call("unregister_push", json!({"endpoint": endpoint})).await;
    assert_eq!(r["result"], json!({}), "idempotent");
    let inv = c.call("make_invoice", json!({"amount": 1_000})).await;
    h.node.pay_invoice(inv["result"]["payment_hash"].as_str().unwrap(), 1_000);
    c.wait_notification("payment_received", SECS).await.expect("relay still notified");
    assert!(recv(&mut rx).await.is_none());
}

#[tokio::test]
async fn push_methods_need_the_notifications_scope_and_valid_params() {
    let h = harness().await;
    let (no_notif, _) = h.connect(&["make_invoice"], None, false).await;
    let info = no_notif.call("get_info", json!({})).await;
    assert!(!info["result"]["methods"].as_array().unwrap().contains(&json!("register_push")));
    assert!(info["result"].get("push_vapid_key").is_none());
    let device = Device::new();
    let r = no_notif.call("register_push", device.params("https://up.example/x")).await;
    assert_eq!(r["error"]["code"], "RESTRICTED", "{r}");

    let (c, _) = h.connect(&["notifications"], None, false).await;
    let r = c.call("register_push", json!({"endpoint": "https://up.example/x"})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "{r}");
    let mut bad = device.params("https://up.example/x");
    bad["p256dh"] = json!("AAAA");
    let r = c.call("register_push", bad).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "{r}");
    for i in 0..allod_core::push::MAX_PER_APP {
        let r = c.call("register_push", device.params(&format!("https://up.example/{i}"))).await;
        assert!(r["result"].is_object(), "{r}");
    }
    let r = c.call("register_push", device.params("https://up.example/more")).await;
    assert_eq!(r["error"]["code"], "QUOTA_EXCEEDED", "{r}");
}
