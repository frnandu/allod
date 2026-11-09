use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use super::*;
use crate::apps::NewApp;

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
                axum::response::Html(if cookie.contains("sessionid=ok") {
                    form.to_string()
                } else {
                    r#"<form id="login"><input type="hidden" name="csrfmiddlewaretoken" value="csrf-1"><input type="hidden" name="cnonce" value=""><input type="hidden" name="nonce" value="0"></form>"#.into()
                })
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
    let backend: Arc<dyn allod_ln::LightningBackend> = Arc::new(allod_ln::mock::MockBackend::new());
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
    T { svc, apps, wallet, db, _dir: dir }
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
