//! NWC-08: a client asks for a connection to its own key and finds the wallet through
//! the info event that names it.

mod common;

use std::time::Duration;

use allod_core::apps::{authorization, AppPatch, AuthorizeRequest};
use allod_core::wallet::InvoiceRequest;
use common::*;
use nostr::prelude::*;
use nostr_sdk::prelude::*;
use serde_json::json;

#[tokio::test]
async fn client_finds_the_wallet_and_gets_only_what_it_asked_for() {
    let h = harness().await;
    let keys = Keys::generate();
    let state = "9f2c4a".repeat(6);
    let req = AuthorizeRequest {
        pubkey: keys.public_key().to_hex(),
        state: state.clone(),
        name: Some("Example App".into()),
        max_amount: Some(50_000),
        renewal_period: Some("monthly".into()),
        request_methods: vec!["pay_invoice".into(), "get_balance".into()],
        optional_request_methods: vec!["make_invoice".into(), "list_transactions".into()],
        granted_optional_methods: Some(vec!["make_invoice".into()]),
        notification_types: vec!["payment_sent".into()],
        relays: vec![h.relay_url.clone()],
        ..Default::default()
    };
    let now = Timestamp::now().as_secs() as i64;
    let (app, uri) = h.apps.create(authorization(&req, true).unwrap(), None, now).await.unwrap();
    assert!(uri.is_none(), "the client keeps its own secret");

    // The client knows only its key, its relay and its state.
    let client = Client::builder().build();
    client.add_relay(h.relay_url.as_str()).await.unwrap();
    client.connect().await;
    client
        .subscribe(
            Filter::new()
                .kinds([Kind::WalletConnectResponse, Kind::Custom(23197)])
                .pubkey(keys.public_key()),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let found = client
        .fetch_events(Filter::new().kind(Kind::WalletConnectInfo).pubkey(keys.public_key()))
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let info = found.first().expect("info event naming the client");
    assert_eq!(Some(info.pubkey.to_hex()), app.wallet_pubkey);
    let tag = |name: &str| {
        info.tags.iter().find(|t| t.kind() == name).and_then(|t| t.content()).map(str::to_string)
    };
    assert_eq!(tag("state"), Some(state.clone()));
    assert_eq!(tag("relay"), Some(h.relay_url.clone()));
    assert_eq!(tag("notifications").as_deref(), Some("payment_sent"));
    assert!(tag("extensions").unwrap().split(' ').any(|e| e == "08"));
    assert_eq!(info.content, "get_info get_budget pay_invoice get_balance make_invoice");
    // Delivered: the client's relays are not kept.
    assert!(h.apps.get(app.id).await.unwrap().auth_relays.is_none());

    let c = C::new(client, keys, info.pubkey);
    let r = c.call("get_info", json!({})).await;
    assert_eq!(
        r["result"]["methods"],
        json!(["get_info", "get_budget", "pay_invoice", "get_balance", "make_invoice"])
    );
    assert_eq!(r["result"]["notifications"], json!(["payment_sent"]));
    assert!(r["result"]["extensions"].as_array().unwrap().contains(&json!("08")));
    assert_eq!(c.call("get_budget", json!({})).await["result"]["total_budget"], 50_000);
    assert!(c.call("get_balance", json!({})).await["result"]["balance"].is_number());

    // `pay_invoice` was asked for; the permission's other methods were not.
    for method in ["pay", "multi_pay_invoice", "make_hold_invoice", "list_transactions"] {
        let r = c.call(method, json!({})).await;
        assert_eq!(r["error"]["code"], "RESTRICTED", "{method}: {r}");
    }
    assert!(c.call("make_invoice", json!({"amount": 1000})).await["result"]["invoice"].is_string());

    // Only the requested notification type is sent: an incoming payment reaches an
    // ordinary connection, and this one stays quiet.
    let (other, _) = h.connect(&["notifications"], None, false).await;
    let inv = h
        .wallet
        .create_invoice(InvoiceRequest { amount_msat: Some(2_000), ..Default::default() })
        .await
        .unwrap();
    h.node.pay_invoice(inv.payment_hash.as_deref().unwrap(), 2_000);
    other.wait_notification("payment_received", Duration::from_secs(10)).await.expect("sent");
    assert!(c.wait_notification("payment_received", Duration::from_secs(2)).await.is_none());

    // When the owner edits the permissions, the owner's choice replaces the client's list.
    let patch = AppPatch { scopes: Some(vec!["pay_invoice".into()]), ..Default::default() };
    let edited = h.apps.update(app.id, patch, now).await.unwrap();
    assert!(edited.methods.is_none() && edited.permits_method("pay"));
}
