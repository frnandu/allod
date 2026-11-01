//! HTTP API (SPEC §11). M0 serves `/api/info`, `/api/health` and the embedded web UI.

mod aliases;
pub mod auth;
mod error;
mod routes;
mod state;
mod ui;

use allod_types::{Check, HealthResponse, HealthStatus, InfoResponse};
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub use auth::AuthState;
pub use routes::owners::normalize_pubkey;
pub use state::{AppState, GossipMonitor, LdkSource};

/// Builds the router.
pub fn router(state: AppState) -> Router {
    // Routes that need a completed setup (SPEC §3.3).
    let after_setup = Router::new()
        .route("/api/owners", get(routes::owners::list).post(routes::owners::add))
        .route("/api/owners/{pubkey}", delete(routes::owners::remove))
        .route("/api/tokens", get(routes::tokens::list).post(routes::tokens::create))
        .route("/api/tokens/{id}", delete(routes::tokens::delete))
        .route("/api/recovery-phrase", post(routes::setup::reveal))
        .route("/api/settings", get(routes::settings::get).patch(routes::settings::patch))
        .route("/api/fiat/currencies", get(routes::settings::currencies))
        .route("/api/fiat/rate", get(routes::settings::rate))
        // node (M2)
        .route("/api/node", get(routes::node::info))
        .route("/api/node/gossip", get(routes::node::gossip))
        .route("/api/fees", get(routes::node::fees))
        .route("/api/balances", get(routes::node::balances))
        .route("/api/channels", get(routes::node::channels).post(routes::node::open))
        .route("/api/channels/{id}", patch(routes::node::update).delete(routes::node::close))
        .route("/api/channels/{id}/splice-in", post(routes::node::splice_in))
        .route("/api/channels/{id}/splice-out", post(routes::node::splice_out))
        .route("/api/peers", get(routes::node::peers).post(routes::node::connect))
        .route("/api/peers/{pubkey}", delete(routes::node::disconnect))
        .route("/api/onchain/address", post(routes::node::new_address))
        .route("/api/onchain/send", post(routes::node::onchain_send))
        .route("/api/onchain/bump-fee", post(routes::node::bump_fee))
        .route("/api/onchain/transactions", get(routes::node::onchain_transactions))
        .route("/api/forwards", get(routes::node::forwards))
        .route("/api/sign-message", post(routes::node::sign_message))
        .route("/api/verify-message", post(routes::node::verify_message))
        // optional fast sync (AssumeUTXO) for the bundled bitcoind
        .route(
            "/api/sync/fast",
            get(routes::fastsync::get)
                .post(routes::fastsync::request)
                .delete(routes::fastsync::cancel),
        )
        // Nostr contacts
        .route("/api/contacts", get(routes::contacts::list))
        .route("/api/profiles", get(routes::contacts::profiles))
        // connections (M4)
        .route("/api/apps", get(routes::apps::list).post(routes::apps::create))
        .route(
            "/api/apps/{id}",
            get(routes::apps::get).patch(routes::apps::update).delete(routes::apps::delete),
        )
        .route("/api/apps/{id}/regenerate", post(routes::apps::regenerate))
        // wallet (M3)
        .route("/api/transactions", get(routes::wallet::list))
        .route("/api/transactions/{id}", get(routes::wallet::get))
        .route("/api/transactions/{id}/labels", patch(routes::wallet::labels))
        .route("/api/payments", post(routes::wallet::pay))
        .route("/api/payments/decode", post(routes::wallet::decode))
        .route("/api/invoices", post(routes::wallet::invoice))
        .route("/api/offers", get(routes::wallet::offers).post(routes::wallet::new_offer))
        .route("/api/offers/{id}", patch(routes::wallet::offer_state))
        .route("/api/receive", post(routes::wallet::receive))
        .route("/api/hold-invoices", post(routes::wallet::hold))
        .route("/api/hold-invoices/settle", post(routes::wallet::hold_settle))
        .route("/api/hold-invoices/cancel", post(routes::wallet::hold_cancel))
        .route("/api/transfers", post(routes::wallet::transfer))
        .route("/api/ledger/check", get(routes::wallet::ledger_check))
        .route("/api/events/ticket", post(routes::wallet::ticket))
        .route("/api/events", get(routes::wallet::events))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), require_setup));

    Router::new()
        .route("/api/info", get(info))
        .route("/api/health", get(health))
        .route("/api/setup", post(routes::setup::run))
        .route("/api/setup/progress", get(routes::setup::progress))
        .route("/api/auth/challenge", get(routes::auth::challenge))
        .route("/api/auth/login", post(routes::auth::login))
        .route("/api/auth/logout", post(routes::auth::logout))
        .route("/api/auth/session", get(routes::auth::session))
        .merge(after_setup)
        .fallback(ui::serve)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state, limit_public_auth))
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; connect-src 'self' wss: ws:; img-src 'self' data: https:; \
                 style-src 'self' 'unsafe-inline'; font-src 'self'; frame-ancestors 'none'",
            ),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
}

