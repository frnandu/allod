//! SPEC §16.4 scenarios 3–4 over Nostr Wallet Connect (M4).

use std::time::Duration;

use allod_e2e::{NwcClient, Stack};
use allod_ln::types::{InvoiceDescription, LnStatus};
use allod_ln::LightningBackend;
use anyhow::Result;
use serde_json::json;

async fn connection(s: &Stack, scopes: &[&str], budget_msat: i64) -> Result<NwcClient> {
    let v = s
        .post(
            "/api/apps",
            json!({"name": "e2e nwc", "scopes": scopes, "max_amount_msat": budget_msat, "budget_renewal": "daily"}),
        )
        .await?;
    let uri = v["pairing_uri"].as_str().unwrap().to_string();
    // Allod resubscribes after a 1 s debounce.
    tokio::time::sleep(Duration::from_secs(3)).await;
    NwcClient::from_uri(&uri).await
}

/// 3. A NWC client pays Bob's bolt11; the budget usage grows by amount + fee.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_03_nwc_pay_invoice() -> Result<()> {
    let s = Stack::connect().await?;
    let c = connection(&s, &["pay_invoice", "get_balance", "notifications"], 100_000_000).await?;
    let info = c.call("get_info", json!({}), Duration::from_secs(20)).await?;
    assert!(
        info["result"]["methods"].as_array().unwrap().iter().any(|m| m == "pay_invoice"),
        "{info}"
    );
    let inv = s
        .bob
        .bolt11_receive(Some(11_000_000), InvoiceDescription::Direct("e2e nwc 3".into()), 600)
        .await?;
    let r = c.call("pay_invoice", json!({"invoice": inv.invoice}), Duration::from_secs(90)).await?;
    assert_eq!(r["result_type"], "pay_invoice");
    let pre = r["result"]["preimage"].as_str().expect("preimage");
    assert_eq!(pre.len(), 64, "{r}");
    let n = c
        .wait_notification("payment_sent", Duration::from_secs(30), |n| {
            n["payment_hash"] == inv.payment_hash.as_str()
        })
        .await?;
    assert_eq!(n["state"], "settled");
    let b = c.call("get_budget", json!({}), Duration::from_secs(20)).await?;
    let used = b["result"]["used_budget"].as_i64().unwrap();
    let fees = r["result"]["fees_paid"].as_i64().unwrap();
    assert_eq!(used, 11_000_000 + fees, "{b}");
    // Over the budget.
    let big = s
        .bob
        .bolt11_receive(Some(95_000_000), InvoiceDescription::Direct("too much".into()), 600)
        .await?;
    let r = c.call("pay_invoice", json!({"invoice": big.invoice}), Duration::from_secs(30)).await?;
    assert_eq!(r["error"]["code"], "QUOTA_EXCEEDED", "{r}");
    s.assert_ledger_ok().await
}

