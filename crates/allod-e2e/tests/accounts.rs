//! SPEC §16.4 scenario 7 with accounts (M6). Run against `just up`:
//! `cargo test -p allod-e2e --test accounts -- --ignored`.

use std::time::Duration;

use allod_e2e::{NwcClient, Stack};
use allod_ln::LightningBackend;
use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

async fn account(s: &Stack, name: &str) -> Result<(i64, NwcClient)> {
    let v = s.post("/api/accounts", json!({"name": name})).await?;
    let id = v["account"]["id"].as_i64().unwrap();
    let uri = v["pairing_uri"].as_str().unwrap().to_string();
    // Allod resubscribes after a 1 s debounce.
    tokio::time::sleep(Duration::from_secs(3)).await;
    Ok((id, NwcClient::from_uri(&uri).await?))
}

/// Σ outbound and inbound msat over Bob's channels.
async fn bob_channels(s: &Stack) -> Result<(u64, u64)> {
    let chans = s.bob.list_channels().await?;
    Ok((
        chans.iter().map(|c| c.outbound_capacity_msat).sum(),
        chans.iter().map(|c| c.inbound_capacity_msat).sum(),
    ))
}

async fn balance(c: &NwcClient) -> Result<i64> {
    let r = c.call("get_balance", json!({}), Duration::from_secs(20)).await?;
    Ok(r["result"]["balance"].as_i64().unwrap_or(-1))
}

/// 7. Two accounts: top-up from Main, they pay each other over NWC without Lightning
/// traffic (Bob's channels do not move), overspending is refused, and closing sweeps
/// the rest back to Main.
#[tokio::test]
#[ignore = "needs the regtest stack (just up)"]
async fn e2e_07_accounts() -> Result<()> {
    let s = Stack::connect().await?;
    let (a_id, a) = account(&s, "e2e 7 A").await?;
    let (b_id, b) = account(&s, "e2e 7 B").await?;
    let main_before = s.get("/api/balances").await?["main_spendable_msat"].as_i64().unwrap();
    s.post(&format!("/api/accounts/{a_id}/topup"), json!({"amount_msat": 50_000_000})).await?;
    assert_eq!(balance(&a).await?, 50_000_000);
    assert_eq!(balance(&b).await?, 0);
    let bob_before = bob_channels(&s).await?;

    // B invoices over NWC, A pays over NWC.
    let inv = b
        .call(
            "make_invoice",
            json!({"amount": 20_000_000, "description": "e2e 7"}),
            Duration::from_secs(20),
        )
        .await?;
    let invoice = inv["result"]["invoice"].as_str().unwrap().to_string();
    let hash = inv["result"]["payment_hash"].as_str().unwrap().to_string();
    let paid = a.call("pay_invoice", json!({"invoice": invoice}), Duration::from_secs(60)).await?;
    assert!(paid["error"].is_null(), "{paid}");
    assert_eq!(paid["result"]["fees_paid"], 0);
    assert_eq!(balance(&a).await?, 30_000_000);
    assert_eq!(balance(&b).await?, 20_000_000);
    let got = b
        .wait_notification("payment_received", Duration::from_secs(20), |n| {
            n["payment_hash"] == hash
        })
        .await?;
    assert_eq!(got["amount"], 20_000_000);
    // No Lightning traffic.
    assert!(s.alice.payment(&hash).await?.is_none(), "internal payment went over Lightning");
    assert_eq!(bob_channels(&s).await?, bob_before, "Bob's channels moved");

    // Overspending is refused.
    let big =
        b.call("make_invoice", json!({"amount": 40_000_000}), Duration::from_secs(20)).await?;
    let r = a
        .call("pay_invoice", json!({"invoice": big["result"]["invoice"]}), Duration::from_secs(20))
        .await?;
    assert_eq!(r["error"]["code"], "INSUFFICIENT_BALANCE", "{r}");

    // Statement and summary.
    let st = s.get(&format!("/api/accounts/{a_id}/statement")).await?;
    assert_eq!(st["balance_msat"], 30_000_000);
    assert_eq!(st["items"][0]["amount_msat"], -20_000_000);

    // Withdraw A to empty, close it; close B with a signed sweep.
    s.post(&format!("/api/accounts/{a_id}/withdraw"), json!({"amount_msat": 30_000_000})).await?;
    s.ok(Method::DELETE, &format!("/api/accounts/{a_id}"), None).await?;
    let path = format!("/api/accounts/{b_id}?sweep=true");
    let (status, _) = s.call(Method::DELETE, &path, None).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "sweeping needs re-authentication");
    let url = format!("{}{path}", s.base);
    let resp = s
        .http
        .delete(&url)
        .header("x-allod-session", s.session_token().await)
        .header("authorization", s.nip98(&url, "DELETE", b"")?)
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let closed: Value = resp.json().await?;
    assert_eq!(closed["sweep"]["amount_msat"], 20_000_000);

    let main_after = s.get("/api/balances").await?["main_spendable_msat"].as_i64().unwrap();
    // Reconciliation may move Main by unrelated node activity; the accounts gave back
    // everything they took.
    assert!((main_after - main_before).abs() < 1_000, "{main_before} -> {main_after}");
    assert!(
        b.call("get_balance", json!({}), Duration::from_secs(5)).await.is_err(),
        "a closed account still answers"
    );
    s.assert_ledger_ok().await
}
