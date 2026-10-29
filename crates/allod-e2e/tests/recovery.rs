//! SPEC §16.4 scenarios 8–9: restarts of Allod and of ldk-server. They restart containers,
//! so they run one at a time (this file is its own test binary; the lock orders the two).

use std::time::Duration;

use allod_e2e::{status_by_hash, Stack};
use allod_ln::types::{InvoiceDescription, LnStatus};
use allod_ln::LightningBackend;
use anyhow::Result;
use serde_json::json;
use tokio::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::const_new(());

/// 8. Allod is killed while a payment is in flight; after the restart the reconciler
/// settles the row.
#[tokio::test]
#[ignore = "needs the regtest stack (just up); restarts Allod"]
async fn e2e_08_crash_recovery() -> Result<()> {
    let _g = SERIAL.lock().await;
    let s = Stack::connect().await?;
    let inv = s
        .bob
        .bolt11_receive(Some(9_000_000), InvoiceDescription::Direct("e2e 8".into()), 600)
        .await?;
    let pay = {
        let (http, base) = (s.http.clone(), s.base.clone());
        let token = s.login().await?;
        let invoice = inv.invoice.clone();
        tokio::spawn(async move {
            http.post(format!("{base}/api/payments"))
                .bearer_auth(token)
                .json(&json!({"payment": invoice}))
                .send()
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    s.restart("allod")?;
    let _ = pay.await;
    s.wait_ready(Duration::from_secs(120)).await?;
    s.login().await?;
    // The row may not exist if Allod died before writing it; then nothing was sent either.
    let rows = s.by_hash(&inv.payment_hash).await?;
    if let Some(row) = rows.iter().find(|t| t["direction"] == "outgoing") {
        let row = s
            .wait_tx(row["public_id"].as_str().unwrap(), Duration::from_secs(90), |t| {
                t["state"] == "SETTLED" || t["state"] == "FAILED"
            })
            .await?;
        let bob_paid = status_by_hash(&s.bob, &inv.payment_hash).await?;
        match row["state"].as_str() {
            Some("SETTLED") => assert_eq!(bob_paid, Some(LnStatus::Succeeded)),
            _ => assert_ne!(bob_paid, Some(LnStatus::Succeeded), "Bob was paid but the row failed"),
        }
    } else {
        eprintln!("Allod stopped before recording the payment; checking nothing was paid");
        assert_ne!(status_by_hash(&s.bob, &inv.payment_hash).await?, Some(LnStatus::Succeeded));
    }
    s.assert_ledger_ok().await
}

/// 9. ldk-server restarts while payments arrive; Allod reconnects to the event stream and
/// reconciles what it missed.
#[tokio::test]
#[ignore = "needs the regtest stack (just up); restarts ldk-server"]
async fn e2e_09_event_stream_drop() -> Result<()> {
    let _g = SERIAL.lock().await;
    let s = Stack::connect().await?;
    let tx =
        s.post("/api/invoices", json!({"amount_msat": 6_000_000, "description": "e2e 9"})).await?;
    s.restart("ldk-server")?;
    // Bob retries until Alice's channels are back.
    let start = std::time::Instant::now();
    loop {
        if let Ok(id) = s.bob.bolt11_send(tx["payment_request"].as_str().unwrap(), None, None).await
        {
            if s.wait_ln(&s.bob, &id, Duration::from_secs(60)).await? == LnStatus::Succeeded {
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(180), "Bob could not pay after the restart");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    s.wait_ready(Duration::from_secs(120)).await?;
    s.wait_tx(tx["public_id"].as_str().unwrap(), Duration::from_secs(120), |t| {
        t["state"] == "SETTLED"
    })
    .await?;
    s.assert_ledger_ok().await
}
