mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::harness;
use nostr::prelude::*;
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
async fn own_domain_nip05_lookup_and_owner_writes() {
    let h = harness().await;
    let (status, _) = h.req_as("bad", "GET", "/api/nip05", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, view) = h
        .req("POST", "/api/domains", Some(json!({"name": "id.example.org", "direct": true})))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let id = view["domains"][0]["id"].as_i64().unwrap();
    let key = Keys::generate().public_key();
    let npub = key.to_bech32().unwrap();
    let (status, body) = h
        .req(
            "POST",
            "/api/nip05",
            Some(json!({"domain_id": id, "username": "Alice", "npub": npub})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["names"][0]["username"], "alice");
    let name_id = body["names"][0]["id"].as_i64().unwrap();
    let (status, created) = h.req("POST", "/api/accounts", Some(json!({"name": "Kid"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let account_id = created["account"]["id"].as_i64().unwrap();
    let (status, linked) = h
        .req("PATCH", &format!("/api/nip05/{name_id}"), Some(json!({"account_id": account_id})))
        .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["names"][0]["account_id"], account_id);

    let req = Request::builder()
        .uri("/.well-known/nostr.json?name=alice")
        .header(header::HOST, "id.example.org")
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"names": {"alice": key.to_hex()}})
    );

    // No name: refuse instead of listing every mapping on the domain.
    let req = Request::builder()
        .uri("/.well-known/nostr.json")
        .header(header::HOST, "id.example.org")
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.app.clone().oneshot(req).await.unwrap().status(), StatusCode::BAD_REQUEST);

    let req = Request::builder()
        .uri("/.well-known/nostr.json?name=unknown")
        .header(header::HOST, "id.example.org")
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), json!({"names": {}}));

    let req = Request::builder()
        .uri("/.well-known/nostr.json?name=alice")
        .header(header::HOST, "other.example.org")
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.app.clone().oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND);
    let other = Keys::generate().public_key();
    let (status, edited) = h
        .req(
            "PATCH",
            &format!("/api/nip05/{name_id}"),
            Some(json!({"username": "bob", "npub": other.to_bech32().unwrap()})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["names"][0]["username"], "bob");
    assert_eq!(edited["names"][0]["pubkey"], other.to_hex());
    let (status, unlinked) =
        h.req("PATCH", &format!("/api/nip05/{name_id}"), Some(json!({"account_id": null}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(unlinked["names"][0]["account_id"].is_null());
    assert_eq!(
        h.req("DELETE", &format!("/api/nip05/{name_id}"), None).await.0,
        StatusCode::NO_CONTENT
    );
}
