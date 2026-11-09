//! LNVPS gateways (SPEC §12.8): Allod rents a small server at LNVPS, pays it from Main
//! over Lightning, sets it up over SSH and renews it.
//!
//! Buying is a sequence of steps kept on the gateway row (`provider_data`), so a restart
//! resumes where it stopped: `order` (SSH key, VM) → `pay` → `provision` (wait for the
//! IPv4) → `setup` (run the setup script) → `done`, and `renew` before it expires. An
//! invoice is recorded before it is paid, and a payment is never started twice for it.

use std::time::Duration;

use base64::Engine as _;
use nostr::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{mailbox, now, ssh, store, DomainError, DomainService, Gateway};
use crate::wallet::{PayRequest, TxFilter};

pub const DEFAULT_URL: &str = "https://api.lnvps.net";
/// The SSH key Allod logs in to its gateways with.
const SSH_SECRET: &str = "gateway_ssh";
/// Renew this long before the paid time ends.
const RENEW_BEFORE_SECS: i64 = 3 * 86_400;
/// After a failed payment, wait this long before asking for a new invoice.
const PAY_RETRY_SECS: i64 = 600;
/// How often a running gateway's expiry is read again from LNVPS.
const REFRESH_SECS: i64 = 6 * 3600;

#[derive(Debug, Clone)]
pub struct Lnvps {
    http: reqwest::Client,
    url: String,
}

/// What Allod orders: the cheapest monthly server and the newest Ubuntu LTS.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    /// `template` (a standard template) or `custom` (a custom template's pricing).
    pub kind: String,
    pub id: u64,
    #[serde(default)]
    pub cpu: u64,
    #[serde(default)]
    pub memory: u64,
    #[serde(default)]
    pub disk: u64,
    #[serde(default)]
    pub disk_type: String,
    #[serde(default)]
    pub disk_interface: String,
    /// Price of one interval, in msat.
    pub price_msat: u64,
    /// `month`, `year`, …
    pub interval: String,
    pub region: String,
    pub image_id: u64,
    pub image: String,
    /// The image's login user.
    pub username: String,
    /// The same price in LNVPS's own currency (cents, e.g. EUR), before VAT.
    #[serde(default)]
    pub fiat_amount: u64,
    #[serde(default)]
    pub fiat_currency: String,
    /// VAT LNVPS charges this account (percent; from the account), and its Lightning
    /// processing fee (percent; from the payment methods).
    #[serde(default)]
    pub vat_rate: f64,
    #[serde(default)]
    pub fee_rate: f64,
    #[serde(default)]
    pub company_id: u64,
}

impl Plan {
    /// What one interval costs with VAT and fees.
    fn with_extras(&self, amount: u64) -> u64 {
        (amount as f64 * (1.0 + self.vat_rate / 100.0) * (1.0 + self.fee_rate / 100.0)).ceil()
            as u64
    }

    /// The total in msat, VAT and fees included.
    pub fn total_msat(&self) -> u64 {
        self.with_extras(self.price_msat)
    }

    /// The total in LNVPS's currency (cents), VAT and fees included.
    pub fn total_fiat(&self) -> u64 {
        self.with_extras(self.fiat_amount)
    }
}

/// An invoice being paid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Payment {
    /// LNVPS's payment id.
    id: String,
    invoice: String,
    amount_sats: u64,
    /// Seconds of service it buys.
    time: i64,
    /// Our transaction, once the payment started.
    tx: Option<String>,
    expires_at: i64,
}

/// The purchase's progress (`gateways.provider_data`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Progress {
    step: String,
    plan: Plan,
    /// Most the first payment may cost.
    max_sats: u64,
    ssh_key_id: Option<u64>,
    payment: Option<Payment>,
    host_key: Option<String>,
    #[serde(default)]
    retry_at: i64,
    #[serde(default)]
    refreshed_at: i64,
    /// LNVPS asked for a verified email before ordering.
    #[serde(default)]
    email_needed: bool,
    /// Sats paid so far.
    #[serde(default)]
    paid_sats: u64,
    /// The address LNVPS was given for the verification (`npub…@uid.ovh`, or one the
    /// owner entered), and since when Allod looks for the message.
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_since: i64,
    /// A cap in a fiat currency (e.g. 10 EUR, VAT and fees included), converted to sats
    /// at the rate source's price when an invoice arrives; it replaces the sats caps.
    #[serde(default)]
    cap: Option<FiatCap>,
}

/// At most `amount` of `currency` per payment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FiatCap {
    pub currency: String,
    pub amount: f64,
}

fn data(v: &Value) -> &Value {
    v.get("data").unwrap_or(v)
}

impl Lnvps {
    pub fn new(url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Lnvps { http, url: url.trim_end_matches('/').to_string() }
    }

