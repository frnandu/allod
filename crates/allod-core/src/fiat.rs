//! Fiat exchange rates (SPEC §13.2). Allod fetches rates server-side from the source
//! the owner chose, so the browser never contacts a rate service; results are cached.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_TTL: Duration = Duration::from_secs(60);

/// Where rates come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateSource {
    /// yadio.io: about 145 currencies (default).
    Yadio,
    /// mempool.space: USD, EUR, GBP, CAD, CHF, AUD, JPY.
    Mempool,
    /// CoinGecko: about 45 fiat currencies.
    Coingecko,
}

impl RateSource {
    pub const ALL: [RateSource; 3] =
        [RateSource::Yadio, RateSource::Mempool, RateSource::Coingecko];

    pub fn as_str(self) -> &'static str {
        match self {
            RateSource::Yadio => "yadio",
            RateSource::Mempool => "mempool",
            RateSource::Coingecko => "coingecko",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Host shown to the user ("rates are fetched from …").
    pub fn host(self) -> &'static str {
        match self {
            RateSource::Yadio => "api.yadio.io",
            RateSource::Mempool => "mempool.space",
            RateSource::Coingecko => "api.coingecko.com",
        }
    }
}

/// BTC price per currency (ISO 4217, uppercase).
pub type Rates = BTreeMap<String, f64>;

#[derive(Debug, thiserror::Error)]
pub enum FiatError {
    #[error("rate source {0} unavailable: {1}")]
    Unavailable(&'static str, String),
    #[error("{0} is not offered by {1}")]
    UnknownCurrency(String, &'static str),
}

/// Codes some sources list that are not fiat money.
const NOT_FIAT: &[&str] = &[
    "BTC", "ETH", "LTC", "BCH", "BNB", "EOS", "XRP", "XLM", "LINK", "DOT", "YFI", "SOL", "BITS",
    "SATS", "XDR", "XAG", "XAU", "USDT", "USDC",
];

fn is_fiat_code(code: &str) -> bool {
    code.len() == 3 && code.chars().all(|c| c.is_ascii_uppercase()) && !NOT_FIAT.contains(&code)
}

/// Parses yadio `GET /exrates/BTC`: `{"BTC": {"USD": 81573.3, ...}}`.
pub fn parse_yadio(v: &serde_json::Value) -> Rates {
    numbers(v.get("BTC"))
}

/// Parses mempool `GET /api/v1/prices`: `{"time": .., "USD": 81548, ...}`.
pub fn parse_mempool(v: &serde_json::Value) -> Rates {
    numbers(Some(v))
}

/// Parses CoinGecko `simple/price?ids=bitcoin&vs_currencies=..`: `{"bitcoin": {"eur": ..}}`.
pub fn parse_coingecko(v: &serde_json::Value) -> Rates {
    numbers(v.get("bitcoin"))
}

fn numbers(v: Option<&serde_json::Value>) -> Rates {
    let mut out = Rates::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            let code = k.to_ascii_uppercase();
            if let Some(n) = val.as_f64() {
                if is_fiat_code(&code) && n.is_finite() && n > 0.0 {
                    out.insert(code, n);
                }
            }
        }
    }
    out
}

/// Fetches and caches rates.
pub struct FiatService {
    http: reqwest::Client,
    cache: Mutex<HashMap<RateSource, (Instant, Rates)>>,
}

impl Default for FiatService {
    fn default() -> Self {
        Self::new()
    }
}

impl FiatService {
    pub fn new() -> Self {
        FiatService {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("allod/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn get(&self, source: RateSource, url: &str) -> Result<serde_json::Value, FiatError> {
        let err = |e: reqwest::Error| FiatError::Unavailable(source.host(), e.to_string());
        self.http
            .get(url)
            .send()
            .await
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }

    async fn fetch(&self, source: RateSource) -> Result<Rates, FiatError> {
        let rates = match source {
            RateSource::Yadio => {
                parse_yadio(&self.get(source, "https://api.yadio.io/exrates/BTC").await?)
            }
            RateSource::Mempool => {
                parse_mempool(&self.get(source, "https://mempool.space/api/v1/prices").await?)
            }
            RateSource::Coingecko => {
                let list = self
                    .get(source, "https://api.coingecko.com/api/v3/simple/supported_vs_currencies")
                    .await?;
                let codes: Vec<String> = list
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str())
                            .filter(|c| is_fiat_code(&c.to_ascii_uppercase()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let url = format!(
                    "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies={}",
                    codes.join(",")
                );
                parse_coingecko(&self.get(source, &url).await?)
            }
        };
        if rates.is_empty() {
            return Err(FiatError::Unavailable(source.host(), "no rates in response".into()));
        }
        Ok(rates)
    }

    /// All rates from `source`, cached for a minute.
    pub async fn rates(&self, source: RateSource) -> Result<Rates, FiatError> {
        let mut cache = self.cache.lock().await;
        if let Some((at, rates)) = cache.get(&source) {
            if at.elapsed() < CACHE_TTL {
                return Ok(rates.clone());
            }
        }
        let rates = self.fetch(source).await?;
        cache.insert(source, (Instant::now(), rates.clone()));
        Ok(rates)
    }

    /// Currencies `source` offers, sorted.
    pub async fn currencies(&self, source: RateSource) -> Result<Vec<String>, FiatError> {
        Ok(self.rates(source).await?.into_keys().collect())
    }

    /// BTC price in `currency`.
    pub async fn price(&self, source: RateSource, currency: &str) -> Result<f64, FiatError> {
        let rates = self.rates(source).await?;
        rates
            .get(currency)
            .copied()
            .ok_or_else(|| FiatError::UnknownCurrency(currency.to_string(), source.host()))
    }

    /// Test helper: preload the cache.
    pub async fn preload(&self, source: RateSource, rates: Rates) {
        self.cache.lock().await.insert(source, (Instant::now(), rates));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_each_source_and_drops_non_fiat() {
        let y = parse_yadio(
            &json!({"BTC": {"BTC": 1, "USD": 81573.34, "ARS": 134492392.27, "bad": "x"}}),
        );
        assert_eq!(y.keys().collect::<Vec<_>>(), ["ARS", "USD"]);
        let m = parse_mempool(&json!({"time": 1793177104, "USD": 81548, "EUR": 73301}));
        assert_eq!(m.get("EUR"), Some(&73301.0));
        assert!(!m.contains_key("TIME"));
        let c = parse_coingecko(&json!({"bitcoin": {"eur": 73237, "xau": 30.1, "sats": 1e8}}));
        assert_eq!(c.keys().collect::<Vec<_>>(), ["EUR"]);
        assert!(parse_yadio(&json!({})).is_empty());
    }

    #[test]
    fn source_names_round_trip() {
        for s in RateSource::ALL {
            assert_eq!(RateSource::parse(s.as_str()), Some(s));
        }
        assert!(RateSource::parse("kraken").is_none());
    }

    #[tokio::test]
    async fn price_lookup_uses_cache() {
        let f = FiatService::new();
        f.preload(RateSource::Mempool, [("EUR".to_string(), 70000.0)].into()).await;
        assert_eq!(f.price(RateSource::Mempool, "EUR").await.unwrap(), 70000.0);
        assert!(matches!(
            f.price(RateSource::Mempool, "ARS").await,
            Err(FiatError::UnknownCurrency(_, _))
        ));
        assert_eq!(f.currencies(RateSource::Mempool).await.unwrap(), ["EUR"]);
    }
}
