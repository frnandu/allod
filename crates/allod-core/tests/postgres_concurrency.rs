//! Run with ALLOD_TEST_POSTGRES_URL pointing at a disposable PostgreSQL database.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use allod_core::wallet::{PayRequest, WalletConfig, WalletError};
use allod_core::Wallet;
use allod_db::Db;
use allod_ln::mock::{sha256_hex, MockBackend, SendBehaviour};
use allod_ln::LightningBackend;

#[tokio::test]
async fn postgres_worker() {
    if std::env::var_os("ALLOD_PG_WORKER").is_none() {
        return;
    }
    let url = std::env::var("ALLOD_TEST_POSTGRES_URL").unwrap();
    let app_id: i64 = std::env::var("ALLOD_PG_APP_ID").unwrap().parse().unwrap();
    let seed: u8 = std::env::var("ALLOD_PG_SEED").unwrap().parse().unwrap();
    let mut start = [0u8; 1];
    std::io::Read::read_exact(&mut std::io::stdin(), &mut start).unwrap();
    let pool = Db::any_pool(&url).await.unwrap();
    let node = Arc::new(MockBackend::new());
    *node.send.lock().unwrap() = SendBehaviour::Hang;
    let wallet = Wallet::new(
        pool,
        node as Arc<dyn LightningBackend>,
        WalletConfig { sync_timeout: Duration::from_millis(20), ..Default::default() },
    );
    let invoice = MockBackend::make_invoice(
        Some(100_000),
        &sha256_hex(format!("{app_id}-{seed}").as_bytes()),
    );
    match wallet
        .pay(PayRequest { app_id: Some(app_id), payment: invoice, ..Default::default() })
        .await
    {
        Ok(tx) => assert_eq!(tx.state, "PENDING"),
        Err(WalletError::InsufficientBalance | WalletError::QuotaExceeded) => {}
        other => panic!("unexpected concurrent reserve result: {other:?}"),
    }
}

#[tokio::test]
async fn postgres_concurrent_processes_cannot_overdraw() {
    let Ok(url) = std::env::var("ALLOD_TEST_POSTGRES_URL") else { return };
    let db = Db::connect(&url).await.unwrap();
    let lock = Db::instance_lock(&url).await.unwrap().unwrap();
    assert!(Db::instance_lock(&url).await.is_err(), "second daemon must be refused");

    let pool = Db::any_pool(&url).await.unwrap();
    let node = Arc::new(MockBackend::new());
    let wallet =
        Wallet::new(pool.clone(), node as Arc<dyn LightningBackend>, WalletConfig::default());
    wallet.reconcile().await.unwrap();
    let executable = std::env::current_exe().unwrap();
    for (case, funding, budget, seeds) in
        [("balance", 150_000, 0, [91, 92]), ("budget", 500_000, 150_000, [93, 94])]
    {
        let unique = format!("pg-race-{case}-{}-{}", std::process::id(), allod_core::wallet::now());
        let app_id: i64 = sqlx::query_scalar(
            "INSERT INTO apps (name, kind, app_pubkey, isolated, max_amount_msat, budget_renewal, created_at, updated_at) \
             VALUES ($1, 'account', $2, TRUE, $3, 'never', 0, 0) RETURNING id",
        )
        .bind(&unique)
        .bind(&unique)
        .bind(budget)
        .fetch_one(&pool)
        .await
        .unwrap();
        wallet.transfer(None, Some(app_id), funding, "fund race test".into()).await.unwrap();

        let mut children = Vec::new();
        for seed in seeds {
            children.push(
                Command::new(&executable)
                    .args(["--exact", "postgres_worker", "--nocapture"])
                    .env("ALLOD_PG_WORKER", "1")
                    .env("ALLOD_PG_APP_ID", app_id.to_string())
                    .env("ALLOD_PG_SEED", seed.to_string())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        for child in &mut children {
            child.stdin.take().unwrap().write_all(b"x").unwrap();
        }
        for child in children {
            let result = child.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "worker failed: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM transactions WHERE app_id = $1 AND direction = 'outgoing' AND state = 'PENDING'",
        )
        .bind(app_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 1, "{case}");
        assert_eq!(wallet.balance(Some(app_id)).await.unwrap(), funding as i64 - 110_000);
        if budget > 0 {
            assert_eq!(wallet.budget(app_id).await.unwrap().unwrap().used_budget, 110_000);
        }
    }
    drop(lock);
    db.close().await;
}
