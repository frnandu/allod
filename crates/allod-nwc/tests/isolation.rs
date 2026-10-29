//! SPEC §16.3 isolation property test. For several random data sets: apps A and B are
//! isolated, C is not. Each makes invoices (paid and unpaid), offers (paid), hold
//! invoices (accepted) and outgoing payments. Then every NWC method is called by every
//! app, including lookups with the other apps' identifiers. No response or notification
//! of an app may contain an identifier of another app's rows.

mod common;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use allod_core::wallet::TxFilter;
use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use common::*;
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};

const SCOPES: &[&str] = &[
    "pay_invoice",
    "get_balance",
    "get_info",
    "make_invoice",
    "lookup_invoice",
    "list_transactions",
    "notifications",
];

/// Every identifier of an app's rows and offers.
async fn identifiers(h: &H, app_id: i64, offers: &[String]) -> HashSet<String> {
    let (rows, _) = h
        .wallet
        .transactions(&TxFilter {
            app_id: Some(Some(app_id)),
            include_unpaid: true,
            limit: 1000,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut ids: HashSet<String> = offers.iter().cloned().collect();
    for t in rows {
        ids.insert(t.public_id);
        ids.extend(t.payment_hash);
        ids.extend(t.payment_request);
        ids.extend(t.offer_id);
        ids.extend(t.preimage);
    }
    ids
}

fn leaks(text: &str, foreign: &HashSet<String>) -> Vec<String> {
    foreign.iter().filter(|id| id.len() >= 20 && text.contains(id.as_str())).cloned().collect()
}

#[tokio::test]
async fn apps_never_see_each_others_rows() {
    for seed in 0..4u64 {
        run(seed).await;
    }
}

async fn run(seed: u64) {
    let mut rng = StdRng::seed_from_u64(seed);
    let h = harness().await;
    *h.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let (a, a_id) = h.connect(SCOPES, None, true).await;
    let (b, b_id) = h.connect(SCOPES, None, true).await;
    let (c, c_id) = h.connect(SCOPES, None, false).await;
    for id in [a_id, b_id] {
        h.wallet.transfer(None, Some(id), 500_000, "fund".into()).await.unwrap();
    }
    let apps = [("A", &a, a_id), ("B", &b, b_id), ("C", &c, c_id)];
    let mut offers: HashMap<i64, Vec<String>> = HashMap::new();
    let mut tag: u8 = (seed as u8) * 60;

    // Data.
    for (_, cl, id) in &apps {
        for _ in 0..rng.gen_range(1..=3) {
            let r = cl.call("make_invoice", json!({"amount": rng.gen_range(1_000..9_000)})).await;
            let hash = r["result"]["payment_hash"].as_str().unwrap().to_string();
            if rng.gen_bool(0.6) {
                h.node.pay_invoice(&hash, r["result"]["amount"].as_u64().unwrap());
            }
        }
        for _ in 0..rng.gen_range(1..=2) {
            let r = cl.call("make_offer", json!({"description": "o"})).await;
            let offer_id = r["result"]["offer_id"].as_str().unwrap().to_string();
            offers
                .entry(*id)
                .or_default()
                .extend([offer_id.clone(), r["result"]["offer"].as_str().unwrap().into()]);
            h.node.pay_offer(&offer_id, rng.gen_range(1_000..5_000), Some("note".into()));
        }
        for _ in 0..rng.gen_range(0..=2) {
            tag = tag.wrapping_add(1);
            let inv = MockBackend::make_invoice(
                Some(rng.gen_range(1_000..4_000)),
                &sha256_hex(&[tag; 32]),
            );
            cl.call("pay_invoice", json!({"invoice": inv})).await;
        }
        if rng.gen_bool(0.5) {
            tag = tag.wrapping_add(1);
            let hash = sha256_hex(&[tag, 7, 7, 7]);
            cl.call("make_hold_invoice", json!({"amount": 3_000, "payment_hash": hash})).await;
            h.node.hold_htlc(&hash, 3_000, 900);
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut ids: HashMap<i64, HashSet<String>> = HashMap::new();
    for (_, _, id) in &apps {
        ids.insert(
            *id,
            identifiers(&h, *id, offers.get(id).map(Vec::as_slice).unwrap_or(&[])).await,
        );
    }

    // Every method, from every app.
    for (name, cl, me) in &apps {
        let foreign: HashSet<String> =
            ids.iter().filter(|(k, _)| *k != me).flat_map(|(_, v)| v.iter().cloned()).collect();
        let mut responses: Vec<Value> = Vec::new();
        for (m, p) in [
            ("get_info", json!({})),
            ("get_balance", json!({})),
            ("get_budget", json!({})),
            ("list_transactions", json!({"unpaid": true, "limit": 100})),
            ("list_transactions", json!({"type": "incoming", "limit": 100})),
            ("make_invoice", json!({"amount": 1_000})),
            ("receive", json!({"amount": 2_000})),
        ] {
            responses.push(cl.call(m, p).await);
        }
        for (other, set) in ids.iter().filter(|(k, _)| *k != me) {
            let (rows, _) = h
                .wallet
                .transactions(&TxFilter {
                    app_id: Some(Some(*other)),
                    include_unpaid: true,
                    limit: 1000,
                    ..Default::default()
                })
                .await
                .unwrap();
            for t in rows.iter().take(4) {
                let hash = t.payment_hash.clone().unwrap_or_default();
                let mut probes = vec![
                    ("lookup_payment", json!({"transaction_id": t.public_id})),
                    ("lookup_invoice", json!({"payment_hash": hash})),
                    ("lookup_payment", json!({"payment_hash": hash})),
                    ("cancel_hold_invoice", json!({"payment_hash": hash})),
                ];
                if let Some(o) = &t.offer_id {
                    probes.push((
                        "lookup_payment",
                        json!({"payment_type": "bolt12", "lookup": {"offer_id": o}}),
                    ));
                }
                if let Some(inv) = &t.payment_request {
                    probes.push(("lookup_invoice", json!({"invoice": inv})));
                }
                for (m, p) in probes {
                    let r = cl.call(m, p.clone()).await;
                    assert!(
                        r.get("result").is_none() || m == "lookup_invoice" && hash.is_empty(),
                        "seed {seed}: {name} {m} {p} on app {other}'s row answered {r}"
                    );
                    assert!(
                        matches!(
                            r["error"]["code"].as_str(),
                            Some("NOT_FOUND") | Some("BAD_REQUEST")
                        ),
                        "seed {seed}: {name} {m}: {r}"
                    );
                }
            }
            let _ = set;
        }
        for r in &responses {
            let text = r.to_string();
            let l = leaks(&text, &foreign);
            assert!(l.is_empty(), "seed {seed}: {name} saw foreign identifiers {l:?} in {text}");
        }
        // Notifications too.
        while let Some(n) = cl.wait_notification("payment_received", Duration::from_millis(1)).await
        {
            let l = leaks(&n.to_string(), &foreign);
            assert!(l.is_empty(), "seed {seed}: {name} notified about {l:?}");
        }
        while let Some(n) =
            cl.wait_notification("hold_invoice_accepted", Duration::from_millis(1)).await
        {
            let l = leaks(&n.to_string(), &foreign);
            assert!(l.is_empty(), "seed {seed}: {name} notified about {l:?}");
        }
        while let Some(n) = cl.wait_notification("payment_sent", Duration::from_millis(1)).await {
            let l = leaks(&n.to_string(), &foreign);
            assert!(l.is_empty(), "seed {seed}: {name} notified about {l:?}");
        }
    }
    // The isolated apps' own lists contain exactly their own rows.
    for (name, cl, me) in &apps[..2] {
        let r = cl.call("list_transactions", json!({"unpaid": true, "limit": 100})).await;
        // Fresh: the probe phase created more of the app's own invoices.
        let own = &identifiers(&h, *me, offers.get(me).map(Vec::as_slice).unwrap_or(&[])).await;
        for t in r["result"]["transactions"].as_array().unwrap() {
            let tid = t["transaction_id"].as_str().unwrap();
            assert!(own.contains(tid), "seed {seed}: {name} listed a row that is not its own: {t}");
        }
    }
    assert!(h.wallet.ledger_check().await.unwrap().is_empty());
}
