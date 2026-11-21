//! API tests for domains, DNS providers, gateways and their addresses (SPEC §12.1,
//! §12.7) and LNURL on served domains (§12), against the mock node.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::json;
use tower::ServiceExt;

async fn tls_ask(h: &H, domain: &str) -> StatusCode {
    let req =
        Request::get(format!("/api/internal/tls-ask?domain={domain}")).body(Body::empty()).unwrap();
    h.app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn domains_and_their_addresses() {
    let h = harness().await;
    let (s, v) = h.req("GET", "/api/domains", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["domains"], json!([]));

    let (s, _) = h.req("POST", "/api/domains", Some(json!({"name": "1.2.3.4"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) =
        h.req("POST", "/api/domains", Some(json!({"name": "https://Pay.Example.com/"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let d = v["domains"][0]["id"].as_i64().unwrap();
    assert_eq!(v["domains"][0]["name"], "pay.example.com");
    assert_eq!(v["domains"][0]["served"], false);
    let (s, _) = h.req("POST", "/api/domains", Some(json!({"name": "pay.example.com"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    let (_, r) = h.req("POST", "/api/accounts", Some(json!({"name": "Kid"}))).await;
    let kid = r["account"]["id"].as_i64().unwrap();
    let path = format!("/api/domains/{d}/addresses");
    let (s, v) = h.req("POST", &path, Some(json!({"username": "Me", "target": "main"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) =
        h.req("POST", &path, Some(json!({"username": "kid", "target": kid.to_string()}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) =
        h.req("POST", &path, Some(json!({"username": "me", "target": kid.to_string()}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "taken on this domain");
    let (s, _) = h.req("POST", &path, Some(json!({"username": "admin", "target": "main"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "reserved");
    let addrs = v["domains"][0]["addresses"].as_array().unwrap().clone();
    assert_eq!(addrs.len(), 2);
    assert_eq!(addrs[0]["address"], "me@pay.example.com");
    assert_eq!(addrs[0]["account_name"], "Main");
    assert_eq!(addrs[1]["account_name"], "Kid");
    assert!(addrs[1]["txt"].as_str().unwrap().starts_with("bitcoin:?lno=lno1"));
    assert_eq!(v["domains"][0]["records"].as_array().unwrap().len(), 2);

    // A closed account's address goes away.
    let (s, _) = h.req("DELETE", &format!("/api/accounts/{kid}"), None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = h.req("POST", "/api/domains/check", None).await;
    assert_eq!(v["domains"][0]["addresses"].as_array().unwrap().len(), 1);
    let me = v["domains"][0]["addresses"][0]["id"].as_i64().unwrap();
    let (s, v) = h.req("DELETE", &format!("/api/addresses/{me}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["domains"][0]["addresses"], json!([]));
    let (s, v) = h.req("DELETE", &format!("/api/domains/{d}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["domains"], json!([]));
    let (s, _) = h.req_as("not-a-session", "GET", "/api/domains", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn prism_splits_of_an_address() {
    let h = harness().await;
    let (_, v) = h.req("POST", "/api/domains", Some(json!({"name": "pay.example.com"}))).await;
    let d = v["domains"][0]["id"].as_i64().unwrap();
    let (_, v) = h
        .req(
            "POST",
            &format!("/api/domains/{d}/addresses"),
            Some(json!({"username": "sats", "target": "main"})),
        )
        .await;
    let me = v["domains"][0]["addresses"][0]["id"].as_i64().unwrap();
    let mut other = Vec::new();
    for username in ["bob", "carol"] {
        let (_, v) = h
            .req(
                "POST",
                &format!("/api/domains/{d}/addresses"),
                Some(json!({"username": username, "target": "main"})),
            )
            .await;
        let id = v["domains"][0]["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["username"] == username)
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        other.push(id);
    }
    // A plain name is checked when saving: our own names are known from their settings.
    // Bob keeps both, so he is BIP-353; carol answers LNURL only.
    let (s, _) = h
        .req("PATCH", &format!("/api/addresses/{}", other[1]), Some(json!({"bip353": false})))
        .await;
    assert_eq!(s, StatusCode::OK);

    // A new address forwards nothing.
    let path = format!("/api/addresses/{me}/prism");
    let (s, v) = h.req("GET", &path, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["splits"], json!([]));
    assert_eq!(v["split_bps"], 0);
    assert_eq!(v["stats"]["failed_msat"], 0);
    // The menus read the failures across every address from one small call.
    let (s, v) = h.req("GET", "/api/prism/failures", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["failed_count"], 0);
    assert_eq!(v["failed_msat"], 0);
    assert_eq!(v["addresses"], json!([]));
    let (s, _) = h.req_as("not-a-session", "GET", "/api/prism/failures", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // The splits come back in the domains view, which the settings page reloads.
    let (s, v) = h
        .req(
            "PUT",
            &path,
            Some(json!({"splits": [
                {"destination": "bob@pay.example.com", "percent_bps": 6_000, "note": "Bob's node"},
                {"destination": "carol@pay.example.com", "percent_bps": 2_500},
            ]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let prism = &v["domains"][0]["addresses"][0]["prism"];
    assert_eq!(prism["split_bps"], 8_500);
    assert_eq!(prism["splits"][0]["destination"], "bob@pay.example.com");
    assert_eq!(prism["splits"][0]["kind"], "bip353");
    assert_eq!(prism["splits"][0]["note"], "Bob's node");
    assert_eq!(prism["splits"][1]["kind"], "lnurl");
    assert_eq!(prism["splits"][1]["percent_bps"], 2_500);
    assert_eq!(prism["splits"][1]["note"], json!(null));

    // A caller that does not send notes keeps the one stored for that destination.
    let (s, v) = h
        .req(
            "PUT",
            &path,
            Some(json!({"splits": [
                {"destination": "bob@pay.example.com", "percent_bps": 6_000},
                {"destination": "carol@pay.example.com", "percent_bps": 2_500},
            ]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["domains"][0]["addresses"][0]["prism"]["splits"][0]["note"], "Bob's node");

    // The payout history starts empty, newest first, and pages by the last id.
    let (s, v) = h.req("GET", &format!("{path}/payouts"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["payouts"], json!([]));
    assert_eq!(v["next_before_id"], json!(null));
    let (s, _) = h.req("GET", "/api/addresses/99999/prism/payouts", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.req_as("not-a-session", "GET", &format!("{path}/payouts"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Sending the id of a stored split edits it in place, so its note and history stay.
    let (s, v) = h.req("GET", &format!("/api/addresses/{me}/prism"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob_id = v["splits"][0]["id"].as_i64().expect("splits carry their id");
    let (s, v) = h
        .req(
            "PUT",
            &path,
            Some(json!({"splits": [
                {"id": bob_id, "destination": "bob@pay.example.com", "percent_bps": 6_000,
                 "note": "Bob's node"},
                {"destination": "carol@pay.example.com", "percent_bps": 2_500},
            ]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["domains"][0]["addresses"][0]["prism"]["splits"][0]["id"], bob_id);
    // A split that no longer exists is refused rather than silently added.
    let (s, _) = h
        .req(
            "PUT",
            &path,
            Some(json!({"splits": [
                {"id": 99999, "destination": "bob@pay.example.com", "percent_bps": 6_000},
            ]})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Clearing a split's failed total is harmless when nothing failed.
    let (s, v) = h.req("POST", &format!("{path}/reset"), Some(json!({"split_id": bob_id}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) =
        h.req("POST", "/api/addresses/99999/prism/reset", Some(json!({"split_id": bob_id}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.req("POST", &format!("{path}/reset"), Some(json!({"split_id": 99999}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .req_as(
            "not-a-session",
            "POST",
            &format!("{path}/reset"),
            Some(json!({"split_id": bob_id})),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // More than 100% is refused, and so is something that is not a Lightning target.
    let (s, _) = h
        .req(
            "PUT",
            &path,
            Some(json!({"splits": [
                {"destination": "bob@pay.example.com", "percent_bps": 9_000},
                {"destination": "carol@pay.example.com", "percent_bps": 2_000},
            ]})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .req(
            "PUT",
            &path,
            Some(json!({"splits": [{"destination": "nonsense", "percent_bps": 100}]})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Nothing failed yet, so a retry queues nothing.
    let (s, v) = h.req("POST", &format!("{path}/retry"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = h.req("GET", "/api/addresses/99999/prism", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Clearing them keeps the address, without splits.
    let (s, v) = h.req("PUT", &path, Some(json!({"splits": []}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["domains"][0]["addresses"][0]["prism"]["splits"], json!([]));
}

#[tokio::test]
async fn certificates_only_for_served_domains() {
    let h = harness().await;
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::NOT_FOUND);
    let (_, v) = h.req("POST", "/api/domains", Some(json!({"name": "pay.example.com"}))).await;
    let d = v["domains"][0]["id"].as_i64().unwrap();
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::NOT_FOUND, "BIP-353 only");
    let (s, v) = h.req("PATCH", &format!("/api/domains/{d}"), Some(json!({"direct": true}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["domains"][0]["served"], true);
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::OK);
    assert_eq!(tls_ask(&h, "evil.example.com").await, StatusCode::NOT_FOUND);

    // Through a gateway instead.
    let (s, v) = h
        .req(
            "POST",
            "/api/gateways",
            Some(json!({"provider": "manual", "ipv4": "203.0.113.10", "label": "Dublin"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let g = v["gateways"][0]["id"].as_i64().unwrap();
    assert_eq!(v["gateways"][0]["mode"], "dedicated");
    let (_, v) = h.req("PATCH", &format!("/api/domains/{d}"), Some(json!({"gateway_id": g}))).await;
    assert_eq!(
        (v["domains"][0]["direct"].clone(), v["domains"][0]["gateway_id"].clone()),
        (json!(false), json!(g))
    );
    let a = v["domains"][0]["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type"] == "A")
        .cloned()
        .unwrap();
    assert_eq!(a["value"], "203.0.113.10");
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::OK);
    let (s, _) = h.req("DELETE", &format!("/api/gateways/{g}"), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "in use");

    // Its setup script, for the owner only.
    let req = Request::get(format!("/api/gateways/{g}/setup"))
        .header("authorization", format!("Bearer {}", h.token))
        .body(Body::empty())
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let script = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        script.starts_with("#!/bin/sh")
            && script.contains(&format!(
                "dnat to {}:443",
                v["gateways"][0]["tunnel"]["home"].as_str().unwrap()
            ))
    );
    let (s, _) = h.req_as("not-a-session", "GET", &format!("/api/gateways/{g}/setup"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (_, v) =
        h.req("PATCH", &format!("/api/domains/{d}"), Some(json!({"gateway_id": null}))).await;
    assert_eq!(v["domains"][0]["served"], false);
    assert_eq!(tls_ask(&h, "pay.example.com").await, StatusCode::NOT_FOUND);
    let (s, _) = h.req("DELETE", &format!("/api/gateways/{g}"), None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn bad_providers_and_gateways() {
    let h = harness().await;
    let (s, _) = h
        .req("POST", "/api/dns-providers", Some(json!({"kind": "cloudflare", "token": "x"})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) =
        h.req("POST", "/api/gateways", Some(json!({"provider": "manual", "ipv4": "nope"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.req("POST", "/api/gateways", Some(json!({"provider": "manual"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .req(
            "POST",
            "/api/gateways",
            Some(json!({"provider": "lnvps", "max_sats": 1, "auto_renew": true})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "auto renewal needs a cap");
    let (s, _) = h
        .req("POST", "/api/domains", Some(json!({"name": "x.example.com", "gateway_id": 99})))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// GET with a `Host` header (LNURL answers by domain).
async fn lnurl_get(h: &H, host: &str, path: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::get(path).header("host", host).body(Body::empty()).unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cors = res.headers().get("access-control-allow-origin").cloned();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    assert_eq!(cors.as_ref().and_then(|c| c.to_str().ok()), Some("*"), "LUD-01 CORS");
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test]
async fn lightning_addresses_answer_lnurl() {
    use nostr::prelude::*;
    use sha2::{Digest, Sha256};

    let h = harness().await;
    let (_, v) = h
        .req("POST", "/api/domains", Some(json!({"name": "pay.example.com", "direct": true})))
        .await;
    let d = v["domains"][0]["id"].as_i64().unwrap();
    h.req(
        "POST",
        &format!("/api/domains/{d}/addresses"),
        Some(json!({"username": "sats", "target": "main"})),
    )
    .await;
    let (_, r) = h
        .req("POST", "/api/accounts", Some(json!({"name": "Alice", "lud16_username": "alice"})))
        .await;
    let alice = r["account"]["id"].as_i64().unwrap();

    // The payRequest, only for this domain and its usernames.
    let (s, p) = lnurl_get(&h, "pay.example.com", "/.well-known/lnurlp/sats").await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["tag"], "payRequest");
    assert_eq!(p["callback"], "https://pay.example.com/lnurlp/sats/callback");
    assert_eq!(p["allowsNostr"], true);
    assert_eq!(p["nostrPubkey"].as_str().unwrap().len(), 64);
    let metadata = p["metadata"].as_str().unwrap().to_string();
    assert!(metadata.contains(r#"["text/identifier","sats@pay.example.com"]"#), "{metadata}");
    let (s, e) = lnurl_get(&h, "other.example.com", "/.well-known/lnurlp/sats").await;
    assert_eq!((s, e["status"].clone()), (StatusCode::NOT_FOUND, json!("ERROR")));
    let (s, _) = lnurl_get(&h, "pay.example.com:443", "/.well-known/lnurlp/nobody").await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // The callback: an invoice owned by the address's account, committing to the metadata.
    let (s, c) =
        lnurl_get(&h, "pay.example.com", "/lnurlp/alice/callback?amount=21000&comment=hi").await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["routes"], json!([]));
    let pr = c["pr"].as_str().unwrap();
    let (_, d) = h.req("POST", "/api/payments/decode", Some(json!({"payment": pr}))).await;
    assert_eq!(d["amount_msat"], 21_000, "{d}");
    let (_, ap) = lnurl_get(&h, "pay.example.com", "/.well-known/lnurlp/alice").await;
    let hash = hex::encode(Sha256::digest(ap["metadata"].as_str().unwrap().as_bytes()));
    let (_, txs) =
        h.req("GET", &format!("/api/transactions?account={alice}&include_unpaid=true"), None).await;
    let tx = txs["items"][0].clone();
    assert_eq!(tx["description_hash"], hash, "{tx}");
    assert_eq!(tx["description"], "hi");

    // LUD-21 verify: unpaid; and not through another address.
    let verify = c["verify"].as_str().unwrap().trim_start_matches("https://pay.example.com");
    let (s, v) = lnurl_get(&h, "pay.example.com", verify).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["status"].clone(), v["settled"].clone()), (json!("OK"), json!(false)));
    let (s, _) = lnurl_get(&h, "pay.example.com", &verify.replace("/alice/", "/sats/")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Bad amounts and a zap request for another amount are refused.
    let (s, _) = lnurl_get(&h, "pay.example.com", "/lnurlp/sats/callback?amount=1").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let oversized =
        format!("/lnurlp/sats/callback?amount=1000&payerdata={}", "x".repeat(24 * 1024));
    let (s, e) = lnurl_get(&h, "pay.example.com", &oversized).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(e["reason"], "callback query is too large");
    let keys = Keys::generate();
    let zap = EventBuilder::new(Kind::ZapRequest, "")
        .tags([
            Tag::public_key(Keys::generate().public_key()),
            Tag::parse(["relays", "wss://relay.example"]).unwrap(),
            Tag::parse(["amount", "5000"]).unwrap(),
        ])
        .finalize(&keys)
        .unwrap()
        .as_json();
    let enc = |s: &str| s.bytes().map(|b| format!("%{b:02X}")).collect::<String>();
    let (s, _) = lnurl_get(
        &h,
        "pay.example.com",
        &format!("/lnurlp/sats/callback?amount=21000&nostr={}", enc(&zap)),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, z) = lnurl_get(
        &h,
        "pay.example.com",
        &format!("/lnurlp/sats/callback?amount=5000&nostr={}", enc(&zap)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{z}");
    let (_, txs) = h.req("GET", "/api/transactions?account=main&include_unpaid=true", None).await;
    let list = txs["items"].as_array().cloned().unwrap_or_default();
    let zap_tx =
        list.iter().find(|t| t["payment_request"] == z["pr"]).expect("zap invoice on Main");
    assert_eq!(zap_tx["description_hash"], hex::encode(Sha256::digest(zap.as_bytes())));
}

#[tokio::test]
async fn lightning_through_a_gateway() {
    let h = harness().await;
    let (s, v) = h.req("GET", "/api/settings/lightning", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["available"].as_bool(), v["gateway_id"].is_null()), (Some(true), true));
    assert_eq!(v["gateways"], json!([]));

    let (_, v) = h
        .req("POST", "/api/gateways", Some(json!({"provider": "manual", "ipv4": "127.0.0.1"})))
        .await;
    let g = v["gateways"][0]["id"].as_i64().unwrap();
    let (s, _) =
        h.req("PUT", "/api/settings/lightning", Some(json!({"gateway_id": g, "port": 22}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "SSH port");
    // Nothing listens on 127.0.0.1:9 here: saved, but not reachable.
    let (s, v) =
        h.req("PUT", "/api/settings/lightning", Some(json!({"gateway_id": g, "port": 9}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["address"], "127.0.0.1:9");
    assert_eq!(v["reach"]["state"], "unreachable", "{v}");
    assert!(v["node"]["pubkey"].is_string(), "{v}");
    assert_eq!(v["announced"], false);
    let (s, _) = h.req("DELETE", &format!("/api/gateways/{g}"), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "Lightning still comes through it");

    let (s, v) = h.req("PUT", "/api/settings/lightning", Some(json!({"gateway_id": null}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["gateway_id"].is_null() && v["reach"].is_null(), "{v}");
    let (s, _) = h.req("DELETE", &format!("/api/gateways/{g}"), None).await;
    assert_eq!(s, StatusCode::OK);
}
