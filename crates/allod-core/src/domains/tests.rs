use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use super::*;
use crate::apps::NewApp;

/// The mock Njalla wallet and registrations: (balance €, owned domains, top-ups asked).
static NJALLA_ACCOUNT: Mutex<(u64, Vec<String>, u32)> = Mutex::new((0, Vec::new(), 0));

/// A stand-in Njalla API keeping records in memory; checks the token.
async fn njalla_mock() -> (Arc<Mutex<Vec<Value>>>, String) {
    let recs: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![
        // A record the owner made: the A record Allod sets replaces it.
        json!({"id": 1, "name": "pay", "type": "A", "content": "198.51.100.1", "ttl": 3600}),
        json!({"id": 2, "name": "@", "type": "MX", "content": "mail.example", "ttl": 3600}),
    ]));
    let r2 = recs.clone();
    let app = Router::new().route(
        "/api/1/",
        post(move |headers: HeaderMap, Json(b): Json<Value>| {
            let recs = r2.clone();
            async move {
                if b["method"] != "add-token"
                    && headers.get("authorization").and_then(|v| v.to_str().ok())
                        != Some("Njalla good-token")
                {
                    return Json(json!({"error": {"code": 403, "message": "Permission denied"}}));
                }
                let p = &b["params"];
                // The website's own call: a logged-in session and its CSRF header.
                if b["method"] == "add-token" {
                    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
                    let csrf = headers.get("x-csrftoken").and_then(|v| v.to_str().ok());
                    if !cookie.contains("sessionid=ok") || csrf != Some("csrf-1") {
                        return Json(json!({"error": {"code": 403, "message": "Permission denied"}}));
                    }
                    assert_eq!(p["comment"], "Allod");
                    return Json(json!({"result": {"comment": "Allod", "key": "good-token"}}));
                }
                // Search, wallet and registration.
                {
                    let mut acct = NJALLA_ACCOUNT.lock().unwrap();
                    let result = match b["method"].as_str() {
                        Some("get-tlds") => Some(json!({".com": {"price": 15, "max_years": 10, "dnssec": true}})),
                        Some("find-domains") => {
                            let q = p["query"].as_str().unwrap_or_default().to_string();
                            let status = if q == "taken.com" { "taken" } else { "available" };
                            Some(json!({"domains": [{"name": q, "status": status, "price": 15},
                                {"name": "other.org", "status": "available", "price": 15}]}))
                        }
                        Some("get-balance") => Some(json!({"balance": acct.0})),
                        Some("add-payment") => {
                            acct.2 += 1;
                            assert_eq!(p["via"], "bitcoin");
                            Some(json!({"id": "PAY1", "amount": p["amount"], "amount_btc": "0.000250",
                                "address": "bc1qtest", "uri": "bitcoin:bc1qtest?amount=0.000250",
                                "status": "Waiting for transaction"}))
                        }
                        Some("get-payment") => Some(json!({"id": "PAY1", "status": "Waiting for transaction"})),
                        Some("register-domain") => {
                            let d = p["domain"].as_str().unwrap().to_string();
                            assert!(acct.0 >= 15, "registered only when funded");
                            acct.0 -= 15;
                            acct.1.push(d);
                            Some(json!({"task": "T1"}))
                        }
                        Some("check-task") => Some(json!({"id": "T1", "status": {"status": "done"}})),
                        Some("get-domain") => {
                            let d = p["domain"].as_str().unwrap_or_default();
                            if !acct.1.iter().any(|x| x == d) {
                                return Json(json!({"error": {"code": 404, "message": "not found"}}));
                            }
                            Some(json!({"name": d}))
                        }
                        Some("edit-domain") => Some(json!({"name": p["domain"], "dnssec": p["dnssec"]})),
                        _ => None,
                    };
                    if let Some(r) = result {
                        return Json(json!({"result": r}));
                    }
                }
                let mut recs = recs.lock().unwrap();
                match b["method"].as_str() {
                    Some("list-domains") => {
                        Json(json!({"result": {"domains": [{"name": "fmar.dev"}]}}))
                    }
                    Some("list-records") => {
                        assert_eq!(p["domain"], "fmar.dev");
                        Json(json!({"result": {"records": *recs}}))
                    }
                    Some("add-record") => {
                        let id = 100 + recs.len();
                        recs.push(json!({"id": id, "name": p["name"], "type": p["type"], "content": p["content"]}));
                        Json(json!({"result": {"id": id}}))
                    }
                    Some("remove-record") => {
                        let id = p["id"].as_str().unwrap_or_default().to_string();
                        recs.retain(|r| {
                            r["id"].as_u64().map(|n| n.to_string()).as_deref() != Some(id.as_str())
                        });
                        Json(json!({"result": {}}))
                    }
                    _ => Json(json!({"error": {"code": 400, "message": "bad method"}})),
                }
            }
        }),
    );
    // The website: sign-up form, confirmation link (logs the session in), settings page.
    let form = r#"<form method="post"><input type="hidden" name="csrfmiddlewaretoken" value="csrf-1"></form>"#;
    let app = app
        .route(
            "/signup/",
            axum::routing::get(move || async move {
                (
                    [("set-cookie", "csrftoken=csrf-1; Path=/")],
                    axum::response::Html(format!(r#"{form}<input name="password_confirm">"#)),
                )
            })
            .post(|headers: HeaderMap, body: String| async move {
                assert!(body.contains("csrfmiddlewaretoken=csrf-1"), "{body}");
                assert!(body.contains("%40uid.ovh"), "{body}");
                assert!(headers.get("referer").is_some());
                axum::response::Html("Please confirm your account to login to Njalla.".to_string())
            }),
        )
        .route(
            "/signin/",
            post(|body: String| async move {
                assert!(body.contains("cnonce="), "{body}");
                ([("set-cookie", "sessionid=ok; Path=/; HttpOnly")], "")
            }),
        )
        .route(
            "/a/{code}",
            axum::routing::get(
                |axum::extract::Path(code): axum::extract::Path<String>| async move {
                    assert_eq!(code, "ImVtYWls:1xQ:sig");
                    ([("set-cookie", "sessionid=ok; Path=/; HttpOnly")], "")
                },
            ),
        )
        .route(
            "/settings/api/",
            axum::routing::get(move |headers: HeaderMap| async move {
                let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
                // Like the real page: no CSRF field, only the `csrftoken` cookie.
                if cookie.contains("sessionid=ok") {
                    return (
                        [("set-cookie", "csrftoken=csrf-1; Path=/")],
                        axum::response::Html("<div class=\"tokenlist\"></div>".to_string()),
                    );
                }
                ([("set-cookie", "csrftoken=csrf-1; Path=/")], axum::response::Html(if false {
                    form.to_string()
                } else {
                    r#"<form id="login"><input type="hidden" name="csrfmiddlewaretoken" value="csrf-1"><input type="hidden" name="cnonce" value=""><input type="hidden" name="nonce" value="0"></form>"#.into()
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (recs, format!("http://{addr}/api/1/"))
}

/// A stand-in deSEC API keeping record sets in memory; checks the token.
async fn desec_mock() -> (Arc<Mutex<Vec<Value>>>, String) {
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    type Sets = Arc<Mutex<Vec<Value>>>;
    let sets: Sets = Arc::new(Mutex::new(vec![
        // The owner's own records: Allod's A record replaces this one, the MX stays.
        json!({"subname": "pay", "type": "A", "ttl": 3600, "records": ["198.51.100.1"]}),
        json!({"subname": "", "type": "MX", "ttl": 3600, "records": ["10 mail.example."]}),
    ]));
    fn authed(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Token good-token")
    }
    fn denied() -> axum::response::Response {
        (StatusCode::UNAUTHORIZED, Json(json!({"detail": "Invalid token."}))).into_response()
    }
    let app = Router::new()
        .route(
            "/api/v1/domains/",
            get(|h: HeaderMap| async move {
                if !authed(&h) {
                    return denied();
                }
                Json(json!([{"name": "example.org"}, {"name": "other.net"}])).into_response()
            }),
        )
        .route(
            "/api/v1/domains/{zone}/",
            get(|h: HeaderMap, Path(zone): Path<String>| async move {
                if !authed(&h) {
                    return denied();
                }
                assert_eq!(zone, "example.org");
                Json(json!({"name": zone, "keys": [{"dnskey": "257 3 13 abc", "managed": true,
                    "ds": ["13746 13 4 aaaa", "13746 13 2 5802e9a6"]}]}))
                .into_response()
            }),
        )
        .route(
            "/api/v1/domains/{zone}/rrsets/",
            get(|h: HeaderMap, State(sets): State<Sets>| async move {
                if !authed(&h) {
                    return denied();
                }
                Json(Value::Array(sets.lock().unwrap().clone())).into_response()
            })
            .patch(
                |h: HeaderMap, State(sets): State<Sets>, Json(body): Json<Value>| async move {
                    if !authed(&h) {
                        return denied();
                    }
                    let mut sets = sets.lock().unwrap();
                    for new in body.as_array().unwrap() {
                        sets.retain(|s| {
                            !(s["subname"] == new["subname"] && s["type"] == new["type"])
                        });
                        if !new["records"].as_array().unwrap().is_empty() {
                            sets.push(new.clone());
                        }
                    }
                    Json(body).into_response()
                },
            ),
        )
        .route(
            "/api/v1/domains/{zone}/rrsets/{sub}/{kind}/",
            get(
                |h: HeaderMap,
                 State(sets): State<Sets>,
                 Path((_zone, sub, kind)): Path<(String, String, String)>| async move {
                    if !authed(&h) {
                        return denied();
                    }
                    let sub = if sub == "@" { String::new() } else { sub };
                    match sets
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|s| s["subname"] == sub && s["type"] == kind)
                    {
                        Some(s) => Json(s.clone()).into_response(),
                        None => (StatusCode::NOT_FOUND, Json(json!({"detail": "Not found."})))
                            .into_response(),
                    }
                },
            ),
        )
        .with_state(sets.clone())
        // Account: captcha "ABCDE", activation code "code12345", then login and tokens.
        .route(
            "/api/v1/captcha/",
            post(|| async { Json(json!({"id": "cap-1", "challenge": "iVBORw0KGgo="})) }),
        )
        .route(
            "/api/v1/auth/",
            post(|Json(b): Json<Value>| async move {
                assert!(b["email"].as_str().unwrap().ends_with("@uid.ovh"));
                assert_eq!(b["outreach_preference"], false);
                if b["captcha"]["solution"] != "ABCDE" {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"captcha": ["Solution wrong."]})),
                    )
                        .into_response();
                }
                StatusCode::ACCEPTED.into_response()
            }),
        )
        .route(
            "/api/v1/v/activate-account/{code}/",
            post(|Path(code): Path<String>| async move {
                if code == "code12345" {
                    Json(json!({"detail": "Success!"})).into_response()
                } else {
                    (StatusCode::NOT_FOUND, Json(json!({"detail": "Not found."}))).into_response()
                }
            }),
        )
        .route(
            "/api/v1/auth/login/",
            post(|Json(b): Json<Value>| async move {
                assert_eq!(b["password"].as_str().unwrap().len(), 48);
                Json(json!({"id": "l1", "token": "login-token"}))
            }),
        )
        .route(
            "/api/v1/auth/tokens/",
            post(|h: HeaderMap, Json(b): Json<Value>| async move {
                assert_eq!(h.get("authorization").unwrap(), "Token login-token");
                assert_eq!(b["perm_create_domain"], true);
                (StatusCode::CREATED, Json(json!({"id": "t1", "token": "good-token"})))
            }),
        )
        .route("/api/v1/auth/logout/", post(|| async { StatusCode::NO_CONTENT }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (sets, format!("http://{addr}/api/v1/"))
}

struct T {
    svc: Arc<DomainService>,
    apps: Arc<AppService>,
    wallet: Arc<Wallet>,
    node: Arc<allod_ln::mock::MockBackend>,
    db: Db,
    _dir: tempfile::TempDir,
}

/// The service on a fresh database; `before` runs first (legacy state to migrate).
async fn harness<F>(cfg: DomainConfig, before: F) -> T
where
    F: AsyncFnOnce(&Db, &SecretStore, &Arc<AppService>),
{
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/d.db", dir.path().display());
    let db = Db::connect(&url).await.unwrap();
    let pool = Db::any_pool(&url).await.unwrap();
    let node = Arc::new(allod_ln::mock::MockBackend::new());
    let backend: Arc<dyn allod_ln::LightningBackend> = node.clone();
    let wallet = Wallet::new(pool.clone(), backend, Default::default());
    let phrase = allod_keys::RecoveryPhrase::generate().unwrap();
    let keys = allod_keys::NostrKeys::from_phrase(&phrase).unwrap();
    let apps =
        AppService::new(pool.clone(), Arc::new(keys), vec!["wss://relay.example".into()], vec![]);
    let secrets = SecretStore::new(dir.path().join("secrets"));
    before(&db, &secrets, &apps).await;
    let (wc, ac) = (Arc::new(OnceCell::new()), Arc::new(OnceCell::new()));
    let _ = wc.set(wallet.clone());
    let _ = ac.set(apps.clone());
    let cfg = DomainConfig {
        // Nothing listens here, so the reachability check fails fast.
        tls_check_addr: Some("127.0.0.1:9".into()),
        ..cfg
    };
    let svc = DomainService::load(db.clone(), pool, secrets, cfg, wc, ac).await.unwrap();
    T { svc, apps, wallet, node, db, _dir: dir }
}

async fn account(apps: &AppService, name: &str, user: Option<&str>) -> i64 {
    let (a, _) = apps
        .create(
            NewApp {
                name: name.into(),
                kind: Some("account".into()),
                lud16_username: user.map(str::to_string),
                ..Default::default()
            },
            None,
            now(),
        )
        .await
        .unwrap();
    a.id
}

#[tokio::test]
async fn nip05_is_scoped_to_served_own_domains() {
    use nostr::prelude::*;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let served = t.svc.add_domain("id.example.org", None, None, true).await.unwrap();
    let unserved = t.svc.add_domain("dns.example.org", None, None, false).await.unwrap();
    let key = Keys::generate().public_key();
    let npub = key.to_bech32().unwrap();

    assert!(t.svc.nip05_add(unserved, "alice", &npub).await.is_err());
    assert!(t.svc.nip05_add(served, "alice", "bad").await.is_err());
    assert!(t.svc.nip05_add(served, "admin", &npub).await.is_err());
    t.svc.nip05_add(served, "Alice", &npub).await.unwrap();
    t.svc.nip05_add(served, "_", &npub).await.unwrap();
    let kid = account(&t.apps, "Kid", None).await;
    let names = t.svc.nip05_list().await.unwrap();
    let alice = names.iter().find(|n| n.username == "alice").unwrap().id;
    let root = names.iter().find(|n| n.username == "_").unwrap().id;
    t.svc.nip05_set_account(alice, Some(kid)).await.unwrap();
    assert_eq!(
        t.svc.nip05_list().await.unwrap().iter().find(|n| n.id == alice).unwrap().account_id,
        Some(kid)
    );
    assert!(matches!(
        t.svc.nip05_set_account(root, Some(kid)).await,
        Err(DomainError::Conflict(_))
    ));
    t.svc.nip05_set_account(alice, None).await.unwrap();
    assert!(matches!(t.svc.nip05_add(served, "alice", &npub).await, Err(DomainError::Conflict(_))));
    assert_eq!(
        t.svc.nip05_names("ID.Example.org:443", "alice").await.unwrap().unwrap(),
        json!({"names": {"alice": key.to_hex()}})
    );
    assert_eq!(
        t.svc.nip05_names("id.example.org", "unknown").await.unwrap().unwrap(),
        json!({"names": {}})
    );
    assert!(t.svc.nip05_names("dns.example.org", "alice").await.unwrap().is_none());
    assert!(t.svc.nip05_names("evil.example.org", "alice").await.unwrap().is_none());
    let names = t.svc.nip05_list().await.unwrap();
    for name in names {
        t.svc.nip05_remove(name.id).await.unwrap();
    }
    assert_eq!(t.svc.nip05_list().await.unwrap().len(), 0);
    t.svc.nip05_add(served, "alice", &npub).await.unwrap();
    t.svc.remove_domain(served).await.unwrap();
    assert!(t.svc.nip05_list().await.unwrap().is_empty(), "domain deletion cascades");
}

#[tokio::test]
async fn the_single_public_name_moves_into_the_tables() {
    let t = harness(Default::default(), async |db: &Db, secrets: &SecretStore, apps: &Arc<AppService>| {
        account(apps, "Alice", Some("alice")).await;
        let old = json!({
            "mode": "own", "name": "pay.fmar.dev", "exposure": "private",
            "main_username": "sats", "main_offer": "o1", "offers": {"1": "o2"}, "changed_at": 5,
            "dns": {"provider": "njalla", "a_record": "203.0.113.10", "managed": {
                "A pay@fmar.dev": {"id": "7", "zone": "fmar.dev", "kind": "A", "name": "pay", "content": "203.0.113.10"}
            }},
            "twelve": {"main": {"user_name": "brave.penguin", "domain": "twelve.cash",
                "offer_id": "o9", "custom": false, "verified": true, "created_at": 1}},
        });
        db.config_set(crate::public_name::CONFIG_KEY, &old.to_string(), false).await.unwrap();
        secrets.write("dns_token", "good-token").unwrap();
    })
    .await;
    let pool = &t.svc.pool;
    let domains = store::list_domains(pool).await.unwrap();
    assert_eq!(domains.len(), 1);
    let d = &domains[0];
    assert_eq!(d.name, "pay.fmar.dev");
    assert!(!d.direct && d.served(), "served through the relay's gateway");
    assert_eq!(d.managed.len(), 1);
    let p = store::list_providers(pool).await.unwrap();
    assert_eq!((p.len(), p[0].kind.as_str()), (1, "njalla"));
    assert_eq!(d.dns_provider_id, Some(p[0].id));
    let token = t.svc.secrets.read(&provider_secret(p[0].id)).unwrap().unwrap();
    assert_eq!(token.as_str(), "good-token");
    assert!(t.svc.secrets.read("dns_token").unwrap().is_none());
    let g = store::list_gateways(pool).await.unwrap();
    assert_eq!((g[0].ipv4.as_deref(), g[0].mode.as_str()), (Some("203.0.113.10"), "shared"));
    let mut users: Vec<_> = store::addresses(pool, None)
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.username, a.app_id.is_some()))
        .collect();
    users.sort();
    assert_eq!(users, vec![("alice".into(), true), ("sats".into(), false)]);
    // The legacy fields are gone; the registry names stay.
    let entry = t.db.config_get(crate::public_name::CONFIG_KEY).await.unwrap().unwrap();
    let cfg: Value = serde_json::from_str(&entry.value).unwrap();
    assert!(cfg.get("mode").is_none() && cfg.get("dns").is_none());
    assert!(cfg.get("main_username").is_none());
    assert_eq!(cfg["twelve"]["main"]["user_name"], "brave.penguin");
    let lud16: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT lud16_username FROM apps").fetch_all(pool).await.unwrap();
    assert!(lud16.iter().all(|(u,)| u.is_none()));
}

#[tokio::test]
async fn addresses_are_per_domain_and_follow_the_accounts() {
    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let a = t.svc.add_domain("https://A.Example.com/", None, None, false).await.unwrap();
    let b = t.svc.add_domain("b.example.com", None, None, true).await.unwrap();
    assert!(matches!(
        t.svc.add_domain("a.example.com", None, None, false).await,
        Err(DomainError::Conflict(_))
    ));
    assert!(t.svc.add_domain("c.example.com", None, Some(1), true).await.is_err(), "both ways");
    let kid = account(&t.apps, "Kid", None).await;
    let main = t.svc.add_address(a, "sats", None).await.unwrap();
    assert!(main.offer_id.is_some(), "its offer at once");
    // The same username on another domain, for another account.
    t.svc.add_address(b, "sats", Some(kid)).await.unwrap();
    assert!(matches!(t.svc.add_address(a, "sats", Some(kid)).await, Err(DomainError::Conflict(_))));
    assert!(t.svc.add_address(a, "admin", None).await.is_err(), "reserved");
    t.svc.reconcile().await;
    let v = t.svc.view().await.unwrap();
    let da = &v["domains"][0];
    assert_eq!(da["name"], "a.example.com");
    assert_eq!(da["served"], false);
    assert_eq!(da["addresses"][0]["address"], "sats@a.example.com");
    assert_eq!(da["addresses"][0]["record_name"], "sats.user._bitcoin-payment.a.example.com.");
    assert!(da["addresses"][0]["txt"].as_str().unwrap().starts_with("bitcoin:?lno=lno1"));
    assert_eq!(da["records"].as_array().unwrap().len(), 1, "no A record without a gateway");
    let db_ = &v["domains"][1];
    assert_eq!(db_["addresses"][0]["target"], kid.to_string());
    assert_eq!(db_["addresses"][0]["account_name"], "Kid");
    assert_eq!(db_["reach"]["state"], "waiting_for_certificate");
    // Each address has its own offer, described with the address.
    let offers = t.wallet.offers(None).await.unwrap();
    let o = offers.iter().find(|o| Some(&o.offer_id) == main.offer_id.as_ref()).unwrap();
    assert_eq!(o.description.as_deref(), Some("₿sats@a.example.com"));

    // Certificates and LNURL only for served domains.
    assert!(!t.svc.allows_certificate("a.example.com").await);
    assert!(t.svc.allows_certificate("B.example.com.").await);
    assert!(t.svc.lnurl_target("a.example.com", "sats").await.is_none());
    let tgt = t.svc.lnurl_target("b.example.com:443", "SATS").await.unwrap();
    assert_eq!(tgt.app_id, Some(kid));
    // BIP-353 only: LNURL stops answering for it, and comes back when switched on.
    let kid_addr = store::addresses(&t.svc.pool, Some(b)).await.unwrap()[0].id;
    t.svc.set_address_lnurl(kid_addr, false).await.unwrap();
    assert!(t.svc.lnurl_target("b.example.com", "sats").await.is_none());
    assert_eq!(t.svc.view().await.unwrap()["domains"][1]["addresses"][0]["lnurl_enabled"], false);
    t.svc.set_address_lnurl(kid_addr, true).await.unwrap();
    assert!(t.svc.lnurl_target("b.example.com", "sats").await.is_some());
    // Pointed at Main: its old offer is disabled and a new one, owned by Main, replaces it.
    let before = store::address(&t.svc.pool, kid_addr).await.unwrap().unwrap();
    t.svc.set_address_target(kid_addr, None).await.unwrap();
    let after = store::address(&t.svc.pool, kid_addr).await.unwrap().unwrap();
    assert_eq!(after.app_id, None);
    assert!(after.offer_id.is_some() && after.offer_id != before.offer_id);
    let offers = t.wallet.offers(None).await.unwrap();
    let old = offers.iter().find(|o| Some(&o.offer_id) == before.offer_id.as_ref()).unwrap();
    assert_eq!(old.state, "disabled");
    assert_eq!(t.svc.lnurl_target("b.example.com", "sats").await.unwrap().app_id, None);
    assert!(t.svc.set_address_target(kid_addr, Some(9_999)).await.is_err(), "no such account");
    t.svc.set_address_target(kid_addr, Some(kid)).await.unwrap();
    // LNURL only: no TXT record listed; neither kind is refused.
    t.svc.set_address_kinds(kid_addr, Some(true), Some(false)).await.unwrap();
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["domains"][1]["addresses"][0]["bip353"], false);
    assert!(v["domains"][1]["records"].as_array().unwrap().iter().all(|r| r["type"] != "TXT"));
    assert!(t.svc.set_address_kinds(kid_addr, Some(false), Some(false)).await.is_err());
    t.svc.set_address_kinds(kid_addr, None, Some(true)).await.unwrap();

    // Editing name/domain rotates offer; a taken name leaves the old address intact.
    let old = store::address(&t.svc.pool, kid_addr).await.unwrap().unwrap();
    assert!(matches!(
        t.svc.rename_address(kid_addr, a, "sats").await,
        Err(DomainError::Conflict(_))
    ));
    assert_eq!(
        store::address(&t.svc.pool, kid_addr).await.unwrap().unwrap().offer_id,
        old.offer_id
    );
    t.svc.rename_address(kid_addr, a, "child").await.unwrap();
    let renamed = store::address(&t.svc.pool, kid_addr).await.unwrap().unwrap();
    assert_eq!(renamed.domain_id, a);
    assert_eq!(renamed.username, "child");
    assert_ne!(renamed.offer_id, old.offer_id);
    let offers = t.wallet.offers(None).await.unwrap();
    assert_eq!(
        offers.iter().find(|o| Some(&o.offer_id) == old.offer_id.as_ref()).unwrap().state,
        "disabled"
    );
    assert_eq!(
        offers
            .iter()
            .find(|o| Some(&o.offer_id) == renamed.offer_id.as_ref())
            .unwrap()
            .description
            .as_deref(),
        Some("₿child@a.example.com")
    );
    t.svc.reconcile().await;
    let view = t.svc.view().await.unwrap();
    assert!(view["domains"][0]["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "child.user._bitcoin-payment.a.example.com."));

    // Closing the account removes its address.
    t.wallet.close_account(kid, false).await.unwrap();
    t.svc.reconcile().await;
    assert!(store::addresses(&t.svc.pool, Some(b)).await.unwrap().is_empty());
    // Removing a domain removes its addresses and disables their offers.
    t.svc.remove_domain(a).await.unwrap();
    assert!(store::addresses(&t.svc.pool, None).await.unwrap().is_empty());
    let o = t.wallet.offers(None).await.unwrap();
    let o = o.iter().find(|o| Some(&o.offer_id) == main.offer_id.as_ref()).unwrap();
    assert_eq!(o.state, "disabled");
}

#[tokio::test]
async fn lnurl_callbacks_bound_unpaid_invoices_and_prune_only_old_expired_rows() {
    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let domain = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let address = t.svc.add_address(domain, "sats", None).await.unwrap();
    let query = std::collections::HashMap::from([("amount".to_string(), "1000".to_string())]);

    for _ in 0..32 {
        t.svc.lnurl_callback("pay.example.com", "sats", &query).await.unwrap();
    }
    let full = t.svc.lnurl_callback("pay.example.com", "sats", &query).await.unwrap_err();
    assert_eq!(full.status, 429);
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transactions WHERE ln_address_id = $1 AND state = 'PENDING'",
    )
    .bind(address.id)
    .fetch_one(&t.svc.pool)
    .await
    .unwrap();
    assert_eq!(pending, 32);

    // Simulate reconciled expiry beyond pruning grace. Settled history survives; old unpaid
    // rows are removed before one replacement invoice is admitted.
    let settled_id: i64 =
        sqlx::query_scalar("SELECT MIN(id) FROM transactions WHERE ln_address_id = $1")
            .bind(address.id)
            .fetch_one(&t.svc.pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE transactions SET state = CASE WHEN id = $1 THEN 'SETTLED' ELSE 'EXPIRED' END, \
         expires_at = 1, updated_at = 1 WHERE ln_address_id = $2",
    )
    .bind(settled_id)
    .bind(address.id)
    .execute(&t.svc.pool)
    .await
    .unwrap();
    t.svc.lnurl_callback("pay.example.com", "sats", &query).await.unwrap();

    let states: Vec<(String, i64)> = sqlx::query_as(
        "SELECT state, COUNT(*) FROM transactions WHERE ln_address_id = $1 GROUP BY state \
         ORDER BY state",
    )
    .bind(address.id)
    .fetch_all(&t.svc.pool)
    .await
    .unwrap();
    assert_eq!(states, vec![("PENDING".into(), 1), ("SETTLED".into(), 1)]);
}

#[tokio::test]
async fn lnurl_verification_ignores_newer_cross_account_hash_collision() {
    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let domain = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let address = t.svc.add_address(domain, "sats", None).await.unwrap();
    let attacker = account(&t.apps, "lnurl-collision-attacker", None).await;
    let invoice = t
        .wallet
        .create_invoice(crate::wallet::InvoiceRequest {
            ln_address_id: Some(address.id),
            amount_msat: Some(21_000),
            ..Default::default()
        })
        .await
        .unwrap();
    let hash = invoice.payment_hash.clone().unwrap();
    sqlx::query(
        "INSERT INTO transactions (public_id, app_id, direction, payment_type, state, \
         amount_msat, payment_hash, payment_secret, hold, created_at, updated_at) \
         VALUES ('lnurl-colliding-hold', $1, 'incoming', 'bolt11', 'PENDING', 99000, \
         $2, $3, TRUE, 1, 1)",
    )
    .bind(attacker)
    .bind(&hash)
    .bind("ee".repeat(32))
    .execute(&t.svc.pool)
    .await
    .unwrap();
    let payment = t.node.pay_invoice(&hash, 21_000);
    t.wallet.reconcile().await.unwrap();

    let verified = t.svc.lnurl_verify("pay.example.com", "sats", &hash).await.unwrap();

    assert_eq!(verified["settled"], true);
    assert_eq!(verified["pr"], invoice.payment_request.unwrap());
    assert_eq!(verified["preimage"], payment.kind.preimage().unwrap());
    assert_eq!(t.wallet.balance(Some(attacker)).await.unwrap(), 0);
}

#[tokio::test]
async fn the_env_domain_is_fixed() {
    let cfg = DomainConfig {
        env_public_url: Some("https://wallet.example.org".into()),
        ..Default::default()
    };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let d = t.svc.first_domain().await.unwrap();
    assert!(d.env && d.direct);
    assert!(t.svc.remove_domain(d.id).await.is_err());
    assert!(t.svc.update_domain(d.id, None, None, Some(false)).await.is_err());
    assert!(t.svc.allows_certificate("wallet.example.org").await);
}

#[tokio::test]
async fn njalla_keeps_the_records_of_a_domain_behind_a_gateway() {
    let (recs, url) = njalla_mock().await;
    let cfg = DomainConfig { njalla_url: Some(url), ..Default::default() };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    assert!(t.svc.add_provider("njalla", None, "bad-token").await.is_err(), "token checked");
    assert!(t.svc.add_provider("cloudflare", None, "good-token").await.is_err());
    let p = t.svc.add_provider("njalla", Some("Mine"), "good-token").await.unwrap();
    let g = t.svc.add_gateway(Some("Dublin"), "203.0.113.10", "dedicated").await.unwrap();
    assert!(t.svc.add_gateway(None, "not-an-ip", "dedicated").await.is_err());
    let d = t.svc.add_domain("pay.fmar.dev", Some(p), Some(g), false).await.unwrap();
    t.svc.add_address(d, "sats", None).await.unwrap();
    t.svc.reconcile().await;

    let names = |recs: &Arc<Mutex<Vec<Value>>>| -> Vec<(String, String, String)> {
        let mut v: Vec<_> = recs
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["type"].as_str().unwrap().to_string(),
                    r["name"].as_str().unwrap().to_string(),
                    r["content"].as_str().unwrap().chars().take(12).collect(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names(&recs),
        vec![
            ("A".into(), "pay".into(), "203.0.113.10".into()),
            ("MX".into(), "@".into(), "mail.example".into()),
            ("TXT".into(), "sats.user._bitcoin-payment.pay".into(), "bitcoin:?lno".into()),
        ],
        "TXT added, the owner's old A replaced, MX untouched"
    );
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["domains"][0]["error"], json!(null));
    assert_eq!(v["dns_providers"][0]["domains"], json!(["pay.fmar.dev"]));
    assert_eq!(v["gateways"][0]["domains"], json!(["pay.fmar.dev"]));
    let records = v["domains"][0]["records"].as_array().unwrap();
    assert_eq!(records.iter().find(|r| r["type"] == "A").unwrap()["value"], "203.0.113.10");

    // Nothing changed: nothing written again.
    let before = recs.lock().unwrap().clone();
    t.svc.reconcile().await;
    assert_eq!(*recs.lock().unwrap(), before);

    // Moved to a gateway still being bought (no IP yet): the A record stays.
    let pending =
        store::insert_gateway(&t.svc.pool, "LNVPS", "lnvps", None, "dedicated", 9).await.unwrap();
    t.svc.update_domain(d, None, Some(Some(pending)), None).await.unwrap();
    t.svc.reconcile().await;
    assert!(
        names(&recs).iter().any(|r| r.0 == "A" && r.2 == "203.0.113.10"),
        "kept until replaced"
    );
    t.svc.update_domain(d, None, Some(Some(g)), None).await.unwrap();
    sqlx::query("DELETE FROM gateways WHERE id = $1")
        .bind(pending)
        .execute(&t.svc.pool)
        .await
        .unwrap();

    // In use: neither the provider nor the gateway can go.
    assert!(matches!(t.svc.remove_provider(p).await, Err(DomainError::Conflict(_))));
    assert!(matches!(t.svc.remove_gateway(g).await, Err(DomainError::Conflict(_))));
    // Not served any more: the A record goes.
    t.svc.update_domain(d, None, Some(None), None).await.unwrap();
    t.svc.reconcile().await;
    assert!(names(&recs).iter().all(|r| r.0 != "A"));
    assert!(!t.svc.allows_certificate("pay.fmar.dev").await);
    // Back to manual: Allod's records are removed from Njalla.
    t.svc.update_domain(d, Some(None), None, None).await.unwrap();
    assert_eq!(names(&recs), vec![("MX".into(), "@".into(), "mail.example".into())]);
    t.svc.remove_provider(p).await.unwrap();
    t.svc.remove_gateway(g).await.unwrap();
}

#[tokio::test]
async fn desec_keeps_the_records_of_a_domain_delegated_to_it() {
    let (sets, url) = desec_mock().await;
    let cfg = DomainConfig { desec_url: Some(url), ..Default::default() };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    assert!(t.svc.add_provider("desec", None, "bad-token").await.is_err(), "token checked");
    let p = t.svc.add_provider("desec", None, "good-token").await.unwrap();
    let g = t.svc.add_gateway(None, "203.0.113.10", "dedicated").await.unwrap();
    let d = t.svc.add_domain("pay.example.org", Some(p), Some(g), false).await.unwrap();
    t.svc.add_address(d, "sats", None).await.unwrap();
    t.svc.reconcile().await;

    let find = |sub: &str, kind: &str| -> Option<Vec<String>> {
        sets.lock().unwrap().iter().find(|s| s["subname"] == sub && s["type"] == kind).map(|s| {
            s["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap().to_string())
                .collect()
        })
    };
    assert_eq!(find("pay", "A"), Some(vec!["203.0.113.10".to_string()]), "the owner's A replaced");
    let txt = find("sats.user._bitcoin-payment.pay", "TXT").expect("BIP-353 record");
    assert_eq!(txt.len(), 1);
    assert!(txt[0].starts_with("\"bitcoin:?lno=lno1"), "quoted: {}", txt[0]);
    assert!(txt[0].split("\" \"").all(|p| p.trim_matches('"').len() <= 255), "255-byte strings");
    assert_eq!(find("", "MX"), Some(vec!["10 mail.example.".to_string()]), "untouched");
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["domains"][0]["error"], json!(null));
    assert_eq!(v["dns_providers"][0]["kind"], "desec");
    assert_eq!(v["dns_providers"][0]["label"], "deSEC");
    assert_eq!(v["domains"][0]["checks"]["provider"], true);
    assert_eq!(v["domains"][0]["ds"][0], "13746 13 2 5802e9a6", "SHA-256 first");

    // Nothing changed: nothing written again.
    let before = sets.lock().unwrap().clone();
    t.svc.reconcile().await;
    assert_eq!(*sets.lock().unwrap(), before);

    // Back to manual: Allod's records are removed, the owner's stay.
    t.svc.update_domain(d, Some(None), None, None).await.unwrap();
    assert_eq!(find("pay", "A"), None);
    assert_eq!(find("sats.user._bitcoin-payment.pay", "TXT"), None);
    assert!(find("", "MX").is_some());
    // A domain outside the account cannot sync.
    let other = t.svc.add_domain("pay.elsewhere.com", Some(p), None, false).await.unwrap();
    t.svc.add_address(other, "sats", None).await.unwrap();
    t.svc.reconcile().await;
    let v = t.svc.view().await.unwrap();
    let e =
        v["domains"].as_array().unwrap().iter().find(|x| x["name"] == "pay.elsewhere.com").unwrap();
    assert!(
        e["error"].as_str().unwrap_or_default().contains("not a domain of this deSEC account"),
        "{e}"
    );
}

#[tokio::test]
async fn allod_makes_its_own_desec_account() {
    let (_sets, url) = desec_mock().await;
    let cfg = DomainConfig { desec_url: Some(url), ..Default::default() };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    *t.svc.test_mail.lock().unwrap() = Some(Vec::new());
    let c = t.svc.desec_captcha().await.unwrap();
    assert_eq!(c["id"], "cap-1");
    let e = t.svc.desec_signup("cap-1", "WRONG").await.unwrap_err();
    assert!(e.to_string().contains("captcha"), "{e}");
    t.svc.desec_signup("cap-1", "ABCDE").await.unwrap();
    assert!(t.svc.desec_signup("cap-1", "ABCDE").await.is_err(), "one at a time");
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["desec_signup"]["state"], "activating");
    let email = v["desec_signup"]["email"].as_str().unwrap().to_string();
    assert!(email.starts_with("npub1") && email.ends_with("@uid.ovh"));
    // No mail yet: still waiting.
    t.svc.desec_signup_upkeep().await;
    assert!(t.svc.desec_signup_active().await);
    // The activation mail arrives (quoted-printable, as deSEC sends it).
    *t.svc.test_mail.lock().unwrap() = Some(vec![
        "Subject: Activate your account\r\n\r\nhttps://desec.io/api/v1/v/activate-account/code=\r\n12345/\r\n".into(),
    ]);
    t.svc.desec_signup_upkeep().await;
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["desec_signup"]["state"], "done", "{}", v["desec_signup"]);
    assert_eq!(v["dns_providers"][0]["kind"], "desec");
    let p = v["dns_providers"][0]["id"].as_i64().unwrap();
    assert_eq!(v["desec_signup"]["provider_id"], p);
    let creds = t.svc.desec_credentials().await.unwrap();
    assert_eq!(creds["email"], email.as_str());
    assert_eq!(creds["password"].as_str().unwrap().len(), 48);
    // The provider works with the token Allod made.
    let d = t.svc.add_domain("pay.example.org", Some(p), None, false).await.unwrap();
    t.svc.add_address(d, "sats", None).await.unwrap();
    t.svc.reconcile().await;
    assert_eq!(t.svc.view().await.unwrap()["domains"][0]["error"], json!(null));

    // Provider removed: the account stays; a new sign-up logs in for a new token.
    t.svc.remove_domain(d).await.unwrap();
    t.svc.remove_provider(p).await.unwrap();
    assert_eq!(t.svc.view().await.unwrap()["desec_signup"]["state"], "removed");
    t.svc.desec_signup("cap-1", "ABCDE").await.unwrap();
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["desec_signup"]["state"], "done", "reconnected at once");
    assert_eq!(v["dns_providers"][0]["kind"], "desec");
}

