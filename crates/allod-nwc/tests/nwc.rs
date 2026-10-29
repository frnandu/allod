//! NWC handler tests, M4 core methods (SPEC §16.2).

mod common;

use std::time::Duration;

use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use common::*;
use futures_util::StreamExt;
use nostr::prelude::*;
use nostr_sdk::client::ClientNotification;
use serde_json::json;

#[tokio::test]
async fn info_event_and_get_info() {
    let h = harness().await;
    let (c, _) = h.connect(&["get_info", "get_balance", "pay_invoice"], None, false).await;
    // Info event signed by the wallet key, listing the effective methods.
    let events = c
        .client
        .fetch_events(Filter::new().kind(Kind::WalletConnectInfo).author(c.wallet))
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let info = events.first().expect("info event");
    assert_eq!(info.content, "get_info get_budget get_balance pay_invoice multi_pay_invoice pay");
    assert!(info.tags.iter().any(|t| t.as_slice() == ["extensions", "06 321"]));
    assert!(info.tags.iter().any(|t| t.as_slice() == ["encryption", "nip44_v2 nip04"]));

    let r = c.call("get_info", json!({})).await;
    assert_eq!(r["result_type"], "get_info");
    assert_eq!(r["result"]["network"], "regtest");
    assert_eq!(
        r["result"]["methods"],
        json!(["get_info", "get_budget", "get_balance", "pay_invoice", "multi_pay_invoice", "pay"])
    );
    let r = c.call("get_balance", json!({})).await;
    assert_eq!(r["result"]["balance"], 1_000_000_000);
    let r = c.call("get_budget", json!({})).await;
    assert_eq!(r["result"], json!({}));
}

#[tokio::test]
async fn pay_invoice_respects_the_budget() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 1_000 };
    let (c, _) = h.connect(&["pay_invoice"], Some(150_000), false).await;
    let inv = MockBackend::make_invoice(Some(100_000), &sha256_hex(&[7; 32]));
    let r = c.call("pay_invoice", json!({"invoice": inv, "metadata": {"order": 1}})).await;
    assert_eq!(r["result_type"], "pay_invoice");
    assert_eq!(r["result"]["fees_paid"], 1_000);
    assert!(r["result"]["preimage"].as_str().is_some_and(|p| p.len() == 64));
    let b = c.call("get_budget", json!({})).await;
    assert_eq!(b["result"]["used_budget"], 101_000);
    assert_eq!(b["result"]["total_budget"], 150_000);
    assert_eq!(b["result"]["renewal_period"], "monthly");
    assert!(b["result"]["renews_at"].as_i64().is_some());
    // 101k used + 60k + reserve > 150k.
    let inv2 = MockBackend::make_invoice(Some(60_000), &sha256_hex(&[8; 32]));
    let r = c.call("pay_invoice", json!({"invoice": inv2})).await;
    assert_eq!(r["error"]["code"], "QUOTA_EXCEEDED");
    // Paying the same invoice twice is an error, not a second payment.
    let r = c.call("pay_invoice", json!({"invoice": inv})).await;
    assert!(r["error"]["code"].is_string());
}

#[tokio::test]
async fn failed_payment_returns_payment_failed() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Fail;
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    let inv = MockBackend::make_invoice(Some(10_000), &sha256_hex(&[9; 32]));
    let r = c.call("pay_invoice", json!({"invoice": inv})).await;
    assert_eq!(r["error"]["code"], "PAYMENT_FAILED");
}

#[tokio::test]
async fn make_and_lookup_invoice_with_visibility() {
    let h = harness().await;
    let (c, _) = h.connect(&["make_invoice", "lookup_invoice"], None, false).await;
    let r = c
        .call(
            "make_invoice",
            json!({"amount": 21_000, "description": "coffee", "metadata": {"a": "b"}}),
        )
        .await;
    let tx = &r["result"];
    assert_eq!(tx["type"], "incoming");
    assert_eq!(tx["state"], "pending");
    assert_eq!(tx["amount"], 21_000);
    assert_eq!(tx["description"], "coffee");
    assert_eq!(tx["metadata"], json!({"a": "b"}));
    let hash = tx["payment_hash"].as_str().unwrap().to_string();
    let invoice = tx["invoice"].as_str().unwrap().to_string();

    let r = c.call("lookup_invoice", json!({"payment_hash": hash})).await;
    assert_eq!(r["result"]["invoice"], invoice.as_str());
    let r = c.call("lookup_invoice", json!({"invoice": invoice})).await;
    assert_eq!(r["result"]["payment_hash"], hash.as_str());

    // An isolated connection cannot see it.
    let (iso, _) = h.connect(&["lookup_invoice"], None, true).await;
    let r = iso.call("lookup_invoice", json!({"payment_hash": hash})).await;
    assert_eq!(r["error"]["code"], "NOT_FOUND");

    // Paid: the lookup reports settled.
    h.node.pay_invoice(&hash, 21_000);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = c.call("lookup_invoice", json!({"payment_hash": hash})).await;
    assert_eq!(r["result"]["state"], "settled");
    assert!(r["result"]["settled_at"].as_i64().is_some());
}

