//! `allod` binary: daemon and CLI.

mod config;

use std::time::Duration;

use allod_api::{AppState, AuthState, LdkSource};
use allod_types::DeploymentInfo;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{Config, ConfigArgs, LogFormat};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "allod", version, about = "Allod Lightning wallet service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon.
    Serve(Box<ConfigArgs>),
    /// Headless first-run setup (SPEC §3.2). The owner key becomes the first owner.
    Setup {
        /// Owner key (hex or npub) that will log in to the web UI.
        #[arg(long)]
        owner: String,
        /// Restore from this recovery phrase file instead of creating a new phrase.
        #[arg(long)]
        restore_file: Option<std::path::PathBuf>,
        /// With --restore-file: block height of the wallet's first transaction.
        #[arg(long)]
        birthday_height: Option<u32>,
        /// Standalone mode: ldk-server TLS certificate (PEM) file.
        #[arg(long)]
        ldk_cert: Option<std::path::PathBuf>,
        /// Standalone mode: ldk-server macaroon file.
        #[arg(long)]
        ldk_macaroon: Option<std::path::PathBuf>,
        #[command(flatten)]
        config: Box<ConfigArgs>,
    },
    /// Manage the Nostr keys that may log in to the web UI. Run on the host; this is
    /// also the recovery path when an owner key is lost (SPEC §11.1).
    Owner {
        #[command(subcommand)]
        action: OwnerAction,
        #[command(flatten)]
        config: Box<ConfigArgs>,
    },
    /// Exit 0 when a local Allod answers /api/health with a status other than
    /// "down". Used by container health checks.
    Healthcheck {
        /// Address of the Allod HTTP server.
        #[arg(long, env = "ALLOD_LISTEN", default_value = config::DEFAULT_LISTEN)]
        listen: std::net::SocketAddr,
    },
}

#[derive(Debug, Subcommand)]
enum OwnerAction {
    /// Add an owner (hex pubkey or npub).
    Add {
        pubkey: String,
        #[arg(long, default_value = "")]
        label: String,
    },
    /// List owners.
    List,
    /// Remove an owner (never the last one).
    Remove { pubkey: String },
}

fn main() -> Result<()> {
    drop_empty_env();
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    match cli.command {
        Command::Serve(args) => {
            let config = Config::load(*args)?;
            init_tracing(&config);
            runtime.block_on(serve(config))
        }
        Command::Setup { owner, restore_file, birthday_height, ldk_cert, ldk_macaroon, config } => {
            let config = Config::load(*config)?;
            runtime.block_on(setup_cli(
                config,
                owner,
                restore_file,
                birthday_height,
                ldk_cert,
                ldk_macaroon,
            ))
        }
        Command::Owner { action, config } => {
            let config = Config::load(*config)?;
            runtime.block_on(owner(config, action))
        }
        Command::Healthcheck { listen } => runtime.block_on(healthcheck(listen)),
    }
}

/// Compose and platform templates pass optional settings as empty strings
/// (`ALLOD_PUBLIC_URL=`). Treat those as unset instead of as invalid values.
/// Runs before any thread is spawned.
fn drop_empty_env() {
    let empty: Vec<_> = std::env::vars_os()
        .filter(|(k, v)| k.to_string_lossy().starts_with("ALLOD_") && v.is_empty())
        .map(|(k, _)| k)
        .collect();
    for key in empty {
        std::env::remove_var(key);
    }
}

fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_new(&config.log).unwrap_or_else(|_| EnvFilter::new("info"));
    use std::io::IsTerminal;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr);
    match config.log_format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.init(),
    }
}

