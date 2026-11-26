//! Wallet tests against the mock backend (SPEC §16.1–16.2).

use std::sync::Arc;
use std::time::Duration;

use allod_ln::mock::{sha256_hex, BalanceGate, MockBackend, SendBehaviour};
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
async fn reconcile_discards_balance_snapshot_after_concurrent_settlement() {
    let t = setup().await;
    let gate = Arc::new(BalanceGate::default());
    *t.node.balance_gate.lock().unwrap() = Some(gate.clone());
    let wallet = t.wallet.clone();
    let reconcile = tokio::spawn(async move { wallet.reconcile().await });
    gate.entered.notified().await;

    let tx = t
        .wallet
        .pay(PayRequest { payment: external_invoice(100_000, 61), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(tx.state, "SETTLED");
    *t.node.lightning_sat.lock().unwrap() = 999_899;
    gate.release.notify_one();
    reconcile.await.unwrap().unwrap();

    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000 - 101_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn reconcile_snapshots_balance_before_scanning_inbound_payments() {
    let t = setup().await;
    let account = add_account(&t, "race-receive").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(account),
            amount_msat: Some(100_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let gate = Arc::new(BalanceGate::default());
    *t.node.balance_before_snapshot_gate.lock().unwrap() = Some(gate.clone());
    let wallet = t.wallet.clone();
    let reconcile = tokio::spawn(async move { wallet.reconcile().await });
    gate.entered.notified().await;

    // Receipt settles after inbound scan but before old reconciliation took its balance
    // snapshot. Deliver its queued event only after reconciliation finishes.
    let payment = t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 100_000);
    *t.node.lightning_sat.lock().unwrap() = 1_000_100;
    gate.release.notify_one();
    reconcile.await.unwrap().unwrap();

    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_eq!(t.wallet.balance(Some(account)).await.unwrap(), 100_000);
    // Delayed event is idempotent after later inbound scan recovered receipt.
    t.wallet.apply_inbound(&payment).await.unwrap();

    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_eq!(t.wallet.balance(Some(account)).await.unwrap(), 100_000);
    let allocation = t.wallet.allocation().await.unwrap();
    assert_eq!(allocation.main_msat + allocation.accounts_total_msat, 1_000_100_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn reconcile_exhausts_payment_history_before_booking_balance_to_main() {
    let t = setup().await;
    let account = add_account(&t, "deep-receive").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(account),
            amount_msat: Some(100_000),
            ..Default::default()
        })
        .await
        .unwrap();
    t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 100_000);
    for n in 0..51_u64 {
        let id = format!("history-{n:02}");
        t.node.add_history_payment(LnPayment {
            id: id.clone(),
            kind: LnPaymentKind::Bolt11 { payment_hash: id, preimage: None, payment_secret: None },
            direction: Direction::Outbound,
            status: LnStatus::Failed,
            amount_msat: Some(1),
            fee_msat: None,
            updated_at: 100 + n,
        });
    }
    *t.node.payment_page_size.lock().unwrap() = Some(1);
    *t.node.lightning_sat.lock().unwrap() = 1_000_100;

    t.wallet.reconcile().await.unwrap();

    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_eq!(t.wallet.balance(Some(account)).await.unwrap(), 100_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn reconcile_rejects_repeated_payment_page_token_before_main_adjustment() {
    let t = setup().await;
    let account = add_account(&t, "repeated-page").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(account),
            amount_msat: Some(100_000),
            ..Default::default()
        })
        .await
        .unwrap();
    t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 100_000);
    t.node.add_history_payment(LnPayment {
        id: "newer-history-entry".into(),
        kind: LnPaymentKind::Bolt11 {
            payment_hash: "newer-history-entry".into(),
            preimage: None,
            payment_secret: None,
        },
        direction: Direction::Outbound,
        status: LnStatus::Failed,
        amount_msat: Some(1),
        fee_msat: None,
        updated_at: 100,
    });
    *t.node.payment_page_size.lock().unwrap() = Some(1);
    t.node.repeat_payment_page_token.store(true, std::sync::atomic::Ordering::Relaxed);
    *t.node.lightning_sat.lock().unwrap() = 1_000_100;

    let error = t.wallet.reconcile().await.unwrap_err();

    assert!(error.to_string().contains("pagination repeated a token"), "{error}");
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_eq!(t.wallet.balance(Some(account)).await.unwrap(), 0);
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
async fn max_fee_overflow_is_rejected_before_any_ledger_write() {
    let t = setup().await;
    let acct = add_account(&t, "c1-max-fee").await;
    let before = t.wallet.allocation().await.unwrap();
    let r = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: external_invoice(1_000, 90),
            max_fee_msat: Some(u64::MAX),
            ..Default::default()
        })
        .await;
    assert!(matches!(r, Err(WalletError::BadRequest(_))), "{r:?}");
    // No transaction row or ledger movement was created, and the balance was not minted.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions").fetch_one(&t.pool).await.unwrap();
    assert_eq!(count, 0);
    assert_eq!(before, t.wallet.allocation().await.unwrap());
    assert_healthy(&t).await;
}

#[tokio::test]
async fn amount_above_i64_range_is_rejected() {
    let t = setup().await;
    let inv = MockBackend::make_invoice(None, &sha256_hex(&[91; 32]));
    let r = t
        .wallet
        .pay(PayRequest { payment: inv, amount_msat: Some(u64::MAX), ..Default::default() })
        .await;
    assert!(matches!(r, Err(WalletError::BadRequest(_))), "{r:?}");
    assert_healthy(&t).await;
}

#[tokio::test]
async fn amount_plus_fee_reserve_overflow_is_rejected() {
    let t = setup().await;
    // amount and reserve are each representable as i64, but their sum overflows.
    let inv = MockBackend::make_invoice(None, &sha256_hex(&[92; 32]));
    let r = t
        .wallet
        .pay(PayRequest {
            payment: inv,
            amount_msat: Some(i64::MAX as u64),
            max_fee_msat: Some(i64::MAX as u64),
            ..Default::default()
        })
        .await;
    assert!(matches!(r, Err(WalletError::BadRequest(_))), "{r:?}");
    assert_healthy(&t).await;
}

#[tokio::test]
async fn fee_reserve_is_hard_backend_cap_for_bolt11_and_bolt12() {
    let t = setup().await;
    let acct = add_account(&t, "fee-capped").await;
    t.wallet.transfer(None, Some(acct), 220_000, "fund".into()).await.unwrap();
    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 11_000 };
    let failed = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: external_invoice(100_000, 62),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(failed.state, "FAILED");
    assert_eq!(*t.node.last_max_fee.lock().unwrap(), Some(10_000));
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 220_000);

    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 1_000 };
    let settled = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: "lno1mockexternal".into(),
            amount_msat: Some(100_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(settled.state, "SETTLED");
    assert_eq!(*t.node.last_max_fee.lock().unwrap(), Some(10_000));
    assert_healthy(&t).await;
}