#[tokio::test]
async fn pipeline_rules() {
    let h = harness().await;
    let (c, id) = h.connect(&["get_balance"], None, false).await;
    // Missing scope → RESTRICTED (and audited).
    let r = c.call("pay_invoice", json!({"invoice": "lnbc1"})).await;
    assert_eq!(r["error"]["code"], "RESTRICTED");
    // Unknown method.
    let r = c.call("make_coffee", json!({})).await;
    assert_eq!(r["error"]["code"], "NOT_IMPLEMENTED");
    // NIP-04 requests get NIP-04 answers.
    let e = c.send_raw(json!({"method": "get_balance", "params": {}}), true, vec![]).await;
    let r = c.wait_response(e.id, true, Duration::from_secs(10)).await.expect("nip04 response");
    assert!(r["result"]["balance"].is_number());
    // Unsupported encryption.
    let ev = EventBuilder::new(Kind::WalletConnectRequest, "x")
        .tags([Tag::public_key(c.wallet), Tag::custom("encryption", ["rot13"])])
        .finalize(&c.keys)
        .unwrap();
    c.client.send_event(&ev).await.unwrap();
    let r = c.wait_response(ev.id, false, Duration::from_secs(10)).await.expect("error response");
    assert_eq!(r["error"]["code"], "UNSUPPORTED_ENCRYPTION");
    // Expired request (NIP-40): no reply.
    let past = Timestamp::from(Timestamp::now().as_secs() - 10);
    let e = c
        .send_raw(
            json!({"method": "get_balance", "params": {}}),
            false,
            vec![Tag::expiration(past)],
        )
        .await;
    assert!(c.wait_response(e.id, false, Duration::from_secs(2)).await.is_none());
    // The same event twice is answered once.
    let e = c.send_raw(json!({"method": "get_balance", "params": {}}), false, vec![]).await;
    assert!(c.wait_response(e.id, false, Duration::from_secs(10)).await.is_some());
    c.client.send_event(&e).await.unwrap();
    assert!(c.wait_response(e.id, false, Duration::from_secs(2)).await.is_none());
    // Request log keeps the method only.
    let log = h.apps.requests(id, 50).await.unwrap();
    assert!(log.iter().any(|l| l.method.as_deref() == Some("pay_invoice") && l.state == "error"));
    assert!(log
        .iter()
        .any(|l| l.method.as_deref() == Some("get_balance") && l.state == "executed"));
}

#[tokio::test]
async fn unknown_and_expired_connections() {
    let h = harness().await;
    let (c, id) = h.connect(&["get_balance"], None, false).await;
    // A stranger writing to this wallet key gets no answer.
    let stranger = C::new(c.client.clone(), Keys::generate(), c.wallet);
    let e = stranger.send_raw(json!({"method": "get_balance", "params": {}}), false, vec![]).await;
    assert!(stranger.wait_response(e.id, false, Duration::from_secs(2)).await.is_none());
    // Expired connection → EXPIRED.
    let now = Timestamp::now().as_secs() as i64;
    h.apps
        .update(
            id,
            allod_core::apps::AppPatch { expires_at: Some(Some(now - 1)), ..Default::default() },
            now,
        )
        .await
        .unwrap();
    // Requests already subscribed still arrive until the service resubscribes.
    let e = c.send_raw(json!({"method": "get_balance", "params": {}}), false, vec![]).await;
    if let Some(r) = c.wait_response(e.id, false, Duration::from_secs(3)).await {
        assert_eq!(r["error"]["code"], "EXPIRED");
    }
}