async fn serve(config: Config) -> Result<()> {
    if config.database_url.starts_with("sqlite:") {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("create data dir {}", config.data_dir.display()))?;
    }
    let db = allod_db::Db::connect(&config.database_url).await.context("open database")?;
    tracing::info!(backend = db.backend_name(), "database ready");

    let setup = std::sync::Arc::new(setup_service(&config, db.clone()).await?);
    if config.dev_auto_setup && !setup.is_complete() {
        let owner = config.dev_owner_pubkey.as_deref().unwrap_or_default();
        let owner = allod_api::normalize_pubkey(owner).map_err(|e| anyhow::anyhow!(e.message))?;
        setup
            .run(allod_core::SetupRequest::Create, &owner, unix_now())
            .await
            .context("dev auto-setup")?;
        tracing::warn!(%owner, "ALLOD_DEV_AUTO_SETUP: setup completed with a new recovery phrase");
    }
    spawn_provisioning(setup.clone());

    // Explicit credential files (ALLOD_LDK_SERVER_*) override what setup stored.
    let overrides = match (
        &config.ldk_server_address,
        &config.ldk_server_cert_file,
        &config.ldk_server_macaroon_file,
    ) {
        (Some(a), Some(c), Some(m)) => Some((a.clone(), c.clone(), m.clone())),
        _ => None,
    };
    let ldk = LdkSource::new(overrides, Some(setup.clone()), config.lsps2_lsp.clone());
    check_network(&config, &ldk).await?;

    let state = AppState {
        version: env!("CARGO_PKG_VERSION").to_string(),
        db: db.clone(),
        ldk,
        deployment: DeploymentInfo {
            mode: config.mode,
            platform: config.platform,
            chain_mode: config.chain,
        },
        auth: std::sync::Arc::new(AuthState::new()),
        public_url: config.public_url.clone(),
        setup,
        fiat: std::sync::Arc::new(allod_core::FiatService::new()),
        wallet: Default::default(),
        apps: Default::default(),
        contacts: Default::default(),
        // Offered only for the bundled bitcoind, which runs the helper.
        fast_sync: config
            .control_dir
            .clone()
            .filter(|_| {
                matches!(
                    config.chain,
                    Some(allod_types::ChainMode::Bundled)
                        | Some(allod_types::ChainMode::BundledRest)
                )
            })
            .map(|d| std::sync::Arc::new(allod_core::fastsync::FastSync::new(d))),
    };
    let _ = state.contacts.set(std::sync::Arc::new(allod_nwc::contacts::Contacts::new(
        config.profile_relays.clone(),
    )));
    spawn_wallet(&config, &state).await?;

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("bind {}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        mode = %config.mode,
        handoff_dir = %config.handoff_dir.display(),
        public_url = config.public_url.as_deref().unwrap_or("-"),
        "allod listening"
    );
    axum::serve(listener, allod_api::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    db.close().await;
    tracing::info!("allod stopped");
    Ok(())
}