    /// One API call; signed with NIP-98 when `keys` is given.
    async fn call(
        &self,
        keys: Option<&Keys>,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let url = format!("{}{path}", self.url);
        let mut req = self.http.request(method.clone(), &url);
        let raw = body.as_ref().map(|b| b.to_string());
        if let Some(keys) = keys {
            let mut tags = vec![
                Tag::parse(["u", url.as_str()]).map_err(|e| e.to_string())?,
                Tag::parse(["method", method.as_str()]).map_err(|e| e.to_string())?,
            ];
            if let Some(r) = &raw {
                let h = hex::encode(Sha256::digest(r.as_bytes()));
                tags.push(Tag::parse(["payload", h.as_str()]).map_err(|e| e.to_string())?);
            }
            let ev = EventBuilder::new(Kind::HttpAuth, "")
                .tags(tags)
                .finalize(keys)
                .map_err(|e| e.to_string())?;
            let auth = base64::engine::general_purpose::STANDARD.encode(ev.as_json());
            req = req.header("Authorization", format!("Nostr {auth}"));
        }
        if let Some(r) = raw {
            req = req.header("content-type", "application/json").body(r);
        }
        let res = req.send().await.map_err(|e| format!("cannot reach LNVPS: {e}"))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        if !status.is_success() {
            let msg = v
                .get("error")
                .and_then(|e| {
                    e.as_str()
                        .map(str::to_string)
                        .or_else(|| e.get("message").and_then(Value::as_str).map(str::to_string))
                })
                .unwrap_or_else(|| text.chars().take(300).collect());
            return Err(format!("LNVPS answered {status}: {msg}"));
        }
        Ok(data(&v).clone())
    }

    /// Sets the account's email. LNVPS replaces the whole account (its contact
    /// settings too), so the current one goes back with only the email changed.
    async fn set_email(&self, keys: &Keys, account: &Value, email: &str) -> Result<(), String> {
        let mut body = if account.is_object() { account.clone() } else { json!({}) };
        body["email"] = json!(email);
        for (k, default) in [("contact_nip17", json!(false)), ("contact_email", json!(true))] {
            if body.get(k).is_none_or(Value::is_null) {
                body[k] = default;
            }
        }
        self.call(Some(keys), reqwest::Method::PATCH, "/api/v1/account", Some(body))
            .await
            .map(|_| ())
    }

    /// The cheapest monthly server (standard or custom template) with the newest Ubuntu
    /// LTS, priced in msat.
    /// With `keys`, the account's VAT and the Lightning fee are included.
    pub async fn quote(&self, keys: Option<&Keys>) -> Result<Plan, String> {
        use reqwest::Method;
        let t = self.call(None, Method::GET, "/api/v1/vm/templates", None).await?;
        let btc = |cost: &Value| -> Option<u64> {
            if cost.get("currency").and_then(Value::as_str) == Some("BTC") {
                return cost.get("amount").and_then(Value::as_u64);
            }
            cost.get("other_price")?.as_array()?.iter().find_map(|o| {
                (o.get("currency")?.as_str()? == "BTC").then(|| o.get("amount")?.as_u64()).flatten()
            })
        };
        let region = |x: &Value| x["region"]["name"].as_str().unwrap_or_default().to_string();
        let mut plans: Vec<Plan> = Vec::new();
        for x in t["templates"].as_array().into_iter().flatten() {
            let cost = &x["cost_plan"];
            if cost["interval_type"] != "month" {
                continue;
            }
            if let Some(p) = btc(cost) {
                plans.push(Plan {
                    fiat_amount: cost["amount"].as_u64().unwrap_or(0),
                    fiat_currency: cost["currency"].as_str().unwrap_or_default().to_string(),
                    company_id: x["region"]["company_id"].as_u64().unwrap_or(0),
                    kind: "template".into(),
                    id: x["id"].as_u64().unwrap_or_default(),
                    price_msat: p,
                    interval: "month".into(),
                    region: region(x),
                    ..Default::default()
                });
            }
        }
        for c in t["custom_template"].as_array().into_iter().flatten() {
            if c["interval_type"] != "month" {
                continue;
            }
            let disks = c["disks"].as_array().cloned().unwrap_or_default();
            let Some(disk) = disks.iter().find(|d| d["disk_type"] == "ssd").or(disks.first())
            else {
                continue;
            };
            let mut plan = Plan {
                kind: "custom".into(),
                id: c["id"].as_u64().unwrap_or_default(),
                cpu: c["min_cpu"].as_u64().unwrap_or(1),
                memory: c["min_memory"].as_u64().unwrap_or(1 << 30),
                disk: disk["min_disk"].as_u64().unwrap_or(10 << 30),
                disk_type: disk["disk_type"].as_str().unwrap_or("ssd").into(),
                disk_interface: disk["disk_interface"].as_str().unwrap_or("pcie").into(),
                interval: "month".into(),
                region: region(c),
                ..Default::default()
            };
            let body = json!({
                "pricing_id": plan.id, "cpu": plan.cpu, "memory": plan.memory, "disk": plan.disk,
                "disk_type": plan.disk_type, "disk_interface": plan.disk_interface,
                "ip4_count": 1, "ip6_count": 1,
            });
            let price = self
                .call(None, Method::POST, "/api/v1/vm/custom-template/price", Some(body))
                .await?;
            if let Some(p) = btc(&price) {
                plan.price_msat = p;
                plan.fiat_amount = price["amount"].as_u64().unwrap_or(0);
                plan.fiat_currency = price["currency"].as_str().unwrap_or_default().to_string();
                plan.company_id = c["region"]["company_id"].as_u64().unwrap_or(0);
                plans.push(plan);
            }
        }
        let mut plan = plans
            .into_iter()
            .min_by_key(|p| p.price_msat)
            .ok_or("LNVPS offers no monthly server right now")?;
        let images = self.call(None, Method::GET, "/api/v1/image", None).await?;
        let (id, name, user) =
            pick_image(&images).ok_or("LNVPS offers no Ubuntu or Debian image")?;
        plan.image_id = id;
        plan.image = name;
        plan.username = user;
        if let Some(keys) = keys {
            let account = self.call(Some(keys), Method::GET, "/api/v1/account", None).await?;
            let taxes = account["tax"].as_array().cloned().unwrap_or_default();
            plan.vat_rate = taxes
                .iter()
                .find(|t| t["company_id"].as_u64() == Some(plan.company_id))
                .or(taxes.first())
                .and_then(|t| t["rate"].as_f64())
                .unwrap_or(0.0);
            let methods = self.call(None, Method::GET, "/api/v1/payment/methods", None).await?;
            plan.fee_rate = methods
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| m["name"] == "lightning")
                .and_then(|m| m["processing_fee_rate"].as_f64())
                .unwrap_or(0.0);
        }
        Ok(plan)
    }
}

