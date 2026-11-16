//! Twelve Cash (twelve.cash): free BIP-353 names that point at a BOLT12 offer, on a
//! DNSSEC-signed domain run by a third party (SPEC §12.3). Free names are random
//! (`adjective.animal@twelve.cash`); custom names cost [`CUSTOM_PRICE_SATS`], paid through
//! the site's Money Dev Kit (MDK) checkout, which Allod drives like the website does.

use std::time::Duration;

use serde_json::{json, Value};

/// The Twelve Cash domain Allod registers free names on.
pub const DOMAIN: &str = "twelve.cash";
/// What twelve.cash charges for a chosen name (its website, 2026-11).
pub const CUSTOM_PRICE_SATS: u64 = 5_000;

/// An MDK checkout, as `/api/mdk` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub id: String,
    /// UNCONFIRMED | CONFIRMED | PENDING_PAYMENT | PAYMENT_RECEIVED | EXPIRED
    pub status: String,
    /// The BOLT11 invoice, once minted.
    pub invoice: Option<String>,
    pub amount_sats: Option<u64>,
}

/// Valid twelve.cash user names: at least 4 of `a-z A-Z 0-9 . - _`.
pub fn valid_custom_name(u: &str) -> bool {
    (4..=64).contains(&u.len())
        && u.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

#[derive(Debug, thiserror::Error)]
pub enum TwelveError {
    #[error("twelve.cash refused the request: {0}")]
    Refused(String),
    #[error("that name is taken: {0}")]
    Taken(String),
    #[error("cannot reach twelve.cash: {0}")]
    Net(String),
    #[error("unexpected answer from twelve.cash: {0}")]
    Format(String),
}

#[derive(Debug, Clone)]
pub struct TwelveCash {
    http: reqwest::Client,
    base: String,
}

impl TwelveCash {
    /// `base` is `https://twelve.cash` (a mock in tests).
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        TwelveCash { http, base: base.trim_end_matches('/').to_string() }
    }

    /// A tRPC mutation (superjson: the input goes in `json`), returning `result.data.json`.
    async fn trpc(&self, procedure: &str, input: Value) -> Result<Value, TwelveError> {
        let res = self
            .http
            .post(format!("{}/api/trpc/{procedure}", self.base))
            .json(&json!({"json": input}))
            .send()
            .await
            .map_err(|e| TwelveError::Net(e.to_string()))?;
        let status = res.status();
        let body: Value = allod_lnurl::bounded_response_json(res, allod_lnurl::JSON_RESPONSE_LIMIT)
            .await
            .map_err(|e| TwelveError::Format(e.to_string()))?;
        if !status.is_success() {
            let msg = body
                .pointer("/error/json/data/zodError/formErrors/0")
                .or_else(|| body.pointer("/error/json/message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            if body.pointer("/error/json/data/code").and_then(Value::as_str) == Some("CONFLICT") {
                return Err(TwelveError::Taken(msg.chars().take(300).collect()));
            }
            return Err(TwelveError::Refused(msg.chars().take(300).collect()));
        }
        Ok(body.pointer("/result/data/json").cloned().unwrap_or(Value::Null))
    }

    /// `payCode.createPayCode`: reserves `user@domain` (PENDING until redeemed) for
    /// `offer`. Returns the pay code id.
    pub async fn create_pay_code(&self, user: &str, offer: &str) -> Result<String, TwelveError> {
        let data = self
            .trpc(
                "payCode.createPayCode",
                json!({"userName": user, "domain": DOMAIN, "lno": offer}),
            )
            .await?;
        data.get("payCodeId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| TwelveError::Format(data.to_string().chars().take(300).collect()))
    }

    /// `payCode.redeemPayCode`: publishes a paid pay code. Call it only after the
    /// checkout shows the payment received.
    pub async fn redeem(&self, pay_code_id: &str) -> Result<(), TwelveError> {
        self.trpc("payCode.redeemPayCode", json!({"payCodeId": pay_code_id})).await.map(|_| ())
    }

    /// `POST /api/mdk {handler, …}` as the MDK browser client sends it: the CSRF token
    /// goes both in the `mdk_csrf` cookie and the header. Returns `data`.
    async fn mdk(&self, handler: &str, payload: Value) -> Result<Value, TwelveError> {
        let token = hex::encode(rand::random::<[u8; 16]>());
        let mut body = json!({"handler": handler});
        if let (Some(o), Value::Object(p)) = (body.as_object_mut(), payload) {
            o.extend(p);
        }
        let res = self
            .http
            .post(format!("{}/api/mdk", self.base))
            .header("cookie", format!("mdk_csrf={token}"))
            .header("x-moneydevkit-csrf-token", &token)
            .json(&body)
            .send()
            .await
            .map_err(|e| TwelveError::Net(e.to_string()))?;
        let status = res.status();
        let body: Value = allod_lnurl::bounded_response_json(res, allod_lnurl::JSON_RESPONSE_LIMIT)
            .await
            .map_err(|e| TwelveError::Format(e.to_string()))?;
        if !status.is_success() {
            let msg = body.get("error").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(TwelveError::Refused(format!(
                "checkout: {}",
                msg.chars().take(300).collect::<String>()
            )));
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| TwelveError::Format("no data in the checkout answer".into()))
    }

    fn checkout(v: &Value) -> Result<Checkout, TwelveError> {
        let id = v.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let status = v.get("status").and_then(Value::as_str).unwrap_or_default().to_string();
        if id.is_empty() || status.is_empty() {
            return Err(TwelveError::Format(v.to_string().chars().take(300).collect()));
        }
        Ok(Checkout {
            id,
            status,
            invoice: v.pointer("/invoice/invoice").and_then(Value::as_str).map(str::to_string),
            amount_sats: v
                .pointer("/invoice/amountSats")
                .or_else(|| v.get("invoiceAmountSats"))
                .and_then(Value::as_u64),
        })
    }

    /// Creates the 5,000-sat checkout for a pay code (as the website does) and makes
    /// sure it has an invoice.
    pub async fn checkout_for(
        &self,
        pay_code_id: &str,
        user: &str,
    ) -> Result<Checkout, TwelveError> {
        let data = self
            .mdk(
                "create_checkout",
                json!({"params": {
                    "type": "AMOUNT",
                    "amount": CUSTOM_PRICE_SATS,
                    "currency": "SAT",
                    "title": "Purchase Pay Code",
                    "description": format!("Purchase pay code: {user}@{DOMAIN}"),
                    "successUrl": format!("/new/success?payCodeId={pay_code_id}"),
                    "metadata": {"payCodeId": pay_code_id, "userName": user, "domain": DOMAIN},
                }}),
            )
            .await?;
        let c = Self::checkout(&data)?;
        if c.invoice.is_some() {
            return Ok(c);
        }
        let data = self.mdk("confirm_checkout", json!({"confirm": {"checkoutId": c.id}})).await?;
        Self::checkout(&data)
    }

    /// `get_checkout`: its current status.
    pub async fn get_checkout(&self, id: &str) -> Result<Checkout, TwelveError> {
        let data = self.mdk("get_checkout", json!({"checkoutId": id})).await?;
        Self::checkout(&data)
    }

    /// `payCode.createRandomPayCode` (tRPC with superjson: the input goes in `json`).
    /// Returns the new user name; the TXT record holds `bitcoin:?lno=<offer>`.
    pub async fn create_random(&self, offer: &str) -> Result<String, TwelveError> {
        let res = self
            .http
            .post(format!("{}/api/trpc/payCode.createRandomPayCode", self.base))
            .json(&json!({"json": {"domain": DOMAIN, "lno": offer}}))
            .send()
            .await
            .map_err(|e| TwelveError::Net(e.to_string()))?;
        let status = res.status();
        let body: Value = allod_lnurl::bounded_response_json(res, allod_lnurl::JSON_RESPONSE_LIMIT)
            .await
            .map_err(|e| TwelveError::Format(e.to_string()))?;
        if !status.is_success() {
            let msg = body
                .pointer("/error/json/data/zodError/formErrors/0")
                .or_else(|| body.pointer("/error/json/message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(TwelveError::Refused(msg.chars().take(300).collect()));
        }
        let data = body.pointer("/result/data/json").unwrap_or(&Value::Null);
        match (
            data.get("userName").and_then(Value::as_str),
            data.get("status").and_then(Value::as_str),
        ) {
            (Some(name), Some("ACTIVE") | None) if !name.is_empty() => Ok(name.to_string()),
            _ => Err(TwelveError::Format(body.to_string().chars().take(300).collect())),
        }
    }
}

/// Splits `user@domain` and checks it is a Twelve Cash name.
pub fn parse_address(address: &str) -> Option<(String, String)> {
    let address = address.trim().trim_start_matches('₿').to_ascii_lowercase();
    let (user, domain) = address.split_once('@')?;
    let ok_user = !user.is_empty()
        && user.len() <= 64
        && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    (ok_user && matches!(domain, "twelve.cash" | "12cash.dev"))
        .then(|| (user.to_string(), domain.to_string()))
}

/// The BIP-353 TXT values published for `user@domain`, each with its strings joined.
/// Asked from the domain's own name servers, so a new record shows up at once.
pub async fn lookup(user: &str, domain: &str) -> Option<Vec<String>> {
    crate::wallet::bip353::authoritative_txt(
        &format!("{user}.user._bitcoin-payment.{domain}."),
        domain,
    )
    .await
}

/// Whether one of the published values pays `offer`.
pub fn points_at(values: &[String], offer: &str) -> bool {
    values.iter().any(|v| {
        v.to_ascii_lowercase().starts_with("bitcoin:")
            && v.split(['?', '&']).any(|p| p.eq_ignore_ascii_case(&format!("lno={offer}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(
            parse_address(" Brave.Penguin@twelve.cash ").unwrap(),
            ("brave.penguin".into(), "twelve.cash".into())
        );
        assert_eq!(parse_address("₿satoshi@12cash.dev").unwrap().0, "satoshi");
        assert!(parse_address("alice@example.com").is_none());
        assert!(parse_address("@twelve.cash").is_none());
        assert!(parse_address("a b@twelve.cash").is_none());
    }

    #[test]
    fn records_must_carry_the_offer() {
        let v = vec!["bitcoin:?lno=lno1abc&sp=sp1x".to_string()];
        assert!(points_at(&v, "lno1abc"));
        assert!(!points_at(&v, "lno1ab"));
        assert!(!points_at(&["lno=lno1abc".into()], "lno1abc"));
    }
}
