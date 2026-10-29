//! Alby Hub `nip47/controllers/*_test.go` cases ported to Allod (SPEC §16.2). Each test
//! names the Hub test it follows; cases already covered elsewhere are listed in the
//! comment at the end. Keysend tests are out of scope (SPEC §1.2).

mod common;

use std::time::Duration;

use allod_core::apps::NewApp;
use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use common::*;
use nostr::prelude::*;
use serde_json::json;

fn inv(amount: Option<u64>, seed: u8) -> String {
    MockBackend::make_invoice(amount, &sha256_hex(&[seed; 32]))
}

// TestHandleGetInfoEvent_NoPermission, TestHandleGetInfoEvent_SubwalletNoPermission, TestHandleGetInfoEvent_WithPermission,
// TestHandleGetInfoEvent_WithMetadata, TestHandleGetInfoEvent_SubwalletWithMetadata, TestHandleGetInfoEvent_WithNotifications
#[tokio::test]
async fn get_info_permissions_and_metadata() {
    let h = harness().await;
    let (plain, _) = h.connect(&["get_balance"], None, false).await;
    let r = plain.call("get_info", json!({})).await;
    assert!(r["result"].get("alias").is_none() && r["result"].get("pubkey").is_none());
    assert_eq!(r["result"]["notifications"], json!([]));
    let (iso, _) = h.connect(&[], None, true).await;
    let r = iso.call("get_info", json!({})).await;
    assert!(r["result"].get("alias").is_none());
    assert_eq!(r["result"]["methods"], json!(["get_info", "get_budget"]));

    let (app, uri) = h
        .apps
        .create(
            NewApp {
                name: "meta".into(),
                scopes: vec!["get_info".into(), "notifications".into()],
                metadata: Some(json!({"a": 123})),
                ..Default::default()
            },
            None,
            Timestamp::now().as_secs() as i64,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let c = client_for(&h, &uri.unwrap(), &app).await;
    let r = c.call("get_info", json!({})).await;
    assert_eq!(r["result"]["metadata"]["a"], 123);
    assert_eq!(r["result"]["metadata"]["id"], app.id);
    assert_eq!(r["result"]["metadata"]["name"], "meta");
    assert!(r["result"]["pubkey"].is_string() && r["result"]["block_height"].is_number());
    assert_eq!(r["result"]["notifications"].as_array().unwrap().len(), 3);

    // Isolated (Hub "subwallet") with metadata: metadata yes, node details no.
    let (iso_app, uri) = h
        .apps
        .create(
            NewApp {
                name: "sub".into(),
                isolated: true,
                metadata: Some(json!({"b": "x"})),
                ..Default::default()
            },
            None,
            Timestamp::now().as_secs() as i64,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let c = client_for(&h, &uri.unwrap(), &iso_app).await;
    let r = c.call("get_info", json!({})).await;
    assert_eq!(r["result"]["metadata"]["b"], "x");
    assert_eq!(r["result"]["metadata"]["id"], iso_app.id);
    assert!(r["result"].get("pubkey").is_none(), "{r}");
}

async fn client_for(h: &H, uri: &str, app: &allod_core::App) -> C {
    let secret = uri.split("secret=").nth(1).unwrap();
    let keys = Keys::parse(secret).unwrap();
    let client = nostr_sdk::Client::builder().build();
    client.add_relay(h.relay_url.as_str()).await.unwrap();
    client.connect().await;
    client
        .subscribe(Filter::new().kind(Kind::WalletConnectResponse).pubkey(keys.public_key()), None)
        .await
        .unwrap();
    // Responses are ephemeral: let the subscription become active first.
    tokio::time::sleep(Duration::from_millis(500)).await;
    C::new(client, keys, PublicKey::from_hex(app.wallet_pubkey.as_deref().unwrap()).unwrap())
}

// TestHandleGetBalanceEvent_IsolatedApp_NoTransactions, TestHandleGetBalanceEvent_IsolatedApp_Transactions
#[tokio::test]
async fn isolated_balance() {
    let h = harness().await;
    let (iso, id) = h.connect(&["get_balance"], None, true).await;
    assert_eq!(iso.call("get_balance", json!({})).await["result"]["balance"], 0);
    h.wallet.transfer(None, Some(id), 12_000, "top up".into()).await.unwrap();
    assert_eq!(iso.call("get_balance", json!({})).await["result"]["balance"], 12_000);
}

// TestHandleGetBudgetEvent_NoRenewal, TestHandleGetBudgetEvent_NoneUsed, TestHandleGetBudgetEvent_HalfUsed, TestHandleGetBudgetEvent_NoBudget,
// TestHandleGetBudgetEvent_NoPayInvoicePermission, TestHandleGetBudgetEvent_DatabaseError (below)
#[tokio::test]
async fn get_budget_cases() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let now = Timestamp::now().as_secs() as i64;
    let (never, id) = h.connect(&["pay_invoice"], Some(400_000), false).await;
    h.apps
        .update(
            id,
            allod_core::apps::AppPatch {
                budget_renewal: Some("never".into()),
                ..Default::default()
            },
            now,
        )
        .await
        .unwrap();
    let r = never.call("get_budget", json!({})).await;
    assert_eq!(r["result"]["used_budget"], 0);
    assert_eq!(r["result"]["renewal_period"], "never");
    assert!(r["result"].get("renews_at").is_none(), "{r}");
    never.call("pay_invoice", json!({"invoice": inv(Some(200_000), 1)})).await;
    let r = never.call("get_budget", json!({})).await;
    assert_eq!(r["result"]["used_budget"], 200_000);
    let (nobudget, _) = h.connect(&["pay_invoice"], None, false).await;
    assert_eq!(nobudget.call("get_budget", json!({})).await["result"], json!({}));
    let (nopay, _) = h.connect(&["get_balance"], Some(400_000), false).await;
    assert_eq!(nopay.call("get_budget", json!({})).await["result"], json!({}));
}

// TestHandlePayInvoiceEvent_ZeroAmount, TestHandlePayInvoiceEvent_MalformedInvoice, TestHandlePayInvoiceEvent_ExpiredInvoice,
// TestHandlePayEvent_WrongNetwork, TestHandlePayEvent_ZeroAmountInvoiceWithoutAmount
#[tokio::test]
async fn pay_invoice_rejections() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    let r = c.call("pay_invoice", json!({"invoice": inv(None, 2)})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "zero-amount invoice without amount: {r}");
    let r = c.call("pay_invoice", json!({"invoice": inv(None, 3), "amount": 1_000})).await;
    assert!(r["result"]["preimage"].is_string(), "zero-amount invoice with amount: {r}");
    let r = c.call("pay_invoice", json!({"invoice": "lnbcrtnotaninvoice"})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST");
    let r = c.call("pay_invoice", json!({"invoice": MockBackend::make_expired_invoice(Some(1_000), &sha256_hex(&[4; 32]))})).await;
    assert_eq!(r["error"]["code"], "EXPIRED", "{r}");
    let r = c.call("pay_invoice", json!({"invoice": MockBackend::make_mainnet_invoice(Some(1_000), &sha256_hex(&[5; 32]))})).await;
    assert_eq!(r["error"]["code"], "UNSUPPORTED_NETWORK", "{r}");
}

// TestHandlePayEvent_ConflictingParamAmount, TestHandlePayEvent_ConflictingUriAmount,
// TestHandlePayEvent_MatchingUriAmount, TestHandlePayEvent_ZeroAmountInvoiceWithUriAmount,
// TestHandlePayEvent_InvalidUri, TestHandlePayEvent_Bolt12PreferredOverBolt11,
// TestHandlePayEvent_Bolt12VariableAmountWithoutAmount
#[tokio::test]
async fn pay_amount_rules() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    let r = c.call("pay", json!({"payment": format!("bitcoin:?lightning={}", inv(Some(1_000), 10)), "amount": 2_000})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "param amount conflicts with invoice: {r}");
    let r = c.call("pay", json!({"payment": format!("bitcoin:?amount=0.00000002&lightning={}", inv(Some(1_000), 11))})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "URI amount conflicts with invoice: {r}");
    let r = c.call("pay", json!({"payment": format!("bitcoin:?amount=0.00000001&lightning={}", inv(Some(1_000), 12))})).await;
    assert_eq!(r["result"]["amount"], 1_000, "matching URI amount: {r}");
    let r = c
        .call(
            "pay",
            json!({"payment": format!("bitcoin:?amount=0.00000003&lightning={}", inv(None, 13))}),
        )
        .await;
    assert_eq!(r["result"]["amount"], 3_000, "zero-amount invoice takes the URI amount: {r}");
    let r = c.call("pay", json!({"payment": "bitcoin:?"})).await;
    assert!(r["error"]["code"].is_string(), "invalid URI: {r}");

    // BOLT12 is preferred over BOLT11 when both are present.
    let offer = h
        .wallet
        .create_offer(allod_core::wallet::OfferRequest {
            description: "x".into(),
            ..Default::default()
        })
        .await;
    let _ = offer;
    let r = c
        .call("pay", json!({"payment": format!("bitcoin:?lightning={}&lno=lno1mockext", inv(Some(4_000), 14)), "amount": 4_000}))
        .await;
    assert_eq!(r["result"]["instruction_type"], "bolt12", "{r}");
    let r = c.call("pay", json!({"payment": "bitcoin:?lno=lno1mockvar"})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "variable offer needs an amount: {r}");
}

