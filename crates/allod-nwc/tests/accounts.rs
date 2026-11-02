//! Accounts over NWC (SPEC §10, §16.4 scenario 7 against the mock backend): each account
//! answers on its own connection, accounts pay each other without Lightning traffic,
//! and a closed account answers nothing.

mod common;

use std::time::Duration;

use allod_ln::mock::SendBehaviour;
use common::*;
use serde_json::json;

#[tokio::test]
async fn accounts_pay_each_other_internally() {
    let h = harness().await;
    // Any payment that reached the node would fail: internal ones never do.
    *h.node.send.lock().unwrap() = SendBehaviour::Fail;
    let (a, a_id) = h.account("Alice").await;
    let (b, b_id) = h.account("Bob").await;
    h.wallet.transfer(None, Some(a_id), 100_000, "top up".into()).await.unwrap();

    let info = a.call("get_info", json!({})).await;
    let methods: Vec<&str> =
        info["result"]["methods"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
    for m in ["pay_invoice", "make_invoice", "get_balance", "list_transactions", "lookup_payment"] {
        assert!(methods.contains(&m), "{m} in {methods:?}");
    }
    assert!(!methods.contains(&"sign_message"));
    assert!(!methods.contains(&"create_connection"));
    assert_eq!(a.call("get_balance", json!({})).await["result"]["balance"], 100_000);
    assert_eq!(b.call("get_balance", json!({})).await["result"]["balance"], 0);

    // B invoices, A pays: a self payment.
    let inv = b.call("make_invoice", json!({"amount": 30_000, "description": "lunch"})).await;
    let invoice = inv["result"]["invoice"].as_str().unwrap().to_string();
    let paid = a.call("pay_invoice", json!({"invoice": invoice})).await;
    assert!(paid["error"].is_null(), "{paid}");
    assert_eq!(paid["result"]["fees_paid"], 0);
    assert_eq!(a.call("get_balance", json!({})).await["result"]["balance"], 70_000);
    assert_eq!(b.call("get_balance", json!({})).await["result"]["balance"], 30_000);
    assert!(a.wait_notification("payment_sent", Duration::from_secs(5)).await.is_some());
    let got = b.wait_notification("payment_received", Duration::from_secs(5)).await.unwrap();
    assert_eq!(got["amount"], 30_000);

    // More than A holds.
    let inv = b.call("make_invoice", json!({"amount": 80_000})).await;
    let r = a.call("pay_invoice", json!({"invoice": inv["result"]["invoice"]})).await;
    assert_eq!(r["error"]["code"], "INSUFFICIENT_BALANCE", "{r}");
    assert_eq!(a.call("get_balance", json!({})).await["result"]["balance"], 70_000);

    // Each sees only its own history.
    let la = a.call("list_transactions", json!({})).await;
    let lb = b.call("list_transactions", json!({})).await;
    assert_eq!(la["result"]["transactions"].as_array().unwrap().len(), 2, "{la}");
    assert_eq!(lb["result"]["transactions"].as_array().unwrap().len(), 1, "{lb}");
    assert!(h.wallet.ledger_check().await.unwrap().is_empty());

    // Closing B (sweeping its balance to Main) cuts its connection off.
    let swept = h.wallet.close_account(b_id, true).await.unwrap().unwrap();
    assert_eq!(swept.amount_msat, 30_000);
    h.apps.notify_changed();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let e = b.send_raw(json!({"method": "get_balance", "params": {}}), false, vec![]).await;
    assert!(b.wait_response(e.id, false, Duration::from_secs(3)).await.is_none());
    // A still works.
    assert_eq!(a.call("get_balance", json!({})).await["result"]["balance"], 70_000);
}
