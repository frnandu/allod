//! SPEC §16.4 scenarios 1–7 through the HTTP API (M2/M3). Run against `just up`:
//! `cargo test -p allod-e2e -- --ignored`.

use std::time::Duration;

use allod_e2e::{preimage, Stack};
use allod_ln::types::{InvoiceDescription, LnStatus};
use allod_ln::LightningBackend;
use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde_json::json;

fn desc(s: &str) -> InvoiceDescription {
    InvoiceDescription::Direct(s.into())
}

/// 1. Setup is complete and the owner signs in with Nostr (NIP-98).
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_01_setup_and_login() -> Result<()> {
    let s = Stack::connect().await?;
    let info = s
        .http
        .get(format!("{}/api/info", s.base))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    assert_eq!(info["setup_completed"], true, "{info}");
    let me = s.get("/api/auth/session").await?;
    assert_eq!(me["kind"], "owner");
    assert_eq!(me["pubkey"], s.keys.public_key().to_hex());
    // Without a session the API is closed.
    let r = s.http.get(format!("{}/api/node", s.base)).send().await?;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let node = s.get("/api/node").await?;
    assert_eq!(node["network"], "regtest");
    Ok(())
}

/// 2. Open a channel Alice→Bob, mine 6 blocks, check channels and balances.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_02_open_channel() -> Result<()> {
    let s = Stack::connect().await?;
    let bob = s.bob.node_info().await?.summary.pubkey;
    let before = s.get("/api/balances").await?;
    let opened = s
        .post("/api/channels", json!({"pubkey": format!("{bob}@ldk-bob:9735"), "amount_sat": 250_000, "public": false}))
        .await?;
    let ucid = opened["user_channel_id"].as_str().unwrap().to_string();
    let start = std::time::Instant::now();
    loop {
        s.mine(1).await?;
        let chans = s.get("/api/channels").await?;
        let ch = chans
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["user_channel_id"] == ucid.as_str())
            .cloned();
        if let Some(ch) = ch {
            assert_eq!(ch["counterparty_node_id"], bob.as_str());
            if ch["is_usable"] == true {
                assert_eq!(ch["capacity_sat"], 250_000);
                assert_eq!(ch["is_announced"], false);
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(90), "channel {ucid} not usable");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let after = s.get("/api/balances").await?;
    let lt = |v: &serde_json::Value| v["lightning"]["total_msat"].as_u64().unwrap();
    assert!(lt(&after) >= lt(&before) + 240_000_000, "before {before}, after {after}");
    let peers = s.get("/api/peers").await?;
    assert!(peers.as_array().unwrap().iter().any(|p| p["node_id"] == bob.as_str()));
    Ok(())
}

/// 3. Alice pays Bob's BOLT11 invoice through the API.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_03_pay_bolt11() -> Result<()> {
    let s = Stack::connect().await?;
    let inv = s.bob.bolt11_receive(Some(21_000_000), desc("e2e 3"), 600).await?;
    let tx = s.post("/api/payments", json!({"payment": inv.invoice})).await?;
    let tx = s.wait_state(tx["public_id"].as_str().unwrap(), "SETTLED").await?;
    assert_eq!(tx["direction"], "outgoing");
    assert_eq!(tx["amount_msat"], 21_000_000);
    assert_eq!(tx["payment_hash"], inv.payment_hash.as_str());
    assert!(tx["preimage"].is_string());
    // O-5: ldk-node's bolt11 payment id is the payment hash.
    let ln = s.alice.payment(&inv.payment_hash).await?.expect("payment keyed by hash");
    assert_eq!(ln.status, LnStatus::Succeeded);
    // Paying the same invoice again is refused.
    let (status, _) =
        s.call(Method::POST, "/api/payments", Some(json!({"payment": inv.invoice}))).await?;
    assert_eq!(status, StatusCode::CONFLICT);
    s.assert_ledger_ok().await
}

/// 4. Bob pays an invoice made through the API.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_04_receive_bolt11() -> Result<()> {
    let s = Stack::connect().await?;
    let tx =
        s.post("/api/invoices", json!({"amount_msat": 33_000_000, "description": "e2e 4"})).await?;
    assert_eq!(tx["state"], "PENDING");
    let id = s.bob.bolt11_send(tx["payment_request"].as_str().unwrap(), None, None).await?;
    assert_eq!(s.wait_ln(&s.bob, &id, Duration::from_secs(60)).await?, LnStatus::Succeeded);
    let tx = s.wait_state(tx["public_id"].as_str().unwrap(), "SETTLED").await?;
    assert_eq!(tx["direction"], "incoming");
    assert_eq!(tx["amount_msat"], 33_000_000);
    s.assert_ledger_ok().await
}

