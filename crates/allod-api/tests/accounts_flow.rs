//! API tests for accounts (SPEC §10, M6) against the mock backend.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use base64::Engine;
use common::*;
use nostr::prelude::*;
use serde_json::{json, Value};
use tower::ServiceExt;

const HOST: &str = "allod.test";

fn nip98(keys: &Keys, path: &str, method: &str) -> String {
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags([
            Tag::parse(["u", &format!("http://{HOST}{path}")]).unwrap(),
            Tag::parse(["method", method]).unwrap(),
        ])
        .custom_created_at(Timestamp::from(now() as u64))
        .finalize(keys)
        .unwrap();
    format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(event.as_json()))
}

async fn delete_signed(h: &H, keys: &Keys, path: &str) -> (StatusCode, Value) {
    let req = Request::delete(path)
        .header(header::HOST, HOST)
        .header("x-allod-session", &h.token)
        .header(header::AUTHORIZATION, nip98(keys, path, "DELETE"))
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn create(h: &H, body: Value) -> (StatusCode, Value) {
    h.req("POST", "/api/accounts", Some(body)).await
}

#[tokio::test]
async fn create_fund_and_statement() {
    let h = harness().await;
    let (s, r) = create(&h, json!({"name": "Kid"})).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let acct = &r["account"];
    let id = acct["id"].as_i64().unwrap();
    assert_eq!(acct["kind"], "account");
    assert_eq!(acct["isolated"], true);
    assert_eq!(acct["metadata"]["allod_kind"], "account");
    assert_eq!(acct["max_amount_msat"], 0, "no budget by default");
    assert_eq!(acct["balance_msat"], 0);
    let scopes: Vec<&str> =
        acct["scopes"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    assert_eq!(
        scopes,
        [
            "pay_invoice",
            "get_balance",
            "get_info",
            "make_invoice",
            "lookup_invoice",
            "list_transactions",
            "notifications"
        ]
    );
    assert!(r["pairing_uri"].as_str().unwrap().starts_with("nostr+walletconnect://"));
    assert!(r["bip353"].is_null(), "no public URL, so no BIP-353 record");

    // Accounts are not connections.
    let (_, conns) = h.req("GET", "/api/apps", None).await;
    assert!(conns.as_array().unwrap().is_empty());
    let (s, _) = h.req("DELETE", &format!("/api/apps/{id}"), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) =
        h.req("PATCH", &format!("/api/apps/{id}"), Some(json!({"scopes": ["get_balance"]}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "account scopes are fixed");
    let (s, renamed) =
        h.req("PATCH", &format!("/api/apps/{id}"), Some(json!({"name": "Kiddo"}))).await;
    assert_eq!(s, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["name"], "Kiddo");

    // Top up and withdraw are internal transfers.
    let (s, t) = h
        .req("POST", &format!("/api/accounts/{id}/topup"), Some(json!({"amount_msat": 300_000})))
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["payment_type"], "internal");
    let (s, _) = h
        .req("POST", &format!("/api/accounts/{id}/withdraw"), Some(json!({"amount_msat": 100_000})))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, e) = h
        .req("POST", &format!("/api/accounts/{id}/withdraw"), Some(json!({"amount_msat": 200_001})))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(e["error"]["code"], "INSUFFICIENT_BALANCE");

    let (_, a) = h.req("GET", &format!("/api/accounts/{id}"), None).await;
    assert_eq!(a["balance_msat"], 200_000);
    let (_, list) = h.req("GET", "/api/accounts", None).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], id);
    let (_, sum) = h.req("GET", "/api/accounts/summary", None).await;
    assert_eq!(sum, json!({"count": 1, "total_balance_msat": 200_000}));
    let (_, b) = h.req("GET", "/api/balances", None).await;
    assert_eq!(b["main_spendable_msat"], 1_000_000_000i64 - 200_000);
    assert_eq!(b["accounts_total_msat"], 200_000);

    // Statement: newest first with the running balance.
    let (s, st) = h.req("GET", &format!("/api/accounts/{id}/statement"), None).await;
    assert_eq!(s, StatusCode::OK, "{st}");
    assert_eq!(st["total"], 2);
    assert_eq!(st["balance_msat"], 200_000);
    let items = st["items"].as_array().unwrap();
    assert_eq!(items[0]["amount_msat"], -100_000);
    assert_eq!(items[0]["balance_msat"], 200_000);
    assert_eq!(items[0]["entry_kind"], "transfer");
    assert_eq!(items[0]["direction"], "outgoing");
    assert_eq!(items[1]["amount_msat"], 300_000);
    assert_eq!(items[1]["balance_msat"], 300_000);
    assert!(items[1]["transaction_id"].is_string());
    let (_, page) =
        h.req("GET", &format!("/api/accounts/{id}/statement?limit=1&offset=1"), None).await;
    assert_eq!(page["items"][0]["amount_msat"], 300_000);
    assert_eq!(page["total"], 2);
    let (_, none) = h.req("GET", &format!("/api/accounts/{id}/statement?until=1"), None).await;
    assert_eq!(none["total"], 0);
    let (s, main) = h.req("GET", "/api/accounts/main/statement", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(main["balance_msat"], 1_000_000_000i64 - 200_000);

    // Account history stays with the account.
    let (_, txs) = h.req("GET", &format!("/api/transactions?account={id}"), None).await;
    assert_eq!(txs["total"], 2);
}

#[tokio::test]
async fn usernames_are_validated() {
    let h = harness().await;
    for bad in ["admin", "Has Space", "", &"x".repeat(65)] {
        let (s, _) = create(&h, json!({"name": "A", "lud16_username": bad})).await;
        // An empty username means none.
        if bad.is_empty() {
            assert_eq!(s, StatusCode::CREATED);
        } else {
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        }
    }
    let (s, r) = create(&h, json!({"name": "A", "lud16_username": "Alice.B"})).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(r["account"]["lud16_username"], "alice.b");
    let (s, e) = create(&h, json!({"name": "B", "lud16_username": "alice.b"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(e["error"]["message"].as_str().unwrap().contains("taken"));
}

#[tokio::test]
async fn connections_and_unknown_ids_are_not_accounts() {
    let h = harness().await;
    let (_, c) = h.req("POST", "/api/apps", Some(json!({"name": "c", "isolated": true}))).await;
    let cid = c["app"]["id"].as_i64().unwrap();
    for path in [
        format!("/api/accounts/{cid}"),
        format!("/api/accounts/{cid}/statement"),
        "/api/accounts/999".into(),
    ] {
        assert_eq!(h.req("GET", &path, None).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    let (s, _) = h
        .req("POST", &format!("/api/accounts/{cid}/topup"), Some(json!({"amount_msat": 1000})))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        h.req("DELETE", &format!("/api/accounts/{cid}"), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn closing_an_account() {
    let owner = Keys::generate();
    let h = harness_for(&owner.public_key().to_hex()).await;

    // An empty account closes without re-authentication.
    let (_, r) = create(&h, json!({"name": "Empty"})).await;
    let empty = r["account"]["id"].as_i64().unwrap();
    let (s, closed) = h.req("DELETE", &format!("/api/accounts/{empty}"), None).await;
    assert_eq!(s, StatusCode::OK, "{closed}");
    assert!(closed["account"]["closed_at"].is_i64());
    assert!(closed["sweep"].is_null());
    let (s, _) = h.req("DELETE", &format!("/api/accounts/{empty}"), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "already closed");
    let (s, _) = h
        .req("POST", &format!("/api/accounts/{empty}/topup"), Some(json!({"amount_msat": 1000})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no top-up of a closed account");
    let (s, _) =
        h.req("PATCH", &format!("/api/apps/{empty}"), Some(json!({"name": "again"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.req("POST", &format!("/api/apps/{empty}/regenerate"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "a closed account gets no new connection");

    // A funded one needs an explicit sweep, and the sweep needs a fresh signature.
    let (_, r) = create(&h, json!({"name": "Funded"})).await;
    let id = r["account"]["id"].as_i64().unwrap();
    h.req("POST", &format!("/api/accounts/{id}/topup"), Some(json!({"amount_msat": 50_000}))).await;
    let (s, e) = h.req("DELETE", &format!("/api/accounts/{id}"), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(e["error"]["message"].as_str().unwrap().contains("holds funds"));
    let path = format!("/api/accounts/{id}?sweep=true");
    let (s, _) = h.req("DELETE", &path, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "sweeping needs re-authentication");
    let (s, _) = delete_signed(&h, &Keys::generate(), &path).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "signed by someone else");
    let (s, closed) = delete_signed(&h, &owner, &path).await;
    assert_eq!(s, StatusCode::OK, "{closed}");
    assert_eq!(closed["sweep"]["amount_msat"], 50_000);
    assert_eq!(closed["account"]["balance_msat"], 0);

    let (_, list) = h.req("GET", "/api/accounts", None).await;
    assert_eq!(list["total"], 0, "closed accounts without funds are not listed");
    let (_, b) = h.req("GET", "/api/balances", None).await;
    assert_eq!(b["main_spendable_msat"], 1_000_000_000i64);
    // History and statement stay with the closed account.
    let (_, st) = h.req("GET", &format!("/api/accounts/{id}/statement"), None).await;
    assert_eq!(st["total"], 2);
    let (_, txs) = h.req("GET", &format!("/api/transactions?account={id}"), None).await;
    assert_eq!(txs["total"], 2);
    let (_, ok) = h.req("GET", "/api/ledger/check", None).await;
    assert_eq!(ok["ok"], true, "{ok}");

    let audit = h.db.audit_entries(Some("app_deleted"), 5).await.unwrap();
    assert_eq!(audit.len(), 2);
}