#[tokio::test]
async fn backend_over_cap_success_is_quarantined_without_overdraft() {
    let t = setup().await;
    let acct = add_account(&t, "over-cap").await;
    t.wallet.transfer(None, Some(acct), 110_000, "fund".into()).await.unwrap();
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let pending = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: external_invoice(100_000, 63),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(pending.state, "PENDING");
    t.node.complete(pending.ln_payment_id.as_deref().unwrap(), true, 20_000);
    assert!(matches!(t.wallet.reconcile().await, Err(WalletError::Internal(_))));
    assert_eq!(t.wallet.transaction(&pending.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 0);
    let alerts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE kind = 'fee_cap_violation'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(alerts, 1);
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
async fn bolt11_retry_ignores_stale_failure_and_keeps_success_debited() {
    let t = setup().await;
    *t.node.outgoing_timestamp.lock().unwrap() = Some(93);
    let app_id: i64 = sqlx::query_scalar(
        "INSERT INTO apps (name, kind, app_pubkey, max_amount_msat, budget_renewal, created_at, updated_at) \
         VALUES ('retry', 'connection', 'retry-pk', 50000, 'monthly', 0, 0) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let invoice = external_invoice(50_000, 93);
    let request = PayRequest {
        app_id: Some(app_id),
        payment: invoice.clone(),
        max_fee_msat: Some(0),
        ..Default::default()
    };

    *t.node.send.lock().unwrap() = SendBehaviour::Fail;
    let first = t.wallet.pay(request.clone()).await.unwrap();
    assert_eq!(first.state, "FAILED");
    let stale = t.node.payment(first.payment_hash.as_deref().unwrap()).await.unwrap().unwrap();
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 0);

    t.wallet.start();
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let gate = Arc::new(BalanceGate::default());
    *t.node.bolt11_send_gate.lock().unwrap() = Some(gate.clone());
    let wallet = t.wallet.clone();
    let retry = tokio::spawn(async move { wallet.pay(request).await });
    gate.entered.notified().await;

    // Retry row exists, but send has not replaced old backend FAILED record.
    t.wallet.reconcile().await.unwrap();
    t.node.emit(LnEvent::PaymentFailed {
        payment: stale.clone(),
        reason: Some("late failure from attempt 1".into()),
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let pending = {
        let mut c = t.pool.acquire().await.unwrap();
        sqlx::query_as::<_, Tx>(&format!(
            "SELECT {} FROM transactions WHERE payment_request = $1 ORDER BY id DESC LIMIT 1",
            store::TX_COLUMNS
        ))
        .bind(&invoice)
        .fetch_one(&mut *c)
        .await
        .unwrap()
    };
    assert_eq!(pending.state, "PENDING");
    assert_eq!(pending.attempt_number, 2);
    assert!(pending.attempt_submitted_at.is_none());
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 50_000);

    gate.release.notify_one();
    let retry = retry.await.unwrap().unwrap();
    assert_eq!(retry.state, "PENDING");
    t.node.emit(LnEvent::PaymentFailed {
        payment: stale,
        reason: Some("delayed failure from attempt 1".into()),
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(t.wallet.transaction(&retry.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 50_000);
    *t.node.lightning_sat.lock().unwrap() = 999_950;
    t.node.complete(retry.payment_hash.as_deref().unwrap(), true, 0);
    for _ in 0..100 {
        if t.wallet.transaction(&retry.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.transaction(&retry.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 50_000);
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000 - 50_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn bolt11_retry_failure_in_same_second_releases_current_reserve() {
    let t = setup().await;
    *t.node.outgoing_timestamp.lock().unwrap() = Some(42);
    *t.node.send.lock().unwrap() = SendBehaviour::Fail;
    let request = PayRequest {
        payment: external_invoice(50_000, 94),
        max_fee_msat: Some(0),
        ..Default::default()
    };
    let first = t.wallet.pay(request.clone()).await.unwrap();
    assert_eq!(first.state, "FAILED");

    // Backend replaces attempt one with attempt two but keeps second-resolution timestamp.
    let retry = t.wallet.pay(request).await.unwrap();
    assert_eq!(retry.attempt_number, 2);
    assert_eq!(retry.state, "FAILED");
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 0);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn ambiguous_same_second_retry_requires_backend_confirmed_manual_resolution() {
    let t = setup().await;
    *t.node.outgoing_timestamp.lock().unwrap() = Some(43);
    *t.node.send.lock().unwrap() = SendBehaviour::Fail;
    let request = PayRequest {
        payment: external_invoice(50_000, 95),
        max_fee_msat: Some(0),
        ..Default::default()
    };
    assert_eq!(t.wallet.pay(request.clone()).await.unwrap().state, "FAILED");

    *t.node.send.lock().unwrap() = SendBehaviour::SubmitThenTimeout;
    let retry = t.wallet.pay(request).await.unwrap();
    assert_eq!(retry.state, "PENDING");
    assert_eq!(retry.attempt_state, "ambiguous");
    let hash = retry.payment_hash.as_deref().unwrap();
    t.node.complete(hash, false, 0);
    sqlx::query("UPDATE transactions SET created_at = created_at - 601 WHERE id = $1")
        .bind(retry.id)
        .execute(&t.pool)
        .await
        .unwrap();

    t.wallet.reconcile().await.unwrap();
    let review = t.wallet.transaction(&retry.public_id).await.unwrap();
    assert_eq!(review.state, "PENDING");
    assert_eq!(review.attempt_state, "review");
    assert!(review.failure_reason.as_deref().unwrap_or_default().contains("owner review"));
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 50_000);

    let resolved = t.wallet.resolve_ambiguous_bolt11_failure(&retry.public_id).await.unwrap();
    assert_eq!(resolved.state, "FAILED");
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 0);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    let alerts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE kind = 'bolt11_attempt_ambiguous'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(alerts, 1);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn proven_success_repairs_previously_released_payment() {
    let t = setup().await;
    let app_id: i64 = sqlx::query_scalar(
        "INSERT INTO apps (name, kind, app_pubkey, max_amount_msat, budget_renewal, created_at, updated_at) \
         VALUES ('repair', 'connection', 'repair-pk', 60000, 'monthly', 0, 0) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    *t.node.send.lock().unwrap() = SendBehaviour::Fail;
    let failed = t
        .wallet
        .pay(PayRequest {
            app_id: Some(app_id),
            payment: external_invoice(60_000, 94),
            max_fee_msat: Some(0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(failed.state, "FAILED");
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 0);

    t.wallet.start();
    *t.node.lightning_sat.lock().unwrap() = 999_940;
    t.node.complete(failed.payment_hash.as_deref().unwrap(), true, 0);
    for _ in 0..100 {
        if t.wallet.transaction(&failed.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.transaction(&failed.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 60_000);
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000 - 60_000);
    assert_healthy(&t).await;
}

/// A reconcile that runs while a sent payment still looks failed books the node's lower
/// balance to Main. When the payment is later proven settled, the payer is debited and
/// that loss goes back to Main, so the payment is counted once (it used to be twice).
#[tokio::test]
async fn late_success_after_reconcile_is_not_counted_twice() {
    let t = setup().await;
    let payer = add_account(&t, "late-success").await;
    t.wallet.transfer(None, Some(payer), 100_000, "fund".into()).await.unwrap();
    *t.node.send.lock().unwrap() = SendBehaviour::Fail;
    let failed = t
        .wallet
        .pay(PayRequest {
            app_id: Some(payer),
            payment: external_invoice(60_000, 95),
            max_fee_msat: Some(0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(failed.state, "FAILED");
    assert_eq!(t.wallet.balance(Some(payer)).await.unwrap(), 100_000);

    // The payment did leave the node; the reconcile sees only a lower balance.
    *t.node.lightning_sat.lock().unwrap() = 999_940;
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000 - 100_000 - 60_000);

    // Then the backend proves it settled.
    t.wallet.start();
    t.node.complete(failed.payment_hash.as_deref().unwrap(), true, 0);
    for _ in 0..100 {
        if t.wallet.transaction(&failed.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.transaction(&failed.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.balance(Some(payer)).await.unwrap(), 100_000 - 60_000);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000 - 100_000);
    assert_healthy(&t).await;

    // Books and node agree again: a later reconcile changes nothing, and a replayed
    // success event does not return the loss a second time.
    t.wallet.reconcile().await.unwrap();
    t.node.complete(failed.payment_hash.as_deref().unwrap(), true, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000 - 100_000);
    assert_eq!(t.wallet.balance(Some(payer)).await.unwrap(), 100_000 - 60_000);
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
async fn bolt12_unknown_submission_stays_reserved_until_late_backend_success() {
    use std::sync::atomic::Ordering;

    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::SubmitThenTimeout;
    t.node.hide_list_payments.store(true, Ordering::Relaxed);
    let app_id: i64 = sqlx::query_scalar(
        "INSERT INTO apps (name, kind, app_pubkey, max_amount_msat, budget_renewal, \
         created_at, updated_at) VALUES ('late-bolt12', 'connection', 'late-bolt12-pk', \
         100000, 'never', 0, 0) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let before = t.wallet.allocation().await.unwrap();
    let tx = t
        .wallet
        .pay(PayRequest {
            app_id: Some(app_id),
            payment: "lno1mockexternal".into(),
            amount_msat: Some(60_000),
            max_fee_msat: Some(2_000),
            ..Default::default()
        })
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET created_at = created_at - 601 WHERE id = $1")
        .bind(tx.id)
        .execute(&t.pool)
        .await
        .unwrap();

    t.wallet.reconcile().await.unwrap();
    let unresolved = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(unresolved.state, "PENDING");
    assert!(unresolved.ln_payment_id.is_none());
    let held = t.wallet.allocation().await.unwrap();
    assert_eq!(held.main_msat, before.main_msat - 62_000);
    assert_eq!(held.pending_out_msat, before.pending_out_msat + 62_000);
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 62_000);

    t.node.hide_list_payments.store(false, Ordering::Relaxed);
    t.wallet.reconcile().await.unwrap();
    let adopted = t.wallet.transaction(&tx.public_id).await.unwrap();
    let id = adopted.ln_payment_id.expect("late backend payment adopted");
    t.node.complete(&id, true, 1_000);
    *t.node.lightning_sat.lock().unwrap() = 999_939;
    t.wallet.reconcile().await.unwrap();
    let settled = t.wallet.transaction(&tx.public_id).await.unwrap();
    assert_eq!(settled.state, "SETTLED");
    assert_eq!(settled.fee_msat, 1_000);
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, before.main_msat - 61_000);
    assert_eq!(t.wallet.budget(app_id).await.unwrap().unwrap().used_budget, 61_000);
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
    let held = t.wallet.held_invoices().await.unwrap();
    assert_eq!((held.count, held.msat, held.next_deadline), (1, 30_000, Some(497)));

    // Another app cannot settle or cancel it (SPEC §6.6.1).
    assert!(matches!(
        t.wallet.settle_hold(Some(other), &preimage).await,
        Err(WalletError::NotFound)
    ));
    assert!(matches!(t.wallet.cancel_hold(Some(other), &hash).await, Err(WalletError::NotFound)));

    let settled = t.wallet.settle_hold(Some(owner), &preimage).await.unwrap();
    assert_eq!(settled.state, "SETTLED");
    assert_eq!(t.wallet.held_invoices().await.unwrap().count, 0);
    let activity = t.wallet.activity(0).await.unwrap();
    assert_eq!(activity.received, Tally { count: 1, msat: 30_000, fee_msat: 0 });
    assert_eq!((activity.sent.count, activity.failed_sends), (0, 0));
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
async fn expired_internal_hold_releases_payer_funds_and_budget_after_grace() {
    let t = setup().await;
    let payer = add_account(&t, "hold-payer").await;
    let receiver = add_account(&t, "hold-receiver").await;
    sqlx::query("UPDATE apps SET max_amount_msat = 9000, budget_renewal = 'never' WHERE id = $1")
        .bind(payer)
        .execute(&t.pool)
        .await
        .unwrap();
    t.wallet.transfer(None, Some(payer), 9_000, "fund payer".into()).await.unwrap();

    let hash = sha256_hex(&[43; 32]);
    let incoming = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest {
                app_id: Some(receiver),
                amount_msat: Some(9_000),
                expiry_secs: Some(60),
                ..Default::default()
            },
            &hash,
        )
        .await
        .unwrap();
    let outgoing = t
        .wallet
        .pay(PayRequest {
            app_id: Some(payer),
            payment: incoming.payment_request.clone().unwrap(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(t.wallet.balance(Some(payer)).await.unwrap(), 0);
    assert_eq!(t.wallet.budget(payer).await.unwrap().unwrap().used_budget, 9_000);

    // Invoice expired, but safety grace still protects concurrent manual settlement.
    sqlx::query("UPDATE transactions SET expires_at = $1 WHERE id = $2")
        .bind(now() - INTERNAL_HOLD_EXPIRY_GRACE_SECS + 1)
        .bind(incoming.id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.transaction(&incoming.public_id).await.unwrap().state, "ACCEPTED");
    assert_eq!(t.wallet.transaction(&outgoing.public_id).await.unwrap().state, "PENDING");

    sqlx::query("UPDATE transactions SET expires_at = $1 WHERE id = $2")
        .bind(now() - INTERNAL_HOLD_EXPIRY_GRACE_SECS - 1)
        .bind(incoming.id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.wallet.reconcile().await.unwrap();

    let incoming = t.wallet.transaction(&incoming.public_id).await.unwrap();
    let outgoing = t.wallet.transaction(&outgoing.public_id).await.unwrap();
    assert_eq!(incoming.state, "CANCELED");
    assert_eq!(incoming.failure_reason.as_deref(), Some("internal hold expired"));
    assert_eq!(outgoing.state, "FAILED");
    assert_eq!(outgoing.failure_reason.as_deref(), Some("internal hold expired"));
    assert_eq!(t.wallet.balance(Some(payer)).await.unwrap(), 9_000);
    assert_eq!(t.wallet.budget(payer).await.unwrap().unwrap().used_budget, 0);
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 0);
    assert_healthy(&t).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn internal_hold_expiry_race_with_settlement_has_one_terminal_outcome() {
    let t = setup().await;
    let receiver = add_account(&t, "hold-race-receiver").await;
    let preimage = "44".repeat(32);
    let hash = sha256_hex(&hex::decode(&preimage).unwrap());
    let incoming = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest {
                app_id: Some(receiver),
                amount_msat: Some(9_000),
                ..Default::default()
            },
            &hash,
        )
        .await
        .unwrap();
    let outgoing = t
        .wallet
        .pay(PayRequest {
            payment: incoming.payment_request.clone().unwrap(),
            ..Default::default()
        })
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET expires_at = $1 WHERE id = $2")
        .bind(now() - INTERNAL_HOLD_EXPIRY_GRACE_SECS - 1)
        .bind(incoming.id)
        .execute(&t.pool)
        .await
        .unwrap();

    let (reconcile, settle) =
        tokio::join!(t.wallet.reconcile(), t.wallet.settle_hold(Some(receiver), &preimage));
    reconcile.unwrap();
    assert!(settle.is_ok() || matches!(settle, Err(WalletError::BadRequest(_))));

    let incoming = t.wallet.transaction(&incoming.public_id).await.unwrap();
    let outgoing = t.wallet.transaction(&outgoing.public_id).await.unwrap();
    match incoming.state.as_str() {
        "SETTLED" => {
            assert_eq!(outgoing.state, "SETTLED");
            assert_eq!(t.wallet.balance(Some(receiver)).await.unwrap(), 9_000);
            assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000 - 9_000);
        }
        "CANCELED" => {
            assert_eq!(outgoing.state, "FAILED");
            assert_eq!(t.wallet.balance(Some(receiver)).await.unwrap(), 0);
            assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
        }
        state => panic!("unexpected incoming state {state}"),
    }
    assert_eq!(t.wallet.allocation().await.unwrap().pending_out_msat, 0);
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
            amount_msat: Some(5_000_000),
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
    // Too small for a channel that could carry it (the LSP's reserve and fees): refused.
    let err = t
        .wallet
        .create_invoice(InvoiceRequest {
            jit: true,
            amount_msat: Some(2_000_000),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("5,000 sats"), "{err}");

    let tx = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: None,
            amount_msat: Some(5_000_000),
            jit: true,
            max_lsp_fee_msat: Some(2_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tx.state, "PENDING");
    assert_eq!(tx.amount_msat, 5_000_000);
    // The node was asked for a JIT invoice with the fee cap.
    let jits = t.node.jit_invoices();
    assert_eq!(jits.len(), 1);
    assert_eq!(jits[0], (tx.payment_hash.clone().unwrap(), Some(2_000)));
    // The payment arrives over the just-in-time channel like any other.
    t.node.pay_invoice(tx.payment_hash.as_deref().unwrap(), 5_000_000);
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.transaction(&tx.public_id).await.unwrap().state, "SETTLED");
    assert_healthy(&t).await;
}

/// LSPS2 opening fee belongs to receiving account: invoice is gross, backend receipt is net.
#[tokio::test]
async fn jit_opening_fee_credits_receiving_account_only_net_amount() {
    let t = setup().await;
    t.wallet.start();
    t.node.jit_receive.store(true, std::sync::atomic::Ordering::Relaxed);
    let account = add_account(&t, "jit-net").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(account),
            amount_msat: Some(5_000_000),
            jit: true,
            max_lsp_fee_msat: Some(1_000_000),
            ..Default::default()
        })
        .await
        .unwrap();

    // LSP takes 1,000 sats from a 5,000-sat invoice. Node liquidity rises by exactly
    // 4,000 sats; Main must not fund missing amount during reconciliation.
    t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 4_000_000);
    *t.node.lightning_sat.lock().unwrap() = 1_004_000;
    for _ in 0..100 {
        if t.wallet.transaction(&invoice.public_id).await.unwrap().state == "SETTLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let settled = t.wallet.transaction(&invoice.public_id).await.unwrap();
    assert_eq!(settled.amount_msat, 4_000_000);
    assert_eq!(settled.fee_msat, 1_000_000);
    assert_eq!(t.wallet.balance(Some(account)).await.unwrap(), 4_000_000);
    let recorded_fee: i64 = sqlx::query_scalar(
        "SELECT b.balance_msat FROM ledger_balances b \
         JOIN ledger_accounts a ON a.id = b.account_id WHERE a.code = 'fees:routing'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(recorded_fee, 1_000_000);
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.balance(Some(account)).await.unwrap(), 4_000_000);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_healthy(&t).await;
}

/// bLIP-52 §3: a JIT invoice must not outlive the LSPS2 offer the LSP honoured (ldk-node
/// selects the cheapest one, valid for about an hour). A longer request is clamped.
#[tokio::test]
async fn jit_invoice_expiry_is_capped_to_the_lsp_offer() {
    let t = setup().await;
    t.node.jit_receive.store(true, std::sync::atomic::Ordering::Relaxed);
    let inv = |expiry_secs| InvoiceRequest {
        amount_msat: Some(5_000_000),
        expiry_secs,
        jit: true,
        ..Default::default()
    };
    // The default (24 h) is too long for a JIT invoice.
    t.wallet.create_invoice(inv(None)).await.unwrap();
    assert_eq!(t.node.last_receive_expiry(), Some(3_600));
    // So is an explicit longer one.
    t.wallet.create_invoice(inv(Some(86_400))).await.unwrap();
    assert_eq!(t.node.last_receive_expiry(), Some(3_600));
    // A shorter one is kept as asked.
    t.wallet.create_invoice(inv(Some(600))).await.unwrap();
    assert_eq!(t.node.last_receive_expiry(), Some(600));
    // Regular invoices keep their long default.
    t.wallet
        .create_invoice(InvoiceRequest { amount_msat: Some(5_000_000), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(t.node.last_receive_expiry(), Some(86_400));
}

/// When the LSP aborts the JIT channel opening, the invoice stays payable but records why
/// the last attempt did not arrive (SPEC §7.4): the receive screen can then say so instead
/// of waiting forever.
#[tokio::test]
async fn jit_channel_open_failure_is_recorded_on_the_invoice() {
    let t = setup().await;
    t.wallet.start();
    t.node.jit_receive.store(true, std::sync::atomic::Ordering::Relaxed);
    let tx = t
        .wallet
        .create_invoice(InvoiceRequest {
            amount_msat: Some(5_000_000),
            jit: true,
            ..Default::default()
        })
        .await
        .unwrap();
    // An unrelated pending invoice must not be blamed.
    let other = t
        .wallet
        .create_invoice(InvoiceRequest {
            amount_msat: Some(7_000),
            description: "not jit".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    t.node.emit(LnEvent::ChannelStateChanged {
        channel_id: "11".repeat(32),
        counterparty_node_id: Some("02".to_string() + &"33".repeat(32)),
        state: "OpenFailed".into(),
        reason: Some(
            "Channel closed because the peer disconnected prior to the channel being funded".into(),
        ),
        user_channel_id: None,
        funding_txid: None,
    });
    for _ in 0..100 {
        if t.wallet.transaction(&tx.public_id).await.unwrap().failure_reason.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let after = t.wallet.transaction(&tx.public_id).await.unwrap();
    // The invoice is still payable: only the attempt is annotated.
    assert_eq!(after.state, "PENDING");
    assert!(after.jit);
    assert_eq!(
        after.failure_reason.as_deref(),
        Some("Channel closed because the peer disconnected prior to the channel being funded")
    );
    assert!(t.wallet.transaction(&other.public_id).await.unwrap().failure_reason.is_none());
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
            amount_msat: Some(5_000_000),
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
                        "maxSendable": 5_000_000, "metadata": "[[\"text/plain\",\"Bob\"]]", "commentAllowed": 50,
                        "allowsNostr": true}))
                }
            }),
        )
        .route(
            "/cb",
            get(move |Query(q): Query<HashMap<String, String>>| async move {
                let amount: u64 = q["amount"].parse::<u64>().unwrap() + u64::from(bad_amount);
                let said = q.get("nostr").or_else(|| q.get("comment"));
                let hash = sha256_hex(said.map(|c| c.as_bytes()).unwrap_or(b"none"));
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

/// Zaps: the signed event is checked against the payment, a NIP-57 request goes to the
/// LNURL callback, and a BOLT12 intent is bound to the payment by its payer note.
#[tokio::test]
async fn pays_zaps() {
    use allod_lnurl::zap;
    use nostr::prelude::*;
    let t = setup().await;
    t.wallet.allow_http_lnurl();
    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let acct = add_account(&t, "zapper").await;
    t.wallet.transfer(None, Some(acct), 100_000, "fund".into()).await.unwrap();
    let (me, them) = (Keys::generate(), Keys::generate());
    let pk = them.public_key().to_hex();
    let relays = vec!["wss://relay.example".to_string()];
    let pay = |payment: &str, amount| PayRequest {
        app_id: Some(acct),
        payment: payment.into(),
        amount_msat: Some(amount),
        ..Default::default()
    };

    // NIP-57.
    let addr = lnurl_service(false).await;
    let request =
        zap::request_template(&pk, 21_000, "gm", &relays, false, 1).sign(&me).unwrap().as_json();
    let wrong = t.wallet.pay_zap(pay(&addr, 22_000), &request, &relays).await;
    assert!(matches!(wrong, Err(WalletError::BadRequest(_))), "{wrong:?}");
    let tx = t.wallet.pay_zap(pay(&addr, 21_000), &request, &relays).await.unwrap();
    assert_eq!(tx.state, "SETTLED");
    assert_eq!(tx.payment_hash.as_deref(), Some(sha256_hex(request.as_bytes()).as_str()));
    let cp = tx.counterparty.expect("counterparty");
    assert_eq!(
        (cp.pubkey, cp.source, cp.comment.as_deref()),
        (Some(pk.clone()), "zap", Some("gm"))
    );

    // NIP-B1.
    let offer = "lno1mockexternal";
    let announcement = zap::Template {
        kind: zap::BOLT12_OFFERS_KIND,
        content: String::new(),
        tags: vec![vec!["offer".into(), offer.into()]],
        created_at: 1,
    }
    .sign(&them)
    .unwrap()
    .as_json();
    let intent = zap::intent_template(&pk, 30_000, "yo", &announcement, &"ab".repeat(16), 2)
        .sign(&me)
        .unwrap();
    let raw = intent.as_json();
    let other = t.wallet.pay_zap(pay("lno1mockother", 30_000), &raw, &relays).await;
    assert!(matches!(other, Err(WalletError::BadRequest(_))), "{other:?}");
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 79_000);
    let tx = t.wallet.pay_zap(pay(offer, 30_000), &raw, &relays).await.unwrap();
    assert_eq!(tx.state, "SETTLED");
    assert_eq!(tx.payer_note, Some(zap::payer_note(&intent)));
    let cp = tx.counterparty.clone().expect("counterparty");
    assert_eq!((cp.pubkey, cp.source, cp.comment.as_deref()), (Some(pk), "zap", Some("yo")));
    // The invoice comes with the node's event, which may trail the poll that settled it.
    let payment = t.node.payment(tx.ln_payment_id.as_deref().unwrap()).await.unwrap().unwrap();
    t.wallet.apply_outgoing_success(&payment, Some("lni1mock".into()), None).await.unwrap();
    assert!(t.wallet.ensure_payer_proof(&tx.public_id).await.unwrap().is_some());
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 49_000);
    assert_healthy(&t).await;
}

/// BIP-353 names resolve to their (verified) record and pay the offer in it; names
/// without a record fall back to LNURL; bad records never pay.
#[tokio::test]
async fn pays_a_bip353_name() {
    use super::bip353::Bip353Resolver;
    use std::collections::HashMap;
    let t = setup().await;
    *t.node.send.lock().unwrap() = SendBehaviour::Succeed { fee_msat: 0 };
    let acct = add_account(&t, "payer").await;
    t.wallet.transfer(None, Some(acct), 100_000, "fund".into()).await.unwrap();
    let rec = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([
        ("alice@example.com".to_string(), rec(&["v=spf1 -all", "bitcoin:?lno=lno1mockexternal"])),
        ("two@example.com".to_string(), rec(&["bitcoin:?lno=lno1a", "bitcoin:?lno=lno1b"])),
        ("chain@example.com".to_string(), rec(&["bitcoin:bcrt1qexample"])),
    ]))));

    let d = t.wallet.decode("₿Alice@example.com").await.unwrap();
    assert_eq!((d.kind, d.bip353.as_deref()), ("bolt12", Some("alice@example.com")));

    let missing =
        t.wallet.pay(PayRequest { payment: "alice@example.com".into(), ..Default::default() });
    assert!(matches!(missing.await, Err(WalletError::BadRequest(_))), "amount is required");
    let tx = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: "alice@example.com".into(),
            amount_msat: Some(21_000),
            payer_note: Some("hi".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!((tx.state.as_str(), tx.payment_type.as_str()), ("SETTLED", "bolt12"));
    assert_eq!(tx.payer_note.as_deref(), Some("hi"));
    let cp = tx.counterparty.clone().expect("counterparty");
    assert_eq!((cp.identifier.as_deref(), cp.source), (Some("alice@example.com"), "bip353"));
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 79_000);

    for (name, why) in [
        ("two@example.com", "more than one"),
        ("chain@example.com", "on-chain"),
        ("₿nobody@example.com", "no BIP-353"),
    ] {
        let r = t
            .wallet
            .pay(PayRequest {
                app_id: Some(acct),
                payment: name.into(),
                amount_msat: Some(1_000),
                ..Default::default()
            })
            .await;
        assert!(
            matches!(r, Err(WalletError::BadRequest(ref m)) if m.contains(why)),
            "{name}: {r:?}"
        );
    }
    // No record and no `₿`: it is an LNURL address (none here, so LNURL fails).
    let r = t.wallet.decode("nobody@example.invalid").await;
    assert!(r.is_err());
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 79_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn closing_accounts() {
    // SPEC §10: close only when empty or with a sweep, never with payments in flight;
    // history stays attached; late payments to a closed account stay withdrawable.
    let t = setup().await;
    t.wallet.start();
    let acct = add_account(&t, "closing").await;
    let offer = t
        .wallet
        .create_offer(OfferRequest {
            app_id: Some(acct),
            description: "tips".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    t.wallet.transfer(None, Some(acct), 300_000, "fund".into()).await.unwrap();

    // A payment in flight blocks closing.
    *t.node.send.lock().unwrap() = SendBehaviour::Hang;
    let pending = t
        .wallet
        .pay(PayRequest {
            app_id: Some(acct),
            payment: external_invoice(100_000, 91),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(pending.state, "PENDING");
    let e = t.wallet.close_account(acct, true).await.unwrap_err();
    assert!(e.to_string().contains("in flight"), "{e}");
    let id = t.wallet.transaction(&pending.public_id).await.unwrap();
    t.node.complete(id.ln_payment_id.as_deref().unwrap(), false, 0);
    for _ in 0..100 {
        if t.wallet.transaction(&pending.public_id).await.unwrap().state == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Funds need an explicit sweep.
    assert!(matches!(t.wallet.close_account(acct, false).await, Err(WalletError::BadRequest(_))));
    let swept = t.wallet.close_account(acct, true).await.unwrap().unwrap();
    assert_eq!(swept.amount_msat, 300_000);
    assert_eq!(swept.app_id, Some(acct));
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 0);
    assert_eq!(t.wallet.allocation().await.unwrap().main_msat, 1_000_000_000);
    assert!(matches!(t.wallet.close_account(acct, true).await, Err(WalletError::BadRequest(_))));
    assert!(matches!(
        t.wallet.transfer(None, Some(acct), 1, "x".into()).await,
        Err(WalletError::BadRequest(_))
    ));
    let offers = t.wallet.offers(Some(Some(acct))).await.unwrap();
    assert_eq!(offers[0].state, "disabled", "its offers are disabled");

    // ldk-server cannot refuse payments to a shared offer (O-3): they still credit the
    // closed account, which can be emptied with a withdrawal.
    t.node.pay_offer(&offer.offer_id, 4_000, None);
    for _ in 0..100 {
        if t.wallet.balance(Some(acct)).await.unwrap() == 4_000 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(t.wallet.balance(Some(acct)).await.unwrap(), 4_000);
    t.wallet.transfer(Some(acct), None, 4_000, "withdraw".into()).await.unwrap();

    // The account's rows are still its own, not Main's.
    let (rows, total) = t
        .wallet
        .transactions(&TxFilter { app_id: Some(Some(acct)), limit: 50, ..Default::default() })
        .await
        .unwrap();
    assert!(total >= 4, "{rows:?}");
    let st = t
        .wallet
        .statement(
            Some(acct),
            &crate::wallet::accounts::StatementQuery { limit: 50, ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(st.balance_msat, 0);
    assert_eq!(st.items[0].balance_msat, 0);
    assert_eq!(st.items.last().unwrap().balance_msat, 300_000);
    // Running balances add up line by line.
    for w in st.items.windows(2) {
        assert_eq!(w[0].balance_msat, w[1].balance_msat + w[0].amount_msat);
    }
    assert_healthy(&t).await;
}

#[tokio::test]
async fn hash_squat_hold_is_rejected() {
    let t = setup().await;
    let a = add_account(&t, "squat-a").await;
    let b = add_account(&t, "squat-b").await;
    let normal = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(a),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = normal.payment_hash.clone().unwrap();
    let hold = t
        .wallet
        .create_hold_invoice(
            InvoiceRequest { app_id: Some(b), amount_msat: Some(99_000), ..Default::default() },
            &hash,
        )
        .await;
    assert!(
        matches!(hold, Err(WalletError::BadRequest(ref message)) if message.contains("already in use")),
        "{hold:?}"
    );
    let payment = t.node.pay_invoice(&hash, 21_000);
    t.wallet.apply_inbound(&payment).await.unwrap();
    let normal = t.wallet.transaction(&normal.public_id).await.unwrap();
    assert_eq!(normal.state, "SETTLED");
    assert_eq!(t.wallet.balance(Some(a)).await.unwrap(), 21_000);
    assert_eq!(t.wallet.balance(Some(b)).await.unwrap(), 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn inbound_bolt11_matches_payment_secret_not_newest_hash() {
    let t = setup().await;
    let owner = add_account(&t, "secret-owner").await;
    let attacker = add_account(&t, "secret-attacker").await;
    let normal = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = normal.payment_hash.clone().unwrap();
    let mut c = t.pool.acquire().await.unwrap();
    let squat = store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(attacker),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            ln_payment_id: Some(hash.clone()),
            payment_hash: Some(hash.clone()),
            payment_secret: Some("ff".repeat(32)),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);

    let payment = t.node.pay_invoice(&hash, 21_000);
    t.wallet.apply_inbound(&payment).await.unwrap();

    assert_eq!(t.wallet.transaction(&normal.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.transaction(&squat.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 21_000);
    assert_eq!(t.wallet.balance(Some(attacker)).await.unwrap(), 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn unambiguous_legacy_invoice_without_secret_still_settles() {
    let t = setup().await;
    let owner = add_account(&t, "legacy-secret-owner").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET payment_secret = NULL WHERE id = $1")
        .bind(invoice.id)
        .execute(&t.pool)
        .await
        .unwrap();

    let payment = t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 21_000);
    t.wallet.apply_inbound(&payment).await.unwrap();

    assert_eq!(t.wallet.transaction(&invoice.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 21_000);
    let main = t.wallet.balance(None).await.unwrap();
    t.wallet.apply_inbound(&payment).await.unwrap();
    assert_eq!(t.wallet.balance(None).await.unwrap(), main, "no Main adoption on replay");
    t.wallet.reconcile().await.unwrap();
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 21_000, "settled legacy replay");
    let receipts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM journal_entries WHERE kind = 'receive_settle'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(receipts, 1, "replay does not post another receive");
    assert_healthy(&t).await;
}

#[tokio::test]
async fn ambiguous_legacy_hash_is_quarantined() {
    let t = setup().await;
    let owner = add_account(&t, "legacy-owner").await;
    let attacker = add_account(&t, "legacy-attacker").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = invoice.payment_hash.clone().unwrap();
    sqlx::query("UPDATE transactions SET payment_secret = NULL WHERE id = $1")
        .bind(invoice.id)
        .execute(&t.pool)
        .await
        .unwrap();
    let mut c = t.pool.acquire().await.unwrap();
    store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(attacker),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            ln_payment_id: Some(hash.clone()),
            payment_hash: Some(hash.clone()),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);

    let payment = t.node.pay_invoice(&hash, 21_000);
    let error = t.wallet.apply_inbound(&payment).await.unwrap_err();

    assert!(error.to_string().contains("ambiguous legacy"), "{error}");
    assert_eq!(t.wallet.transaction(&invoice.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 0);
    assert_eq!(t.wallet.balance(Some(attacker)).await.unwrap(), 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn colliding_secret_bearing_rows_are_ambiguous_not_newest_wins() {
    let t = setup().await;
    let owner = add_account(&t, "same-secret-owner").await;
    let attacker = add_account(&t, "same-secret-attacker").await;
    let normal = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = normal.payment_hash.clone().unwrap();
    let secret = normal.payment_secret.clone().unwrap();
    let mut c = t.pool.acquire().await.unwrap();
    store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(attacker),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            ln_payment_id: Some(hash.clone()),
            payment_hash: Some(hash.clone()),
            payment_secret: Some(secret),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);

    let payment = t.node.pay_invoice(&hash, 21_000);
    let error = t.wallet.apply_inbound(&payment).await.unwrap_err();

    assert!(error.to_string().contains("ambiguous"), "{error}");
    assert_eq!(t.wallet.transaction(&normal.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 0);
    assert_eq!(t.wallet.balance(Some(attacker)).await.unwrap(), 0);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn colliding_hash_without_event_secret_is_quarantined() {
    let t = setup().await;
    let owner = add_account(&t, "missing-event-secret").await;
    let other = add_account(&t, "other-secret").await;
    let normal = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = normal.payment_hash.clone().unwrap();
    let mut c = t.pool.acquire().await.unwrap();
    store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(other),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            payment_hash: Some(hash.clone()),
            payment_secret: Some("ee".repeat(32)),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);
    let mut payment = t.node.pay_invoice(&hash, 21_000);
    payment.kind = LnPaymentKind::Bolt11 {
        payment_hash: hash,
        preimage: payment.kind.preimage().map(str::to_string),
        payment_secret: None,
    };

    let error = t.wallet.apply_inbound(&payment).await.unwrap_err();

    assert!(error.to_string().contains("secret"), "{error}");
    assert_eq!(t.wallet.transaction(&normal.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 0);
    assert_eq!(t.wallet.balance(Some(other)).await.unwrap(), 0);
    assert_eq!(t.wallet.balance(None).await.unwrap(), 1_000_000_000);
    let external: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transactions WHERE app_id IS NULL AND state = 'SETTLED'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(external, 0, "an unmatched collision must not become an external Main receipt");
    assert_healthy(&t).await;
}

#[tokio::test]
async fn successful_receipt_without_amount_does_not_settle_zero() {
    let t = setup().await;
    let owner = add_account(&t, "unknown-amount").await;
    let invoice = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut payment = t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 21_000);
    payment.amount_msat = None;

    let error = t.wallet.apply_inbound(&payment).await.unwrap_err();

    assert!(error.to_string().contains("amount"), "{error}");
    let current = t.wallet.transaction(&invoice.public_id).await.unwrap();
    assert_eq!(current.state, "PENDING");
    assert_eq!(current.amount_msat, 21_000);
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 0);
    let zero: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM journal_entries WHERE kind = 'receive_settle'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(zero, 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn colliding_hold_owner_can_settle_and_other_owner_can_cancel() {
    let t = setup().await;
    let a = add_account(&t, "hold-collision-a").await;
    let b = add_account(&t, "hold-collision-b").await;
    let preimage = "42".repeat(32);
    let hash = sha256_hex(&hex::decode(&preimage).unwrap());
    let secret_a = "aa".repeat(32);
    let secret_b = "bb".repeat(32);
    let mut c = t.pool.acquire().await.unwrap();
    let hold_a = store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(a),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 21_000,
            ln_payment_id: Some("hold-payment-a".into()),
            payment_hash: Some(hash.clone()),
            payment_secret: Some(secret_a.clone()),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    let hold_b = store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(b),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            ln_payment_id: Some("hold-payment-b".into()),
            payment_hash: Some(hash.clone()),
            payment_secret: Some(secret_b.clone()),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);
    for (id, secret, amount) in
        [("hold-payment-a", secret_a, 21_000), ("hold-payment-b", secret_b, 99_000)]
    {
        t.node.add_history_payment(LnPayment {
            id: id.into(),
            kind: LnPaymentKind::Bolt11 {
                payment_hash: hash.clone(),
                preimage: None,
                payment_secret: Some(secret),
            },
            direction: Direction::Inbound,
            amount_msat: Some(amount),
            status: LnStatus::Pending,
            fee_msat: None,
            updated_at: 1,
        });
    }
    let payment_a = t.node.payment("hold-payment-a").await.unwrap().unwrap();
    t.wallet.apply_claimable(&payment_a, Some(500), 21_000).await.unwrap();
    assert_eq!(t.wallet.transaction(&hold_a.public_id).await.unwrap().state, "ACCEPTED");

    let settled = t.wallet.settle_hold(Some(a), &preimage).await.unwrap();
    assert_eq!(settled.public_id, hold_a.public_id);
    assert_eq!(settled.amount_msat, 21_000);
    assert_eq!(t.wallet.balance(Some(a)).await.unwrap(), 21_000);
    assert_eq!(t.wallet.balance(Some(b)).await.unwrap(), 0);
    assert_eq!(t.node.claimed(), vec![("hold-payment-a".into(), preimage)]);

    let canceled = t.wallet.cancel_hold(Some(b), &hash).await.unwrap();
    assert_eq!(canceled.public_id, hold_b.public_id);
    assert_eq!(canceled.state, "CANCELED");
    assert_eq!(t.wallet.balance(Some(b)).await.unwrap(), 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn normal_and_later_hold_collision_uses_secret_and_actual_amount_matrix() {
    for received in [10_000i64, 120_000] {
        let t = setup().await;
        let owner = add_account(&t, &format!("matrix-owner-{received}")).await;
        let other = add_account(&t, &format!("matrix-other-{received}")).await;
        let normal = t
            .wallet
            .create_invoice(InvoiceRequest {
                app_id: Some(owner),
                amount_msat: Some(21_000),
                ..Default::default()
            })
            .await
            .unwrap();
        let hash = normal.payment_hash.clone().unwrap();
        let mut c = t.pool.acquire().await.unwrap();
        let hold = store::insert(
            &mut c,
            &store::NewTx {
                app_id: Some(other),
                direction: "incoming",
                payment_type: "bolt11",
                state: "PENDING",
                amount_msat: 99_000,
                ln_payment_id: Some("different-backend-id".into()),
                payment_hash: Some(hash.clone()),
                payment_secret: Some("dd".repeat(32)),
                hold: true,
                ..Default::default()
            },
            now(),
        )
        .await
        .unwrap();
        drop(c);

        let payment = t.node.pay_invoice(&hash, received as u64);
        t.wallet.apply_inbound(&payment).await.unwrap();
        t.wallet.apply_inbound(&payment).await.unwrap();

        let settled = t.wallet.transaction(&normal.public_id).await.unwrap();
        assert_eq!(settled.state, "SETTLED");
        assert_eq!(settled.amount_msat, received);
        assert_eq!(t.wallet.transaction(&hold.public_id).await.unwrap().state, "PENDING");
        assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), received);
        assert_eq!(t.wallet.balance(Some(other)).await.unwrap(), 0);
        assert_healthy(&t).await;
    }
}

#[tokio::test]
async fn outgoing_collision_cannot_settle_newer_different_secret_row() {
    let t = setup().await;
    let a = add_account(&t, "out-collision-a").await;
    let b = add_account(&t, "out-collision-b").await;
    t.wallet.transfer(None, Some(a), 200_000, "fund".into()).await.unwrap();
    t.wallet.transfer(None, Some(b), 200_000, "fund".into()).await.unwrap();
    let hash = "ab".repeat(32);
    let secret_a = "aa".repeat(32);
    let secret_b = "bb".repeat(32);
    let reserve = |owner, amount, secret: &str, request: String| NewTx {
        app_id: Some(owner),
        direction: "outgoing",
        payment_type: "bolt11",
        state: "PENDING",
        amount_msat: amount,
        payment_hash: Some(hash.clone()),
        payment_secret: Some(secret.into()),
        payment_request: Some(request),
        ln_payment_id: Some(hash.clone()),
        attempt_state: "submitted",
        ..Default::default()
    };
    let payment_a = t
        .wallet
        .reserve(
            &PayRequest { app_id: Some(a), ..Default::default() },
            reserve(a, 21_000, &secret_a, MockBackend::make_invoice(Some(21_000), &hash)),
            None,
        )
        .await
        .unwrap();
    let payment_b = t
        .wallet
        .reserve(
            &PayRequest { app_id: Some(b), ..Default::default() },
            reserve(b, 99_000, &secret_b, MockBackend::make_invoice(Some(99_000), &hash)),
            None,
        )
        .await
        .unwrap();
    let payment = LnPayment {
        id: hash.clone(),
        kind: LnPaymentKind::Bolt11 {
            payment_hash: hash,
            preimage: Some("11".repeat(32)),
            payment_secret: Some(secret_a),
        },
        direction: Direction::Outbound,
        status: LnStatus::Succeeded,
        amount_msat: Some(21_000),
        fee_msat: Some(0),
        updated_at: 100,
    };

    t.wallet.apply_outgoing_success(&payment, None, None).await.unwrap();

    assert_eq!(t.wallet.transaction(&payment_a.public_id).await.unwrap().state, "SETTLED");
    assert_eq!(t.wallet.transaction(&payment_b.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(a)).await.unwrap(), 179_000);
    assert_eq!(t.wallet.balance(Some(b)).await.unwrap(), 101_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn delegated_invoice_lookup_rejects_duplicate_payment_hash() {
    let t = setup().await;
    let owner = add_account(&t, "lookup-collision").await;
    let normal = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(owner),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = normal.payment_hash.clone().unwrap();
    let mut c = t.pool.acquire().await.unwrap();
    store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(owner),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            payment_hash: Some(hash.clone()),
            payment_secret: Some("cc".repeat(32)),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);

    let error = t.wallet.lookup_for_app(owner, true, Some(&hash), None).await.unwrap_err();

    assert!(error.to_string().contains("several payments match"), "{error}");
}

#[tokio::test]
async fn same_owner_duplicate_holds_are_ambiguous_for_settle_and_cancel() {
    let t = setup().await;
    let owner = add_account(&t, "same-owner-holds").await;
    let preimage = "52".repeat(32);
    let hash = sha256_hex(&hex::decode(&preimage).unwrap());
    let mut c = t.pool.acquire().await.unwrap();
    for (n, secret) in [("a", "aa"), ("b", "bb")] {
        store::insert(
            &mut c,
            &store::NewTx {
                public_id: Some(format!("same-owner-hold-{n}")),
                app_id: Some(owner),
                direction: "incoming",
                payment_type: "bolt11",
                state: "PENDING",
                amount_msat: 21_000,
                payment_hash: Some(hash.clone()),
                payment_secret: Some(secret.repeat(32)),
                hold: true,
                ..Default::default()
            },
            now(),
        )
        .await
        .unwrap();
    }
    drop(c);

    let settle = t.wallet.settle_hold(Some(owner), &preimage).await.unwrap_err();
    let cancel = t.wallet.cancel_hold(Some(owner), &hash).await.unwrap_err();

    assert!(settle.to_string().contains("ambiguous"), "{settle}");
    assert!(cancel.to_string().contains("ambiguous"), "{cancel}");
    assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn collision_hold_state_and_amount_matrix() {
    for (initial, received) in [
        ("PENDING", 10_000i64),
        ("PENDING", 120_000),
        ("ACCEPTED", 120_000),
        ("CANCELING", 120_000),
    ] {
        let t = setup().await;
        let owner = add_account(&t, &format!("state-owner-{initial}-{received}")).await;
        let other = add_account(&t, &format!("state-other-{initial}-{received}")).await;
        let normal = t
            .wallet
            .create_invoice(InvoiceRequest {
                app_id: Some(owner),
                amount_msat: Some(21_000),
                ..Default::default()
            })
            .await
            .unwrap();
        let hash = normal.payment_hash.clone().unwrap();
        let hold_secret = "dd".repeat(32);
        let mut c = t.pool.acquire().await.unwrap();
        let hold = store::insert(
            &mut c,
            &store::NewTx {
                app_id: Some(other),
                direction: "incoming",
                payment_type: "bolt11",
                state: "PENDING",
                amount_msat: 99_000,
                ln_payment_id: Some("hold-state-backend-id".into()),
                payment_hash: Some(hash.clone()),
                payment_secret: Some(hold_secret.clone()),
                hold: true,
                ..Default::default()
            },
            now(),
        )
        .await
        .unwrap();
        drop(c);
        sqlx::query("UPDATE transactions SET state = $1, claimable_amount_msat = $2 WHERE id = $3")
            .bind(initial)
            .bind((initial == "ACCEPTED").then_some(received))
            .bind(hold.id)
            .execute(&t.pool)
            .await
            .unwrap();
        let mut backend = LnPayment {
            id: "hold-state-backend-id".into(),
            kind: LnPaymentKind::Bolt11 {
                payment_hash: hash.clone(),
                preimage: None,
                payment_secret: Some(hold_secret),
            },
            direction: Direction::Inbound,
            status: LnStatus::Pending,
            amount_msat: Some(received as u64),
            fee_msat: None,
            updated_at: 1,
        };
        t.node.add_history_payment(backend.clone());

        t.wallet.apply_claimable(&backend, Some(500), received as u64).await.unwrap();

        if initial == "PENDING" && received >= 99_000 || initial == "ACCEPTED" {
            assert_eq!(t.wallet.transaction(&hold.public_id).await.unwrap().state, "ACCEPTED");
            let backend_preimage =
                t.node.payment(&hash).await.unwrap().unwrap().kind.preimage().unwrap().to_string();
            let settled = t.wallet.settle_hold(Some(other), &backend_preimage).await.unwrap();
            assert_eq!(settled.amount_msat, received);
            assert_eq!(t.wallet.balance(Some(other)).await.unwrap(), received);
            backend.status = LnStatus::Succeeded;
            backend.amount_msat = Some(received as u64);
            t.wallet.apply_inbound(&backend).await.unwrap();
            t.wallet.apply_inbound(&backend).await.unwrap();
            assert_eq!(t.wallet.balance(Some(other)).await.unwrap(), received, "settled replay");
        } else {
            let current = t.wallet.transaction(&hold.public_id).await.unwrap();
            assert_eq!(current.state, "CANCELED", "{initial} / {received}");
            assert_eq!(t.wallet.balance(Some(other)).await.unwrap(), 0);
        }
        t.wallet.apply_inbound(&t.node.pay_invoice(&hash, 21_000)).await.unwrap();
        assert_eq!(t.wallet.transaction(&normal.public_id).await.unwrap().state, "SETTLED");
        assert_eq!(t.wallet.balance(Some(owner)).await.unwrap(), 21_000);
        assert_healthy(&t).await;
    }
}

#[tokio::test]
async fn outgoing_failure_collision_cannot_release_newer_different_secret_row() {
    let t = setup().await;
    let a = add_account(&t, "out-fail-a").await;
    let b = add_account(&t, "out-fail-b").await;
    t.wallet.transfer(None, Some(a), 200_000, "fund".into()).await.unwrap();
    t.wallet.transfer(None, Some(b), 200_000, "fund".into()).await.unwrap();
    let hash = "cd".repeat(32);
    let secret_a = "aa".repeat(32);
    let reserve = |owner, amount, secret: &str| NewTx {
        app_id: Some(owner),
        direction: "outgoing",
        payment_type: "bolt11",
        state: "PENDING",
        amount_msat: amount,
        payment_hash: Some(hash.clone()),
        payment_secret: Some(secret.into()),
        payment_request: Some(MockBackend::make_invoice(Some(amount as u64), &hash)),
        ln_payment_id: Some(hash.clone()),
        attempt_state: "submitted",
        ..Default::default()
    };
    let payment_a = t
        .wallet
        .reserve(
            &PayRequest { app_id: Some(a), ..Default::default() },
            reserve(a, 21_000, &secret_a),
            None,
        )
        .await
        .unwrap();
    let payment_b = t
        .wallet
        .reserve(
            &PayRequest { app_id: Some(b), ..Default::default() },
            reserve(b, 99_000, &"bb".repeat(32)),
            None,
        )
        .await
        .unwrap();
    let payment = LnPayment {
        id: hash.clone(),
        kind: LnPaymentKind::Bolt11 {
            payment_hash: hash,
            preimage: None,
            payment_secret: Some(secret_a),
        },
        direction: Direction::Outbound,
        status: LnStatus::Failed,
        amount_msat: Some(21_000),
        fee_msat: Some(0),
        updated_at: 100,
    };

    t.wallet
        .handle_event(LnEvent::PaymentFailed { payment, reason: Some("route failed".into()) })
        .await
        .unwrap();

    assert_eq!(t.wallet.transaction(&payment_a.public_id).await.unwrap().state, "FAILED");
    assert_eq!(t.wallet.transaction(&payment_b.public_id).await.unwrap().state, "PENDING");
    assert_eq!(t.wallet.balance(Some(a)).await.unwrap(), 200_000);
    assert_eq!(t.wallet.balance(Some(b)).await.unwrap(), 101_000);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn duplicate_self_payment_invoice_does_not_choose_receiver() {
    let t = setup().await;
    let a = add_account(&t, "self-collision-a").await;
    let b = add_account(&t, "self-collision-b").await;
    let incoming = t
        .wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(a),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut c = t.pool.acquire().await.unwrap();
    store::insert(
        &mut c,
        &store::NewTx {
            app_id: Some(b),
            direction: "incoming",
            payment_type: "bolt11",
            state: "PENDING",
            amount_msat: 99_000,
            payment_hash: incoming.payment_hash.clone(),
            payment_secret: incoming.payment_secret.clone(),
            payment_request: incoming.payment_request.clone(),
            hold: true,
            ..Default::default()
        },
        now(),
    )
    .await
    .unwrap();
    drop(c);

    let error = t
        .wallet
        .pay(PayRequest {
            payment: incoming.payment_request.clone().unwrap(),
            ..Default::default()
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("ambiguous self-payment"), "{error}");
    assert_eq!(t.wallet.balance(Some(a)).await.unwrap(), 0);
    assert_eq!(t.wallet.balance(Some(b)).await.unwrap(), 0);
    let outgoing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE direction = 'outgoing'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(outgoing, 0);
    assert_healthy(&t).await;
}

#[tokio::test]
async fn attempt_numbers_never_order_distinct_invoice_identities() {
    let t = setup().await;
    let mut first = t.wallet.create_invoice(InvoiceRequest::default()).await.unwrap();
    let mut second = t.wallet.create_invoice(InvoiceRequest::default()).await.unwrap();
    first.direction = "outgoing".into();
    first.payment_type = "bolt11".into();
    first.attempt_number = 1;
    second.direction = "outgoing".into();
    second.payment_type = "bolt11".into();
    second.attempt_number = 99;
    let backend = LnPayment {
        id: "shared-backend-id".into(),
        kind: LnPaymentKind::Bolt11 {
            payment_hash: "aa".repeat(32),
            preimage: None,
            payment_secret: Some("bb".repeat(32)),
        },
        direction: Direction::Outbound,
        status: LnStatus::Succeeded,
        amount_msat: Some(1_000),
        fee_msat: Some(0),
        updated_at: 1,
    };

    let error = Wallet::latest_attempt(vec![first, second], &backend).unwrap_err();

    assert!(error.to_string().contains("ambiguous outgoing"), "{error}");
}

/// A channel's funding transactions: the opening one, then each splice's, kept although
/// ldk-server only reports the current one.
#[tokio::test]
async fn channel_fundings_keep_the_opening_and_each_splice() {
    let t = setup().await;
    t.wallet.start();
    let (open, splice) = ("aa".repeat(32), "bb".repeat(32));
    t.node.emit(LnEvent::ChannelStateChanged {
        channel_id: "11".repeat(32),
        counterparty_node_id: None,
        state: "Pending".into(),
        reason: None,
        user_channel_id: Some("u1".into()),
        funding_txid: Some(open.clone()),
    });
    t.node.emit(LnEvent::SpliceNegotiated {
        user_channel_id: "u1".into(),
        funding_txid: splice.clone(),
    });
    // Seen again (the channel list after the splice confirmed): nothing new.
    for _ in 0..100 {
        if t.wallet.channel_fundings().await.unwrap().get("u1").is_some_and(|f| f.len() == 2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    t.wallet.record_funding("u1", &splice, false, Some(9_000)).await.unwrap();
    let f = t.wallet.channel_fundings().await.unwrap();
    let kinds: Vec<(&str, &str)> =
        f["u1"].iter().map(|x| (x.txid.as_str(), x.kind.as_str())).collect();
    assert_eq!(kinds, vec![(open.as_str(), "open"), (splice.as_str(), "splice")]);
    // Seen in the list while current: its capacity is kept.
    assert_eq!(f["u1"][1].capacity_sat, Some(9_000));
    // A channel first seen in the list (opened before Allod kept these): its funding is the opening.
    t.wallet.record_funding("u2", &"cc".repeat(32), false, None).await.unwrap();
    assert_eq!(t.wallet.channel_fundings().await.unwrap()["u2"][0].kind, "open");
    // A splice Allod started: its direction is kept with its transaction.
    t.wallet.note_splice("u2", "out");
    t.wallet.record_funding("u2", &"dd".repeat(32), true, None).await.unwrap();
    assert_eq!(
        t.wallet.channel_fundings().await.unwrap()["u2"][1].direction.as_deref(),
        Some("out")
    );
}