/// The newest Ubuntu LTS (`YY.04` with an even year), else the newest Debian; x86_64.
fn pick_image(images: &Value) -> Option<(u64, String, String)> {
    let list = images.as_array()?;
    let version =
        |i: &Value| -> f64 { i["version"].as_str().unwrap_or("0").parse().unwrap_or(0.0) };
    let lts = |i: &&Value| {
        let v = i["version"].as_str().unwrap_or_default();
        i["distribution"] == "ubuntu"
            && v.ends_with(".04")
            && v.split('.').next().and_then(|y| y.parse::<u32>().ok()).is_some_and(|y| y % 2 == 0)
    };
    let x86 = |i: &&Value| i["cpu_arch"].as_str().is_none_or(|a| a == "x86_64");
    let best = list
        .iter()
        .filter(x86)
        .filter(lts)
        .max_by(|a, b| version(a).total_cmp(&version(b)))
        .or_else(|| {
            list.iter()
                .filter(x86)
                .filter(|i| i["distribution"] == "debian")
                .max_by(|a, b| version(a).total_cmp(&version(b)))
        })?;
    Some((
        best["id"].as_u64()?,
        format!("{} {}", best["distribution"].as_str()?, best["version"].as_str()?),
        best["default_username"].as_str().unwrap_or("root").to_string(),
    ))
}