/// 4. Bob pays an invoice made with NWC `make_invoice`; `lookup_invoice` reports it settled.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_04_nwc_make_invoice() -> Result<()> {
    let s = Stack::connect().await?;
    let c = connection(&s, &["make_invoice", "lookup_invoice", "notifications"], 0).await?;
    let r = c
        .call(
            "make_invoice",
            json!({"amount": 13_000_000, "description": "e2e nwc 4"}),
            Duration::from_secs(20),
        )
        .await?;
    let invoice = r["result"]["invoice"].as_str().expect("invoice").to_string();
    let hash = r["result"]["payment_hash"].as_str().unwrap().to_string();
    let id = s.bob.bolt11_send(&invoice, None, None).await?;
    assert_eq!(s.wait_ln(&s.bob, &id, Duration::from_secs(60)).await?, LnStatus::Succeeded);
    let start = std::time::Instant::now();
    loop {
        let r = c
            .call("lookup_invoice", json!({"payment_hash": hash}), Duration::from_secs(20))
            .await?;
        if r["result"]["state"] == "settled" {
            assert_eq!(r["result"]["amount"], 13_000_000);
            assert!(r["result"]["preimage"].is_string());
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "not settled: {r}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    s.assert_ledger_ok().await
}

/// 5. BOLT12: Bob pays a variable-amount offer from NWC `make_offer` twice → two settled
/// rows on the owning connection; Alice pays Bob's offer via `pay` with a payer note;
/// `lookup_payment` by offer_id + payment_hash.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_05_nwc_bolt12() -> Result<()> {
    let s = Stack::connect().await?;
    let c = connection(
        &s,
        &["make_invoice", "lookup_invoice", "list_transactions", "pay_invoice", "notifications"],
        0,
    )
    .await?;
    let r =
        c.call("make_offer", json!({"description": "e2e nwc 5"}), Duration::from_secs(20)).await?;
    let offer = r["result"]["offer"].as_str().expect("offer").to_string();
    let offer_id = r["result"]["offer_id"].as_str().unwrap().to_string();
    let mut hashes = Vec::new();
    for amount in [3_000_000u64, 4_000_000] {
        let id = s.bob.bolt12_send(&offer, Some(amount), None, None).await?;
        assert_eq!(s.wait_ln(&s.bob, &id, Duration::from_secs(60)).await?, LnStatus::Succeeded);
        let n = c
            .wait_notification("payment_received", Duration::from_secs(30), |n| {
                n["amount"] == amount
            })
            .await?;
        assert_eq!(n["details"]["offer_id"], offer_id.as_str());
        hashes.push(n["details"]["payment_hash"].as_str().unwrap().to_string());
    }
    let r = c
        .call(
            "lookup_payment",
            json!({"payment_type": "bolt12", "lookup": {"offer_id": offer_id}}),
            Duration::from_secs(20),
        )
        .await?;
    assert_eq!(r["error"]["code"], "MULTIPLE_MATCHES", "{r}");
    let r = c
        .call(
            "lookup_payment",
            json!({"payment_type": "bolt12", "lookup": {"offer_id": offer_id, "payment_hash": hashes[1]}}),
            Duration::from_secs(20),
        )
        .await?;
    assert_eq!(r["result"]["amount"], 4_000_000, "{r}");
    assert_eq!(r["result"]["state"], "settled");
    let r = c
        .call(
            "list_transactions",
            json!({"type": "incoming", "limit": 20}),
            Duration::from_secs(20),
        )
        .await?;
    let rows = r["result"]["transactions"].as_array().unwrap();
    assert_eq!(rows.iter().filter(|t| t["details"]["offer_id"] == offer_id.as_str()).count(), 2);

    // Alice pays Bob's offer with a payer note.
    let bob_offer = s.bob.bolt12_receive("bob e2e nwc 5", Some(2_000_000), None).await?;
    let r = c
        .call("pay", json!({"payment": format!("bitcoin:?lno={}", bob_offer.offer), "payer_note": "from nwc"}), Duration::from_secs(90))
        .await?;
    assert_eq!(r["result"]["instruction_type"], "bolt12", "{r}");
    assert_eq!(r["result"]["state"], "settled");
    let tid = r["result"]["transaction_id"].as_str().unwrap();
    let r =
        c.call("lookup_payment", json!({"transaction_id": tid}), Duration::from_secs(20)).await?;
    assert_eq!(r["result"]["details"]["payer_note"], "from nwc");
    assert_eq!(r["result"]["details"]["offer_id"], bob_offer.offer_id.as_str());
    s.assert_ledger_ok().await
}

/// 6. Hold invoice via NWC: make → Bob pays → `hold_invoice_accepted` with a
/// settle_deadline below the real deadline → settle → settled. Then cancel → Bob fails.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_06_nwc_hold_invoice() -> Result<()> {
    let s = Stack::connect().await?;
    let c = connection(&s, &["make_invoice", "lookup_invoice", "notifications"], 0).await?;
    let height = s.bob.node_info().await?.summary.block_height as i64;
    for settle in [true, false] {
        let (pre, hash) = allod_e2e::preimage();
        let r = c
            .call(
                "make_hold_invoice",
                json!({"amount": 5_000_000, "payment_hash": hash}),
                Duration::from_secs(20),
            )
            .await?;
        let invoice = r["result"]["invoice"].as_str().expect("invoice").to_string();
        let bob_id = s.bob.bolt11_send(&invoice, None, None).await?;
        let n = c
            .wait_notification("hold_invoice_accepted", Duration::from_secs(60), |n| {
                n["payment_hash"] == hash.as_str()
            })
            .await?;
        let deadline = n["settle_deadline"].as_i64().expect("settle_deadline");
        assert!(deadline > height, "deadline {deadline} is in the future (tip {height})");
        if settle {
            let r = c
                .call("settle_hold_invoice", json!({"preimage": pre}), Duration::from_secs(30))
                .await?;
            assert_eq!(r["result"], json!({}), "{r}");
            assert_eq!(
                s.wait_ln(&s.bob, &bob_id, Duration::from_secs(60)).await?,
                LnStatus::Succeeded
            );
            let r = c
                .call("lookup_invoice", json!({"payment_hash": hash}), Duration::from_secs(20))
                .await?;
            assert_eq!(r["result"]["state"], "settled");
        } else {
            let r = c
                .call("cancel_hold_invoice", json!({"payment_hash": hash}), Duration::from_secs(30))
                .await?;
            assert_eq!(r["result"], json!({}), "{r}");
            assert_eq!(
                s.wait_ln(&s.bob, &bob_id, Duration::from_secs(60)).await?,
                LnStatus::Failed
            );
        }
    }
    s.assert_ledger_ok().await
}
