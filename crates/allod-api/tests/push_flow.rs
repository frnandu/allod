//! API tests for device notifications (SPEC §8.9).

mod common;

use common::*;
use serde_json::json;

const P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

fn sub(endpoint: &str) -> serde_json::Value {
    json!({"endpoint": endpoint, "keys": {"p256dh": P256DH, "auth": AUTH}, "label": "Firefox on Linux"})
}

#[tokio::test]
async fn register_update_and_remove_a_device() {
    let h = harness().await;
    let (s, v) = h.req("GET", "/api/push", None).await;
    assert_eq!(s, 200);
    assert_eq!(v["vapid_public_key"].as_str().unwrap().len(), 87);
    assert_eq!(v["events"], json!(["payment_received", "payment_sent", "hold_invoice_accepted"]));
    assert_eq!(v["subscriptions"], json!([]));

    let (s, v) = h.req("POST", "/api/push/subscriptions", Some(sub("http://push.example/a"))).await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = h.req("POST", "/api/push/subscriptions", Some(sub("https://192.168.1.2/a"))).await;
    assert_eq!(s, 400, "{v}");

    let endpoint = "https://updates.push.example/wpush/v2/abc";
    let (s, v) = h.req("POST", "/api/push/subscriptions", Some(sub(endpoint))).await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["endpoint_host"], "updates.push.example");
    assert_eq!(v["label"], "Firefox on Linux");
    assert_eq!(v["hide_amounts"], false);
    assert!(v.get("endpoint").is_none() && v.get("p256dh").is_none() && v.get("auth").is_none());

    // The browser re-registers on every visit: same subscription, preferences kept.
    let (s, v) = h
        .req(
            "PATCH",
            &format!("/api/push/subscriptions/{id}"),
            Some(json!({"events": ["payment_sent"], "hide_amounts": true})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = h
        .req(
            "POST",
            "/api/push/subscriptions",
            Some(json!({"endpoint": endpoint, "keys": {"p256dh": P256DH, "auth": AUTH}})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["id"], id);
    assert_eq!(v["events"], json!(["payment_sent"]));
    assert_eq!(v["hide_amounts"], true);

    let (s, _) = h
        .req(
            "PATCH",
            &format!("/api/push/subscriptions/{id}"),
            Some(json!({"events": ["everything"]})),
        )
        .await;
    assert_eq!(s, 400);

    let (_, v) = h.req("GET", "/api/push", None).await;
    assert_eq!(v["subscriptions"].as_array().unwrap().len(), 1);

    let (s, _) = h.req("DELETE", &format!("/api/push/subscriptions/{id}"), None).await;
    assert_eq!(s, 204);
    let (s, _) = h.req("DELETE", &format!("/api/push/subscriptions/{id}"), None).await;
    assert_eq!(s, 404);
    let (_, v) = h.req("GET", "/api/push", None).await;
    assert_eq!(v["subscriptions"], json!([]));
}

#[tokio::test]
async fn test_push_reports_delivery() {
    let h = harness().await;
    h.push.allow_local_endpoints();
    let app = axum::Router::new()
        .route("/push/{id}", axum::routing::post(|| async { axum::http::StatusCode::CREATED }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (_, v) =
        h.req("POST", "/api/push/subscriptions", Some(sub(&format!("http://{addr}/push/1")))).await;
    let id = v["id"].as_i64().unwrap();
    let (s, v) = h.req("POST", &format!("/api/push/subscriptions/{id}/test"), None).await;
    assert_eq!(s, 200);
    assert_eq!(v, json!({"delivered": true, "status": 201}));
    let (_, v) = h.req("GET", "/api/push", None).await;
    assert!(v["subscriptions"][0]["last_success_at"].is_i64());

    let (s, _) = h.req("POST", "/api/push/subscriptions/999/test", None).await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn api_tokens_cannot_register_devices() {
    let h = harness().await;
    let (s, v) =
        h.req("POST", "/api/tokens", Some(json!({"name": "script", "permission": "full"}))).await;
    assert_eq!(s, 201);
    let token = v["token"].as_str().unwrap().to_string();
    let (s, _) = h
        .req_as(&token, "POST", "/api/push/subscriptions", Some(sub("https://push.example/x")))
        .await;
    assert_eq!(s, 403);
    let (s, _) = h.req_as(&token, "GET", "/api/push", None).await;
    assert_eq!(s, 200);
}