async fn limit_public_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    use auth::PublicEndpoint;
    let endpoint = match (req.method(), req.uri().path()) {
        (&Method::GET | &Method::HEAD, "/api/auth/challenge") => Some(PublicEndpoint::Challenge),
        (&Method::POST, "/api/auth/login") => Some(PublicEndpoint::Login),
        (&Method::POST, "/api/setup") => Some(PublicEndpoint::Setup),
        _ => None,
    };
    if let Some(endpoint) = endpoint {
        let source = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|address| address.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        if !state.auth.allow_public(endpoint, source) {
            return error::ApiError::too_many("too many requests").into_response();
        }
    }
    next.run(req).await
}

async fn require_setup(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if !state.setup.check_complete().await {
        return error::ApiError::new(StatusCode::CONFLICT, "SETUP_REQUIRED", "finish setup first")
            .into_response();
    }
    next.run(req).await
}

async fn info(State(state): State<AppState>) -> Json<InfoResponse> {
    let network = match state.ldk.backend().await {
        Ok(backend) => backend.node_info().await.ok().and_then(|i| i.network),
        Err(_) => None,
    };
    Json(InfoResponse {
        version: state.version.clone(),
        setup_completed: state.setup.check_complete().await,
        running: true,
        backend_type: "ldk-server".into(),
        network,
        deployment: state.deployment.clone(),
    })
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let db = match state.db.ping().await {
        Ok(()) => Check::pass(state.db.backend_name()),
        Err(e) => Check::fail(e.to_string()),
    };
    let (ldk_server, node) = match state.ldk.backend().await {
        Ok(backend) => match backend.node_info().await {
            Ok(info) => (Check::pass(format!("ldk-server {}", info.version)), Some(info.summary)),
            Err(e) => (Check::fail(e.to_string()), None),
        },
        Err(e) => (Check::fail(e), None),
    };
    let status = if !db.ok {
        HealthStatus::Down
    } else if !ldk_server.ok {
        HealthStatus::Degraded
    } else {
        HealthStatus::Ok
    };
    let code =
        if status == HealthStatus::Down { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK };
    (code, Json(HealthResponse { status, db, ldk_server, node }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use allod_types::{DeploymentInfo, DeploymentMode, Platform};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = allod_db::Db::connect(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let setup = std::sync::Arc::new(
            allod_core::SetupService::new(
                db.clone(),
                allod_core::SetupConfig {
                    mode: DeploymentMode::Standalone,
                    secrets_dir: dir.path().join("secrets"),
                    handoff_dir: dir.path().join("handoff"),
                    managed_ldk_address: "ldk-server:3536".into(),
                    network: None,
                    bitcoind: None,
                },
            )
            .await
            .unwrap(),
        );
        let state = AppState {
            version: "test".into(),
            db,
            ldk: LdkSource::new(None, Some(setup.clone()), None),
            setup,
            fiat: std::sync::Arc::new(allod_core::FiatService::new()),
            wallet: Default::default(),
            apps: Default::default(),
            contacts: Default::default(),
            fast_sync: None,
            gossip: std::sync::Arc::new(GossipMonitor::new(None)),
            auth: std::sync::Arc::new(AuthState::new()),
            public_url: None,
            deployment: DeploymentInfo {
                mode: DeploymentMode::Standalone,
                platform: Platform::Docker,
                chain_mode: None,
                node_explorer_url: None,
            },
        };
        (state, dir)
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn health_is_degraded_without_ldk_server() {
        let (state, _dir) = test_state().await;
        let (status, body) = get_json(router(state), "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "degraded");
        assert_eq!(body["db"]["ok"], true);
        assert_eq!(body["ldk_server"]["ok"], false);
    }

    #[tokio::test]
    async fn info_reports_deployment() {
        let (state, _dir) = test_state().await;
        let (status, body) = get_json(router(state), "/api/info").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["backend_type"], "ldk-server");
        assert_eq!(body["deployment"]["mode"], "standalone");
        assert_eq!(body["setup_completed"], false);
    }

    #[tokio::test]
    async fn unknown_paths_serve_the_spa_with_security_headers() {
        let (state, _dir) = test_state().await;
        let resp = router(state)
            .oneshot(Request::get("/wallet/send").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::X_FRAME_OPTIONS], "DENY");
        assert!(resp.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    }

    #[tokio::test]
    async fn unknown_api_paths_are_404() {
        let (state, _dir) = test_state().await;
        let resp = router(state)
            .oneshot(Request::get("/api/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
