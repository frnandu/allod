//! Wallet tests against the mock backend (SPEC §16.1–16.2).

use std::sync::Arc;
use std::time::Duration;

use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use allod_ln::LightningBackend;

use super::*;

struct T {
    wallet: Arc<Wallet>,
    node: Arc<MockBackend>,
    pool: AnyPool,
    _dir: tempfile::TempDir,
}

fn fast() -> WalletConfig {
    WalletConfig {
        settle_safety_blocks: 3,
        sync_timeout: Duration::from_millis(300),
        reconcile_interval: Duration::from_secs(3600),
        poll_interval: Duration::from_millis(20),
    }
}

async fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/w.db", dir.path().display());
    allod_db::Db::connect(&url).await.unwrap();
    let pool = allod_db::Db::any_pool(&url).await.unwrap();
    let node = Arc::new(MockBackend::new());
    let wallet = Wallet::new(pool.clone(), node.clone() as Arc<dyn LightningBackend>, fast());
    wallet.reconcile().await.unwrap();
    T { wallet, node, pool, _dir: dir }
}

async fn add_account(t: &T, name: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO apps (name, kind, app_pubkey, isolated, created_at, updated_at) \
         VALUES ($1, 'account', $2, TRUE, 0, 0) RETURNING id",
    )
    .bind(name)
    .bind(format!("pk-{name}"))
    .fetch_one(&t.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn delegated_siblings_share_ancestor_budget_under_write_lock() {
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let parent: i64 = sqlx::query_scalar(
        "INSERT INTO apps (name, kind, app_pubkey, max_amount_msat, budget_renewal, created_at, updated_at) \
         VALUES ('parent', 'connection', 'parent-pk', 100000, 'monthly', 0, 0) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let mut children = Vec::new();
    for n in 0..2 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO apps (name, kind, app_pubkey, max_amount_msat, budget_renewal, created_by_app_id, created_at, updated_at) \
             VALUES ('child', 'connection', $1, 100000, 'monthly', $2, 0, 0) RETURNING id",
        )
        .bind(format!("child-{n}"))
        .bind(parent)
        .fetch_one(&t.pool)
        .await
        .unwrap();
        children.push(id);
    }
    let grandchild: i64 = sqlx::query_scalar(
        "INSERT INTO apps (name, kind, app_pubkey, max_amount_msat, budget_renewal, created_by_app_id, created_at, updated_at) \
         VALUES ('grandchild', 'connection', 'grandchild-pk', 100000, 'monthly', $1, 0, 0) RETURNING id",
    )
    .bind(children[1])
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let pay = |app_id, amount, seed| PayRequest {
        app_id: Some(app_id),
        payment: external_invoice(amount, seed),
        max_fee_msat: Some(0),
        ..Default::default()
    };
    let (a, b) = tokio::join!(
        t.wallet.pay(pay(children[0], 60_000, 81)),
        t.wallet.pay(pay(children[1], 60_000, 82))
    );
    assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1, "{a:?} {b:?}");
    assert!(matches!(
        (&a, &b),
        (Err(WalletError::QuotaExceeded), _) | (_, Err(WalletError::QuotaExceeded))
    ));
    assert_eq!(t.wallet.budget(parent).await.unwrap().unwrap().used_budget, 60_000);
    t.wallet.pay(pay(grandchild, 40_000, 83)).await.unwrap();
    assert!(matches!(t.wallet.pay(pay(children[0], 1, 84)).await, Err(WalletError::QuotaExceeded)));
    assert_eq!(t.wallet.budget(parent).await.unwrap().unwrap().used_budget, 100_000);
}

fn external_invoice(amount_msat: u64, seed: u8) -> String {
    MockBackend::make_invoice(Some(amount_msat), &sha256_hex(&[seed; 32]))
}

async fn assert_healthy(t: &T) {
    let problems = t.wallet.ledger_check().await.unwrap();
    assert!(problems.is_empty(), "ledger problems: {problems:?}");
}

