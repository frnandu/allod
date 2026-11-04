//! Settings: chain source of the managed ldk-server (SPEC §2.3.2).

mod common;

use allod_types::ChainMode;
use axum::http::StatusCode;
use common::*;
use serde_json::{json, Value};

const PATH: &str = "/api/settings/chain-source";

/// What ldk-server's entrypoint writes after starting with the request.
fn node_restarted(h: &H, mode: &str, kind: &str, extra: Value) {
    let handoff = h.dir.path().join("handoff");
    let req: Value =
        serde_json::from_str(&std::fs::read_to_string(handoff.join("chain-source.json")).unwrap())
            .unwrap();
    let mut active = json!({"mode": mode, "kind": kind, "request_id": req["id"]});
    active.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    std::fs::write(handoff.join("chain-source.active.json"), active.to_string()).unwrap();
}

#[tokio::test]
async fn switches_from_bundled_to_esplora_and_back() {
    let h = harness_chain(ChainMode::Bundled).await;
    let (st, v) = h.req("GET", PATH, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["editable"], true);
    assert_eq!(v["default_mode"], "bundled");
    assert!(v["presets"].as_array().unwrap().iter().any(|p| p["id"] == "alby-esplora"));

    let (st, v) = h.req("PUT", PATH, Some(json!({"kind": "electrum", "url": "https://x"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

    let (st, v) = h
        .req("PUT", PATH, Some(json!({"kind": "esplora", "url": "https://mempool.space/api/"})))
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["requested"], json!({"kind": "esplora", "url": "https://mempool.space/api"}));
    assert_eq!(v["restart_pending"], true);

    node_restarted(&h, "esplora", "esplora", json!({"url": "https://mempool.space/api"}));
    let (_, v) = h.req("GET", PATH, None).await;
    assert_eq!(v["restart_pending"], false);
    assert_eq!(v["active"]["mode"], "esplora");
    let (_, info) = h.req("GET", "/api/info", None).await;
    assert_eq!(info["deployment"]["chain_mode"], "esplora");

    // Back to the stack's own bitcoind.
    let (st, v) = h.req("PUT", PATH, Some(json!({"kind": "default"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["restart_pending"], true);
}

#[tokio::test]
async fn external_bitcoind_hides_the_password() {
    let h = harness_chain(ChainMode::Electrum).await;
    let body = json!({"kind": "bitcoind", "rpc_address": "10.0.0.5:8332", "rpc_user": "ldk"});
    let (st, _) = h.req("PUT", PATH, Some(body.clone())).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "password required");
    let mut with_pass = body.clone();
    with_pass["rpc_password"] = json!("s3cret");
    let (st, v) = h.req("PUT", PATH, Some(with_pass)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        v["requested"],
        json!({"kind": "bitcoind", "rpc_address": "10.0.0.5:8332", "rpc_user": "ldk"})
    );
    assert!(!v.to_string().contains("s3cret"));
    // Re-saving the same node keeps the stored password.
    let (st, _) = h.req("PUT", PATH, Some(body)).await;
    assert_eq!(st, StatusCode::OK);
    let stored = std::fs::read_to_string(h.dir.path().join("handoff/chain-source.json")).unwrap();
    assert!(stored.contains("s3cret"));
}

#[tokio::test]
async fn starts_the_standby_bitcoind() {
    // No standby node in this stack: nothing to offer or start.
    let h = harness_chain(ChainMode::ExternalBitcoind).await;
    let (_, v) = h.req("GET", PATH, None).await;
    assert_eq!(v["standby"], Value::Null);
    let (st, _) = h.req("POST", &format!("{PATH}/standby"), None).await;
    assert_eq!(st, StatusCode::CONFLICT);

    let h = harness_standby(ChainMode::ExternalBitcoind).await;
    let (_, v) = h.req("GET", PATH, None).await;
    assert_eq!(v["standby"]["enabled"], false);
    assert_eq!(v["standby"]["in_use"], false);
    assert_eq!(v["standby"]["suggested"], false, "the external node was never unreachable");

    let (st, v) = h.req("POST", &format!("{PATH}/standby"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(h.dir.path().join("control/bitcoind-enabled").exists(), "container told to start");
    assert_eq!(v["standby"]["enabled"], true);
    assert_eq!(v["standby"]["in_use"], true);
    assert_eq!(
        v["requested"],
        json!({"kind": "bitcoind", "rpc_address": "allod_bitcoind_1:8332", "rpc_user": "ldk", "standby": true})
    );
    assert!(!v.to_string().contains("standby-pass"));
    assert_eq!(v["restart_pending"], true);
    let stored = std::fs::read_to_string(h.dir.path().join("handoff/chain-source.json")).unwrap();
    assert!(stored.contains("standby-pass"), "ldk-server gets the RPC password");

    node_restarted(
        &h,
        "external-bitcoind",
        "bitcoind",
        json!({"rpc_address": "allod_bitcoind_1:8332"}),
    );
    let (_, v) = h.req("GET", PATH, None).await;
    assert_eq!(v["restart_pending"], false);
}

#[tokio::test]
async fn fast_sync_follows_the_standby_bitcoind() {
    let h = harness_standby(ChainMode::ExternalBitcoind).await;
    // The node still reads from the external node: no fast sync to offer.
    let (_, v) = h.req("GET", "/api/sync/fast", None).await;
    assert_eq!(v["available"], false);
    let snapshot = json!({"url": "https://mirror.example/utxo-935000.dat"});
    let (st, _) = h.req("POST", "/api/sync/fast", Some(snapshot.clone())).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);

    let (st, _) = h.req("POST", &format!("{PATH}/standby"), None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = h.req("GET", "/api/sync/fast", None).await;
    assert_eq!(v["available"], true, "{v}");
    let (st, v) = h.req("POST", "/api/sync/fast", Some(snapshot)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(h.dir.path().join("control/fastsync-request.json").exists(), "helper asked");

    // Back to the external node: not offered any more.
    let (st, _) = h.req("PUT", PATH, Some(json!({"kind": "default"}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = h.req("GET", "/api/sync/fast", None).await;
    assert_eq!(v["available"], false);
}