// TestHandleMultiPayInvoiceEvent_OneMalformedInvoice, TestHandleMultiPayInvoiceEvent_OneExpiredInvoice,
// TestHandleMultiPayInvoiceEvent_LNClient_OnePaymentFailed
#[tokio::test]
async fn multi_pay_mixed_results() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    let e = c
        .send_raw(
            json!({"method": "multi_pay_invoice", "params": {"invoices": [
                {"id": "ok", "invoice": inv(Some(1_000), 20)},
                {"id": "bad", "invoice": "lnbcrtgarbage"},
                {"id": "old", "invoice": MockBackend::make_expired_invoice(Some(1_000), &sha256_hex(&[21; 32]))},
            ]}}),
            false,
            vec![],
        )
        .await;
    let got = collect_multi(&c, e.id, 3).await;
    assert!(got["ok"]["result"]["preimage"].is_string(), "{got:?}");
    assert_eq!(got["bad"]["error"]["code"], "BAD_REQUEST");
    assert_eq!(got["old"]["error"]["code"], "EXPIRED");

    *h.node.send.lock().unwrap() = SendBehaviour::Fail;
    let e = c
        .send_raw(json!({"method": "multi_pay_invoice", "params": {"invoices": [{"invoice": inv(Some(1_000), 22)}]}}), false, vec![])
        .await;
    let got = collect_multi(&c, e.id, 1).await;
    // Without an id, the d tag is the payment hash.
    assert_eq!(got[&sha256_hex(&[22; 32])]["error"]["code"], "PAYMENT_FAILED", "{got:?}");
}