/// 5. BOLT12: Bob pays a variable-amount offer twice; Alice pays Bob's offer with a note.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_05_bolt12() -> Result<()> {
    let s = Stack::connect().await?;
    let offer = s.post("/api/offers", json!({"description": "e2e 5 tips"})).await?;
    let (offer_str, offer_id) = (
        offer["offer"].as_str().unwrap().to_string(),
        offer["offer_id"].as_str().unwrap().to_string(),
    );
    for amount in [7_000_000u64, 8_000_000] {
        let id = s.bob.bolt12_send(&offer_str, Some(amount), None, None).await?;
        assert_eq!(s.wait_ln(&s.bob, &id, Duration::from_secs(60)).await?, LnStatus::Succeeded);
    }
    let start = std::time::Instant::now();
    let rows = loop {
        let v = s.get("/api/transactions?direction=incoming&limit=200").await?;
        let rows: Vec<_> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["offer_id"] == offer_id.as_str() && t["state"] == "SETTLED")
            .cloned()
            .collect();
        if rows.len() == 2 || start.elapsed() > Duration::from_secs(60) {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(rows.len(), 2, "two settled rows for the offer");
    let mut amounts: Vec<_> = rows.iter().map(|t| t["amount_msat"].as_u64().unwrap()).collect();
    amounts.sort_unstable();
    assert_eq!(amounts, vec![7_000_000, 8_000_000]);

    let bob_offer = s.bob.bolt12_receive("bob e2e 5", Some(5_000_000), None).await?;
    let tx = s
        .post("/api/payments", json!({"payment": bob_offer.offer, "payer_note": "thanks bob"}))
        .await?;
    let tx = s.wait_state(tx["public_id"].as_str().unwrap(), "SETTLED").await?;
    assert_eq!(tx["payment_type"], "bolt12");
    assert_eq!(tx["payer_note"], "thanks bob");
    assert_eq!(tx["offer_id"], bob_offer.offer_id.as_str());
    s.assert_ledger_ok().await
}

/// 6. Hold invoice: accept → settle; then a second one is canceled and Bob's payment fails.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_06_hold_invoice() -> Result<()> {
    let s = Stack::connect().await?;
    for settle in [true, false] {
        let (pre, hash) = preimage();
        let tx = s
            .post(
                "/api/hold-invoices",
                json!({"amount_msat": 12_000_000, "payment_hash": hash, "description": "e2e 6"}),
            )
            .await?;
        let pid = tx["public_id"].as_str().unwrap().to_string();
        let bob_id = s.bob.bolt11_send(tx["payment_request"].as_str().unwrap(), None, None).await?;
        let accepted = s.wait_state(&pid, "ACCEPTED").await?;
        let deadline = accepted["settle_deadline"].as_i64().expect("settle_deadline");
        assert!(deadline > 0);
        assert_eq!(accepted["claimable_amount_msat"], 12_000_000);
        if settle {
            s.post("/api/hold-invoices/settle", json!({"preimage": pre})).await?;
            s.wait_state(&pid, "SETTLED").await?;
            assert_eq!(
                s.wait_ln(&s.bob, &bob_id, Duration::from_secs(60)).await?,
                LnStatus::Succeeded
            );
        } else {
            s.post("/api/hold-invoices/cancel", json!({"payment_hash": hash})).await?;
            s.wait_state(&pid, "CANCELED").await?;
            assert_eq!(
                s.wait_ln(&s.bob, &bob_id, Duration::from_secs(60)).await?,
                LnStatus::Failed
            );
        }
    }
    s.assert_ledger_ok().await
}

/// 7. Self payment: Alice pays her own invoice internally, with no Lightning traffic.
/// (Isolated accounts and transfers between them are covered in M6.)
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_07_self_payment() -> Result<()> {
    let s = Stack::connect().await?;
    let inv =
        s.post("/api/invoices", json!({"amount_msat": 4_000_000, "description": "e2e 7"})).await?;
    let out = s.post("/api/payments", json!({"payment": inv["payment_request"]})).await?;
    let out = s.wait_state(out["public_id"].as_str().unwrap(), "SETTLED").await?;
    assert_eq!(out["self_payment"], true);
    assert_eq!(out["fee_msat"], 0);
    let inc = s.wait_state(inv["public_id"].as_str().unwrap(), "SETTLED").await?;
    assert_eq!(inc["self_payment"], true);
    // No Lightning traffic: Alice's node has no payment for this hash.
    let hash = out["payment_hash"].as_str().unwrap();
    assert!(s.alice.payment(hash).await?.is_none(), "self payment went over Lightning");
    // Overspending is refused before anything is sent.
    let big = s.bob.bolt11_receive(Some(50_000_000_000_000), desc("too much"), 600).await;
    if let Ok(big) = big {
        let (status, v) =
            s.call(Method::POST, "/api/payments", Some(json!({"payment": big.invoice}))).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    }
    s.assert_ledger_ok().await
}
