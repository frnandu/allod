//! Allod's own layer at a glance, for the top card of the Node page.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::auth::{unix_now, Auth};
use crate::AppState;

/// Payments are summed over this window.
const ACTIVITY_WINDOW_SECS: i64 = 24 * 3600;

/// `GET /api/allod/status`: NWC relay connections, payments of the last 24 hours, paid
/// hold invoices waiting for their receiver, push devices, and the database. Each part is
/// `null` when its service has not started (the wallet waits for the Lightning node) or
/// could not answer; the others are still filled in.
pub async fn status(State(state): State<AppState>, _auth: Auth) -> Json<Value> {
    let now = unix_now();
    let wallet = state.wallet.get().cloned();
    let (activity, held) = match &wallet {
        Some(w) => {
            let (activity, held) =
                tokio::join!(w.activity(now - ACTIVITY_WINDOW_SECS), w.held_invoices());
            (ok("payment activity", activity), ok("held invoices", held))
        }
        None => (None, None),
    };
    let relays = state.apps.get().map(|apps| {
        json!({
            "configured": apps.relays(),
            "states": apps.relay_states(),
        })
    });
    let push = match state.push.get() {
        Some(push) => ok("push subscriptions", push.list().await).map(|subs| {
            let devices = subs.iter().filter(|s| s.owner_pubkey.is_some());
            json!({
                "devices": devices.clone().count(),
                "failing": devices.clone().filter(|s| s.failures > 0).count(),
                "last_success_at": devices.filter_map(|s| s.last_success_at).max(),
                "app_endpoints": subs.iter().filter(|s| s.app_id.is_some()).count(),
            })
        }),
        None => None,
    };
    let (size, litestream) = tokio::join!(state.db.size_bytes(), state.db.litestream_replicated());
    Json(json!({
        "relays": relays,
        "activity": activity,
        "held_invoices": held,
        "push": push,
        "database": {
            "backend": state.db.backend_name(),
            "size_bytes": ok("database size", size),
            "litestream": ok("litestream check", litestream),
        },
    }))
}

/// The value, or `None` with a warning: one failing part must not hide the others.
fn ok<T, E: std::fmt::Display>(what: &str, r: Result<T, E>) -> Option<T> {
    r.map_err(|e| tracing::warn!(error = %e, "allod status: {what}")).ok()
}
