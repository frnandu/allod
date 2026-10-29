//! NWC extension tests, M5 (SPEC §16.2): notifications, hold invoices, BOLT12,
//! lookup_payment, list_transactions, create_connection, self payments, multi_pay, 321.

mod common;

use std::time::Duration;

use allod_core::wallet::InvoiceRequest;
use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use common::*;
use futures_util::StreamExt;
use nostr::prelude::*;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};

const SECS: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(1500);

fn preimage(seed: u8) -> (String, String) {
    let p = [seed; 32];
    (hex::encode(p), sha256_hex(&p))
}

#[tokio::test]
async fn notifications_follow_visibility() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, c_id) = h.connect(&["notifications", "pay_invoice"], None, false).await;
    let (a, _) = h.connect(&["notifications", "make_invoice"], None, true).await;
    // A listener that never sends a request (legacy-style client).
    let (d, _) = h.connect(&["notifications"], None, false).await;
    let info = c.call("get_info", json!({})).await;
    assert_eq!(
        info["result"]["notifications"],
        json!(["payment_received", "payment_sent", "hold_invoice_accepted"])
    );
    assert_eq!(info["result"]["extensions"], json!(["02", "06", "321"]));

    // Main receives: the non-isolated connection is told, the isolated one is not.
    let main = h
        .wallet
        .create_invoice(InvoiceRequest {
            amount_msat: Some(5_000),
            description: "main".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    h.node.pay_invoice(main.payment_hash.as_deref().unwrap(), 5_000);
    let n = c.wait_notification("payment_received", SECS).await.expect("C notified");
    assert_eq!(n["payment_hash"], main.payment_hash.as_deref().unwrap());
    assert_eq!(n["state"], "settled");
    assert!(
        a.wait_notification("payment_received", QUIET).await.is_none(),
        "isolated app must not see Main"
    );

    // The isolated app's own invoice: only it is told.
    let inv = a.call("make_invoice", json!({"amount": 7_000})).await;
    let hash = inv["result"]["payment_hash"].as_str().unwrap().to_string();
    h.node.pay_invoice(&hash, 7_000);
    let n = a.wait_notification("payment_received", SECS).await.expect("A notified");
    assert_eq!(n["payment_hash"], hash.as_str());
    assert_eq!(n["details"]["payment_hash"], hash.as_str());
    assert!(
        c.wait_notification("payment_received", QUIET).await.is_none(),
        "C must not see A's payment"
    );

    // payment_sent for the payer.
    let ext = MockBackend::make_invoice(Some(1_000), &sha256_hex(&[42; 32]));
    let r = c.call("pay_invoice", json!({"invoice": ext})).await;
    assert!(r["result"]["preimage"].is_string(), "{r}");
    let n = c.wait_notification("payment_sent", SECS).await.expect("payment_sent");
    assert_eq!(n["type"], "outgoing");

    // C sent NIP-44 requests: NIP-44 notifications only. D never sent a request: both.
    assert!(!c.notification_kinds().contains(&23196), "{:?}", c.notification_kinds());
    d.wait_notification("payment_received", SECS).await.expect("D notified");
    assert!(d.notification_kinds().contains(&23196), "{:?}", d.notification_kinds());
    let _ = c_id;
}

#[tokio::test]
async fn hold_invoices_ownership_and_notifications() {
    let h = harness().await;
    let (a, _) = h.connect(&["notifications", "make_invoice", "lookup_invoice"], None, true).await;
    let (b, _) = h.connect(&["make_invoice"], None, true).await;

    let (pre, hash) = preimage(1);
    let r = a
        .call(
            "make_hold_invoice",
            json!({"amount": 10_000, "payment_hash": hash, "description": "hold"}),
        )
        .await;
    assert_eq!(r["result"]["state"], "pending", "{r}");
    let r = a
        .call(
            "make_hold_invoice",
            json!({"amount": 10_000, "payment_hash": preimage(9).1, "min_cltv_expiry_delta": 144}),
        )
        .await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST");

    // Another app cannot touch it.
    let r = b.call("settle_hold_invoice", json!({"preimage": pre})).await;
    assert_eq!(r["error"]["code"], "NOT_FOUND");
    let r = b.call("cancel_hold_invoice", json!({"payment_hash": hash})).await;
    assert_eq!(r["error"]["code"], "NOT_FOUND");

    h.node.hold_htlc(&hash, 10_000, 1_000);
    let n = a.wait_notification("hold_invoice_accepted", SECS).await.expect("accepted");
    assert_eq!(n["state"], "accepted");
    let deadline = n["settle_deadline"].as_i64().unwrap();
    assert!(deadline < 1_000, "settle_deadline below the claim deadline: {deadline}");
    let r = a.call("settle_hold_invoice", json!({"preimage": pre})).await;
    assert_eq!(r["result"], json!({}), "{r}");
    let r = a.call("lookup_invoice", json!({"payment_hash": hash})).await;
    assert_eq!(r["result"]["state"], "settled");
    assert!(h.node.claimed().iter().any(|(id, p)| id == &hash && p == &pre));

    // Cancel path.
    let (_, hash2) = preimage(2);
    a.call("make_hold_invoice", json!({"amount": 3_000, "payment_hash": hash2})).await;
    h.node.hold_htlc(&hash2, 3_000, 1_000);
    a.wait_notification("hold_invoice_accepted", SECS).await.expect("accepted");
    let r = a.call("cancel_hold_invoice", json!({"payment_hash": hash2})).await;
    assert_eq!(r["result"], json!({}));
    assert!(h.node.failed().contains(&hash2));

    // Underpaid: failed back, and no notification.
    let (_, hash3) = preimage(3);
    a.call("make_hold_invoice", json!({"amount": 8_000, "payment_hash": hash3})).await;
    h.node.hold_htlc(&hash3, 5_000, 1_000);
    assert!(a.wait_notification("hold_invoice_accepted", QUIET).await.is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.node.failed().contains(&hash3));
}

#[tokio::test]
async fn offers_credit_and_notify_their_owner() {
    let h = harness().await;
    let (a, a_id) = h
        .connect(&["notifications", "make_invoice", "lookup_invoice", "get_balance"], None, true)
        .await;
    let (c, _) = h.connect(&["notifications"], None, false).await;

    for (params, what) in [
        (json!({"issuer": "example.com"}), "issuer"),
        (json!({"single_use": true}), "single_use"),
        (json!({"amount": 1000}), "description"),
    ] {
        let r = a.call("make_offer", params).await;
        assert_eq!(r["error"]["code"], "BAD_REQUEST", "{what}: {r}");
    }
    let r = a.call("make_offer", json!({"description": "tips", "metadata": {"k": "v"}})).await;
    let offer_id = r["result"]["offer_id"].as_str().unwrap().to_string();
    assert!(r["result"]["offer"].as_str().unwrap().starts_with("lno1"));
    assert_eq!(r["result"]["amount"], Value::Null);
    assert_eq!(r["result"]["single_use"], false);

    let p1 = h.node.pay_offer(&offer_id, 4_000, Some("hi".into()));
    let n = a.wait_notification("payment_received", SECS).await.expect("offer payment notified");
    assert_eq!(n["payment_type"], "bolt12");
    assert_eq!(n["details"]["offer_id"], offer_id.as_str());
    assert_eq!(n["details"]["payer_note"], "hi");
    assert!(
        c.wait_notification("payment_received", QUIET).await.is_none(),
        "only the owner is told"
    );
    let b = a.call("get_balance", json!({})).await;
    assert_eq!(b["result"]["balance"], 4_000);
    let _ = a_id;

    // lookup_payment by offer: one match, then two.
    let r = a
        .call("lookup_payment", json!({"payment_type": "bolt12", "lookup": {"offer_id": offer_id}}))
        .await;
    assert_eq!(r["result"]["amount"], 4_000, "{r}");
    h.node.pay_offer(&offer_id, 6_000, None);
    a.wait_notification("payment_received", SECS).await.expect("second payment");
    let r = a
        .call("lookup_payment", json!({"payment_type": "bolt12", "lookup": {"offer_id": offer_id}}))
        .await;
    assert_eq!(r["error"]["code"], "MULTIPLE_MATCHES");
    let hash1 = p1.kind.payment_hash().unwrap().to_string();
    let r = a
        .call("lookup_payment", json!({"payment_type": "bolt12", "lookup": {"offer_id": offer_id, "payment_hash": hash1}}))
        .await;
    assert_eq!(r["result"]["amount"], 4_000);
    assert_eq!(r["result"]["metadata"], json!({"k": "v"}));
}

#[tokio::test]
async fn lookup_payment_forms() {
    let h = harness().await;
    let (a, _) = h.connect(&["make_invoice", "lookup_invoice"], None, false).await;
    let (iso, _) = h.connect(&["lookup_invoice"], None, true).await;
    let r = a.call("make_invoice", json!({"amount": 2_000, "description": "x"})).await;
    let tid = r["result"]["transaction_id"].as_str().unwrap().to_string();
    let hash = r["result"]["payment_hash"].as_str().unwrap().to_string();
    let inv = r["result"]["invoice"].as_str().unwrap().to_string();

    for params in [
        json!({"transaction_id": tid}),
        json!({"payment_hash": hash}),
        json!({"invoice": inv}),
        json!({"payment_type": "bolt11", "lookup": {"payment_hash": hash}}),
    ] {
        let r = a.call("lookup_payment", params.clone()).await;
        assert_eq!(r["result"]["transaction_id"], tid.as_str(), "{params}: {r}");
        assert_eq!(r["result"]["payment_type"], "bolt11");
        assert_eq!(r["result"]["details"]["payment_hash"], hash.as_str());
        assert!(r["result"]["details"].get("preimage").is_none(), "unpaid: no preimage");
    }
    let r = a.call("lookup_payment", json!({"transaction_id": tid, "payment_hash": hash})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST");
    let r = a
        .call("lookup_payment", json!({"payment_type": "onchain", "lookup": {"txid": "00"}}))
        .await;
    assert_eq!(r["error"]["code"], "UNSUPPORTED_PAYMENT_TYPE");
    let r = iso.call("lookup_payment", json!({"transaction_id": tid})).await;
    assert_eq!(r["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn list_transactions_pages_and_filters() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (a, _) =
        h.connect(&["make_invoice", "pay_invoice", "list_transactions"], None, false).await;
    for i in 0..3u64 {
        a.call("make_invoice", json!({"amount": 1_000 + i})).await;
    }
    let ext = MockBackend::make_invoice(Some(500), &sha256_hex(&[77; 32]));
    a.call("pay_invoice", json!({"invoice": ext})).await;

    let r = a.call("list_transactions", json!({})).await;
    assert_eq!(r["result"]["total_count"], 1, "unpaid invoices are left out: {r}");
    let r = a.call("list_transactions", json!({"unpaid": true})).await;
    assert_eq!(r["result"]["total_count"], 4);
    let r =
        a.call("list_transactions", json!({"unpaid": true, "type": "incoming", "limit": 2})).await;
    assert_eq!(r["result"]["total_count"], 3);
    let items = r["result"]["transactions"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|t| t["type"] == "incoming" && t["details"].is_object()));
    let r = a.call("list_transactions", json!({"limit": 101})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST");
}

#[tokio::test]
async fn create_connection_cannot_escalate() {
    let h = harness().await;
    let (s, s_id) = h.connect(&["superuser", "pay_invoice"], Some(100_000), false).await;
    let pk = || Keys::generate().public_key().to_hex();
    let r = s.call("create_connection", json!({"pubkey": pk(), "name": "x", "request_methods": ["create_connection"], "max_amount": 1000})).await;
    assert_eq!(r["error"]["code"], "RESTRICTED");
    let r = s
        .call(
            "create_connection",
            json!({"pubkey": pk(), "name": "x", "request_methods": ["pay_invoice"]}),
        )
        .await;
    assert_eq!(
        r["error"]["code"], "QUOTA_EXCEEDED",
        "max_amount required when the caller has a budget"
    );
    let r = s.call("create_connection", json!({"pubkey": pk(), "name": "x", "request_methods": ["pay_invoice"], "max_amount": 200_000})).await;
    assert_eq!(r["error"]["code"], "QUOTA_EXCEEDED");
    let new_pk = pk();
    let r = s
        .call(
            "create_connection",
            json!({"pubkey": new_pk, "name": "child", "request_methods": ["pay_invoice", "get_balance"], "max_amount": 50_000, "notification_types": ["payment_sent"]}),
        )
        .await;
    let wallet_pk = r["result"]["wallet_pubkey"].as_str().expect("wallet_pubkey").to_string();
    let child = h.apps.by_app_pubkey(&new_pk).await.unwrap().unwrap();
    assert_eq!(child.created_by_app_id, Some(s_id));
    assert_eq!(child.wallet_pubkey.as_deref(), Some(wallet_pk.as_str()));
    assert_eq!(child.max_amount_msat, 50_000);
    assert!(child.has_scope("notifications") && !child.has_scope("superuser"));

    let (iso, _) = h.connect(&["superuser"], None, true).await;
    let r = iso
        .call(
            "create_connection",
            json!({"pubkey": pk(), "name": "y", "request_methods": ["get_balance"]}),
        )
        .await;
    assert_eq!(r["error"]["code"], "RESTRICTED");
    let r = iso.call("create_connection", json!({"pubkey": pk(), "name": "y", "request_methods": ["get_balance"], "isolated": true})).await;
    assert!(r["result"]["wallet_pubkey"].is_string(), "{r}");
}

#[tokio::test]
async fn self_payments_between_isolated_apps() {
    let h = harness().await;
    // Any Lightning send would fail: self payments must stay internal.
    *h.node.send.lock().unwrap() = SendBehaviour::Fail;
    let (a, a_id) = h.connect(&["pay_invoice", "get_balance"], None, true).await;
    let (b, b_id) = h.connect(&["make_invoice", "get_balance", "notifications"], None, true).await;
    h.wallet.transfer(None, Some(a_id), 100_000, "fund A".into()).await.unwrap();

    // bolt11
    let inv = b.call("make_invoice", json!({"amount": 10_000})).await;
    let r = a.call("pay_invoice", json!({"invoice": inv["result"]["invoice"]})).await;
    assert_eq!(r["result"]["fees_paid"], 0, "{r}");
    // bolt12 through `pay`
    let o = b.call("make_offer", json!({"amount": 5_000, "description": "item"})).await;
    let uri = format!("bitcoin:?lno={}", o["result"]["offer"].as_str().unwrap());
    let r = a.call("pay", json!({"payment": uri})).await;
    assert_eq!(r["result"]["instruction_type"], "bolt12", "{r}");
    assert_eq!(r["result"]["state"], "settled");
    // hold: A's payment waits until B settles.
    let (pre, hash) = preimage(5);
    let hold = b.call("make_hold_invoice", json!({"amount": 2_000, "payment_hash": hash})).await;
    let invoice = hold["result"]["invoice"].as_str().unwrap().to_string();
    let payer = tokio::spawn(async move {
        let r = a.call("pay_invoice", json!({"invoice": invoice})).await;
        (a, r)
    });
    let n =
        b.wait_notification("hold_invoice_accepted", SECS).await.expect("internal hold accepted");
    assert_eq!(n["amount"], 2_000);
    let r = b.call("settle_hold_invoice", json!({"preimage": pre})).await;
    assert_eq!(r["result"], json!({}), "{r}");
    let (a, r) = payer.await.unwrap();
    assert_eq!(r["result"]["preimage"], pre.as_str());

    let ba = a.call("get_balance", json!({})).await;
    let bb = b.call("get_balance", json!({})).await;
    assert_eq!(ba["result"]["balance"], 100_000 - 17_000);
    assert_eq!(bb["result"]["balance"], 17_000);
    assert!(h.wallet.ledger_check().await.unwrap().is_empty());
    let _ = b_id;
}

#[tokio::test]
async fn multi_pay_invoice_answers_per_item() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 10 };
    // Each payment reserves 10,000 + the 10-sat minimum fee reserve; the budget fits one.
    let (c, _) = h.connect(&["pay_invoice"], Some(25_000), false).await;
    let invs: Vec<String> = (0..3u8)
        .map(|i| MockBackend::make_invoice(Some(10_000), &sha256_hex(&[100 + i; 32])))
        .collect();
    let e = c
        .send_raw(
            json!({"method": "multi_pay_invoice", "params": {"invoices": [
                {"id": "a", "invoice": invs[0]}, {"id": "b", "invoice": invs[1]}, {"id": "c", "invoice": invs[2]}
            ]}}),
            false,
            vec![],
        )
        .await;
    let mut n = c.client.notifications();
    let mut got: Vec<(String, Value)> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while got.len() < 3 {
        let left =
            deadline.checked_duration_since(tokio::time::Instant::now()).expect("3 responses");
        if let Ok(Some(ClientNotification::Event { event, .. })) =
            tokio::time::timeout(left, n.next()).await
        {
            if event.kind == Kind::WalletConnectResponse
                && event.tags.event_ids().any(|id| id == e.id)
            {
                let plain = nip44::decrypt(c.keys.secret_key(), &c.wallet, &event.content).unwrap();
                got.push((
                    event.tags.identifier().unwrap().to_string(),
                    serde_json::from_str(&plain).unwrap(),
                ));
            }
        }
    }
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(got.iter().map(|g| g.0.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    let ok = got.iter().filter(|g| g.1["result"]["preimage"].is_string()).count();
    let quota = got.iter().filter(|g| g.1["error"]["code"] == "QUOTA_EXCEEDED").count();
    assert_eq!((ok, quota), (1, 2), "{got:?}");
}

#[tokio::test]
async fn pay_and_receive_bip321() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 3 };
    let (c, _) = h.connect(&["pay_invoice", "make_invoice"], None, false).await;
    let r = c.call("receive", json!({"amount": 9_000, "description": "both"})).await;
    let uri = r["result"]["bip321"].as_str().unwrap();
    assert!(uri.contains("lightning=") && uri.contains("lno="), "{uri}");
    assert!(r["result"]["transaction_id"].is_string());
    let r = c.call("receive", json!({})).await;
    assert!(r["result"]["bip321"].as_str().unwrap().starts_with("bitcoin:?lno="));
    assert!(r["result"].get("transaction_id").is_none());

    let bolt11 = MockBackend::make_invoice(Some(1_000), &sha256_hex(&[61; 32]));
    let r = c
        .call("pay", json!({"payment": format!("bitcoin:?lightning={bolt11}"), "payer_note": "hi"}))
        .await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "payer_note needs BOLT12");
    let r =
        c.call("pay", json!({"payment": format!("bitcoin:?lightning={bolt11}&req-pop=x")})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST");
    let r = c
        .call("pay", json!({"payment": "bitcoin:bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080"}))
        .await;
    assert_eq!(r["error"]["code"], "UNSUPPORTED_PAYMENT_INSTRUCTION", "{r}");
    let r = c
        .call("pay", json!({"payment": format!("bitcoin:?lightning={bolt11}"), "max_fee": 50}))
        .await;
    assert_eq!(r["result"]["instruction_type"], "bolt11", "{r}");
    assert_eq!(r["result"]["fees_paid"], 3);
    assert!(r["result"]["transaction_id"].is_string());
    let r = c
        .call(
            "pay",
            json!({"payment": "lno1mockexternal", "amount": 2_000, "payer_note": "thanks"}),
        )
        .await;
    assert_eq!(r["result"]["instruction_type"], "bolt12", "{r}");
}

#[tokio::test]
async fn stale_payment_requests_are_dropped() {
    let h = harness().await;
    let (c, _) = h.connect(&["pay_invoice", "get_balance"], None, false).await;
    let old = Timestamp::from(Timestamp::now().as_secs() - 7 * 3600);
    let body =
        json!({"method": "pay_invoice", "params": {"invoice": "lnbcrtmock1000hx"}}).to_string();
    let content = nip44::encrypt(c.keys.secret_key(), &c.wallet, body, nip44::Version::V2).unwrap();
    let ev = EventBuilder::new(Kind::WalletConnectRequest, content)
        .tags([Tag::public_key(c.wallet), Tag::custom("encryption", ["nip44_v2"])])
        .custom_created_at(old)
        .finalize(&c.keys)
        .unwrap();
    c.client.send_event(&ev).await.unwrap();
    assert!(c.wait_response(ev.id, false, Duration::from_secs(2)).await.is_none());
}

#[tokio::test]
async fn sign_message_for_non_isolated() {
    let h = harness().await;
    let (c, _) = h.connect(&["sign_message"], None, false).await;
    let r = c.call("sign_message", json!({"message": "hello"})).await;
    assert_eq!(r["result"]["message"], "hello");
    assert!(r["result"]["signature"].is_string(), "{r}");
}