#[tokio::test]
async fn allod_makes_its_own_njalla_account() {
    let (_recs, url) = njalla_mock().await;
    let cfg = DomainConfig { njalla_url: Some(url), ..Default::default() };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    *t.svc.test_mail.lock().unwrap() = Some(Vec::new());
    t.svc.njalla_signup().await.unwrap();
    assert!(t.svc.njalla_signup().await.is_err(), "one at a time");
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["njalla_signup"]["state"], "activating");
    t.svc.njalla_signup_upkeep().await;
    assert!(t.svc.njalla_signup_active().await, "no mail yet");
    // The confirmation mail, quoted-printable, with a password-reset mail beside it.
    *t.svc.test_mail.lock().unwrap() = Some(vec![
        "Subject: Njalla Password reset\r\n\r\nhttps://njal.la/r/notthisone12345\r\n".into(),
        "Subject: Njalla: email confirmation\r\n\r\nhttps://njal.la/a/ImVtYWls:1x=\r\nQ:sig\r\n"
            .into(),
    ]);
    t.svc.njalla_signup_upkeep().await;
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["njalla_signup"]["state"], "done", "{}", v["njalla_signup"]);
    assert_eq!(v["dns_providers"][0]["kind"], "njalla");
    assert_eq!(v["njalla_signup"]["provider_id"], v["dns_providers"][0]["id"]);
    let creds = t.svc.njalla_credentials().await.unwrap();
    assert!(creds["email"].as_str().unwrap().ends_with("@uid.ovh"));
    assert_eq!(creds["password"].as_str().unwrap().len(), 48);

    // Provider removed: the account stays; signing up again logs in for a new token.
    let p = v["dns_providers"][0]["id"].as_i64().unwrap();
    t.svc.remove_provider(p).await.unwrap();
    assert_eq!(t.svc.view().await.unwrap()["njalla_signup"]["state"], "removed");
    t.svc.njalla_signup().await.unwrap();
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["njalla_signup"]["state"], "done", "reconnected at once");
    assert_eq!(v["dns_providers"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_domain_is_bought_at_njalla_once_the_wallet_is_topped_up() {
    let (_recs, url) = njalla_mock().await;
    *NJALLA_ACCOUNT.lock().unwrap() = (0, Vec::new(), 0);
    let cfg = DomainConfig { njalla_url: Some(url), ..Default::default() };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let p = t.svc.add_provider("njalla", None, "good-token").await.unwrap();
    let g = t.svc.add_gateway(None, "203.0.113.10", "dedicated").await.unwrap();

    let found = t.svc.njalla_search(p, "mynode.com").await.unwrap();
    assert_eq!(found["domains"][0]["name"], "mynode.com", "the exact name first");
    assert!(t.svc.njalla_buy(p, "taken.com", 1, None, false).await.is_err());
    assert!(
        t.svc.njalla_buy(p, "pay.mynode.com", 1, None, false).await.is_err(),
        "registered names only"
    );
    assert!(t.svc.njalla_buy(p, "mynode.xyz", 1, None, false).await.is_err(), "not sold");

    // Short of 15 €: a 15 € bitcoin top-up to pay; nothing registered yet.
    let b = t.svc.njalla_buy(p, "mynode.com", 1, Some(g), false).await.unwrap();
    assert_eq!(b.state, "topup");
    let top = b.topup.unwrap();
    assert_eq!((top.amount_eur, top.sats, top.address.as_str()), (15, 25_000, "bc1qtest"));
    assert!(t.svc.njalla_buy(p, "mynode.com", 1, None, false).await.is_err(), "one at a time");
    t.svc.njalla_buy_upkeep().await;
    assert!(NJALLA_ACCOUNT.lock().unwrap().1.is_empty());
    assert!(t.svc.njalla_purchase_active().await);

    // The top-up arrives: registered, DNSSEC on, added to Allod behind the gateway.
    NJALLA_ACCOUNT.lock().unwrap().0 = 15;
    t.svc.njalla_buy_upkeep().await;
    assert_eq!(t.svc.view().await.unwrap()["njalla_purchase"]["state"], "registering");
    t.svc.njalla_buy_upkeep().await;
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["njalla_purchase"]["state"], "done", "{}", v["njalla_purchase"]);
    let d = v["domains"].as_array().unwrap().iter().find(|d| d["name"] == "mynode.com").unwrap();
    assert_eq!(d["dns_provider_id"], p);
    assert_eq!(d["gateway_id"], g);
    assert_eq!(NJALLA_ACCOUNT.lock().unwrap().2, 1, "one top-up");
    // Closing a finished purchase forgets it.
    t.svc.njalla_buy_cancel().await.unwrap();
    assert_eq!(t.svc.view().await.unwrap()["njalla_purchase"], json!(null));
}

#[tokio::test]
async fn an_expiring_gateway_alerts_the_owner_once_per_stage() {
    let t =
        harness(DomainConfig::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {})
            .await;
    let g =
        store::insert_gateway(&t.svc.pool, "Dublin", "lnvps", Some("203.0.113.10"), "dedicated", 9)
            .await
            .unwrap();
    t.svc.add_domain("pay.example.com", None, Some(g), false).await.unwrap();
    let set_exp = |exp: i64| {
        let pool = t.svc.pool.clone();
        async move {
            sqlx::query("UPDATE gateways SET expires_at = $1 WHERE id = $2")
                .bind(exp)
                .bind(g)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    let active = || crate::alerts::active(&t.svc.pool);
    // Far away: nothing.
    set_exp(now() + 30 * 86_400).await;
    t.svc.gateway_alerts(g, "done", None).await;
    assert!(active().await.unwrap().is_empty());
    // Within a week: one alert, raised once.
    let exp = now() + 5 * 86_400;
    set_exp(exp).await;
    t.svc.gateway_alerts(g, "done", None).await;
    t.svc.gateway_alerts(g, "done", None).await;
    let a = active().await.unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].title, "Gateway Dublin expires in 5 days");
    assert!(a[0].body.contains("pay.example.com"), "{}", a[0].body);
    // Two days left: a second one.
    set_exp(now() + 86_400).await;
    t.svc.gateway_alerts(g, "done", None).await;
    assert!(active().await.unwrap().iter().any(|a| a.title == "Gateway Dublin expires in 1 day"));
    // Renewed: the old alerts go.
    set_exp(now() + 60 * 86_400).await;
    t.svc.gateway_alerts(g, "done", None).await;
    assert!(active().await.unwrap().is_empty(), "renewal clears them");
    // Ended: an alert, also with automatic renewal on.
    sqlx::query("UPDATE gateways SET auto_renew = TRUE WHERE id = $1")
        .bind(g)
        .execute(&t.svc.pool)
        .await
        .unwrap();
    set_exp(now() - 60).await;
    t.svc
        .gateway_alerts(g, "renew", Some("LNVPS asks 20000 sats, more than the 15000 allowed"))
        .await;
    let a = active().await.unwrap();
    assert!(a.iter().any(|a| a.title == "Gateway Dublin has expired"));
    assert!(a.iter().any(|a| a.title.contains("automatic renewal failed")));
    // Dismissed: gone from the list, not raised again.
    for x in &a {
        assert!(crate::alerts::dismiss(&t.svc.pool, x.id).await.unwrap());
    }
    t.svc.gateway_alerts(g, "renew", Some("again")).await;
    assert!(active().await.unwrap().is_empty());
}

#[tokio::test]
async fn gateways_get_a_tunnel_and_a_setup_script() {
    let tunnel = tempfile::tempdir().unwrap();
    let cfg = DomainConfig { tunnel_dir: Some(tunnel.path().to_path_buf()), ..Default::default() };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let g1 = t.svc.add_gateway(None, "203.0.113.10", "dedicated").await.unwrap();
    let g2 = t.svc.add_gateway(Some("Shared"), "198.51.100.4", "shared").await.unwrap();
    assert!(t.svc.add_gateway(None, "198.51.100.4", "weird").await.is_err());
    t.svc.add_domain("pay.example.com", None, Some(g2), false).await.unwrap();
    t.svc.reconcile().await;

    let conf = std::fs::read_to_string(tunnel.path().join("wg0.conf")).unwrap();
    let (t1, t2) = (t.svc.tunnel(g1).unwrap(), t.svc.tunnel(g2).unwrap());
    assert_ne!((t1.port, &t1.home_ip), (t2.port, &t2.home_ip), "distinct tunnels");
    assert!(conf.contains(&format!("Address = {}/24, {}/24", t1.home_ip, t2.home_ip)), "{conf}");
    assert!(conf.contains(&format!("Endpoint = 203.0.113.10:{}", t1.port)));
    assert!(conf.contains(&format!("AllowedIPs = {}/32", t2.gateway_ip)));
    // The route list of each gateway, by its tunnel id.
    let routes = std::fs::read_to_string(tunnel.path().join("routes").join(&t2.id)).unwrap();
    assert_eq!(routes.trim_end(), "pay.example.com");
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["gateways"][0]["state"], "setup");
    assert_eq!(v["gateways"][0]["label"], "203.0.113.10");

    let script = t.svc.setup_script(g2).await.unwrap();
    assert!(script.contains(&format!("route add \"$d\" {}:443", t2.home_ip)));
    assert!(script.contains(r#"WANT="pay.example.com""#));
    let home_key = t.svc.secrets.read("wireguard_home").unwrap().unwrap();
    let home = gateway::public_key(&home_key).unwrap();
    assert!(script.contains(&format!("PublicKey = {home}")));
    // The gateway's key in the script matches the peer at home.
    let gw_key = t.svc.secrets.read(&format!("gateway_{g2}_wg")).unwrap().unwrap();
    assert!(script.contains(&format!("PrivateKey = {}", gw_key.as_str())));
    let gw_pub = gateway::public_key(&gw_key).unwrap();
    assert!(conf.contains(&format!("PublicKey = {gw_pub}")));

    // A recent handshake makes it active.
    std::fs::write(
        tunnel.path().join("status.json"),
        json!({"peers": [{"public_key": gw_pub, "latest_handshake": now() - 5}]}).to_string(),
    )
    .unwrap();
    t.svc.reconcile().await;
    let v = t.svc.view().await.unwrap();
    assert_eq!(v["gateways"][1]["state"], "active");
    assert_eq!(v["gateways"][0]["state"], "setup");

    t.svc
        .update_gateway(g1, Some("Home relay"), Some("192.0.2.1"), Some("shared"), None, None)
        .await
        .unwrap();
    let g = store::gateway(&t.svc.pool, g1).await.unwrap().unwrap();
    assert_eq!(
        (g.label.as_str(), g.ipv4.as_deref(), g.mode.as_str()),
        ("Home relay", Some("192.0.2.1"), "shared")
    );
    t.svc.remove_gateway(g1).await.unwrap();
    assert!(t.svc.secrets.read(&format!("gateway_{g1}_wg")).unwrap().is_none());
}

/// What the mock LNVPS saw.
#[derive(Default)]
struct LnvpsMock {
    signed: usize,
    unsigned_private: usize,
    vms: usize,
    invoices: usize,
    payment_checks: usize,
}

async fn lnvps_mock() -> (Arc<Mutex<LnvpsMock>>, String) {
    use axum::extract::{Path, State};
    use axum::routing::get;
    type S = Arc<Mutex<LnvpsMock>>;
    let m: S = Arc::default();
    fn auth(m: &S, h: &HeaderMap) -> bool {
        let ok = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Nostr "));
        let mut m = m.lock().unwrap();
        if ok {
            m.signed += 1
        } else {
            m.unsigned_private += 1
        }
        ok
    }
    let app = Router::new()
        .route(
            "/api/v1/vm/templates",
            get(|| async {
                Json(json!({"data": {
                    "templates": [{"id": 16, "cost_plan": {"interval_type": "year", "currency": "EUR",
                        "amount": 15000, "other_price": [{"currency": "BTC", "amount": 220_000_000}]},
                        "region": {"name": "Dublin"}}],
                    "custom_template": [{"id": 7, "interval_type": "month", "min_cpu": 1,
                        "min_memory": 1073741824u64, "region": {"name": "Dublin", "company_id": 1},
                        "disks": [{"min_disk": 10737418240u64, "disk_type": "ssd", "disk_interface": "pcie"}]}],
                }}))
            }),
        )
        .route(
            "/api/v1/vm/custom-template/price",
            post(|Json(b): Json<Value>| async move {
                assert_eq!(b["pricing_id"], 7);
                assert_eq!(b["cpu"], 1);
                Json(json!({"data": {"currency": "EUR", "amount": 500, "interval_type": "month",
                    "other_price": [{"currency": "BTC", "amount": 7_348_000}]}}))
            }),
        )
        .route(
            "/api/v1/image",
            get(|| async {
                Json(json!({"data": [
                    {"id": 1, "distribution": "ubuntu", "version": "24.04", "default_username": "ubuntu"},
                    {"id": 15, "distribution": "ubuntu", "version": "26.04", "default_username": "ubuntu"},
                ]}))
            }),
        )
        .route(
            "/api/v1/account",
            get(|State(m): State<S>, h: HeaderMap| async move {
                assert!(auth(&m, &h));
                Json(json!({"data": {"email_verified": true, "contact_nip17": false,
                    "tax": [{"company_id": 1, "rate": 23.0, "country_code": "IRL"}]}}))
            }),
        )
        .route(
            "/api/v1/payment/methods",
            get(|| async { Json(json!({"data": [{"name": "lightning", "currencies": ["BTC"]}]})) }),
        )
        .route(
            "/api/v1/ssh-key",
            post(|State(m): State<S>, h: HeaderMap, Json(b): Json<Value>| async move {
                assert!(auth(&m, &h));
                assert!(b["key_data"].as_str().unwrap().starts_with("ssh-ed25519 "));
                Json(json!({"data": {"id": 5, "name": "allod"}}))
            }),
        )
        .route(
            "/api/v1/vm/custom-template",
            post(|State(m): State<S>, h: HeaderMap, Json(b): Json<Value>| async move {
                assert!(auth(&m, &h));
                assert_eq!((b["image_id"].as_u64(), b["ssh_key_id"].as_u64()), (Some(15), Some(5)));
                m.lock().unwrap().vms += 1;
                Json(json!({"data": {"id": 42}}))
            }),
        )
        .route(
            "/api/v1/vm/42/renew",
            get(|State(m): State<S>, h: HeaderMap| async move {
                assert!(auth(&m, &h));
                let n = {
                    let mut m = m.lock().unwrap();
                    m.invoices += 1;
                    m.invoices
                };
                // 23 % VAT on top of the price.
                // Rounded up to a whole sat, as LNVPS does.
                let inv = allod_ln::mock::MockBackend::make_invoice(Some(9_039_000), &format!("{n:064x}"));
                Json(json!({"data": {"id": format!("p{n}"), "amount": 7_348_000, "tax": 1_690_040,
                    "processing_fee": 0, "currency": "BTC",
                    "time": 2_592_000, "data": {"lightning": inv}}}))
            }),
        )
        .route(
            "/api/v1/payment/{id}",
            get(|State(m): State<S>, h: HeaderMap, Path(_id): Path<String>| async move {
                assert!(auth(&m, &h));
                let mut m = m.lock().unwrap();
                m.payment_checks += 1;
                // Recorded on the second look (after Allod paid).
                Json(json!({"data": {"is_paid": m.payment_checks >= 2}}))
            }),
        )
        .route(
            "/api/v1/vm/42",
            get(|State(m): State<S>, h: HeaderMap| async move {
                assert!(auth(&m, &h));
                Json(json!({"data": {"id": 42, "status": {"state": "running"},
                    "expires": "2030-01-01T00:00:00Z", "image": {"default_username": "ubuntu"},
                    "ip_assignments": [{"ip": "127.0.0.1/8"}, {"ip": "::1/128"}]}}))
            }),
        )
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (m, format!("http://{addr}"))
}

#[tokio::test]
async fn an_lnvps_gateway_is_bought_paid_once_and_set_up() {
    let (m, url) = lnvps_mock().await;
    let tunnel = tempfile::tempdir().unwrap();
    let cfg = DomainConfig {
        lnvps_url: Some(url),
        tunnel_dir: Some(tunnel.path().to_path_buf()),
        ..Default::default()
    };
    let t = harness(cfg, async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.reconcile().await.unwrap();
    // €10 is 11,111 sats at 90,000 EUR/BTC.
    let fiat = Arc::new(crate::FiatService::new());
    fiat.preload(crate::RateSource::Yadio, [("EUR".to_string(), 90_000.0)].into()).await;
    t.svc.set_fiat(fiat);
    let q = t.svc.lnvps_quote().await.unwrap();
    assert_eq!(q["price_sats"], 7_348);
    assert_eq!(q["total_sats"], 9_039, "with 23 % VAT");
    assert_eq!(q["total_fiat"], json!({"amount": 615, "currency": "EUR"}));
    assert_eq!(q["plan"]["kind"], "custom");
    assert_eq!(q["plan"]["image"], "ubuntu 26.04");

    // Over the cap: nothing is ordered.
    let e = t.svc.lnvps_buy(None, 8_000, true, Some(8_000), None).await.unwrap_err();
    assert!(e.to_string().contains("9039 sats"), "{e}");
    let five = Some(lnvps::FiatCap { currency: "EUR".into(), amount: 5.0 });
    assert!(t.svc.lnvps_buy(None, 0, true, None, five).await.is_err(), "€5 < €6.15");
    assert_eq!(m.lock().unwrap().vms, 0);

    let before = t.wallet.balance(None).await.unwrap();
    let ten = Some(lnvps::FiatCap { currency: "EUR".into(), amount: 10.0 });
    let g = t.svc.lnvps_buy(Some("Dublin"), 0, true, None, ten).await.unwrap();
    let state = |t: &T| {
        let pool = t.svc.pool.clone();
        async move { store::gateway(&pool, g).await.unwrap().unwrap() }
    };
    assert_eq!(state(&t).await.state, "ordering");
    // Order, then record the invoice, then pay it, then see LNVPS has it.
    for _ in 0..8 {
        t.svc.lnvps_upkeep().await;
        let row = state(&t).await;
        if row.ipv4.is_some() && row.error.is_some() {
            break;
        }
    }
    let row = state(&t).await;
    {
        let lm = m.lock().unwrap();
        assert_eq!(lm.vms, 1, "ordered once");
        assert_eq!(lm.invoices, 1, "one invoice");
        assert_eq!(lm.unsigned_private, 0, "every private call is NIP-98 signed");
    }
    let spent = before - t.wallet.balance(None).await.unwrap();
    assert!((9_039_000..=9_200_000).contains(&spent), "paid once, VAT included: {spent}");
    assert_eq!(row.provider_ref.as_deref(), Some("42"));
    assert_eq!(row.ipv4.as_deref(), Some("127.0.0.1"));
    assert_eq!(row.ipv6.as_deref(), Some("::1"));
    assert!(row.expires_at.unwrap() > now());
    // The setup step tried SSH (nothing to log in to here) and keeps the error.
    let progress: Value = serde_json::from_str(row.provider_data.as_deref().unwrap()).unwrap();
    assert_eq!(progress["step"], "setup", "{row:?}");
    assert!(row.error.as_deref().unwrap().contains("SSH"), "{row:?}");
    // Its key pair exists; the gateway is a peer of the home tunnel.
    assert!(t.svc.secrets.read("gateway_ssh").unwrap().is_some());
    t.svc.reconcile().await;
    let conf = std::fs::read_to_string(tunnel.path().join("wg0.conf")).unwrap();
    let port = t.svc.tunnel(g).unwrap().port;
    assert!(conf.contains(&format!("Endpoint = 127.0.0.1:{port}")), "{conf}");
    // More passes do not pay again.
    t.svc.lnvps_upkeep().await;
    assert_eq!(m.lock().unwrap().invoices, 1);
    assert_eq!(before - t.wallet.balance(None).await.unwrap(), spent);
}

#[tokio::test]
async fn lnvps_keeps_invoice_when_wallet_fails_after_submission() {
    use allod_ln::mock::{BalanceGate, SendBehaviour};
    use allod_ln::LightningBackend;

    let (m, url) = lnvps_mock().await;
    let t = harness(
        DomainConfig { lnvps_url: Some(url), ..Default::default() },
        async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {},
    )
    .await;
    t.wallet.reconcile().await.unwrap();
    let fiat = Arc::new(crate::FiatService::new());
    fiat.preload(crate::RateSource::Yadio, [("EUR".to_string(), 90_000.0)].into()).await;
    t.svc.set_fiat(fiat);
    let cap = Some(lnvps::FiatCap { currency: "EUR".into(), amount: 10.0 });
    let gateway = t.svc.lnvps_buy(None, 0, true, None, cap).await.unwrap();

    // First pass orders VM; second records invoice without paying it.
    t.svc.lnvps_upkeep().await;
    t.svc.lnvps_upkeep().await;
    let before = store::gateway(&t.svc.pool, gateway).await.unwrap().unwrap();
    let before: Value = serde_json::from_str(before.provider_data.as_deref().unwrap()).unwrap();
    let payment_before = before["payment"].clone();
    assert!(!payment_before.is_null());
    assert_eq!(m.lock().unwrap().invoices, 1);

    // Break only wallet persistence after reserve and backend submission. Old behavior
    // discarded payment identity here, allowing next pass to order another invoice.
    *t.node.send.lock().unwrap() = SendBehaviour::SubmitThenTimeout;
    let gate = Arc::new(BalanceGate::default());
    *t.node.bolt11_send_gate.lock().unwrap() = Some(gate.clone());
    let service = t.svc.clone();
    let upkeep = tokio::spawn(async move { service.lnvps_upkeep().await });
    gate.entered.notified().await;
    sqlx::query("ALTER TABLE transactions RENAME TO transactions_unavailable")
        .execute(&t.svc.pool)
        .await
        .unwrap();
    gate.release.notify_one();
    upkeep.await.unwrap();
    sqlx::query("ALTER TABLE transactions_unavailable RENAME TO transactions")
        .execute(&t.svc.pool)
        .await
        .unwrap();

    let after = store::gateway(&t.svc.pool, gateway).await.unwrap().unwrap();
    let progress: Value = serde_json::from_str(after.provider_data.as_deref().unwrap()).unwrap();
    assert_eq!(progress["payment"], payment_before, "exact invoice identity must survive");
    assert!(after.error.is_some());
    assert_eq!(m.lock().unwrap().invoices, 1);
    let hash = progress["payment"]["invoice"].as_str().unwrap().rsplit_once('h').unwrap().1;
    assert!(t.node.payment(hash).await.unwrap().is_some(), "backend accepted payment");
}

/// A prism address forwards its percentages and keeps the rest; a destination that cannot
/// be paid keeps its amount visible, is retried later, and is never paid twice.
#[tokio::test]
async fn prism_splits_forward_their_share() {
    use crate::wallet::bip353::Bip353Resolver;
    use allod_ln::mock::MockBackend;
    use std::collections::HashMap;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    // Both splits resolve through BIP-353: a 40 sat invoice, and one that will be gone by
    // the time it is paid (the owner removes the record in between).
    let invoice = MockBackend::make_invoice(Some(40_000), &"11".repeat(32));
    let gone = MockBackend::make_invoice(Some(25_000), &"33".repeat(32));
    let working = Bip353Resolver::Fixed(Arc::new(HashMap::from([(
        "split@example.com".to_string(),
        vec![format!("bitcoin:?lightning={invoice}")],
    )])));
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([
        ("split@example.com".to_string(), vec![format!("bitcoin:?lightning={invoice}")]),
        ("missing@example.com".to_string(), vec![format!("bitcoin:?lightning={gone}")]),
    ]))));

    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    let splits = t
        .svc
        .set_prism_splits(
            a.id,
            &[
                ("₿split@example.com".to_string(), 4_000),
                ("₿missing@example.com".to_string(), 2_500),
            ],
        )
        .await
        .unwrap();
    assert_eq!(splits.len(), 2);
    assert_eq!(splits[0].kind, "bip353");
    assert_eq!(splits.iter().map(|s| s.percent_bps).sum::<i64>(), 6_500);

    // Wallet startup reconciles mock node funds asynchronously.
    let mut main_before = t.wallet.balance(None).await.unwrap();
    for _ in 0..200 {
        if main_before > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        main_before = t.wallet.balance(None).await.unwrap();
    }
    assert!(main_before > 0);
    // A 100 sat payment through the address's LNURL callback.
    let q = HashMap::from([("amount".to_string(), "100000".to_string())]);
    let callback = t.svc.lnurl_callback("pay.example.com", "sats", &q).await.unwrap();
    let pr = callback["pr"].as_str().unwrap().to_string();
    let incoming = t.wallet.incoming_by_invoice(&pr).await.unwrap().unwrap();
    assert_eq!(incoming.ln_address_id, Some(a.id));
    assert!(
        incoming.metadata.as_deref().unwrap().contains(&format!("\"ln_address_id\":{}", a.id)),
        "{:?}",
        incoming.metadata
    );
    t.node.pay_invoice(incoming.payment_hash.as_deref().unwrap(), 100_000);
    let mut settled = incoming;
    for _ in 0..200 {
        settled = t.wallet.transaction(&settled.public_id).await.unwrap();
        if settled.state == "SETTLED" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(settled.state, "SETTLED");

    // Splitting and paying happen outside the wallet's own event loop.
    assert!(t.svc.prism_payment(&t.wallet, &settled.public_id).await);
    // The second record is gone now: that payout fails and stays visible for a retry.
    t.wallet.set_bip353_resolver(working);
    t.svc.prism_sweep(&t.wallet).await;

    let per_split = t.svc.prism_split_stats(a.id).await.unwrap();
    assert_eq!(per_split["₿split@example.com"].forwarded_msat, 40_000);
    assert_eq!(per_split["₿missing@example.com"].failed_msat, 25_000);
    assert_eq!(per_split["₿missing@example.com"].failed_count, 1);
    assert!(
        per_split["₿missing@example.com"].last_error.as_deref().unwrap().contains("BIP-353"),
        "{:?}",
        per_split["₿missing@example.com"].last_error
    );
    let totals = t.svc.prism_stats(a.id).await.unwrap();
    assert_eq!((totals.forwarded_msat, totals.failed_msat), (40_000, 25_000));
    assert_eq!(totals.payments, 1);
    // The menus can warn about it without loading every domain and address.
    let failures = t.svc.prism_failures().await.unwrap();
    assert_eq!(failures["failed_count"], 1);
    assert_eq!(failures["failed_msat"], 25_000);
    assert_eq!(failures["addresses"][0]["address"], "sats@pay.example.com");
    assert_eq!(failures["addresses"][0]["failed_count"], 1);
    // The remainder stays in Main, minus the forwarded split and its routing fee.
    let fee: i64 = {
        use sqlx::Row;
        sqlx::query("SELECT fee_msat FROM prism_payouts WHERE state = 'PAID'")
            .fetch_one(&t.svc.pool)
            .await
            .unwrap()
            .try_get(0)
            .unwrap()
    };
    assert!(fee >= 0);
    assert_eq!(t.wallet.balance(None).await.unwrap(), main_before + 100_000 - 40_000 - fee);

    // Payout stores the outgoing transaction id before submission.
    let outgoing =
        t.wallet.transaction_by_id(t.svc.payout_out_tx(1).await.unwrap().unwrap()).await.unwrap();
    assert_eq!(outgoing.amount_msat, 40_000);

    // The owner sees it in the domains view.
    let v = t.svc.view().await.unwrap();
    let prism = &v["domains"][0]["addresses"][0]["prism"];
    assert_eq!(prism["split_bps"], 6_500);
    assert_eq!(prism["stats"]["failed_msat"], 25_000);
    assert_eq!(prism["splits"][0]["percent_bps"], 4_000);

    // Every payout is readable, newest first, with the split's note for context.
    sqlx::query("UPDATE prism_splits SET note = 'Alice''s node' WHERE destination = $1")
        .bind("₿split@example.com")
        .execute(&t.svc.pool)
        .await
        .unwrap();
    let history = t.svc.prism_payout_history(a.id, None, 20).await.unwrap();
    assert_eq!(history["payouts"].as_array().unwrap().len(), 2);
    assert_eq!(history["payouts"][1]["state"], "PAID");
    assert_eq!(history["payouts"][1]["amount_msat"], 40_000);
    assert_eq!(history["payouts"][1]["note"], "Alice's node");
    assert_eq!(history["payouts"][0]["state"], "FAILED");
    assert_eq!(history["payouts"][0]["destination"], "₿missing@example.com");
    assert!(history["payouts"][0]["last_error"].as_str().unwrap().contains("BIP-353"));
    assert_eq!(history["next_before_id"], serde_json::json!(null));

    // One at a time, with the oldest id as the cursor for the next page.
    let page = t.svc.prism_payout_history(a.id, None, 1).await.unwrap();
    assert_eq!(page["payouts"].as_array().unwrap().len(), 1);
    let before = page["next_before_id"].as_i64().unwrap();
    let page = t.svc.prism_payout_history(a.id, Some(before), 1).await.unwrap();
    assert_eq!(page["payouts"][0]["state"], "PAID");
    assert_eq!(page["next_before_id"], serde_json::json!(null));

    // "Retry now": the same payout is attempted again, not a second one.
    assert_eq!(t.svc.prism_retry(a.id).await.unwrap(), 1);
    t.svc.prism_sweep(&t.wallet).await;
    let per_split = t.svc.prism_split_stats(a.id).await.unwrap();
    assert_eq!(per_split["₿missing@example.com"].failed_msat, 25_000);
    let totals = t.svc.prism_stats(a.id).await.unwrap();
    assert_eq!((totals.failed_count, totals.payments), (1, 1));

    // A destination that is not a target is refused, and so is more than 100%.
    assert!(t.svc.set_prism_splits(a.id, &[("not a target".to_string(), 1_000)]).await.is_err());
    assert!(t
        .svc
        .set_prism_splits(
            a.id,
            &[("₿split@example.com".to_string(), 6_000), ("₿other@example.com".to_string(), 5_000)],
        )
        .await
        .is_err());
    // Clearing them stops forwarding. A removed split is never paid behind the owner's back:
    // what never went out is canceled, while the paid payout stays as history.
    assert!(t.svc.set_prism_splits(a.id, &[]).await.unwrap().is_empty());
    let totals = t.svc.prism_stats(a.id).await.unwrap();
    assert_eq!((totals.forwarded_msat, totals.failed_msat), (40_000, 0));
    assert_eq!(t.svc.prism_retry(a.id).await.unwrap(), 0);
    let states: Vec<String> = {
        use sqlx::Row;
        sqlx::query("SELECT state FROM prism_payouts ORDER BY id")
            .fetch_all(&t.svc.pool)
            .await
            .unwrap()
            .iter()
            .map(|r| r.try_get("state").unwrap())
            .collect()
    };
    assert_eq!(states, ["PAID", "CANCELED"]);
}

#[tokio::test]
async fn forged_invoice_metadata_cannot_trigger_another_accounts_prism() {
    use crate::wallet::{bip353::Bip353Resolver, InvoiceRequest};
    use allod_ln::mock::MockBackend;
    use std::collections::HashMap;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let attacker = account(&t.apps, "attacker", None).await;
    let victim = account(&t.apps, "victim", None).await;
    assert!(t
        .apps
        .create(
            NewApp {
                name: "forged child".into(),
                metadata: Some(json!({"account": victim})),
                ..Default::default()
            },
            Some(attacker),
            now()
        )
        .await
        .is_err());
    let dest = MockBackend::make_invoice(Some(10_000), &"a1".repeat(32));
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([(
        "attacker@example.com".to_string(),
        vec![format!("bitcoin:?lightning={dest}")],
    )]))));
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let main_address = t.svc.add_address(d, "alice", None).await.unwrap();
    let victim_address = t.svc.add_address(d, "victim", Some(victim)).await.unwrap();
    for address in [&main_address, &victim_address] {
        t.svc
            .set_prism_splits(address.id, &[("₿attacker@example.com".to_string(), 5_000)])
            .await
            .unwrap();
        let forged = json!({"ln_address_id": address.id,
            "lightning_address": format!("{}@pay.example.com", address.username),
            "bip353": format!("{}@pay.example.com", address.username)});
        let invoice = t
            .wallet
            .create_invoice(InvoiceRequest {
                app_id: Some(attacker),
                amount_msat: Some(100_000),
                metadata: Some(forged.to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        t.node.pay_invoice(invoice.payment_hash.as_deref().unwrap(), 100_000);
        let mut settled = invoice;
        for _ in 0..200 {
            settled = t.wallet.transaction(&settled.public_id).await.unwrap();
            if settled.state == "SETTLED" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(settled.state, "SETTLED");
        assert!(!t.svc.prism_payment(&t.wallet, &settled.public_id).await);
    }
    assert!(t.svc.unsettled_splits(now()).await.unwrap().is_empty());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prism_payouts")
        .fetch_one(&t.svc.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn forged_outgoing_metadata_cannot_complete_a_prism_payout() {
    use crate::wallet::bip353::Bip353Resolver;
    use allod_ln::mock::MockBackend;
    use std::collections::HashMap;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    let attacker = account(&t.apps, "attacker", None).await;
    let dest = MockBackend::make_invoice(Some(10_000), &"a2".repeat(32));
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([(
        "attacker@example.com".to_string(),
        vec![format!("bitcoin:?lightning={dest}")],
    )]))));
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "alice", None).await.unwrap();
    t.svc.set_prism_splits(a.id, &[("₿attacker@example.com".to_string(), 5_000)]).await.unwrap();
    let time = now() - 121;
    sqlx::query(
        "INSERT INTO prism_payouts (address_id, incoming_tx_id, destination, kind, \
        amount_msat, percent_bps, state, attempts, created_at, updated_at) \
        VALUES ($1, 1, $2, 'bip353', 10_000, 5_000, 'PAYING', 1, $3, $3)",
    )
    .bind(a.id)
    .bind("₿attacker@example.com")
    .bind(time)
    .execute(&t.svc.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO transactions (public_id, app_id, direction, payment_type, state, \
        amount_msat, metadata, created_at, updated_at) \
        VALUES ('forged-prism-payment', $1, 'outgoing', 'bolt11', 'SETTLED', 10_000, \
        '{\"prism_payout\":\"1\"}', $2, $2)",
    )
    .bind(attacker)
    .bind(time)
    .execute(&t.svc.pool)
    .await
    .unwrap();

    t.svc.prism_sweep(&t.wallet).await;
    let (state, out_tx_id): (String, Option<i64>) =
        sqlx::query_as("SELECT state, out_tx_id FROM prism_payouts WHERE id = 1")
            .fetch_one(&t.svc.pool)
            .await
            .unwrap();
    assert_eq!(state, "FAILED");
    assert_eq!(out_tx_id, None);
}

/// A split can carry a note for the owner: it stays with its destination when the splits
/// are saved again, and an empty note clears it.
#[tokio::test]
async fn prism_split_notes_stay_with_their_destination() {
    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    t.svc.add_address(d, "bob", None).await.unwrap();

    let splits = t
        .svc
        .set_prism_splits_with_notes(
            a.id,
            &[("bob@pay.example.com".to_string(), 1_000, Some("  Bob's\n node ".to_string()))],
        )
        .await
        .unwrap();
    assert_eq!(splits[0].note.as_deref(), Some("Bob's node"));
    assert_eq!(t.svc.prism_view(a.id).await.unwrap()["splits"][0]["note"], "Bob's node");

    // A caller that does not know about notes keeps the stored one.
    t.svc.set_prism_splits(a.id, &[("bob@pay.example.com".to_string(), 2_000)]).await.unwrap();
    let splits = t.svc.prism_splits(a.id).await.unwrap();
    assert_eq!(splits[0].percent_bps, 2_000);
    assert_eq!(splits[0].note.as_deref(), Some("Bob's node"));

    // An empty note clears it; a note that is too long is refused.
    t.svc
        .set_prism_splits_with_notes(
            a.id,
            &[("bob@pay.example.com".to_string(), 2_000, Some("   ".to_string()))],
        )
        .await
        .unwrap();
    assert_eq!(t.svc.prism_splits(a.id).await.unwrap()[0].note, None);
    let e = t
        .svc
        .set_prism_splits_with_notes(
            a.id,
            &[("bob@pay.example.com".to_string(), 2_000, Some("x".repeat(201)))],
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("at most 200 characters"), "{e}");
}

/// A split keeps its id and its payouts when its destination is edited: what failed to
/// arrive follows the split to the new destination and is retried there, while what was
/// already paid stays as history under the old one. The owner can clear a split's failed
/// total without paying it.
#[tokio::test]
async fn prism_failed_sats_follow_an_edited_split() {
    use crate::domains::prism::SplitInput;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    t.svc.add_address(d, "bob", None).await.unwrap();
    t.svc.add_address(d, "carol", None).await.unwrap();

    let splits = t
        .svc
        .set_prism_splits_with_notes(
            a.id,
            &[("bob@pay.example.com".to_string(), 2_500, Some("Bob's node".to_string()))],
        )
        .await
        .unwrap();
    let split_id = splits[0].id;

    // Two received payments: one could not be sent, one went out.
    t.svc.split_payment(a.id, 700, 100_000, now()).await.unwrap();
    t.svc.split_payment(a.id, 701, 100_000, now()).await.unwrap();
    sqlx::query(
        "UPDATE prism_payouts SET state = 'FAILED', attempts = 8, next_retry_at = NULL, \
         last_error = 'no route to the destination' WHERE incoming_tx_id = 700",
    )
    .execute(&t.svc.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE prism_payouts SET state = 'PAID', paid_at = $1 WHERE incoming_tx_id = 701")
        .bind(now())
        .execute(&t.svc.pool)
        .await
        .unwrap();
    let stats = t.svc.prism_split_stats(a.id).await.unwrap();
    assert_eq!(stats["bob@pay.example.com"].failed_msat, 25_000);

    // The same split now points at Carol: its id, its failed sats and its pending payout
    // go with it. The note described Bob, so it is not carried over to Carol.
    let renamed = t
        .svc
        .set_prism_splits_inputs(
            a.id,
            &[SplitInput {
                id: Some(split_id),
                destination: "carol@pay.example.com".into(),
                percent_bps: 2_500,
                note: None,
            }],
        )
        .await
        .unwrap();
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].id, split_id, "the split is edited in place");
    assert_eq!(renamed[0].note, None);

    let stats = t.svc.prism_split_stats(a.id).await.unwrap();
    assert_eq!(stats["carol@pay.example.com"].failed_msat, 25_000);
    assert_eq!(stats["carol@pay.example.com"].failed_count, 1);
    assert_eq!(
        stats["bob@pay.example.com"].forwarded_msat, 25_000,
        "what was already paid keeps the destination it went to"
    );
    assert_eq!(stats["bob@pay.example.com"].failed_count, 0);

    let history = t.svc.prism_payout_history(a.id, None, 20).await.unwrap();
    let payouts = history["payouts"].as_array().unwrap();
    let failed =
        payouts.iter().find(|p| p["destination"] == "carol@pay.example.com").expect("moved");
    assert_eq!(failed["state"], "FAILED");
    assert_eq!(failed["amount_msat"], 25_000);
    let paid = payouts.iter().find(|p| p["destination"] == "bob@pay.example.com").expect("kept");
    assert_eq!(paid["state"], "PAID");

    // Clearing the failed total cancels it without sending anything.
    assert_eq!(t.svc.prism_reset_failed(a.id, split_id).await.unwrap(), 1);
    let totals = t.svc.prism_stats(a.id).await.unwrap();
    assert_eq!((totals.failed_count, totals.failed_msat), (0, 0));
    assert_eq!(totals.forwarded_msat, 25_000, "the paid payout is untouched");
    let history = t.svc.prism_payout_history(a.id, None, 20).await.unwrap();
    let cleared = history["payouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["destination"] == "carol@pay.example.com")
        .unwrap();
    assert_eq!(cleared["state"], "CANCELED");
    assert!(cleared["last_error"].as_str().unwrap().contains("cleared"));
    // Nothing left to clear, and a split of another address is refused.
    assert_eq!(t.svc.prism_reset_failed(a.id, split_id).await.unwrap(), 0);
    assert!(t.svc.prism_reset_failed(a.id, split_id + 999).await.is_err());
}

/// A BOLT12 payment to the address's offer is split too: the offer's metadata names the
/// address, and its row records a payout like an LNURL payment does.
#[tokio::test]
async fn prism_splits_a_bolt12_payment_to_the_address() {
    use crate::wallet::bip353::Bip353Resolver;
    use allod_ln::mock::MockBackend;
    use std::collections::HashMap;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let invoice = MockBackend::make_invoice(Some(10_000), &"22".repeat(32));
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([(
        "split@example.com".to_string(),
        vec![format!("bitcoin:?lightning={invoice}")],
    )]))));

    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    let offer_id = a.offer_id.clone().expect("the address has an offer");
    t.svc.set_prism_splits(a.id, &[("₿split@example.com".to_string(), 1_000)]).await.unwrap();

    // Nothing was paid yet, so catching up finds nothing.
    assert!(t.svc.unsettled_splits(now()).await.unwrap().is_empty());

    t.node.pay_offer(&offer_id, 100_000, None);
    let mut recorded = false;
    for _ in 0..200 {
        if !t.svc.unsettled_splits(now()).await.unwrap().is_empty() {
            recorded = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(recorded, "the offer payment is in the wallet's ledger");

    // What the worker does on a restart: split what was missed, then pay it.
    t.svc.catch_up().await;
    t.svc.prism_sweep(&t.wallet).await;

    let stats = t.svc.prism_split_stats(a.id).await.unwrap();
    assert_eq!(stats["₿split@example.com"].forwarded_msat, 10_000);
    assert_eq!(t.svc.prism_stats(a.id).await.unwrap().payments, 1);

    // One payout for the one split; the payment itself is not split twice.
    let payouts = sqlx::query("SELECT destination, incoming_tx_id FROM prism_payouts")
        .fetch_all(&t.svc.pool)
        .await
        .unwrap();
    assert_eq!(payouts.len(), 1);
}

/// A plain `user@domain` of ours is a BIP-353 name when it publishes a record (the wallet
/// resolves BIP-353 before LNURL); an external plain address stays an LNURL target.
#[tokio::test]
async fn a_plain_address_of_ours_keeps_its_bip353_kind() {
    use crate::wallet::bip353::Bip353Resolver;
    use std::collections::HashMap;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    let b = t.svc.add_address(d, "bob", None).await.unwrap();

    // Our own address, both names on: it answers BIP-353, so it is a BIP-353 name.
    let splits =
        t.svc.set_prism_splits(a.id, &[("bob@pay.example.com".to_string(), 1_000)]).await.unwrap();
    assert_eq!(splits[0].kind, "bip353");
    assert_eq!(t.svc.prism_splits(a.id).await.unwrap()[0].kind, "bip353");

    // Rows saved before this fix say "address" (migration 0021); reading them shows what
    // our own address is now.
    sqlx::query("UPDATE prism_splits SET kind = 'address' WHERE address_id = $1")
        .bind(a.id)
        .execute(&t.svc.pool)
        .await
        .unwrap();
    assert_eq!(t.svc.prism_splits(a.id).await.unwrap()[0].kind, "bip353");

    // A BIP-353-off address of ours is only an LNURL address, and naming it with ₿ is
    // refused because no record is published.
    t.svc.set_address_kinds(b.id, None, Some(false)).await.unwrap();
    let splits =
        t.svc.set_prism_splits(a.id, &[("bob@pay.example.com".to_string(), 1_000)]).await.unwrap();
    assert_eq!(splits[0].kind, "lnurl");
    let e = t
        .svc
        .set_prism_splits(a.id, &[("₿bob@pay.example.com".to_string(), 1_000)])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("does not publish a BIP-353 record"), "{e}");

    // Someone else's plain address: the save asks it. A local LNURL service answers, so
    // it is stored as LNURL; one that answers nothing is refused instead of accepted.
    t.wallet.allow_http_lnurl();
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::new())));
    let app = Router::new().route(
        "/.well-known/lnurlp/{user}",
        axum::routing::get(|| async {
            Json(json!({
                "tag": "payRequest",
                "callback": "https://example.com/cb",
                "minSendable": 1_000,
                "maxSendable": 100_000_000,
                "metadata": "[]",
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let splits = t
        .svc
        .set_prism_splits(a.id, &[(format!("someone@127.0.0.1:{port}"), 1_000)])
        .await
        .unwrap();
    assert_eq!(splits[0].kind, "lnurl");
    let e = t
        .svc
        .set_prism_splits(a.id, &[("nobody@127.0.0.1:1".to_string(), 1_000)])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("cannot receive a Lightning payment"), "{e}");

    // A BIP-353 record that only names an on-chain address is not a Lightning target.
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([(
        "chain@example.org".to_string(),
        vec!["bitcoin:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string()],
    )]))));
    let e = t
        .svc
        .set_prism_splits(a.id, &[("chain@example.org".to_string(), 1_000)])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("on-chain"), "{e}");
}

/// A split that would pay one of our own addresses and come back is refused when saved, in
/// every way of naming the address, and a loop of any length is seen.
#[tokio::test]
async fn prism_splits_refuse_loops() {
    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    let b = t.svc.add_address(d, "bob", None).await.unwrap();
    let c = t.svc.add_address(d, "carol", None).await.unwrap();
    // Offers are bech32 without a checksum, like the node writes them.
    let offer = bech32::encode::<bech32::NoChecksum>(
        bech32::Hrp::parse("lno").unwrap(),
        b"prism-loop-test",
    )
    .unwrap();
    let a_offer = a.offer_id.clone().expect("the address has an offer");

    // Every way of naming this address is refused as a split of itself.
    let lnurl = bech32::encode::<bech32::Bech32>(
        bech32::Hrp::parse("lnurl").unwrap(),
        b"https://pay.example.com/.well-known/lnurlp/sats",
    )
    .unwrap();
    for destination in [
        "sats@pay.example.com",
        "₿sats@pay.example.com",
        "lnurlp://pay.example.com/.well-known/lnurlp/sats",
        lnurl.as_str(),
    ] {
        let e =
            t.svc.set_prism_splits(a.id, &[(destination.to_string(), 1_000)]).await.unwrap_err();
        assert!(e.to_string().contains("itself"), "{destination}: {e}");
    }
    // Its own BOLT12 offer is it too.
    sqlx::query("UPDATE offers SET offer = $1 WHERE offer_id = $2")
        .bind(&offer)
        .bind(&a_offer)
        .execute(&t.svc.pool)
        .await
        .unwrap();
    let e = t.svc.set_prism_splits(a.id, &[(offer.clone(), 1_000)]).await.unwrap_err();
    assert!(e.to_string().contains("itself"), "{e}");

    // One way between two of our addresses is fine; the way back would loop.
    t.svc.set_prism_splits(b.id, &[("sats@pay.example.com".to_string(), 1_000)]).await.unwrap();
    let e = t
        .svc
        .set_prism_splits(a.id, &[("bob@pay.example.com".to_string(), 1_000)])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("through another prism"), "{e}");

    // A third address closes the longer cycle too: carol → bob → sats → carol.
    t.svc.set_prism_splits(a.id, &[("carol@pay.example.com".to_string(), 1_000)]).await.unwrap();
    let e = t
        .svc
        .set_prism_splits(c.id, &[("bob@pay.example.com".to_string(), 1_000)])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("through another prism"), "{e}");

    // Pointing at one of ours that does not lead back here is allowed.
    let splits = t
        .svc
        .set_prism_splits(b.id, &[("carol@pay.example.com".to_string(), 1_000)])
        .await
        .unwrap();
    assert_eq!(splits[0].kind, "bip353");
}