async fn collect_multi(
    c: &C,
    req: EventId,
    n: usize,
) -> std::collections::HashMap<String, serde_json::Value> {
    use nostr_sdk::RelayPoolNotification;
    let mut rx = c.client.notifications();
    let mut out = std::collections::HashMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while out.len() < n {
        let left =
            deadline.checked_duration_since(tokio::time::Instant::now()).expect("all responses");
        if let Ok(Ok(RelayPoolNotification::Event { event, .. })) =
            tokio::time::timeout(left, rx.recv()).await
        {
            if event.kind == Kind::WalletConnectResponse
                && event.tags.event_ids().any(|id| *id == req)
            {
                let plain = nip44::decrypt(c.keys.secret_key(), &c.wallet, &event.content).unwrap();
                out.insert(
                    event.tags.identifier().unwrap().to_string(),
                    serde_json::from_str(&plain).unwrap(),
                );
            }
        }
    }
    out
}

// TestHandleMakeHoldInvoiceEvent_MissingPaymentHash, TestHandleSettleHoldInvoiceEvent_InvalidPreimage,
// TestHandleCancelHoldInvoiceEvent_InvoiceNotFound
#[tokio::test]
async fn hold_invoice_bad_inputs() {
    let h = harness().await;
    let (c, _) = h.connect(&["make_invoice"], None, false).await;
    let r = c.call("make_hold_invoice", json!({"amount": 1_000})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST");
    let r = c.call("settle_hold_invoice", json!({"preimage": "zz"})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "{r}");
    let r = c.call("settle_hold_invoice", json!({"preimage": "11".repeat(32)})).await;
    assert_eq!(r["error"]["code"], "NOT_FOUND", "{r}");
    let r = c.call("cancel_hold_invoice", json!({"payment_hash": "22".repeat(32)})).await;
    assert_eq!(r["error"]["code"], "NOT_FOUND");
}

// TestHandleCreateConnectionEvent_PubkeyAlreadyExists, TestHandleCreateConnectionEvent_NoMethods, TestHandleCreateConnectionEvent_UnsupportedMethod,
// TestHandleCreateConnectionEvent_UnsupportedNotificationType
#[tokio::test]
async fn create_connection_validation() {
    let h = harness().await;
    let (s, _) = h.connect(&["superuser"], None, false).await;
    let pk = Keys::generate().public_key().to_hex();
    let ok = s
        .call(
            "create_connection",
            json!({"pubkey": pk, "name": "a", "request_methods": ["get_info"]}),
        )
        .await;
    assert!(ok["result"]["wallet_pubkey"].is_string(), "{ok}");
    for (params, why) in [
        (json!({"pubkey": pk, "name": "b", "request_methods": ["get_info"]}), "pubkey exists"),
        (
            json!({"pubkey": Keys::generate().public_key().to_hex(), "name": "c", "request_methods": []}),
            "no methods",
        ),
        (
            json!({"pubkey": Keys::generate().public_key().to_hex(), "name": "d", "request_methods": ["fly"]}),
            "unknown method",
        ),
        (
            json!({"pubkey": Keys::generate().public_key().to_hex(), "name": "e", "request_methods": ["get_info"], "notification_types": ["weather"]}),
            "unknown notification type",
        ),
    ] {
        let r = s.call("create_connection", params).await;
        assert_eq!(r["error"]["code"], "BAD_REQUEST", "{why}: {r}");
    }
}

// TestHandleReceiveEvent_NoAmountBolt12Offer, TestHandleMakeInvoiceEvent,
// TestHandleListTransactionsEvent_Paid
#[tokio::test]
async fn receive_and_history() {
    let h = harness().await;
    let (c, _) = h.connect(&["make_invoice", "list_transactions"], None, false).await;
    let r =
        c.call("make_invoice", json!({"amount": 1_000, "description": "d", "expiry": 600})).await;
    let hash = r["result"]["payment_hash"].as_str().unwrap().to_string();
    assert!(
        r["result"]["expires_at"].as_i64().unwrap() > r["result"]["created_at"].as_i64().unwrap()
    );
    h.node.pay_invoice(&hash, 1_000);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = c.call("list_transactions", json!({})).await;
    let items = r["result"]["transactions"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["state"], "settled");
    assert_eq!(items[0]["details"]["preimage"].as_str().map(str::len), Some(64));
}

// Hub cases covered by tests in other files:
// TestHandleGetBalanceEvent -> nwc.rs info_event_and_get_info
// TestHandleLookupInvoiceEvent -> nwc.rs make_and_lookup_invoice_with_visibility
// TestHandlePayInvoiceEvent -> nwc.rs pay_invoice_respects_the_budget
// TestHandleMakeHoldInvoiceEvent, TestHandleSettleHoldInvoiceEvent, TestHandleCancelHoldInvoiceEvent
//   -> nwc_ext.rs hold_invoices_ownership_and_notifications
// TestHandleCreateConnectionEvent, TestHandleCreateConnectionEvent_DoNotAllowCreateConnectionMethod
//   -> nwc_ext.rs create_connection_cannot_escalate
// TestHandleMultiPayInvoiceEvent_Success -> nwc_ext.rs multi_pay_invoice_answers_per_item
// TestHandleMultiPayInvoiceEvent_LNClient_OnePaymentFailed -> multi_pay_mixed_results (above)
// TestHandlePayEvent, TestHandlePayEvent_Bolt12, TestHandlePayEvent_Bolt12WithPayerNote,
// TestHandlePayEvent_PayerNoteUnsupported, TestHandlePayEvent_UnknownRequiredParam
//   -> nwc_ext.rs pay_and_receive_bip321
// TestHandleReceiveEvent, TestHandleReceiveEvent_NoAmount -> nwc_ext.rs pay_and_receive_bip321
// TestHandleListTransactionsEvent -> nwc_ext.rs list_transactions_pages_and_filters

// TestHandlePayEvent_Bolt12FeeLimit: max_fee reaches the node, and a route that does not
// fit it is FEE_LIMIT_EXCEEDED (NWC-321).
#[tokio::test]
async fn pay_fee_limit() {
    let h = harness().await;
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let r = c
        .call(
            "pay",
            json!({"payment": "bitcoin:?lno=lno1mockfee1", "amount": 123_000, "max_fee": 0}),
        )
        .await;
    assert_eq!(r["result"]["fees_paid"], 0, "{r}");
    assert_eq!(*h.node.last_max_fee.lock().unwrap(), Some(0));
    *h.node.send.lock().unwrap() = SendBehaviour::Fail;
    let r = c
        .call(
            "pay",
            json!({"payment": "bitcoin:?lno=lno1mockfee2", "amount": 123_000, "max_fee": 0}),
        )
        .await;
    assert_eq!(r["error"]["code"], "FEE_LIMIT_EXCEEDED", "{r}");
    let r = c.call("pay", json!({"payment": format!("bitcoin:?lightning={}", inv(Some(5_000), 40)), "max_fee": 1})).await;
    assert_eq!(r["error"]["code"], "FEE_LIMIT_EXCEEDED", "{r}");
    // Without a cap the same failure is a plain payment failure.
    let r = c
        .call("pay", json!({"payment": format!("bitcoin:?lightning={}", inv(Some(5_000), 41))}))
        .await;
    assert_eq!(r["error"]["code"], "PAYMENT_FAILED", "{r}");
    // A failed or refused attempt leaves no money reserved.
    assert!(h.wallet.ledger_check().await.unwrap().is_empty());
    assert_eq!(h.wallet.allocation().await.unwrap().pending_out_msat, 0);
}

// TestHandlePayEvent_Bolt12WrongNetwork, TestHandlePayEvent_Bolt12Expired
#[tokio::test]
async fn pay_bolt12_network_and_expiry() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    let r = c.call("pay", json!({"payment": "bitcoin:?lno=lno1mockmain1", "amount": 1_000})).await;
    assert_eq!(r["error"]["code"], "UNSUPPORTED_NETWORK", "{r}");
    let r = c.call("pay", json!({"payment": "bitcoin:?lno=lno1mockexp1", "amount": 1_000})).await;
    assert_eq!(r["error"]["code"], "EXPIRED", "{r}");
    assert!(
        h.wallet
            .transactions(&allod_core::wallet::TxFilter {
                limit: 100,
                include_unpaid: true,
                ..Default::default()
            })
            .await
            .unwrap()
            .0
            .is_empty(),
        "nothing recorded"
    );
}