#[tokio::test]
async fn reconcile_books_node_balance_to_main() {
    let t = setup().await;
    let a = t.wallet.allocation().await.unwrap();
    assert_eq!(a.main_msat, 1_000_000_000);
    assert_eq!(a.accounts_total_msat, 0);
    // Running again changes nothing.
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn pay_success_debits_amount_and_fee() {
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 2_000 };
    let tx = t
        .wallet
        .pay(PayRequest { payment: external_invoice(100_000, 1), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(tx.state, "SETTLED");
    assert_eq!(tx.fee_msat, 2_000);
    assert_eq!(tx.fee_reserve_msat, 0);
    assert!(tx.preimage.is_some());
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000 - 102_000);
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 0);
    // Paying the same invoice again is refused.
    let again = t
        .wallet
        .pay(PayRequest { payment: external_invoice(100_000, 1), ..Default::default() })
        .await;
    assert!(matches!(again, Err(WalletError::AlreadyPaid)));
    assert_healthy(&t).await;
}

#[tokio::test]
async fn failed_payment_releases_reserve() {
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::Fail;
    let tx = t
        .wallet
        .pay(PayRequest { payment: external_invoice(50_000, 2), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(tx.state, "FAILED");
    assert!(tx.failure_reason.is_some());
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn pending_payment_settles_from_event() {
    let t = setup().await;
    t.wallet.start();
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let inv = external_invoice(10_000_000, 3);
    let tx = t.wallet.pay(PayRequest { payment: inv, ..Default::default() }).await.unwrap();
    assert_eq!(tx.state, "PENDING");
    let reserve = fee_reserve_msat(10_000_000, None) as i64;
    assert_eq!(reserve, 100_000);
    let a = t.wallet.allocation().await.unwrap();
    assert_eq!(a.pending_out_msat, 10_000_000 + reserve);
    assert_eq!(a.main_msat, 1_000_000_000 - 10_000_000 - reserve);
    assert_healthy(&t).await;

    t.node.complete(tx.payment_hash.as_deref().unwrap(), true, 5_000);
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let done = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(done.state, "SETTLED");
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000 - 10_005_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn insufficient_balance_and_bad_inputs() {
    let t = setup().await;
    let big = t
        .wallet
        .pay(PayRequest { payment: external_invoice(2_000_000_000, 4), ..Default::default() })
        .await;
    assert!(matches!(big, Err(WalletError::InsufficientBalance)));
    let note = t
        .wallet
        .pay(PayRequest {
            payment: external_invoice(1_000, 5),
            payer_note: Some("hi".into()),
            ..Default::default()
        })
        .await;
    assert!(matches!(note, Err(WalletError::BadRequest(_))));
    let zero = MockBackend::make_invoice(None, &sha256_hex(&[6; 32]));
    assert!(matches!(
        t.wallet.pay(PayRequest { payment: zero.clone(), ..Default::default() }).await,
        Err(WalletError::BadRequest(_))
    ));
    let conflict = t
        .wallet
        .pay(PayRequest {
            payment: external_invoice(1_000, 7),
            amount_msat: Some(2_000),
            ..Default::default()
        })
        .await;
    assert!(matches!(conflict, Err(WalletError::BadRequest(_))));
    let ok = t
        .wallet
        .pay(PayRequest { payment: zero, amount_msat: Some(7_000), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(ok.amount_msat, 7_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn received_invoice_credits_owner() {
    let t = setup().await;
    t.wallet.start();
    let acct = add_account(&t, "kid").await;
    let tx = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(acct),
            amount_msat: Some(21_000),
            description: "pocket money".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tx.state, "PENDING");
    t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 21_000);
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 21_000);
    // Replayed event changes nothing.
    t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 21_000);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 21_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn offer_payments_credit_the_offer_owner() {
    let t = setup().await;
    t.wallet.start();
    let acct = add_account(&t, "shop").await;
    let offer = t
        .wallet
        .create_offer(OfferRequest {
            app_id: Some(acct),
            description: "tips".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    t.node.pay_offer(&offer.offer_id, 5_000, Some("thanks".into()));
    t.node.pay_offer(&offer.offer_id, 7_000, None);
    for _ in 0..100 {
        if t.wallet.balance(Some(acct)).await.unwrap() == 12_000 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 12_000);
    let (txs, _) = t
        .wallet
        .transactions(&TxFilter { app_id: Some(Some(acct)), limit: 10, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(txs.len(), 2);
    assert!(txs.iter().any(|x| x.payer_note.as_deref() == Some("thanks")));
    assert_healthy(&t).await;
}

#[tokio::test]
async fn bolt12_timeout_after_submission_adopts_and_settles() {
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::SubmitThenTimeout;
    let tx = t
        .wallet
        .pay(PayRequest {
            payment: "lno1mockexternal".into(),
            amount_msat: Some(50_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tx.state, "PENDING");
    assert!(tx.ln_payment_id.is_none());
    sqlx::query("UPDATE transactions SET created_at = created_at - 601 WHERE id = $1")
        .bind(tx.id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.wallet.reconcile().await.unwrap();
    let adopted = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(adopted.state, "PENDING");
    let id = adopted.ln_payment_id.unwrap();
    t.node.complete(&id, true, 1_000);
    t.wallet.reconcile().await.unwrap();
    let settled = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(settled.state, "SETTLED");
    assert_eq!(settled.fee_reserve_msat, 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn bolt12_crash_before_id_persistence_recovers_from_backend() {
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let tx = t
        .wallet
        .pay(PayRequest {
            payment: "lno1mockexternal".into(),
            amount_msat: Some(80_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = tx.ln_payment_id.as_deref().unwrap().to_string();
    sqlx::query(
        "UPDATE transactions SET ln_payment_id = NULL, created_at = created_at - 601 WHERE id = $1",
    )
    .bind(tx.id)
    .execute(&t.pool)
    .await
    .unwrap();
    t.node.complete(&id, true, 2_000);
    let restarted =
        Wallet::new(t.pool.clone(), t.node.clone() as Arc<dyn LightningBackend>, fast());
    restarted.reconcile().await.unwrap();
    let recovered = restarted.transaction(&tx.public_id).await.unwrap();
    assert_eq!(recovered.state, "SETTLED");
    assert_eq!(recovered.ln_payment_id.as_deref(), Some(id.as_str()));
    assert_healthy(&t).await;
}

#[tokio::test]
async fn bolt12_multiple_matches_keep_reserve_and_raise_audit() {
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::SubmitThenTimeout;
    let tx = t
        .wallet
        .pay(PayRequest {
            payment: "lno1mockexternal".into(),
            amount_msat: Some(70_000),
            ..Default::default()
        })
        .await
        .unwrap();
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    t.node.bolt12_send("lno1mockexternal", Some(70_000), None, None).await.unwrap();
    sqlx::query("UPDATE transactions SET created_at = created_at - 601 WHERE id = $1")
        .bind(tx.id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.wallet.reconcile().await.unwrap();
    t.wallet.reconcile().await.unwrap();
    let unresolved = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(unresolved.state, "PENDING");
    assert!(unresolved.ln_payment_id.is_none());
    let alerts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE kind = 'bolt12_reconcile_ambiguous'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(alerts, 1);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn hold_invoice_lifecycle_and_ownership() {
    let t = setup().await;
    t.wallet.start();
    let owner = add_account(&t, "owner").await;
    let other = add_account(&t, "other").await;
    let preimage = "11".repeat(32);
    let hash = sha256_hex(&hex::decode(&preimage).unwrap());
    let tx = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { app_id: Some(owner), amount_msat: Some(30_000), ..Default::default() },
            &hash,
        )
        .await
        .unwrap();
    assert!(tx.hold);
    t.node.hold_htlc(&hash, 30_000, 500);
    for _ in 0..100 {
        let current = t.wallet.transaction(&tx.public_id).await.unwrap();
        if current.state == "ACCEPTED" && current.settle_deadline == Some(497) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let accepted = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(accepted.state, "ACCEPTED");
    assert_eq!(accepted.settle_deadline, Some(497), "claim deadline minus safety blocks");

    // Another app cannot settle or cancel it (SPEC §6.6.1).
    assert!(matches!(
        t.wallet.settle_hold(Some(other), &preimage).await,
        Err(WalletError::NotFound)
    ));
    assert!(matches!(t.wallet.cancel_hold(Some(other), &hash).await, Err(WalletError::NotFound)));

    let settled = t.wallet.settle_hold(Some(owner), &preimage).await.unwrap();
    assert_eq!(settled.state, "SETTLED");
    assert_eq!(t.node.claimed(), vec![(hash.clone(), preimage.clone())]);
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 30_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn underpaid_hold_invoice_is_failed_back() {
    let t = setup().await;
    t.wallet.start();
    let hash = sha256_hex(&[9; 32]);
    let tx = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { amount_msat: Some(30_000), ..Default::default() },
            &hash,
        )
        .await
        .unwrap();
    t.node.hold_htlc(&hash, 29_000, 500);
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().state == "CANCELED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let tx = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(tx.state, "CANCELED");
    assert_eq!(tx.failure_reason.as_deref(), Some("underpaid"));
    assert_eq!(t.node.failed(), vec![hash]);
}

#[tokio::test]
async fn self_payments_move_funds_internally() {
    let t = setup().await;
    let acct = add_account(&t, "alice").await;
    // Main pays an invoice created for the account: no network traffic, no fee.
    let inv = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(acct),
            amount_msat: Some(40_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let out = t
        .wallet
        .pay(PayRequest { payment: inv.payment_request.clone().unwrap(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(out.state, "SETTLED");
    assert!(out.self_payment);
    assert_eq!(out.fee_msat, 0);
    assert_eq!(t.wallet.transaction(&inv.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 40_000);
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000 - 40_000);

    // The account pays an offer owned by Main.
    let offer = t
        .wallet
        .create_offer(OfferRequest { description: "main".into(), ..Default::default() })
        .await
        .unwrap();
    let back = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: offer.offer,
            amount_msat: Some(15_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(back.self_payment);
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 25_000);

    // The account cannot spend more than it holds.
    let inv2 = t
        .wallet
        .create_invoice(InvoiceRequest { amount_msat: Some(30_000), ..Default::default() })
        .await
        .unwrap();
    let r = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: inv2.payment_request.unwrap(),
            ..Default::default()
        })
        .await;
    assert!(matches!(r, Err(WalletError::InsufficientBalance)));
    assert_healthy(&t).await;
}

#[tokio::test]
async fn internal_hold_invoice_settles_and_cancels() {
    let t = setup().await;
    let acct = add_account(&t, "escrow").await;
    let pre = "22".repeat(32);
    let hash = sha256_hex(&hex::decode(&pre).unwrap());
    let inv = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { app_id: Some(acct), amount_msat: Some(9_000), ..Default::default() },
            &hash,
        )
        .await
        .unwrap();
    let out = t
        .wallet
        .pay(PayRequest { payment: inv.payment_request.clone().unwrap(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(out.state, "PENDING");
    assert_eq!(t.wallet.transaction(&inv.public_id).await.unwrap().state, "ACCEPTED");
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 9_000);
    t.wallet.settle_hold(Some(acct), &pre).await.unwrap();
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 9_000);
    assert_eq!(t.wallet.transaction(&out.public_id).await.unwrap().state, "SETTLED");
    assert_healthy(&t).await;

    let hash2 = sha256_hex(&[3; 32]);
    let inv2 = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { app_id: Some(acct), amount_msat: Some(4_000), ..Default::default() },
            &hash2,
        )
        .await
        .unwrap();
    let out2 = t
        .wallet
        .pay(PayRequest { payment: inv2.payment_request.clone().unwrap(), ..Default::default() })
        .await
        .unwrap();
    t.wallet.cancel_hold(Some(acct), &hash2).await.unwrap();
    assert_eq!(t.wallet.transaction(&out2.public_id).await.unwrap().state, "FAILED");
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000 - 9_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn transfers_between_main_and_accounts() {
    let t = setup().await;
    let acct = add_account(&t, "bob").await;
    t.wallet.transfer(None, Some(acct), 500_000, "top up".into()).await.unwrap();
    t.wallet.transfer(Some(acct), None, 200_000, "withdraw".into()).await.unwrap();
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 300_000);
    assert!(matches!(
        t.wallet.transfer(Some(acct), None, 400_000, "x".into()).await,
        Err(WalletError::InsufficientBalance)
    ));
    let a = t.wallet.allocation().await.unwrap();
    assert_eq!(a.main_msat + a.accounts_total_msat, 1_000_000_000);
    assert_healthy(&t).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_payments_never_overspend() {
    // SPEC §16.1: 100 concurrent payments against a balance for exactly 10.
    let t = setup().await;
    let acct = add_account(&t, "spender").await;
    let each = 99_000_000u64; // reserve 990_000 -> 99_990_000 per payment
    t.wallet.transfer(None, Some(acct), 10 * 99_990_000, "fund".into()).await.unwrap();
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let mut handles = Vec::new();
    for n in 0..100u8 {
        let w = t.wallet.clone();
        handles.push(tokio::spawn(async move {
            w.pay(PayRequest {
                app_id: Some(acct),
                payment: external_invoice(each, n.wrapping_add(100)),
                ..Default::default()
            })
            .await
        }));
    }
    let mut ok = 0;
    let mut insufficient = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => ok += 1,
            Err(WalletError::InsufficientBalance) => insufficient += 1,
            Err(e) => panic!("unexpected error {e}"),
        }
    }
    assert_eq!((ok, insufficient), (10, 90));
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 0);
    assert_healthy(&t).await;
}

mod props {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Receive(u64),
        Pay(u64, bool),
        Transfer(bool, u64),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (1_000u64..5_000_000).prop_map(Op::Receive),
            ((1_000u64..3_000_000), any::<bool>()).prop_map(|(a, s)| Op::Pay(a, s)),
            (any::<bool>(), 1_000u64..3_000_000).prop_map(|(d, a)| Op::Transfer(d, a)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]
        #[test]
        fn random_operations_keep_the_ledger_consistent(ops in proptest::collection::vec(op(), 1..25)) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let t = setup().await;
                let acct = add_account(&t, "p").await;
                let mut seed = 0u8;
                for o in ops {
                    seed = seed.wrapping_add(1);
                    match o {
                        Op::Receive(a) => {
                            let inv = t.wallet.create_invoice(InvoiceRequest { app_id: Some(acct), amount_msat: Some(a), ..Default::default() }).await.unwrap();
                            let p = t.node.pay_invoice(inv.payment_hash.as_deref().unwrap(), a);
                            t.wallet.apply_inbound(&p).await.unwrap();
                        }
                        Op::Pay(a, succeed) => {
                            *t.node.send.lock().unwrap() = if succeed { SendBehaviour::Succeed { fee_msat: a / 200 } } else { SendBehaviour::Fail };
                            let _ = t.wallet.pay(PayRequest { app_id: Some(acct), payment: external_invoice(a, seed), ..Default::default() }).await;
                        }
                        Op::Transfer(to_acct, a) => {
                            let (f, to) = if to_acct { (None, Some(acct)) } else { (Some(acct), None) };
                            let _ = t.wallet.transfer(f, to, a, "p".into()).await;
                        }
                    }
                    let problems = t.wallet.ledger_check().await.unwrap();
                    assert!(problems.is_empty(), "{problems:?}");
                    assert!(t.wallet.balance(Some(acct)).await.unwrap() >= 0);
                }
            });
        }
    }
}

/// Regression: settling a BOLT12 payment fetches a payer proof, which must not wait for
/// the write lock that the settlement still holds (deadlock found in e2e 5).
#[tokio::test]
async fn bolt12_settlement_with_payer_proof_releases_the_lock() {
    let t = setup().await;
    t.wallet.start();
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let tx = t
        .wallet
        .pay(PayRequest {
            payment: "lno1mockexternal".into(),
            amount_msat: Some(20_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tx.state, "PENDING");
    t.node.complete(tx.ln_payment_id.as_deref().unwrap(), true, 0);
    let mut done = None;
    for _ in 0..100 {
        let cur = t.wallet.transaction(&tx.public_id).await.unwrap();
        if cur.state == "SETTLED" && cur.payer_proof.is_some() {
            done = Some(cur);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(done.is_some(), "settled with a payer proof");
    let next = tokio::time::timeout(
        Duration::from_secs(2),
        t.wallet.create_invoice(InvoiceRequest { amount_msat: Some(1_000), ..Default::default() }),
    )
    .await;
    assert!(matches!(next, Ok(Ok(_))), "wallet writes still work");
    assert_healthy(&t).await;
}

/// BOLT12 needs a description for offers with an amount; an empty one gets a default.
#[tokio::test]
async fn offer_with_amount_and_no_description_gets_a_default() {
    let t = setup().await;
    let offer = t
        .wallet
        .create_offer(OfferRequest { amount_msat: Some(5_000), ..Default::default() })
        .await
        .unwrap();
    assert!(!offer.description.clone().unwrap_or_default().trim().is_empty());
    let (uri, tx) = t
        .wallet
        .receive(InvoiceRequest { amount_msat: Some(5_000), ..Default::default() })
        .await
        .unwrap();
    assert!(uri.contains("lno=") && uri.contains("lightning="));
    assert!(tx.is_some());
}

/// JIT receive (SPEC §7.4): needs a configured LSP and an amount; the invoice settles
/// like any other BOLT11 payment when the LSP's channel delivers it.
#[tokio::test]
async fn jit_channel_invoice_needs_lsp_and_amount_then_settles() {
    let t = setup().await;
    t.wallet.start();
    // Without a configured LSP the request is rejected before reaching the node.
    let err = t
        .wallet
        .create_invoice(InvoiceRequest {
            jit: true,
            amount_msat: Some(5_000),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("LSPS2"), "{err}");
    // A JIT channel is sized for the payment, so the invoice needs an amount.
    t.node.jit_receive.store(true, std::sync::atomic::Ordering::Relaxed);
    let err = t
        .wallet
        .create_invoice(InvoiceRequest { jit: true, ..Default::default() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("amount"), "{err}");

    let tx = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: None,
            amount_msat: Some(5_000),
            jit: true,
            max_lsp_fee_msat: Some(2_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tx.state, "PENDING");
    assert_eq!(tx.amount_msat, 5_000);
    // The node was asked for a JIT invoice with the fee cap.
    let jits = t.node.jit_invoices();
    assert_eq!(jits.len(), 1);
    assert_eq!(jits[0], (tx.payment_hash.clone().unwrap(), Some(2_000)));
    // The payment arrives over the just-in-time channel like any other.
    t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 5_000);
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.transaction(&tx.public_id).await.unwrap().state, "SETTLED");
    assert_healthy(&t).await;
}

/// BIP-321 receive with `jit`: the invoice goes through the LSP, the offer stays normal.
#[tokio::test]
async fn receive_bip321_can_ask_for_a_jit_invoice() {
    let t = setup().await;
    t.node.jit_receive.store(true, std::sync::atomic::Ordering::Relaxed);
    let (uri, tx) = t
        .wallet
        .receive(InvoiceRequest {
            amount_msat: Some(5_000),
            jit: true,
            max_lsp_fee_msat: Some(1_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(uri.contains("lightning=") && uri.contains("lno="));
    let tx = tx.expect("with an amount receive returns an invoice");
    assert_eq!(t.node.jit_invoices(), vec![(tx.payment_hash.clone().unwrap(), Some(1_000))]);
}

async fn accepted_hold(t: &T, seed: u8) -> Tx {
    let hash = sha256_hex(&[seed; 32]);
    let tx = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest {
                amount_msat: Some(9_000),
                expiry_secs: Some(60),
                ..Default::default()
            },
            &hash,
        )
        .await
        .unwrap();
    t.node.hold_htlc(&hash, 9_000, 500);
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().state == "ACCEPTED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let tx = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(tx.state, "ACCEPTED");
    tx
}

/// Hub fund-loss note 3: a hold the node failed on its own is reported canceled, not
/// still accepted, whether we hear the event or only find it on reconcile.
#[tokio::test]
async fn hold_failed_by_the_node_is_not_reported_accepted() {
    let t = setup().await;
    t.wallet.start();
    let tx = accepted_hold(&t, 31).await;
    t.node.fail_inbound(tx.payment_hash.as_deref().unwrap(), true);
    let mut state = String::new();
    for _ in 0..100 {
        state = t.wallet.transaction(&tx.public_id).await.unwrap().state;
        if state != "ACCEPTED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(state, "CANCELED", "event path");

    let tx = accepted_hold(&t, 32).await;
    t.node.fail_inbound(tx.payment_hash.as_deref().unwrap(), false);
    t.wallet.reconcile().await.unwrap();
    let tx = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(tx.state, "CANCELED", "reconcile path");
    assert_eq!(tx.failure_reason.as_deref(), Some("canceled by the node"));
    assert_healthy(&t).await;
}

/// Hub fund-loss note 4: a still-settleable hold stays "accepted" after its bolt11
/// expiry; only unpaid (PENDING) invoices expire.
#[tokio::test]
async fn accepted_hold_does_not_expire_with_its_invoice() {
    let t = setup().await;
    t.wallet.start();
    let tx = accepted_hold(&t, 33).await;
    sqlx::query("UPDATE transactions SET expires_at = 1 WHERE id = $1")
        .bind(tx.id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.transaction(&tx.public_id).await.unwrap().state, "ACCEPTED");
}

#[tokio::test]
async fn reconcile_recovers_missed_claimable_even_after_invoice_expiry() {
    let t = setup().await; // No event subscriber: PaymentClaimable is lost.
    let preimage = "34".repeat(32);
    let hash = sha256_hex(&hex::decode(&preimage).unwrap());
    let tx = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { amount_msat: Some(9_000), ..Default::default() },
            &hash,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET expires_at = 1 WHERE id = $1")
        .bind(tx.id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.node.hold_htlc(&hash, 9_000, 500);

    t.wallet.reconcile().await.unwrap();
    let recovered = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(recovered.state, "ACCEPTED");
    assert_eq!(recovered.settle_deadline, None);
    assert_eq!(recovered.claimable_amount_msat, Some(9_000));
    t.wallet.settle_hold(None, &preimage).await.unwrap();
    assert_eq!(t.wallet.transaction(&tx.public_id).await.unwrap().state, "SETTLED");
    assert_healthy(&t).await;
}

#[tokio::test]
async fn cancel_unknown_outcome_keeps_hold_nonterminal_and_credits_late_success() {
    let t = setup().await;
    let owner = add_account(&t, "hold-owner").await;
    let preimage = "35".repeat(32);
    let hash = sha256_hex(&hex::decode(&preimage).unwrap());
    let tx = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { app_id: Some(owner), amount_msat: Some(9_000), ..Default::default() },
            &hash,
        )
        .await
        .unwrap();
    t.node.hold_htlc(&hash, 9_000, 500);
    *t.node.hold_fail_error.lock().unwrap() = Some(BackendError::Timeout);

    let cancel = t.wallet.cancel_hold(Some(owner), &hash).await.unwrap();
    assert_eq!(cancel.state, "CANCELING");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 0);

    t.node.bolt11_claim_for_id(&hash, 9_000, &preimage).await.unwrap();
    *t.node.lightning_sat.lock().unwrap() += 9;
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.transaction(&tx.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 9_000);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn cancel_before_htlc_retries_when_backend_later_sees_payment() {
    let t = setup().await;
    let hash = sha256_hex(&[36; 32]);
    let tx = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { amount_msat: Some(9_000), ..Default::default() },
            &hash,
        )
        .await
        .unwrap();
    assert_eq!(t.wallet.cancel_hold(None, &hash).await.unwrap().state, "CANCELING");
    *t.node.lightning_sat.lock().unwrap() += 10;
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_010_000);
    t.node.hold_htlc(&hash, 9_000, 500);
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.transaction(&tx.public_id).await.unwrap().state, "CANCELED");
    assert_eq!(t.node.failed(), vec![hash]);
    assert_healthy(&t).await;
}

/// A local LNURL-pay service for `name`, answering with mock invoices. `bad_amount`
/// makes it return an invoice for 1 msat more than asked.
async fn lnurl_service(bad_amount: bool) -> String {
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let cb = format!("http://{host}/cb");
    let app = Router::new()
        .route(
            "/.well-known/lnurlp/bob",
            get(move || {
                let cb = cb.clone();
                async move {
                    Json(serde_json::json!({"tag": "payRequest", "callback": cb, "minSendable": 1000,
                        "maxSendable": 5_000_000, "metadata": "[[\"text/plain\",\"Bob\"]]", "commentAllowed": 50}))
                }
            }),
        )
        .route(
            "/cb",
            get(move |Query(q): Query<HashMap<String, String>>| async move {
                let amount: u64 = q["amount"].parse::<u64>().unwrap() + u64::from(bad_amount);
                let hash = sha256_hex(q.get("comment").map(|c| c.as_bytes()).unwrap_or(b"none"));
                Json(serde_json::json!({"pr": MockBackend::make_invoice(Some(amount), &hash)}))
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("bob@{host}")
}

#[tokio::test]
async fn pays_a_lightning_address() {
    let t = setup().await;
    t.wallet.allow_http_lnurl();
    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let addr = lnurl_service(false).await;

    let d = t.wallet.decode(&addr).await.unwrap();
    assert_eq!(d.kind, "lnurl");
    assert_eq!(
        (d.min_msat, d.max_msat, d.comment_allowed),
        (Some(1000), Some(5_000_000), Some(50))
    );
    assert_eq!(d.description.as_deref(), Some("Bob"));

    let missing = t.wallet.pay(PayRequest { payment: addr.clone(), ..Default::default() }).await;
    assert!(matches!(missing, Err(WalletError::BadRequest(_))), "amount is required");
    let tx = t
        .wallet
        .pay(PayRequest {
            payment: addr.clone(),
            amount_msat: Some(21_000),
            payer_note: Some("for pizza".into()),
            metadata: Some(r#"{"recipient":{"pubkey":"bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22","name":"Bob"}}"#.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tx.state, "SETTLED");
    assert_eq!(tx.amount_msat, 21_000);
    let cp = tx.counterparty.clone().expect("counterparty");
    assert_eq!(
        (cp.name.as_deref(), cp.comment.as_deref(), cp.source),
        (Some("Bob"), Some("for pizza"), "contact")
    );
    assert_eq!(cp.identifier.as_deref(), Some(addr.as_str()));
    assert_eq!(
        tx.payment_hash.as_deref(),
        Some(sha256_hex(b"for pizza").as_str()),
        "comment reached the service"
    );
    // Out of range, and an invoice that does not match the request.
    let r = t
        .wallet
        .pay(PayRequest { payment: addr.clone(), amount_msat: Some(10), ..Default::default() })
        .await;
    assert!(matches!(r, Err(WalletError::BadRequest(_))));
    let bad = lnurl_service(true).await;
    let r = t
        .wallet
        .pay(PayRequest { payment: bad, amount_msat: Some(2_000), ..Default::default() })
        .await;
    assert!(
        matches!(r, Err(WalletError::BadRequest(ref m)) if m.contains("does not match")),
        "{r:?}"
    );
    assert_healthy(&t).await;
}