/// A loop that reaches the database anyway (an older configuration, or a save racing
/// another) is not paid: the worker cancels the payout and nothing leaves.
#[tokio::test]
async fn prism_payouts_do_not_loop() {
    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let a = t.svc.add_address(d, "sats", None).await.unwrap();
    let b = t.svc.add_address(d, "bob", None).await.unwrap();
    // sats → bob was fine when saved.
    t.svc.set_prism_splits(a.id, &[("bob@pay.example.com".to_string(), 1_000)]).await.unwrap();
    // Now bob also forwards back to sats, stored directly as an older version could have.
    let t0 = now();
    sqlx::query(
        "INSERT INTO prism_splits (address_id, position, destination, kind, percent_bps, \
         created_at) VALUES ($1, 0, $2, 'bip353', 1_000, $3)",
    )
    .bind(b.id)
    .bind("sats@pay.example.com")
    .bind(t0)
    .execute(&t.svc.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO prism_payouts (address_id, incoming_tx_id, destination, kind, amount_msat, \
         percent_bps, state, next_retry_at, created_at, updated_at) \
         VALUES ($1, 1, $2, 'bip353', 10_000, 1_000, 'PENDING', $3, $3, $3)",
    )
    .bind(a.id)
    .bind("bob@pay.example.com")
    .bind(t0)
    .execute(&t.svc.pool)
    .await
    .unwrap();

    t.svc.prism_sweep(&t.wallet).await;

    let (state, error): (String, Option<String>) = {
        use sqlx::Row;
        let r = sqlx::query("SELECT state, last_error FROM prism_payouts WHERE id = 1")
            .fetch_one(&t.svc.pool)
            .await
            .unwrap();
        (r.try_get(0).unwrap(), r.try_get(1).unwrap())
    };
    assert_eq!(state, "CANCELED");
    assert!(error.unwrap().contains("avoid a loop"));
    // Not even an outgoing transaction was written.
    assert!(t.svc.payout_out_tx(1).await.unwrap().is_none());
    let outgoing: i64 = {
        use sqlx::Row;
        sqlx::query("SELECT COUNT(*) FROM transactions WHERE direction = 'outgoing'")
            .fetch_one(&t.svc.pool)
            .await
            .unwrap()
            .try_get(0)
            .unwrap()
    };
    assert_eq!(outgoing, 0);
}