// TestHandleGetBudgetEvent_DatabaseError: a storage failure is INTERNAL, without details.
#[tokio::test]
async fn get_budget_database_error() {
    let h = harness().await;
    let (c, _) = h.connect(&["pay_invoice"], Some(10_000), false).await;
    sqlx::query("ALTER TABLE transactions RENAME TO transactions_gone")
        .execute(&h.pool)
        .await
        .unwrap();
    let r = c.call("get_budget", json!({})).await;
    assert_eq!(r["error"]["code"], "INTERNAL", "{r}");
    assert_eq!(r["error"]["message"], "internal error");
}

// TestHandlePayEvent_Bolt12Unsupported: a node without BOLT12 refuses an offer-only URI
// with UNSUPPORTED_PAYMENT_INSTRUCTION and records nothing; with a BOLT11 alternative in
// the URI it pays that instead. `make_offer` is not advertised.
#[tokio::test]
async fn pay_without_bolt12_support() {
    let h = harness().await;
    h.node.bolt12.store(false, std::sync::atomic::Ordering::Relaxed);
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, _) = h.connect(&["pay_invoice", "make_invoice"], None, false).await;
    let r = c.call("pay", json!({"payment": "bitcoin:?lno=lno1mocknob12", "amount": 1_000})).await;
    assert_eq!(r["error"]["code"], "UNSUPPORTED_PAYMENT_INSTRUCTION", "{r}");
    assert!(h
        .wallet
        .transactions(&allod_core::wallet::TxFilter {
            limit: 100,
            include_unpaid: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .0
        .is_empty());
    let both = format!("bitcoin:?lightning={}&lno=lno1mocknob12", inv(Some(2_000), 50));
    let r = c.call("pay", json!({"payment": both})).await;
    assert_eq!(r["result"]["instruction_type"], "bolt11", "{r}");
    assert_eq!(r["result"]["state"], "settled");
    let info = c.call("get_info", json!({})).await;
    assert!(
        !info["result"]["methods"].as_array().unwrap().iter().any(|m| m == "make_offer"),
        "{info}"
    );
}

// TestHandleCreateConnectionEvent_IsolatedUnsupportedBackendType: Hub refuses isolated
// connections on backends that cannot isolate (Cashu). Allod isolates in its own ledger
// (SPEC §6.3), so every backend supports it; the counterpart asserts that an isolated
// child is created and starts with its own, empty balance.
#[tokio::test]
async fn isolated_connections_need_no_backend_support() {
    let h = harness().await;
    let (s, _) = h.connect(&["superuser"], None, false).await;
    let pk = Keys::generate().public_key().to_hex();
    let r = s
        .call("create_connection", json!({"pubkey": pk, "name": "iso", "request_methods": ["get_balance"], "isolated": true}))
        .await;
    assert!(r["result"]["wallet_pubkey"].is_string(), "{r}");
    let child = h.apps.by_app_pubkey(&pk).await.unwrap().unwrap();
    assert!(child.isolated);
    assert_eq!(h.wallet.app_balance(Some(child.id)).await.unwrap(), 0);
}

// TestHandleMultiPayInvoiceEvent_IsolatedApp_OneBudgetExceeded: an isolated app with
// 200,000 msat pays two 123,000 msat invoices at once: one succeeds, the other is
// INSUFFICIENT_BALANCE.
#[tokio::test]
async fn multi_pay_isolated_one_over_balance() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, id) = h.connect(&["pay_invoice"], None, true).await;
    h.wallet.transfer(None, Some(id), 200_000, "fund".into()).await.unwrap();
    let e = c
        .send_raw(
            json!({"method": "multi_pay_invoice", "params": {"invoices": [
                {"id": "x", "invoice": inv(Some(123_000), 60)}, {"id": "y", "invoice": inv(Some(123_000), 61)}
            ]}}),
            false,
            vec![],
        )
        .await;
    let got = collect_multi(&c, e.id, 2).await;
    let ok = got.values().filter(|r| r["result"]["preimage"].is_string()).count();
    let short = got.values().filter(|r| r["error"]["code"] == "INSUFFICIENT_BALANCE").count();
    assert_eq!((ok, short), (1, 1), "{got:?}");
}

