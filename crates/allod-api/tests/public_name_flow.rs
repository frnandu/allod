//! API tests for addresses on third-party services (twelve.cash, clank.money, ZEUS Pay,
//! Alby; SPEC §12.3–§12.6), against the mock node.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn twelve_cash_is_mainnet_only() {
    let h = harness().await;
    let (s, v) =
        h.req("POST", "/api/public-name/twelve-cash", Some(json!({"target": "main"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "the mock node is not on mainnet: {v}");
    let (s, _) = h.req("POST", "/api/public-name/twelve-cash", Some(json!({"target": "x"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "network is checked first");
    let (s, v) = h
        .req("GET", "/api/public-name/twelve-cash/offer?target=main&address=me@twelve.cash", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let offer = v["offer"].as_str().unwrap().to_string();
    assert!(offer.starts_with("lno1"));
    let (_, offers) = h.req("GET", "/api/offers", None).await;
    let o = offers.as_array().unwrap().iter().find(|o| o["offer"] == offer.as_str()).unwrap();
    assert_eq!(o["description"], "₿me@twelve.cash", "the offer is described with the name");
    let (s, _) = h
        .req("GET", "/api/public-name/twelve-cash/offer?target=main&address=me@example.com", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.req("DELETE", "/api/public-name/twelve-cash/main", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nothing to forget");
}

#[tokio::test]
async fn clank_is_mainnet_only() {
    let h = harness().await;
    let (s, v) = h.req("GET", "/api/public-name", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["clank_available"], false);
    assert_eq!(v["clank"], json!([]));
    let (s, v) = h
        .req(
            "POST",
            "/api/public-name/clank",
            Some(json!({"target": "main", "username": "mynode", "max_price_sats": 1000})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "the mock node is not on mainnet: {v}");
    let (s, v) = h.req("GET", "/api/public-name/clank/available?username=-x", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["valid"], false);
    let (s, _) = h.req("DELETE", "/api/public-name/clank/main", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nothing to forget");
    let (s, v) = h.req("POST", "/api/public-name/names/check", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn zeus_pay_is_mainnet_only() {
    let h = harness().await;
    let (_, v) = h.req("GET", "/api/public-name", None).await;
    assert_eq!(v["zeus_available"], false);
    assert!(v["zeus"].is_null());
    let (s, v) = h.req("POST", "/api/public-name/zeus", Some(json!({"handle": "mynode"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "the mock node is not on mainnet: {v}");
    let (s, _) = h.req("DELETE", "/api/public-name/zeus", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nothing to remove");
    let (s, _) = h
        .req("PUT", "/api/public-name/zeus", Some(json!({"handle": "x", "domain": "zeuspay.com"})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nothing to rename");
}

#[tokio::test]
async fn questions_and_alby() {
    let h = harness().await;
    let (s, v) = h
        .req("PUT", "/api/public-name/prefs", Some(json!({"wallets": "any", "site": "home"})))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["prefs"], json!({"wallets": "any", "domain": null, "site": "home"}));
    let (s, _) = h.req("PUT", "/api/public-name/prefs", Some(json!({"domain": "maybe"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = h.req("DELETE", "/api/public-name/prefs", None).await;
    assert_eq!(v["prefs"]["wallets"], json!(null));

    let (_, v) = h.req("GET", "/api/public-name", None).await;
    assert_eq!(v["alby_available"], false);
    let (s, _) = h.req("POST", "/api/public-name/alby/connection", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "mainnet only");
    let (s, _) = h.req("DELETE", "/api/public-name/alby", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "nothing to remove");
}

#[tokio::test]
async fn needs_a_session() {
    let h = harness().await;
    let (s, _) = h.req_as("not-a-session", "GET", "/api/public-name", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