/// SPEC §16.2: a duplicate event id is handled once, and a NIP-40 expired request is
/// dropped. The events go straight into the pipeline, so a relay's own dedupe or
/// expiration handling cannot hide a bug in Allod.
#[tokio::test]
async fn duplicate_and_expired_requests_bypassing_the_relay() {
    let h = harness().await;
    let (c, id) = h.connect(&["get_balance"], None, false).await;
    let make = |extra: Vec<Tag>| {
        let body = json!({"method": "get_balance", "params": {}}).to_string();
        let content =
            nip44::encrypt(c.keys.secret_key(), &c.wallet, body, nip44::Version::V2).unwrap();
        let mut tags = vec![Tag::public_key(c.wallet), Tag::custom("encryption", ["nip44_v2"])];
        tags.extend(extra);
        EventBuilder::new(Kind::WalletConnectRequest, content).tags(tags).finalize(&c.keys).unwrap()
    };

    // Duplicate: the pipeline sees the same event three times, logs and answers it once.
    let dup = make(vec![]);
    let mut rx = c.client.notifications();
    for _ in 0..3 {
        h.nwc.clone().handle_event(dup.clone()).await;
    }
    let answers = count_responses(&mut rx, dup.id).await;
    assert_eq!(answers, 1, "one response for a duplicated event");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_events WHERE nostr_id = $1")
        .bind(dup.id.to_hex())
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);

    // NIP-40: expired → dropped without a reply; logged as an error. A future expiration
    // is answered normally.
    let expired = make(vec![Tag::expiration(Timestamp::from(Timestamp::now().as_secs() - 5))]);
    let mut rx = c.client.notifications();
    h.nwc.clone().handle_event(expired.clone()).await;
    assert_eq!(count_responses(&mut rx, expired.id).await, 0, "expired request answered");
    let state: String = sqlx::query_scalar("SELECT state FROM request_events WHERE nostr_id = $1")
        .bind(expired.id.to_hex())
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(state, "error");
    let live = make(vec![Tag::expiration(Timestamp::from(Timestamp::now().as_secs() + 600))]);
    let mut rx = c.client.notifications();
    h.nwc.clone().handle_event(live.clone()).await;
    assert_eq!(count_responses(&mut rx, live.id).await, 1);

    // Stale (> 6 h) payment request: dropped; a stale read-only request is still answered.
    let old = Timestamp::from(Timestamp::now().as_secs() - 7 * 3600);
    let body = json!({"method": "pay_invoice", "params": {"invoice": "lnbcrtmock1hx"}}).to_string();
    let content = nip44::encrypt(c.keys.secret_key(), &c.wallet, body, nip44::Version::V2).unwrap();
    let stale_pay = EventBuilder::new(Kind::WalletConnectRequest, content)
        .tags([Tag::public_key(c.wallet), Tag::custom("encryption", ["nip44_v2"])])
        .custom_created_at(old)
        .finalize(&c.keys)
        .unwrap();
    let mut rx = c.client.notifications();
    h.nwc.clone().handle_event(stale_pay.clone()).await;
    assert_eq!(count_responses(&mut rx, stale_pay.id).await, 0, "stale payment answered");
    let body = json!({"method": "get_balance", "params": {}}).to_string();
    let content = nip44::encrypt(c.keys.secret_key(), &c.wallet, body, nip44::Version::V2).unwrap();
    let stale_read = EventBuilder::new(Kind::WalletConnectRequest, content)
        .tags([Tag::public_key(c.wallet), Tag::custom("encryption", ["nip44_v2"])])
        .custom_created_at(old)
        .finalize(&c.keys)
        .unwrap();
    let mut rx = c.client.notifications();
    h.nwc.clone().handle_event(stale_read.clone()).await;
    assert_eq!(count_responses(&mut rx, stale_read.id).await, 1, "stale read-only request");
    let _ = id;
}

/// Responses to `req` seen by the client within 2 s.
async fn count_responses(
    rx: &mut (impl futures_util::Stream<Item = ClientNotification> + Unpin),
    req: EventId,
) -> usize {
    let mut n = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Some(left) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        match tokio::time::timeout(left, rx.next()).await {
            Ok(Some(ClientNotification::Event { event, .. }))
                if event.kind == Kind::WalletConnectResponse
                    && event.tags.event_ids().any(|e| e == req) =>
            {
                n += 1
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    n
}

/// Request log retention: old entries go, each connection keeps its newest 50, and
/// response rows go with their requests.
#[tokio::test]
async fn request_log_is_pruned() {
    let h = harness().await;
    let (_c, a) = h.connect(&["get_balance"], None, false).await;
    let (_d, b) = h.connect(&["get_balance"], None, false).await;
    let day = 86_400;
    let now = Timestamp::now().as_secs() as i64;
    // A: 80 old requests (10 days), B: 30 old and 5 recent.
    for i in 0..80 {
        let id = h
            .apps
            .record_request(&format!("a-old-{i}"), Some(a), now - 10 * day + i)
            .await
            .unwrap()
            .unwrap();
        h.apps.record_response(id, &format!("ra-{i}"), true, now - 10 * day + i).await.unwrap();
    }
    for i in 0..30 {
        h.apps.record_request(&format!("b-old-{i}"), Some(b), now - 9 * day + i).await.unwrap();
    }
    for i in 0..5 {
        h.apps.record_request(&format!("b-new-{i}"), Some(b), now - 60 + i).await.unwrap();
    }
    let deleted = h.apps.prune_requests(now - 7 * day, 50).await.unwrap();
    assert_eq!(deleted, 30, "A keeps its newest 50 of 80; B keeps 35 (under 50)");
    assert_eq!(h.apps.requests(a, 1000).await.unwrap().len(), 50);
    assert_eq!(h.apps.requests(b, 1000).await.unwrap().len(), 35);
    let responses: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM response_events")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(responses, 50, "responses of deleted requests are gone too");
    // With nothing to keep per connection, all old entries go, recent ones stay.
    h.apps.prune_requests(now - 7 * day, 0).await.unwrap();
    assert_eq!(h.apps.requests(a, 1000).await.unwrap().len(), 0);
    assert_eq!(h.apps.requests(b, 1000).await.unwrap().len(), 5);
}