/// If ldk-server is reachable now and reports a network other than the expected
/// one, refuse to start (SPEC §3.1). An unreachable server is not fatal: in
/// managed mode it starts after Allod.
async fn check_network(config: &Config, ldk: &LdkSource) -> Result<()> {
    let Some(expected) = config.network else { return Ok(()) };
    let Ok(backend) = ldk.backend().await else { return Ok(()) };
    match backend.node_info().await {
        Ok(info) if info.network.is_some_and(|n| n != expected) => anyhow::bail!(
            "ldk-server runs on {:?} but ALLOD_NETWORK is {expected}",
            info.network.map(|n| n.to_string())
        ),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(error = %e, "ldk-server not reachable yet");
            Ok(())
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn setup_service(config: &Config, db: allod_db::Db) -> Result<allod_core::SetupService> {
    let bitcoind = config.bitcoind_rpc_url.clone().map(|url| allod_core::BitcoindRpc {
        url,
        user: config.bitcoind_rpc_user.clone().unwrap_or_default(),
        pass: config.bitcoind_rpc_pass.clone().unwrap_or_default(),
    });
    let cfg = allod_core::SetupConfig {
        mode: config.mode,
        secrets_dir: config.data_dir.join("secrets"),
        handoff_dir: config.handoff_dir.clone(),
        managed_ldk_address: config
            .ldk_server_address
            .clone()
            .unwrap_or_else(|| config::MANAGED_LDK_ADDRESS.to_string()),
        network: config.network,
        bitcoind,
    };
    Ok(allod_core::SetupService::new(db, cfg).await?)
}

/// Starts the wallet (SPEC §6–7) as soon as the Lightning backend is reachable.
async fn spawn_wallet(config: &Config, state: &AppState) -> Result<()> {
    let pool =
        allod_db::Db::any_pool(&config.database_url).await.context("open wallet database")?;
    let ldk = state.ldk.clone();
    let cell = state.wallet.clone();
    let apps_cell = state.apps.clone();
    let setup = state.setup.clone();
    let relays = (config.relays.clone(), config.client_relays.clone());
    let regtest = config.network == Some(allod_types::Network::Regtest);
    tokio::spawn(async move {
        let mut last = String::new();
        loop {
            match ldk.backend().await {
                Ok(backend) => match backend.node_info().await {
                    Ok(_) => {
                        let wallet = allod_core::Wallet::new(
                            pool.clone(),
                            backend.clone(),
                            allod_core::WalletConfig::default(),
                        );
                        if regtest {
                            // Local LNURL test services speak plain http.
                            wallet.allow_http_lnurl();
                        }
                        wallet.start();
                        let _ = cell.set(wallet.clone());
                        tracing::info!("wallet started");
                        match start_nwc(&setup, pool, wallet, backend, relays) {
                            Ok(apps) => {
                                let _ = apps_cell.set(apps);
                            }
                            Err(e) => tracing::error!(error = %e, "NWC service not started"),
                        }
                        return;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg != last {
                            tracing::info!(reason = %msg, "waiting for the Lightning node before starting the wallet");
                            last = msg;
                        }
                    }
                },
                Err(e) => {
                    if e != last {
                        tracing::info!(reason = %e, "waiting for the Lightning node before starting the wallet");
                        last = e;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    Ok(())
}

/// Starts the connections service and the NWC wallet service (SPEC §8). The Nostr
/// wallet keys derive from the recovery phrase (§4.2).
fn start_nwc(
    setup: &allod_core::SetupService,
    pool: sqlx::AnyPool,
    wallet: std::sync::Arc<allod_core::Wallet>,
    backend: std::sync::Arc<dyn allod_ln::LightningBackend>,
    (relays, client_relays): (Vec<String>, Vec<String>),
) -> Result<std::sync::Arc<allod_core::AppService>> {
    let words = setup.recovery_phrase()?.context("no recovery phrase")?;
    let phrase = allod_keys::RecoveryPhrase::parse(&words)?;
    let keys = allod_keys::NostrKeys::from_phrase(&phrase)?;
    let apps = allod_core::AppService::new(pool, std::sync::Arc::new(keys), relays, client_relays);
    let nwc = allod_nwc::NwcService::new(apps.clone(), wallet, backend);
    nwc.start();
    tracing::info!("NWC service started");
    Ok(apps)
}

/// Managed mode: keep trying to obtain Allod' own ldk-server macaroon until it exists
/// (ldk-server only starts once bitcoind has synced; SPEC §2.3.3).
fn spawn_provisioning(setup: std::sync::Arc<allod_core::SetupService>) {
    if setup.mode() != allod_types::DeploymentMode::Managed {
        return;
    }
    tokio::spawn(async move {
        let mut last = String::new();
        loop {
            match setup.provision_managed(unix_now()).await {
                Ok(allod_core::ProvisionState::Done) => break,
                Ok(state) => {
                    let now = format!("{state:?}");
                    if now != last {
                        tracing::info!(?state, "ldk-server credentials");
                        last = now;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "ldk-server credential provisioning failed"),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

async fn setup_cli(
    config: Config,
    owner: String,
    restore_file: Option<std::path::PathBuf>,
    birthday_height: Option<u32>,
    ldk_cert: Option<std::path::PathBuf>,
    ldk_macaroon: Option<std::path::PathBuf>,
) -> Result<()> {
    if config.database_url.starts_with("sqlite:") {
        std::fs::create_dir_all(&config.data_dir)?;
    }
    let db = allod_db::Db::connect(&config.database_url).await.context("open database")?;
    let setup = setup_service(&config, db.clone()).await?;
    let owner = allod_api::normalize_pubkey(&owner).map_err(|e| anyhow::anyhow!(e.message))?;
    let phrase = match &restore_file {
        Some(path) => Some(zeroize::Zeroizing::new(
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
        )),
        None => None,
    };
    let req = match config.mode {
        allod_types::DeploymentMode::Managed => match phrase {
            Some(phrase) => allod_core::SetupRequest::Restore { phrase, birthday_height },
            None => allod_core::SetupRequest::Create,
        },
        allod_types::DeploymentMode::Standalone => {
            let (Some(address), Some(cert), Some(mac)) =
                (config.ldk_server_address.clone(), ldk_cert, ldk_macaroon)
            else {
                anyhow::bail!(
                    "standalone setup needs --ldk-server-address, --ldk-cert and --ldk-macaroon"
                );
            };
            let raw = std::fs::read(&mac)?;
            let text = String::from_utf8_lossy(&raw).trim().to_string();
            let macaroon_hex = if text.chars().all(|c| c.is_ascii_hexdigit()) {
                text
            } else {
                raw.iter().map(|b| format!("{b:02x}")).collect()
            };
            allod_core::SetupRequest::Standalone {
                address,
                tls_cert_pem: std::fs::read_to_string(&cert)?,
                macaroon_hex: zeroize::Zeroizing::new(macaroon_hex),
                phrase,
            }
        }
    };
    let outcome = setup.run(req, &owner, unix_now()).await?;
    println!("setup complete; owner {owner}");
    if let Some(words) = outcome.generated_phrase {
        println!(
            "\nRecovery phrase (write it down now; it is also in {}):\n\n{}\n",
            config.data_dir.join("secrets/recovery_phrase").display(),
            words.as_str()
        );
    }
    db.close().await;
    Ok(())
}

async fn owner(config: Config, action: OwnerAction) -> Result<()> {
    if config.database_url.starts_with("sqlite:") {
        std::fs::create_dir_all(&config.data_dir)?;
    }
    let db = allod_db::Db::connect(&config.database_url).await.context("open database")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match action {
        OwnerAction::Add { pubkey, label } => {
            let hex =
                allod_api::normalize_pubkey(&pubkey).map_err(|e| anyhow::anyhow!(e.message))?;
            if db.add_owner(&hex, &label, now).await? {
                db.audit("owner_added", None, &format!(r#"{{"pubkey":"{hex}","by":"cli"}}"#), now)
                    .await?;
                println!("added owner {hex}");
            } else {
                println!("{hex} is already an owner");
            }
        }
        OwnerAction::List => {
            for o in db.owners().await? {
                println!("{}  {}", o.pubkey, o.label);
            }
        }
        OwnerAction::Remove { pubkey } => {
            let hex =
                allod_api::normalize_pubkey(&pubkey).map_err(|e| anyhow::anyhow!(e.message))?;
            if db.remove_owner(&hex).await? {
                db.audit(
                    "owner_removed",
                    None,
                    &format!(r#"{{"pubkey":"{hex}","by":"cli"}}"#),
                    now,
                )
                .await?;
                println!("removed owner {hex}");
            } else {
                println!("{hex} is not an owner");
            }
        }
    }
    db.close().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
    tracing::info!("shutdown requested");
}

/// Minimal HTTP/1.1 GET without extra dependencies, so distroless images can
/// run their own health check.
async fn healthcheck(listen: std::net::SocketAddr) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = if listen.ip().is_unspecified() {
        std::net::SocketAddr::new([127, 0, 0, 1].into(), listen.port())
    } else {
        listen
    };
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .context("connect timeout")??;
    stream
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut buf))
        .await
        .context("read timeout")??;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
    // Chunked encoding may wrap the JSON; find the object.
    let json = body.find('{').map(|i| &body[i..]).unwrap_or(body);
    let json = json.rfind('}').map(|i| &json[..=i]).unwrap_or(json);
    let health: allod_types::HealthResponse =
        serde_json::from_str(json).context("parse /api/health")?;
    println!("{}", serde_json::to_string(&health)?);
    if health.status == allod_types::HealthStatus::Down {
        anyhow::bail!("allod is down");
    }
    Ok(())
}
