//! Buying a domain at Njalla (SPEC §12.1): search (`find-domains`, prices from
//! `get-tlds`), a bitcoin top-up of the Njalla wallet when it is short (`add-payment`,
//! paid on-chain from this node or any wallet), then, once the balance covers the price,
//! `register-domain` (a task, followed with `check-task` and `get-domain`). The bought
//! domain gets DNSSEC and is added to Allod with the Njalla provider, served the way the
//! owner chose, so its records follow by themselves. One purchase at a time.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{now, provider_secret, store, DomainError, DomainService};

/// Config key of the purchase in progress (or the last one).
const PURCHASE_KEY: &str = "domains.njalla_purchase";
/// Njalla top-ups: 5 € or a multiple of 15 €, at most 300 €.
const MAX_TOPUP_EUR: u64 = 300;
/// Give up waiting for the top-up after this long (the BTC price was quoted).
const TOPUP_SECS: i64 = 3 * 86_400;
/// Give up on a registration that does not finish in a day.
const REGISTER_SECS: i64 = 86_400;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopUp {
    pub id: String,
    pub amount_eur: u64,
    pub amount_btc: String,
    /// The amount in sats (rounded up).
    pub sats: u64,
    pub address: String,
    /// BIP21 URI.
    pub uri: String,
    /// Njalla's words for it ("Waiting for transaction …").
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Purchase {
    pub provider_id: i64,
    pub domain: String,
    pub years: u32,
    /// Price for all years, euros.
    pub price_eur: u64,
    pub dnssec: bool,
    /// `topup` (waiting for the wallet), `registering`, `done` or `failed`.
    pub state: String,
    pub topup: Option<TopUp>,
    pub balance_eur: Option<u64>,
    pub task: Option<String>,
    pub error: Option<String>,
    pub since: i64,
    /// How the domain is served once bought.
    pub gateway_id: Option<i64>,
    pub direct: bool,
    /// The Allod domain, once added.
    pub domain_id: Option<i64>,
}

/// The smallest top-up Njalla accepts that covers `need` euros.
pub fn topup_amount(need: u64) -> Option<u64> {
    let a = if need <= 5 { 5 } else { need.div_ceil(15) * 15 };
    (a <= MAX_TOPUP_EUR).then_some(a)
}

/// Sats for a BTC amount written as text ("0.000083"), rounded up.
pub fn btc_to_sats(btc: &str) -> Option<u64> {
    let (whole, frac) = btc.trim().split_once('.').unwrap_or((btc.trim(), ""));
    if frac.len() > 8 || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let frac: u64 = format!("{frac:0<8}").parse().ok()?;
    whole.checked_mul(100_000_000)?.checked_add(frac)
}

impl DomainService {
    async fn njalla_token(&self, provider_id: i64) -> Result<String, DomainError> {
        let p = store::provider(&self.pool, provider_id)
            .await?
            .ok_or_else(|| super::not_found("DNS provider"))?;
        if p.kind != "njalla" {
            return Err(DomainError::BadRequest(
                "domains are bought with a Njalla provider".into(),
            ));
        }
        Ok(self
            .secrets
            .read(&provider_secret(provider_id))?
            .ok_or_else(|| {
                DomainError::Conflict("the Njalla token is missing; enter it again".into())
            })?
            .to_string())
    }

    async fn njalla_rpc(&self, token: &str, method: &str, params: Value) -> Result<Value, String> {
        self.njalla.rpc(token, method, params).await.map_err(|e| e.to_string())
    }

    /// Names like `query` at Njalla: `[{name, status, price}]` (euros a year; status
    /// `available`, `taken`, `unavailable`, `own`, `in progress` or `failed`).
    pub async fn njalla_search(&self, provider_id: i64, query: &str) -> Result<Value, DomainError> {
        let query = query.trim().to_ascii_lowercase();
        if query.is_empty() || query.contains(char::is_whitespace) {
            return Err(DomainError::BadRequest("enter a name, like mynode or mynode.com".into()));
        }
        let token = self.njalla_token(provider_id).await?;
        let r = self
            .njalla_rpc(&token, "find-domains", json!({"query": query}))
            .await
            .map_err(DomainError::Upstream)?;
        let mut list: Vec<Value> = r["domains"].as_array().cloned().unwrap_or_default();
        // The exact name first, then cheaper ones.
        list.sort_by_key(|d| {
            (d["name"] != query.as_str(), d["price"].as_u64().unwrap_or(u64::MAX))
        });
        Ok(json!({"domains": list}))
    }

    async fn purchase(&self) -> Option<Purchase> {
        let e = self.db.config_get(PURCHASE_KEY).await.ok()??;
        serde_json::from_str(&e.value).ok()
    }

    async fn save_purchase(&self, p: &Purchase) -> Result<(), DomainError> {
        self.db
            .config_set(PURCHASE_KEY, &serde_json::to_string(p).unwrap_or_default(), false)
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    pub(super) async fn njalla_purchase_view(&self) -> Value {
        self.purchase()
            .await
            .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
            .unwrap_or(Value::Null)
    }

    pub(super) async fn njalla_purchase_active(&self) -> bool {
        self.purchase().await.is_some_and(|p| p.state == "topup" || p.state == "registering")
    }

    /// Starts buying `domain` for `years`: tops the Njalla wallet up when it is short (the
    /// owner pays the returned bitcoin address), then registers it in the background.
    pub async fn njalla_buy(
        &self,
        provider_id: i64,
        domain: &str,
        years: u32,
        gateway_id: Option<i64>,
        direct: bool,
    ) -> Result<Purchase, DomainError> {
        if self.njalla_purchase_active().await {
            return Err(DomainError::Conflict("a domain is being bought already".into()));
        }
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let Some((_, tld)) = domain.split_once('.') else {
            return Err(DomainError::BadRequest("give the whole name, like mynode.com".into()));
        };
        if domain.matches('.').count() != 1 {
            return Err(DomainError::BadRequest(
                "buy the registered name itself, like mynode.com".into(),
            ));
        }
        if let Some(g) = gateway_id {
            store::gateway(&self.pool, g).await?.ok_or_else(|| super::not_found("gateway"))?;
        }
        if store::list_domains(&self.pool).await?.iter().any(|d| d.name == domain) {
            return Err(DomainError::Conflict(format!("{domain} is in Allod already")));
        }
        let token = self.njalla_token(provider_id).await?;
        let tlds =
            self.njalla_rpc(&token, "get-tlds", json!({})).await.map_err(DomainError::Upstream)?;
        let t = &tlds[format!(".{tld}")];
        let price = t["price"].as_u64().ok_or_else(|| {
            DomainError::BadRequest(format!("Njalla does not sell .{tld} domains"))
        })?;
        let max_years = t["max_years"].as_u64().unwrap_or(1).max(1) as u32;
        if years == 0 || years > max_years {
            return Err(DomainError::BadRequest(format!("1 to {max_years} years for .{tld}")));
        }
        // Available? A failed or pending check is not a yes: money would wait in the
        // Njalla wallet for a name that cannot be bought.
        let found = self
            .njalla_rpc(&token, "find-domains", json!({"query": domain}))
            .await
            .map_err(DomainError::Upstream)?;
        let status = found["domains"]
            .as_array()
            .and_then(|l| l.iter().find(|d| d["name"] == domain.as_str()))
            .and_then(|d| d["status"].as_str().map(str::to_string))
            .unwrap_or_else(|| "failed".into());
        match status.as_str() {
            "available" => {}
            "in progress" | "failed" => {
                return Err(DomainError::Upstream(format!(
                    "Njalla cannot say yet whether {domain} is free; try again in a minute"
                )))
            }
            "own" => {
                return Err(DomainError::Conflict(format!(
                    "{domain} is yours at Njalla already: add it in Domains"
                )))
            }
            _ => return Err(DomainError::Conflict(format!("{domain} is taken"))),
        }
        let price_eur = price * u64::from(years);
        let mut p = Purchase {
            provider_id,
            domain: domain.clone(),
            years,
            price_eur,
            dnssec: t["dnssec"].as_bool().unwrap_or(false),
            state: "topup".into(),
            since: now(),
            gateway_id,
            direct: direct && gateway_id.is_none(),
            ..Default::default()
        };
        let balance = self.njalla_balance(&token).await.map_err(DomainError::Upstream)?;
        p.balance_eur = Some(balance);
        if balance < price_eur {
            let amount = topup_amount(price_eur - balance).ok_or_else(|| {
                DomainError::BadRequest(format!(
                    "{price_eur} € is more than one top-up (300 €) covers"
                ))
            })?;
            p.topup = Some(self.njalla_topup(&token, amount).await.map_err(DomainError::Upstream)?);
        }
        tracing::info!(%domain, price_eur, balance, "Njalla: buying a domain");
        self.save_purchase(&p).await?;
        self.trigger();
        Ok(p)
    }

    async fn njalla_balance(&self, token: &str) -> Result<u64, String> {
        let r = self.njalla_rpc(token, "get-balance", json!({})).await?;
        Ok(r["balance"].as_f64().map(|b| b.max(0.0).floor() as u64).unwrap_or(0))
    }

    async fn njalla_topup(&self, token: &str, amount: u64) -> Result<TopUp, String> {
        let r = self
            .njalla_rpc(token, "add-payment", json!({"amount": amount, "via": "bitcoin"}))
            .await?;
        let btc = r["amount_btc"].as_str().ok_or("Njalla gave no bitcoin amount")?.to_string();
        let sats = btc_to_sats(&btc).ok_or("Njalla gave an unreadable bitcoin amount")?;
        let address = r["address"].as_str().ok_or("Njalla gave no bitcoin address")?.to_string();
        Ok(TopUp {
            id: r["id"].as_str().unwrap_or_default().to_string(),
            amount_eur: r["amount"].as_u64().unwrap_or(amount),
            uri: r["uri"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("bitcoin:{address}?amount={btc}")),
            amount_btc: btc,
            sats,
            address,
            status: r["status"].as_str().map(str::to_string),
        })
    }

    /// Stops a purchase waiting for its top-up (money already sent stays in the Njalla
    /// wallet, for the next one), or forgets a finished one (done or stopped), so it is no
    /// longer shown.
    pub async fn njalla_buy_cancel(&self) -> Result<(), DomainError> {
        let Some(mut p) = self.purchase().await else { return Ok(()) };
        match p.state.as_str() {
            "registering" => Err(DomainError::Conflict(
                "the domain is being registered; it cannot be stopped now".into(),
            )),
            "topup" => {
                p.state = "failed".into();
                p.error = Some("cancelled".into());
                self.save_purchase(&p).await
            }
            _ => {
                self.db
                    .config_delete(PURCHASE_KEY)
                    .await
                    .map_err(|e| DomainError::Internal(e.to_string()))?;
                Ok(())
            }
        }
    }

    /// Advances the purchase: wallet funded → register → domain added.
    pub(super) async fn njalla_buy_upkeep(&self) {
        let Some(mut p) = self.purchase().await else { return };
        if p.state != "topup" && p.state != "registering" {
            return;
        }
        let before = serde_json::to_string(&p).unwrap_or_default();
        if let Err(e) = self.njalla_buy_step(&mut p).await {
            tracing::warn!(error = %e, domain = %p.domain, "Njalla purchase");
            p.error = Some(e);
        }
        if serde_json::to_string(&p).unwrap_or_default() != before {
            let _ = self.save_purchase(&p).await;
        }
    }

    async fn njalla_buy_step(&self, p: &mut Purchase) -> Result<(), String> {
        let token = self.njalla_token(p.provider_id).await.map_err(|e| e.to_string())?;
        if p.state == "topup" {
            let balance = self.njalla_balance(&token).await?;
            if p.balance_eur != Some(balance) {
                tracing::info!(balance, price = p.price_eur, "Njalla: wallet balance");
            }
            p.balance_eur = Some(balance);
            if balance < p.price_eur {
                if let Some(t) = p.topup.as_mut().filter(|t| !t.id.is_empty()) {
                    if let Ok(r) = self.njalla_rpc(&token, "get-payment", json!({"id": t.id})).await
                    {
                        t.status = r["status"].as_str().map(str::to_string);
                    }
                }
                if now() - p.since > TOPUP_SECS {
                    p.state = "failed".into();
                    p.error = Some("the Njalla wallet was not topped up within 3 days".into());
                }
                return Ok(());
            }
            let r = self
                .njalla_rpc(
                    &token,
                    "register-domain",
                    json!({"domain": p.domain, "years": p.years}),
                )
                .await?;
            p.task = r["task"].as_str().map(str::to_string);
            p.state = "registering".into();
            p.since = now();
            p.error = None;
            tracing::info!(domain = %p.domain, "Njalla: wallet funded, registering the domain");
            return Ok(());
        }
        // Registering: done once the domain is in the account.
        if self.njalla_rpc(&token, "get-domain", json!({"domain": p.domain})).await.is_err() {
            if let Some(task) = &p.task {
                let r = self.njalla_rpc(&token, "check-task", json!({"id": task})).await?;
                let st = r["status"].to_string().to_ascii_lowercase();
                if st.contains("fail") || st.contains("error") {
                    p.state = "failed".into();
                    p.error =
                        Some(format!("Njalla could not register {}: {}", p.domain, r["status"]));
                    return Ok(());
                }
            }
            if now() - p.since > REGISTER_SECS {
                p.state = "failed".into();
                p.error = Some("the registration did not finish within a day; check Njalla".into());
            }
            return Ok(());
        }
        tracing::info!(domain = %p.domain, "Njalla: domain registered");
        if p.dnssec {
            if let Err(e) = self
                .njalla_rpc(&token, "edit-domain", json!({"domain": p.domain, "dnssec": true}))
                .await
            {
                tracing::warn!(error = %e, "Njalla: could not turn DNSSEC on");
            }
        }
        let id = self
            .add_domain(&p.domain, Some(p.provider_id), p.gateway_id, p.direct)
            .await
            .map_err(|e| e.to_string())?;
        p.domain_id = Some(id);
        p.state = "done".into();
        p.error = None;
        self.trigger();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topups_and_amounts() {
        assert_eq!(topup_amount(1), Some(5));
        assert_eq!(topup_amount(5), Some(5));
        assert_eq!(topup_amount(6), Some(15));
        assert_eq!(topup_amount(30), Some(30));
        assert_eq!(topup_amount(31), Some(45));
        assert_eq!(topup_amount(301), None);
        assert_eq!(btc_to_sats("0.000083"), Some(8_300));
        assert_eq!(btc_to_sats("1.5"), Some(150_000_000));
        assert_eq!(btc_to_sats("0.123456789"), None);
        assert_eq!(btc_to_sats("abc"), None);
    }
}