// TestHandleMultiPayInvoiceEvent_IsolatedApp_ConcurrentPayments: many concurrent payments
// from an isolated app never spend more than it holds.
#[tokio::test]
async fn multi_pay_isolated_concurrent_never_overspends() {
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, id) = h.connect(&["pay_invoice", "get_balance"], None, true).await;
    h.wallet.transfer(None, Some(id), 200_000, "fund".into()).await.unwrap();
    let invoices: Vec<_> = (0..10u8)
        .map(|i| json!({"id": format!("p{i}"), "invoice": inv(Some(30_000), 70 + i)}))
        .collect();
    let e = c
        .send_raw(
            json!({"method": "multi_pay_invoice", "params": {"invoices": invoices}}),
            false,
            vec![],
        )
        .await;
    let got = collect_multi(&c, e.id, 10).await;
    let ok = got.values().filter(|r| r["result"]["preimage"].is_string()).count();
    // 30,000 + 10,000 fee reserve each while in flight; settled ones cost 30,000.
    assert!((5..=6).contains(&ok), "{ok} paid: {got:?}");
    let bal = c.call("get_balance", json!({})).await["result"]["balance"].as_i64().unwrap();
    assert_eq!(bal, 200_000 - 30_000 * ok as i64);
    assert!(bal >= 0);
    assert!(h.wallet.ledger_check().await.unwrap().is_empty());
}