/// Resolution is repeated at payment time. An external name may change after validation and
/// return an invoice minted by one of our own public callbacks; that must not become an internal
/// transfer which is then split again.
#[tokio::test]
async fn prism_payout_rejects_indirect_self_payment() {
    use crate::wallet::bip353::Bip353Resolver;
    use allod_ln::mock::MockBackend;
    use std::collections::HashMap;

    let t =
        harness(Default::default(), async |_: &Db, _: &SecretStore, _: &Arc<AppService>| {}).await;
    t.wallet.start();
    let external = MockBackend::make_invoice(Some(10_000), &"44".repeat(32));
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([
        ("relay@example.com".to_string(), vec![format!("bitcoin:?lightning={external}")]),
        ("offer-relay@example.com".to_string(), vec![format!("bitcoin:?lightning={external}")]),
    ]))));

    let d = t.svc.add_domain("pay.example.com", None, None, true).await.unwrap();
    let source = t.svc.add_address(d, "source", None).await.unwrap();
    t.svc
        .set_prism_splits(
            source.id,
            &[
                ("₿relay@example.com".to_string(), 1_000),
                ("₿offer-relay@example.com".to_string(), 1_000),
            ],
        )
        .await
        .unwrap();
    let receiver = t.svc.add_address(d, "receiver", None).await.unwrap();
    let local_offer: String = sqlx::query_scalar("SELECT offer FROM offers WHERE offer_id = $1")
        .bind(receiver.offer_id.as_deref().unwrap())
        .fetch_one(&t.svc.pool)
        .await
        .unwrap();
    let callback = t
        .svc
        .lnurl_callback(
            "pay.example.com",
            "receiver",
            &HashMap::from([("amount".to_string(), "10000".to_string())]),
        )
        .await
        .unwrap();
    let local_invoice = callback["pr"].as_str().unwrap().to_string();
    let incoming = t.wallet.incoming_by_invoice(&local_invoice).await.unwrap().unwrap();

    // Destination changed after save: it now hides our local invoice behind signed DNS.
    t.wallet.set_bip353_resolver(Bip353Resolver::Fixed(Arc::new(HashMap::from([
        ("relay@example.com".to_string(), vec![format!("bitcoin:?lightning={local_invoice}")]),
        ("offer-relay@example.com".to_string(), vec![format!("bitcoin:?lightning={local_offer}")]),
    ]))));
    let t0 = now();
    sqlx::query(
        "INSERT INTO prism_payouts (address_id, incoming_tx_id, destination, kind, amount_msat, \
         percent_bps, state, next_retry_at, created_at, updated_at) \
         VALUES ($1, $4, $2, 'bip353', 10_000, 1_000, 'PENDING', $3, $3, $3)",
    )
    .bind(source.id)
    .bind("₿relay@example.com")
    .bind(t0)
    .bind(incoming.id)
    .execute(&t.svc.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO prism_payouts (address_id, incoming_tx_id, destination, kind, amount_msat, \
         percent_bps, state, next_retry_at, created_at, updated_at) \
         VALUES ($1, $4, $2, 'bip353', 10_000, 1_000, 'PENDING', $3, $3, $3)",
    )
    .bind(source.id)
    .bind("₿offer-relay@example.com")
    .bind(t0)
    .bind(incoming.id)
    .execute(&t.svc.pool)
    .await
    .unwrap();

    t.svc.prism_sweep(&t.wallet).await;

    let payouts: Vec<(String, Option<i64>, Option<String>)> =
        sqlx::query_as("SELECT state, out_tx_id, last_error FROM prism_payouts ORDER BY id")
            .fetch_all(&t.svc.pool)
            .await
            .unwrap();
    assert_eq!(payouts.len(), 2);
    for (state, out_tx_id, error) in payouts {
        assert_eq!(state, "FAILED");
        assert_eq!(out_tx_id, None);
        assert!(error.unwrap().contains("resolved to this wallet"));
    }
    assert_eq!(t.wallet.transaction(&incoming.public_id).await.unwrap().state, "PENDING");
    let outgoing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE direction = 'outgoing'")
            .fetch_one(&t.svc.pool)
            .await
            .unwrap();
    assert_eq!(outgoing, 0);
}
