//! Against a local LNURL-pay service.

use allod_lnurl::{LnurlClient, LnurlError};
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let cb = format!("{base}/cb");
    let app = Router::new()
        .route(
            "/.well-known/lnurlp/alice",
            get(move || {
                let cb = cb.clone();
                async move {
                    Json(json!({"tag": "payRequest", "callback": cb, "minSendable": 1000, "maxSendable": 1_000_000,
                        "metadata": "[[\"text/plain\",\"Pay Alice\"],[\"text/identifier\",\"alice@example.com\"]]",
                        "commentAllowed": 20, "allowsNostr": true}))
                }
            }),
        )
        .route(
            "/cb",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let c = q.get("comment").cloned().unwrap_or_default();
                Json::<Value>(json!({"pr": format!("lnbcrtmock{}h{}", q["amount"], hex_of(&c))}))
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn hex_of(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn resolve_and_fetch_invoice() {
    let base = serve().await;
    let host = base.trim_start_matches("http://");
    let c = LnurlClient::new(true);
    let p = c.resolve(&format!("alice@{host}")).await.unwrap();
    assert_eq!(p.description.as_deref(), Some("Pay Alice"));
    assert_eq!(p.identifier.as_deref(), Some("alice@example.com"));
    assert_eq!(
        (p.min_sendable_msat, p.max_sendable_msat, p.comment_allowed),
        (1000, 1_000_000, 20)
    );
    assert!(p.allows_nostr);
    let pr = c.invoice(&p, 21_000, Some("hi")).await.unwrap();
    assert_eq!(pr, format!("lnbcrtmock21000h{}", hex_of("hi")));
    assert_eq!(
        c.invoice(&p, 10, None).await,
        Err(LnurlError::Amount { min: 1000, max: 1_000_000 })
    );
    assert_eq!(
        c.invoice(&p, 5000, Some(&"x".repeat(21))).await,
        Err(LnurlError::CommentTooLong(20))
    );
    // https only, unless allowed.
    assert!(LnurlClient::new(false).resolve(&format!("alice@{host}")).await.is_err());
}
