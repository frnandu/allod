//! API tests for the public name (SPEC §12.1) with an own domain, against the mock node.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::json;
use tower::ServiceExt;

async fn tls_ask(h: &H, domain: &str) -> StatusCode {
    let req =
        Request::get(format!("/api/internal/tls-ask?domain={domain}")).body(Body::empty()).unwrap();
    h.app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn own_domain_lists_bip353_records_for_accounts() {
    let h = harness().await;
    let (s, v) = h.req("GET", "/api/public-name", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mode"], "none");

    // An account made before the name exists gets its offer from the reconcile.
    let (s, early) = h
        .req("POST", "/api/accounts", Some(json!({"name": "Early", "lud16_username": "early"})))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{early}");
    assert!(early["bip353"].is_null(), "no name yet");

    let (s, v) = h
        .req(
            "PUT",
            "/api/public-name",
            Some(json!({"mode": "own", "name": "https://Pay.Example.com/", "exposure": "private"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "pay.example.com");
    assert_eq!(v["lightning_domain"], "pay.example.com");

    let (s, r) = h
        .req("POST", "/api/accounts", Some(json!({"name": "Alice", "lud16_username": "alice"})))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["bip353"]["name"], "alice.user._bitcoin-payment.pay.example.com.");
    let alice = r["account"]["id"].as_i64().unwrap();
    let alice_txt = r["bip353"]["txt"].as_str().unwrap().to_string();

    let (s, v) = h.req("POST", "/api/public-name/refresh", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let records = v["records"].as_array().unwrap();
    assert_eq!(records.len(), 2, "{v}");
    let a = records.iter().find(|r| r["username"] == "alice").unwrap();
    assert_eq!(a["address"], "alice@pay.example.com");
    assert_eq!(a["txt"], alice_txt, "the offer made at creation is reused");
    assert_eq!(a["published"], false, "own domains are not written by Allod");
    let e = records.iter().find(|r| r["username"] == "early").unwrap();
    assert!(e["txt"].as_str().unwrap().starts_with("bitcoin:?lno="));
    let (_, offers) = h.req("GET", &format!("/api/offers?app_id={alice}"), None).await;
    assert_eq!(offers.as_array().unwrap().len(), 1, "no second offer: {offers}");

    // Closing an account removes its record.
    let (s, _) = h.req("DELETE", &format!("/api/accounts/{alice}"), None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = h.req("POST", "/api/public-name/refresh", None).await;
    let names: Vec<_> =
        v["records"].as_array().unwrap().iter().map(|r| r["username"].clone()).collect();
    assert_eq!(names, [json!("early")]);
}

#[tokio::test]
async fn certificates_only_for_the_current_public_name() {
    let h = harness().await;
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::NOT_FOUND);
    h.req("PUT", "/api/public-name", Some(json!({"mode": "own", "name": "pay.example.com"}))).await;
    assert_eq!(
        tls_ask(&h, "pay.example.com").await,
        StatusCode::NOT_FOUND,
        "private: no certificate"
    );

    let (s, v) = h
        .req(
            "PUT",
            "/api/public-name",
            Some(json!({"mode": "own", "name": "pay.example.com", "exposure": "public"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["exposure"], "public");
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::OK);
    assert_eq!(tls_ask(&h, "evil.example.com").await, StatusCode::NOT_FOUND);

    let (s, _) = h.req("PUT", "/api/public-name", Some(json!({"mode": "none"}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bad_input_and_auth() {
    let h = harness().await;
    let (s, _) =
        h.req("PUT", "/api/public-name", Some(json!({"mode": "own", "name": "1.2.3.4"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.req("PUT", "/api/public-name", Some(json!({"mode": "own"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = h.req("PUT", "/api/public-name", Some(json!({"mode": "desec"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "no deSEC account to use again: {v}");
    let (s, v) = h.req("GET", "/api/public-name/desec/available?label=-bad", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["valid"], false);
    let (s, _) = h.req_as("not-a-session", "GET", "/api/public-name", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h.req("POST", "/api/public-name/desec/credentials", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "needs a fresh signature");
}

#[tokio::test]
async fn twelve_cash_is_mainnet_only() {
    let h = harness().await;
    let (s, v) =
        h.req("POST", "/api/public-name/twelve-cash", Some(json!({"target": "main"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "the mock node is not on mainnet: {v}");
    let (s, _) = h.req("POST", "/api/public-name/twelve-cash", Some(json!({"target": "x"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "network is checked first");
    let (s, v) = h.req("GET", "/api/public-name/twelve-cash/offer?target=main", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["offer"].as_str().unwrap().starts_with("lno1"));
    let (s, _) = h.req("DELETE", "/api/public-name/twelve-cash/main", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nothing to forget");
}