// TestHandlePayEvent_Bolt12FixedAmount, TestHandlePayEvent_Bolt12ConflictingAmount
#[tokio::test]
async fn pay_bolt12_fixed_amount() {
    use allod_ln::LightningBackend;
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (c, _) = h.connect(&["pay_invoice"], None, false).await;
    // An offer from "another node" (not in Allod's offers table) for 7,000 msat.
    let fixed = h.node.bolt12_receive("fixed", Some(7_000), None).await.unwrap();
    let uri = format!("bitcoin:?lno={}", fixed.offer);
    let r = c.call("pay", json!({"payment": uri, "amount": 8_000})).await;
    assert_eq!(r["error"]["code"], "BAD_REQUEST", "conflicting amount: {r}");
    let r = c.call("pay", json!({"payment": uri})).await;
    assert_eq!(r["result"]["amount"], 7_000, "the offer's amount: {r}");
    assert_eq!(r["result"]["instruction_type"], "bolt12");
}

// TestHandleListTransactionsEvent_UnpaidOutgoingOnly, TestHandleListTransactionsEvent_UnpaidIncomingOnly,
// TestHandleListTransactionsEvent_Unpaid
#[tokio::test]
async fn list_transactions_unpaid_by_type() {
    let h = harness().await;
    let (c, _) =
        h.connect(&["pay_invoice", "make_invoice", "list_transactions"], None, false).await;
    *h.node.send.lock().unwrap() = SendBehaviour::Hang;
    // An outgoing payment still in flight (unpaid) …
    let pending = tokio::spawn({
        let inv = inv(Some(2_000), 90);
        let c2 = C::new(c.client.clone(), c.keys.clone(), c.wallet);
        async move {
            c2.send_raw(json!({"method": "pay_invoice", "params": {"invoice": inv}}), false, vec![])
                .await
        }
    });
    pending.await.unwrap();
    // … and two unpaid invoices.
    c.call("make_invoice", json!({"amount": 1_000})).await;
    c.call("make_invoice", json!({"amount": 1_500})).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let out = c.call("list_transactions", json!({"unpaid": true, "type": "outgoing"})).await;
    let rows = out["result"]["transactions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{out}");
    assert!(rows.iter().all(|t| t["type"] == "outgoing" && t["state"] == "pending"));
    let inc = c.call("list_transactions", json!({"unpaid": true, "type": "incoming"})).await;
    let rows = inc["result"]["transactions"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{inc}");
    assert!(rows.iter().all(|t| t["type"] == "incoming" && t["state"] == "pending"));
    let all = c.call("list_transactions", json!({"unpaid": true})).await;
    assert_eq!(all["result"]["total_count"], 3);
    let paid = c.call("list_transactions", json!({})).await;
    assert_eq!(
        paid["result"]["total_count"], 1,
        "in-flight payments are listed, unpaid invoices are not: {paid}"
    );
}