impl DomainService {
    pub(super) fn lnvps_keys(&self) -> Result<Keys, DomainError> {
        let k =
            self.apps()?.gateway_account_key().map_err(|e| DomainError::Internal(e.to_string()))?;
        Keys::parse(k.expose_secret_hex().as_str())
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    fn ssh_key(&self) -> Result<String, DomainError> {
        if let Some(k) = self.secrets.read(SSH_SECRET)? {
            return Ok(k.to_string());
        }
        let k = ssh::new_key().map_err(DomainError::Internal)?;
        self.secrets.write(SSH_SECRET, &k)?;
        Ok(k)
    }

    /// What a gateway at LNVPS costs now.
    pub async fn lnvps_quote(&self) -> Result<Value, DomainError> {
        let keys = self.lnvps_keys()?;
        let plan = self.lnvps.quote(Some(&keys)).await.map_err(DomainError::Upstream)?;
        Ok(json!({
            "plan": plan,
            "price_sats": plan.price_msat.div_ceil(1000),
            "total_sats": plan.total_msat().div_ceil(1000),
            "total_fiat": {"amount": plan.total_fiat(), "currency": plan.fiat_currency},
            "vat_rate": plan.vat_rate,
            "fee_rate": plan.fee_rate,
            "interval": plan.interval,
        }))
    }

    /// Starts buying a gateway at LNVPS, paying at most `max_sats` for the first month.
    pub async fn lnvps_buy(
        &self,
        label: Option<&str>,
        max_sats: u64,
        auto_renew: bool,
        renew_max_sats: Option<u64>,
        cap: Option<FiatCap>,
    ) -> Result<i64, DomainError> {
        self.wallet
            .get()
            .ok_or_else(|| DomainError::Conflict("the wallet is not running yet".into()))?;
        self.lnvps_keys()?;
        let keys = self.lnvps_keys()?;
        let plan = self.lnvps.quote(Some(&keys)).await.map_err(DomainError::Upstream)?;
        let price = plan.total_msat().div_ceil(1000);
        let max_sats = match &cap {
            Some(c) => self.fiat_cap_sats(c).await.map_err(DomainError::Conflict)?,
            None => max_sats,
        };
        if price > max_sats {
            return Err(DomainError::Conflict(format!(
                "a server at LNVPS costs {price} sats a {} with VAT and fees, more than the {max_sats} sats allowed",
                plan.interval
            )));
        }
        let index = store::free_tunnel_index(&self.pool).await?;
        let label = label.map(str::trim).filter(|l| !l.is_empty()).unwrap_or("LNVPS").to_string();
        let id =
            store::insert_gateway(&self.pool, &label, "lnvps", None, "dedicated", index).await?;
        self.new_gateway_key(id).await?;
        let progress = Progress { step: "order".into(), plan, max_sats, cap, ..Default::default() };
        sqlx::query(
            "UPDATE gateways SET state = 'ordering', provider_data = $1, auto_renew = $2, \
             renew_max_sats = $3 WHERE id = $4",
        )
        .bind(serde_json::to_string(&progress).unwrap_or_default())
        .bind(auto_renew)
        .bind(renew_max_sats.map(|m| m as i64))
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.trigger();
        Ok(id)
    }

    /// Gives LNVPS an email address (it may ask for a verified one before ordering).
    pub async fn lnvps_set_email(&self, email: &str) -> Result<(), DomainError> {
        let email = email.trim();
        if !email.contains('@') || email.len() > 254 {
            return Err(DomainError::BadRequest("enter an email address".into()));
        }
        let keys = self.lnvps_keys()?;
        let account = self
            .lnvps
            .call(Some(&keys), reqwest::Method::GET, "/api/v1/account", None)
            .await
            .map_err(DomainError::Upstream)?;
        self.lnvps.set_email(&keys, &account, email).await.map_err(DomainError::Upstream)?;
        // The owner clicks the link in that mailbox; Allod stops looking at its own.
        for g in
            store::list_gateways(&self.pool).await?.into_iter().filter(|g| g.provider == "lnvps")
        {
            let Some(mut p) =
                g.provider_data.as_deref().and_then(|d| serde_json::from_str::<Progress>(d).ok())
            else {
                continue;
            };
            p.email = Some(email.to_string());
            p.email_since = now();
            sqlx::query("UPDATE gateways SET provider_data = $1 WHERE id = $2")
                .bind(serde_json::to_string(&p).unwrap_or_default())
                .bind(g.id)
                .execute(&self.pool)
                .await?;
        }
        self.trigger();
        Ok(())
    }

    /// The LNVPS account as Allod sees it (for the providers section).
    pub(super) async fn lnvps_view(&self) -> Value {
        let pubkey = self.lnvps_keys().ok().map(|k| k.public_key().to_hex());
        json!({ "account_pubkey": pubkey })
    }

    /// Advances every LNVPS gateway (buying, setting up, renewing).
    pub(super) async fn lnvps_upkeep(&self) {
        let Ok(_buying) = self.buying.try_lock() else { return };
        let Ok(gateways) = store::list_gateways(&self.pool).await else { return };
        let mut active = false;
        for g in gateways.into_iter().filter(|g| g.provider == "lnvps") {
            let mut p: Progress = g
                .provider_data
                .as_deref()
                .and_then(|d| serde_json::from_str(d).ok())
                .unwrap_or_default();
            let before = serde_json::to_string(&p).unwrap_or_default();
            let step_before = p.step.clone();
            let result = self.lnvps_step(&g, &mut p).await;
            let error = result.err();
            if let Some(e) = &error {
                tracing::warn!(gateway = g.id, error = %e, "LNVPS gateway");
            }
            // Mid-purchase and not failing: the next step comes soon (waiting on LNVPS,
            // the payment or the server).
            active |= error.is_none() && p.step != "done";
            if p.step != step_before {
                tracing::info!(gateway = g.id, from = %step_before, to = %p.step, "LNVPS gateway: next step");
                self.trigger();
            }
            let after = serde_json::to_string(&p).unwrap_or_default();
            if after != before || error != g.error {
                let _ =
                    sqlx::query("UPDATE gateways SET provider_data = $1, error = $2 WHERE id = $3")
                        .bind(after)
                        .bind(error)
                        .bind(g.id)
                        .execute(&self.pool)
                        .await;
            }
        }
        self.lnvps_active.store(active, std::sync::atomic::Ordering::Relaxed);
    }

    /// LNVPS wants a verified email before ordering. Allod gives it the account key's
    /// own address (`npub…@uid.ovh`, SPEC §12.8), reads the message from its Nostr
    /// mailbox and follows the verification link. True once verified.
    async fn lnvps_verify_email(&self, keys: &Keys, p: &mut Progress) -> Result<bool, String> {
        let account =
            self.lnvps.call(Some(keys), reqwest::Method::GET, "/api/v1/account", None).await?;
        if account["email_verified"] == true {
            p.email_needed = false;
            return Ok(true);
        }
        let own = mailbox::address(keys)?;
        if p.email.is_none() {
            tracing::info!(email = %own, "LNVPS: verifying the account's email through its uid.ovh mailbox");
            mailbox::publish_relay_lists(keys).await?;
            self.lnvps.set_email(keys, &account, &own).await?;
            p.email = Some(own.clone());
            p.email_since = now();
            return Ok(false);
        }
        // An address the owner entered: they click the link themselves.
        if p.email.as_deref() != Some(own.as_str()) {
            return Ok(false);
        }
        let mail = mailbox::fetch(keys, p.email_since).await?;
        let Some(token) = mail.iter().find_map(|m| mailbox::lnvps_token(m)) else {
            tracing::debug!(messages = mail.len(), "LNVPS: no verification mail yet");
            return Ok(false);
        };
        self.lnvps
            .call(
                Some(keys),
                reqwest::Method::GET,
                &format!("/api/v1/account/verify-email?token={token}"),
                None,
            )
            .await?;
        tracing::info!("LNVPS: email verified through uid.ovh");
        p.email_needed = false;
        Ok(true)
    }

    /// A fiat cap in sats now, at the owner's rate source. No rate, no payment.
    async fn fiat_cap_sats(&self, c: &FiatCap) -> Result<u64, String> {
        let fiat = self.fiat.get().ok_or("no exchange rates here to convert the cap")?;
        let source = self
            .db
            .config_get(allod_db::config_keys::FIAT_SOURCE)
            .await
            .ok()
            .flatten()
            .and_then(|e| crate::RateSource::parse(&e.value))
            .unwrap_or(crate::RateSource::Yadio);
        let price = fiat
            .price(source, &c.currency.to_ascii_uppercase())
            .await
            .map_err(|e| format!("cannot convert the cap of {} {}: {e}", c.amount, c.currency))?;
        if !(price.is_finite() && price > 0.0) {
            return Err("the exchange rate is not usable".into());
        }
        Ok((c.amount / price * 100_000_000.0).floor() as u64)
    }

    /// Sets the fiat cap of an LNVPS gateway (first payment and renewals); `None`
    /// goes back to the sats caps.
    pub async fn lnvps_set_cap(&self, id: i64, cap: Option<FiatCap>) -> Result<(), DomainError> {
        if let Some(c) = &cap {
            if !(c.amount.is_finite() && c.amount > 0.0) || c.currency.trim().len() != 3 {
                return Err(DomainError::BadRequest(
                    "the cap is an amount and a 3-letter currency".into(),
                ));
            }
        }
        let g = store::gateway(&self.pool, id).await?.ok_or_else(|| super::not_found("gateway"))?;
        if g.provider != "lnvps" {
            return Err(DomainError::BadRequest("only LNVPS gateways are paid by Allod".into()));
        }
        let mut p: Progress = g
            .provider_data
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or_default();
        p.cap = cap.map(|c| FiatCap {
            currency: c.currency.trim().to_ascii_uppercase(),
            amount: c.amount,
        });
        // A new cap is worth trying at once.
        p.retry_at = 0;
        sqlx::query("UPDATE gateways SET provider_data = $1, error = NULL WHERE id = $2")
            .bind(serde_json::to_string(&p).unwrap_or_default())
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.trigger();
        Ok(())
    }

    /// Runs the steps that can run now.
    async fn lnvps_step(&self, g: &Gateway, p: &mut Progress) -> Result<(), String> {
        let keys = self.lnvps_keys().map_err(|e| e.to_string())?;
        for _ in 0..6 {
            match p.step.as_str() {
                "order" => {
                    if p.email_needed && !self.lnvps_verify_email(&keys, p).await? {
                        return Ok(());
                    }
                    let key = self.ssh_key().map_err(|e| e.to_string())?;
                    if p.ssh_key_id.is_none() {
                        tracing::info!(gateway = g.id, "LNVPS: registering Allod's SSH key");
                        let public = ssh::public_openssh(&key)?;
                        let r = self
                            .lnvps
                            .call(
                                Some(&keys),
                                reqwest::Method::POST,
                                "/api/v1/ssh-key",
                                Some(json!({"name": "allod", "key_data": public})),
                            )
                            .await;
                        let r = match r {
                            // Expected the first time: verification starts on the next run.
                            Err(e) if wants_email(&e) => {
                                p.email_needed = true;
                                self.trigger();
                                return Ok(());
                            }
                            r => r?,
                        };
                        p.ssh_key_id = r["id"].as_u64();
                    }
                    let ssh_key_id = p.ssh_key_id.ok_or("LNVPS did not return the SSH key id")?;
                    let (path, body) = if p.plan.kind == "custom" {
                        (
                            "/api/v1/vm/custom-template",
                            json!({
                                "pricing_id": p.plan.id, "cpu": p.plan.cpu, "memory": p.plan.memory,
                                "disk": p.plan.disk, "disk_type": p.plan.disk_type,
                                "disk_interface": p.plan.disk_interface, "ip4_count": 1, "ip6_count": 1,
                                "image_id": p.plan.image_id, "ssh_key_id": ssh_key_id,
                            }),
                        )
                    } else {
                        (
                            "/api/v1/vm",
                            json!({"template_id": p.plan.id, "image_id": p.plan.image_id, "ssh_key_id": ssh_key_id}),
                        )
                    };
                    tracing::info!(gateway = g.id, plan = p.plan.id, region = %p.plan.region, "LNVPS: ordering the server");
                    let vm = match self
                        .lnvps
                        .call(Some(&keys), reqwest::Method::POST, path, Some(body))
                        .await
                    {
                        // Expected the first time: verification starts on the next run.
                        Err(e) if wants_email(&e) => {
                            p.email_needed = true;
                            self.trigger();
                            return Ok(());
                        }
                        r => r?,
                    };
                    let vm_id = vm["id"].as_u64().ok_or("LNVPS did not return the server's id")?;
                    tracing::info!(
                        gateway = g.id,
                        vm = vm_id,
                        "LNVPS: server ordered, paying it next"
                    );
                    p.email_needed = false;
                    self.set_gateway(g.id, Some(&vm_id.to_string()), None, None, None).await;
                    p.step = "pay".into();
                    // Saved before anything is paid; paying follows in seconds, not at
                    // the next 5-minute run.
                    self.trigger();
                    return Ok(());
                }
                "pay" | "renew" => {
                    let vm = g.provider_ref.as_deref().ok_or("no server id")?;
                    let cap = match &p.cap {
                        Some(c) => self.fiat_cap_sats(c).await?,
                        None if p.step == "pay" => p.max_sats,
                        None => g.renew_max_sats.unwrap_or(0).max(0) as u64,
                    };
                    if !self.lnvps_pay(&keys, vm, cap, p).await? {
                        return Ok(());
                    }
                    p.step = if p.step == "pay" { "provision".into() } else { "done".into() };
                    p.refreshed_at = 0;
                }
                "provision" => {
                    let vm = self.lnvps_vm(&keys, g).await?;
                    let ipv4 = vm_ip(&vm, false);
                    if vm["status"]["state"] != "running" || ipv4.is_none() {
                        tracing::info!(gateway = g.id, state = %vm["status"]["state"], "LNVPS: waiting for the server to start");
                        return Ok(());
                    }
                    tracing::info!(gateway = g.id, ipv4 = ?ipv4, "LNVPS: server running");
                    let ipv6 = vm_ip(&vm, true);
                    self.set_gateway(g.id, None, ipv4.as_deref(), ipv6.as_deref(), vm_expires(&vm))
                        .await;
                    if let Some(u) = vm["image"]["default_username"].as_str() {
                        p.plan.username = u.to_string();
                    }
                    p.step = "setup".into();
                    return Ok(()); // the next pass sees the IPv4 on the row
                }
                "setup" => {
                    let ip = g.ipv4.as_deref().ok_or("no IPv4 yet")?;
                    let script = self.setup_script(g.id).await.map_err(|e| e.to_string())?;
                    let key = self.ssh_key().map_err(|e| e.to_string())?;
                    let command = if p.plan.username == "root" { "sh -s" } else { "sudo sh -s" };
                    tracing::info!(gateway = g.id, ip, user = %p.plan.username, "LNVPS: running the gateway setup over SSH");
                    let out = ssh::run(
                        ip,
                        &p.plan.username,
                        &key,
                        p.host_key.as_deref(),
                        command,
                        script.as_bytes(),
                    )
                    .await?;
                    if p.host_key.is_none() {
                        p.host_key = out.host_key.clone();
                    }
                    if out.exit != Some(0) {
                        return Err(format!(
                            "the setup script failed ({:?}): {}",
                            out.exit,
                            out.output.lines().last().unwrap_or_default()
                        ));
                    }
                    let _ = sqlx::query("UPDATE gateways SET state = 'setup' WHERE id = $1")
                        .bind(g.id)
                        .execute(&self.pool)
                        .await;
                    tracing::info!(
                        gateway = g.id,
                        ip,
                        "LNVPS: gateway set up; payments can go through it"
                    );
                    p.step = "done".into();
                    return Ok(());
                }
                "done" => {
                    if now() - p.refreshed_at > REFRESH_SECS {
                        let vm = self.lnvps_vm(&keys, g).await?;
                        self.set_gateway(
                            g.id,
                            None,
                            vm_ip(&vm, false).as_deref(),
                            vm_ip(&vm, true).as_deref(),
                            vm_expires(&vm),
                        )
                        .await;
                        p.refreshed_at = now();
                        return Ok(());
                    }
                    let Some(exp) = g.expires_at else { return Ok(()) };
                    if now() > exp && g.state != "expired" {
                        let _ = sqlx::query("UPDATE gateways SET state = 'expired' WHERE id = $1")
                            .bind(g.id)
                            .execute(&self.pool)
                            .await;
                    }
                    if g.auto_renew && exp - now() < RENEW_BEFORE_SECS {
                        tracing::info!(
                            gateway = g.id,
                            expires_at = exp,
                            "LNVPS: renewing the server"
                        );
                        p.step = "renew".into();
                        continue;
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    async fn lnvps_vm(&self, keys: &Keys, g: &Gateway) -> Result<Value, String> {
        let vm = g.provider_ref.as_deref().ok_or("no server id")?;
        self.lnvps.call(Some(keys), reqwest::Method::GET, &format!("/api/v1/vm/{vm}"), None).await
    }

    async fn set_gateway(
        &self,
        id: i64,
        provider_ref: Option<&str>,
        ipv4: Option<&str>,
        ipv6: Option<&str>,
        expires_at: Option<i64>,
    ) {
        let _ = sqlx::query(
            "UPDATE gateways SET provider_ref = COALESCE($1, provider_ref), ipv4 = COALESCE($2, ipv4), \
             ipv6 = COALESCE($3, ipv6), expires_at = COALESCE($4, expires_at), \
             state = CASE WHEN state = 'expired' AND $4 > $5 THEN 'setup' ELSE state END WHERE id = $6",
        )
        .bind(provider_ref)
        .bind(ipv4)
        .bind(ipv6)
        .bind(expires_at)
        .bind(now())
        .bind(id)
        .execute(&self.pool)
        .await;
    }

    /// Pays one interval of server `vm`, at most `cap` sats. True once LNVPS has the
    /// payment. Never starts a second payment for an invoice.
    async fn lnvps_pay(
        &self,
        keys: &Keys,
        vm: &str,
        cap: u64,
        p: &mut Progress,
    ) -> Result<bool, String> {
        let wallet = self.wallet.get().cloned().ok_or("the wallet is not running yet")?;
        if let Some(pay) = p.payment.clone() {
            let st = self
                .lnvps
                .call(
                    Some(keys),
                    reqwest::Method::GET,
                    &format!("/api/v1/payment/{}", pay.id),
                    None,
                )
                .await?;
            if st["is_paid"] == true {
                tracing::info!(sats = pay.amount_sats, "LNVPS: payment confirmed by LNVPS");
                p.paid_sats += pay.amount_sats;
                p.payment = None;
                return Ok(true);
            }
            // Our side: the payment we started, or one started before a restart.
            let tx = match &pay.tx {
                Some(id) => wallet.transaction(id).await.ok(),
                None => {
                    let invoice = pay.invoice.to_ascii_lowercase();
                    wallet
                        .transactions(&TxFilter {
                            app_id: Some(None),
                            direction: Some("outgoing".into()),
                            include_unpaid: true,
                            limit: 50,
                            ..Default::default()
                        })
                        .await
                        .ok()
                        .and_then(|(l, _)| {
                            l.into_iter().find(|t| {
                                t.payment_request.as_deref().map(str::to_ascii_lowercase).as_deref()
                                    == Some(invoice.as_str())
                            })
                        })
                }
            };
            match tx {
                Some(t) if t.state == "FAILED" => {
                    p.payment = None;
                    p.retry_at = now() + PAY_RETRY_SECS;
                    return Err(format!(
                        "paying LNVPS failed: {}",
                        t.failure_reason.unwrap_or_else(|| "payment failed".into())
                    ));
                }
                // In flight or settled: LNVPS records it shortly.
                Some(t) => {
                    tracing::info!(state = %t.state, sats = pay.amount_sats, "LNVPS: payment sent, waiting for LNVPS to confirm it");
                    if let Some(x) = p.payment.as_mut() {
                        x.tx = Some(t.public_id);
                    }
                    return Ok(false);
                }
                None if now() > pay.expires_at => {
                    p.payment = None;
                }
                None => return self.lnvps_send(&wallet, p).await,
            }
        }
        if now() < p.retry_at {
            return Ok(false);
        }
        let inv = self
            .lnvps
            .call(
                Some(keys),
                reqwest::Method::GET,
                &format!("/api/v1/vm/{vm}/renew?method=lightning&intervals=1"),
                None,
            )
            .await?;
        // The invoice carries the price plus VAT and the processing fee (all msat).
        let part = |k: &str| inv[k].as_u64().unwrap_or(0);
        let price = inv["amount"].as_u64().ok_or("LNVPS sent no amount")?;
        let amount_msat = price + part("tax") + part("processing_fee");
        if inv["currency"] != "BTC" {
            return Err(format!("LNVPS asks for {} instead of sats", inv["currency"]));
        }
        let invoice = inv["data"]["lightning"].as_str().ok_or("LNVPS sent no Lightning invoice")?;
        let sats = amount_msat.div_ceil(1000);
        if sats > cap {
            p.retry_at = now() + PAY_RETRY_SECS;
            return Err(format!(
                "LNVPS asks {sats} sats (with VAT and fees), more than the {cap} sats allowed"
            ));
        }
        let d = wallet
            .decode(invoice)
            .await
            .map_err(|e| format!("LNVPS sent an unreadable invoice: {e}"))?;
        // LNVPS rounds the invoice up to a whole sat.
        let fits = d
            .amount_msat
            .is_some_and(|a| a >= amount_msat && a <= amount_msat.div_ceil(1000) * 1000);
        if d.kind != "bolt11" || !d.network_ok || d.is_expired || !fits {
            p.retry_at = now() + PAY_RETRY_SECS;
            return Err(format!(
                "LNVPS sent an invoice for {:?} msat; its price is {price} + {} VAT + {} fee msat",
                d.amount_msat,
                part("tax"),
                part("processing_fee")
            ));
        }
        p.payment = Some(Payment {
            id: inv["id"].as_str().map(str::to_string).unwrap_or_else(|| inv["id"].to_string()),
            invoice: invoice.to_string(),
            amount_sats: sats,
            time: inv["time"].as_i64().unwrap_or(0),
            tx: None,
            expires_at: now() + 600,
        });
        // Recorded before paying: saved by the caller, then paid on the next pass.
        Ok(false)
    }

    /// Starts the recorded payment.
    async fn lnvps_send(&self, wallet: &crate::Wallet, p: &mut Progress) -> Result<bool, String> {
        let Some(pay) = p.payment.as_mut() else { return Ok(false) };
        tracing::info!(sats = pay.amount_sats, "LNVPS: paying the invoice from Main");
        let tx = wallet
            .pay(PayRequest {
                app_id: None,
                payment: pay.invoice.clone(),
                metadata: Some(
                    json!({"recipient_data": {"identifier": "LNVPS"}, "comment": "Allod gateway server (LNVPS)"})
                        .to_string(),
                ),
                ..Default::default()
            })
            .await;
        match tx {
            Ok(t) => {
                pay.tx = Some(t.public_id);
                Ok(false)
            }
            Err(e) => {
                p.payment = None;
                p.retry_at = now() + PAY_RETRY_SECS;
                Err(format!("paying LNVPS failed: {e}"))
            }
        }
    }
}

/// The server's IPv4 (or IPv6), without the prefix length.
fn vm_ip(vm: &Value, v6: bool) -> Option<String> {
    vm["ip_assignments"].as_array()?.iter().find_map(|a| {
        let ip = a["ip"].as_str()?.split('/').next()?.to_string();
        (ip.contains(':') == v6).then_some(ip)
    })
}

/// The paid-until time of a server.
fn vm_expires(vm: &Value) -> Option<i64> {
    crate::public_name::zeus::parse_time(&vm["expires"])
}

/// LNVPS refuses orders until the account's email is verified (403 "Email verification
/// is required…"): not a problem to show, the next step of buying.
fn wants_email(e: &str) -> bool {
    e.to_ascii_lowercase().contains("email")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_and_ips() {
        let images = json!([
            {"id": 1, "distribution": "ubuntu", "version": "24.04", "default_username": "ubuntu", "cpu_arch": "x86_64"},
            {"id": 15, "distribution": "ubuntu", "version": "26.04", "default_username": "ubuntu", "cpu_arch": "x86_64"},
            {"id": 3, "distribution": "ubuntu", "version": "25.10", "default_username": "ubuntu", "cpu_arch": "x86_64"},
            {"id": 6, "distribution": "debian", "version": "13", "default_username": "debian", "cpu_arch": "x86_64"},
        ]);
        assert_eq!(pick_image(&images), Some((15, "ubuntu 26.04".into(), "ubuntu".into())));
        let debian = json!([{"id": 6, "distribution": "debian", "version": "13", "default_username": "debian"}]);
        assert_eq!(pick_image(&debian).unwrap().0, 6);
        let vm = json!({"ip_assignments": [{"ip": "2a13:2c0::1/64"}, {"ip": "185.18.0.9/24"}]});
        assert_eq!(vm_ip(&vm, false).as_deref(), Some("185.18.0.9"));
        assert_eq!(vm_ip(&vm, true).as_deref(), Some("2a13:2c0::1"));
    }
}
