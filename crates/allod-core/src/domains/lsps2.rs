//! Settings → Lightning → Liquidity (SPEC §2.3): whether Receive may ask LSPS2 LSPs for a
//! just-in-time channel, which ones, and whether their channels count before they confirm
//! (0-conf). ldk-node asks every chosen LSP for an offer and takes the cheapest that fits
//! the payment. The stack's own LSP is suggested; the owner can add others
//! (`pubkey@host:port`, with the API token the LSP issued, if any).
//!
//! ldk-server reads its LSPs only when it starts. Allod writes the choice to
//! `{handoff_dir}/lsps2.json` (tokens included, mode 0640); the entrypoint picks it up,
//! restarts ldk-server when it changes and reports what it started with in
//! `lsps2.active.json` (never the tokens). Until the owner saves a choice the stack's own
//! LSP applies (`ALLOD_LSPS2_LSP`, which mirrors `LDK_LSPS2_LSP`). Tokens live in the
//! secret store, never in the database.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{DomainError, DomainService};
use crate::lsps0::Peer;

/// `config` key of the owner's choice.
pub const CONFIG_KEY: &str = "lightning.lsps2";
/// Written into the handoff directory for ldk-server's entrypoint.
pub const REQUEST_FILE: &str = "lsps2.json";
/// What ldk-server started with (written by its entrypoint).
pub const ACTIVE_FILE: &str = "lsps2.active.json";

const MAX_CUSTOM: usize = 10;
const MAX_NAME: usize = 64;
const MAX_TOKEN: usize = 1024;

/// An LSP the owner added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomLsp {
    pub name: String,
    /// `pubkey@host:port`.
    pub lsp: String,
}

/// The saved choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lsps2Choice {
    pub enabled: bool,
    /// The LSPs asked for offers (`pubkey@host:port`), each one of the sources.
    #[serde(default)]
    pub lsps: Vec<String>,
    /// Accept their channels before they confirm (trusts them not to double-spend).
    #[serde(default)]
    pub trust_0conf: bool,
    #[serde(default)]
    pub custom: Vec<CustomLsp>,
    /// One LSP, as an earlier version saved it; read into `lsps`, never written.
    #[serde(default, skip_serializing)]
    pub lsp: Option<String>,
    /// When ldk-server was last handed a change (unix seconds): it restarts after this.
    #[serde(default)]
    pub saved_at: Option<i64>,
}

/// One custom LSP in a save request. `token`: left out keeps the saved one, empty
/// removes it.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomLspRequest {
    /// Optional: the page names an LSP by the alias it announces.
    #[serde(default)]
    pub name: String,
    pub lsp: String,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Lsps2Request {
    pub enabled: bool,
    #[serde(default)]
    pub lsps: Vec<String>,
    #[serde(default)]
    pub trust_0conf: bool,
    #[serde(default)]
    pub custom: Vec<CustomLspRequest>,
}

/// What ldk-server reports it started with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Active {
    lsps: Vec<String>,
    trust_0conf: bool,
    /// When this start of ldk-server began (unix seconds): the page follows a restart.
    started_at: Option<i64>,
    /// What became of the owner's choice: `none`, `applied`, `off`, `ignored`, `unreadable`.
    request: Option<String>,
    request_error: Option<String>,
}

/// `pubkey@host:port` with a compressed pubkey (lower-cased), host and port.
pub fn normalize_lsp(lsp: &str) -> Option<String> {
    let (pubkey, address) = lsp.trim().split_once('@')?;
    let pubkey = pubkey.to_ascii_lowercase();
    let hex = pubkey.len() == 66 && pubkey.chars().all(|c| c.is_ascii_hexdigit());
    if !hex || !(pubkey.starts_with("02") || pubkey.starts_with("03")) {
        return None;
    }
    let (host, port) = address.rsplit_once(':')?;
    let host_ok = if let Some(inner) = host.strip_prefix('[') {
        inner.strip_suffix(']').is_some_and(|h| h.parse::<std::net::Ipv6Addr>().is_ok())
    } else {
        !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    };
    let port_ok = port.parse::<u16>().is_ok_and(|p| p > 0);
    (host_ok && port_ok).then(|| format!("{pubkey}@{address}"))
}

/// "034066…53b0": an LSP with no name of its own.
fn short_lsp(lsp: &str) -> String {
    let p = pubkey(lsp);
    format!("{}…{}", &p[..6.min(p.len())], &p[p.len().saturating_sub(4)..])
}

/// Secret name of a custom LSP's token (by its pubkey).
fn token_secret(lsp: &str) -> String {
    format!("lsps2_token_{}", lsp.split('@').next().unwrap_or(lsp))
}

fn pubkey(lsp: &str) -> &str {
    lsp.split('@').next().unwrap_or(lsp)
}

/// A source offered in the settings.
struct Source {
    id: String,
    name: String,
    lsp: String,
    suggested: bool,
    custom: bool,
}

impl DomainService {
    /// The stack's own LSPs (`LDK_LSPS2_LSP`, comma-separated), in order.
    fn stack_lsps(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in self.cfg.env_lsps2.as_deref().unwrap_or("").split(',').filter_map(normalize_lsp) {
            if !out.iter().any(|o| pubkey(o) == pubkey(&l)) {
                out.push(l);
            }
        }
        out
    }

    /// The suggested LSPs: the stack's own.
    fn builtin_lsps(&self) -> Vec<Source> {
        self.stack_lsps()
            .into_iter()
            .enumerate()
            .map(|(i, lsp)| Source {
                id: if i == 0 { "default".into() } else { format!("default-{}", i + 1) },
                name: "Default LSP".into(),
                lsp,
                suggested: true,
                custom: false,
            })
            .collect()
    }

    /// The LSPs and 0-conf setting ldk-server started with; `None` not reported (an
    /// ldk-server image without the setting, or one Allod does not manage).
    fn lsps2_active(&self) -> Option<Active> {
        let dir = self.cfg.handoff_dir.as_ref()?;
        let text = std::fs::read_to_string(dir.join(ACTIVE_FILE)).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        let lsps = v["lsps"]
            .as_array()?
            .iter()
            .filter_map(|l| l.as_str().and_then(normalize_lsp))
            .collect();
        let text_of = |k: &str| v[k].as_str().map(str::to_string);
        Some(Active {
            lsps,
            trust_0conf: v["trust_0conf"].as_bool().unwrap_or(false),
            started_at: v["started_at"].as_i64(),
            request: text_of("request"),
            request_error: text_of("request_error"),
        })
    }

    /// The saved choice, or the stack's own before the owner saved one.
    pub async fn lsps2_choice(&self) -> Result<Lsps2Choice, DomainError> {
        if let Some(e) = self.db.config_get(CONFIG_KEY).await? {
            if let Ok(mut c) = serde_json::from_str::<Lsps2Choice>(&e.value) {
                if let Some(l) = c.lsp.take().filter(|_| c.lsps.is_empty()) {
                    c.lsps.push(l);
                }
                return Ok(c);
            }
        }
        let stack = self.stack_lsps();
        Ok(Lsps2Choice {
            enabled: !stack.is_empty(),
            lsps: stack,
            // The stack's 0-conf setting is ldk-server's own; it reports it.
            trust_0conf: self.lsps2_active().is_some_and(|a| a.trust_0conf),
            custom: Vec::new(),
            lsp: None,
            saved_at: None,
        })
    }

    /// The settings: the choice, the sources to pick from and what the node runs with.
    pub async fn lsps2_view(&self) -> Result<Value, DomainError> {
        let choice = self.lsps2_choice().await?;
        let mut sources: Vec<Source> = self.builtin_lsps();
        for (i, c) in choice.custom.iter().enumerate() {
            sources.push(Source {
                id: format!("custom-{i}"),
                name: c.name.clone(),
                lsp: c.lsp.clone(),
                suggested: false,
                custom: true,
            });
        }
        let active = self.lsps2_active();
        let mut wanted: Vec<String> = if choice.enabled { choice.lsps.clone() } else { Vec::new() };
        wanted.sort();
        let applied = active.as_ref().map(|a| {
            let mut have = a.lsps.clone();
            have.sort();
            have == wanted && (wanted.is_empty() || a.trust_0conf == choice.trust_0conf)
        });
        Ok(json!({
            "available": self.cfg.handoff_dir.is_some(),
            "enabled": choice.enabled,
            "lsps": choice.lsps,
            "trust_0conf": choice.trust_0conf,
            // When ldk-server was last handed a change; `active.started_at` after it means
            // it has restarted since.
            "saved_at": choice.saved_at,
            "sources": sources.iter().map(|s| json!({
                "id": s.id,
                "name": s.name,
                "lsp": s.lsp,
                "suggested": s.suggested,
                "custom": s.custom,
                "has_token": s.custom && self.secrets.exists(&token_secret(&s.lsp)),
            })).collect::<Vec<_>>(),
            // What ldk-server runs with, and whether that is the saved choice yet (it
            // restarts to pick a change up).
            "active": active.map(|a| json!({
                "lsps": a.lsps,
                "trust_0conf": a.trust_0conf,
                "started_at": a.started_at,
                "request": a.request,
                "request_error": a.request_error,
            })),
            "applied": applied,
        }))
    }

    /// Saves the choice and hands it to ldk-server, which restarts when it changed.
    pub async fn set_lsps2(&self, req: Lsps2Request) -> Result<Value, DomainError> {
        let Some(dir) = self.cfg.handoff_dir.clone() else {
            return Err(DomainError::Conflict(
                "Allod does not manage this Lightning node's configuration".into(),
            ));
        };
        let bad = |m: &str| DomainError::BadRequest(m.to_string());
        if req.custom.len() > MAX_CUSTOM {
            return Err(bad("at most 10 LSPs of your own"));
        }
        let builtin = self.builtin_lsps();
        let mut custom: Vec<CustomLsp> = Vec::new();
        let mut tokens: Vec<(String, Option<String>)> = Vec::new();
        for c in &req.custom {
            let name = c.name.trim();
            if name.chars().count() > MAX_NAME {
                return Err(bad("an LSP's name is at most 64 characters"));
            }
            let lsp = normalize_lsp(&c.lsp).ok_or_else(|| {
                bad(&format!("{}: the LSP must be pubkey@host:port", c.lsp.trim()))
            })?;
            let label = if name.is_empty() { short_lsp(&lsp) } else { name.to_string() };
            let known =
                builtin.iter().map(|b| b.lsp.as_str()).chain(custom.iter().map(|o| o.lsp.as_str()));
            if known.map(pubkey).any(|p| p == pubkey(&lsp)) {
                return Err(bad(&format!("{label}: this LSP is already in the list")));
            }
            if let Some(t) = &c.token {
                let t = t.trim();
                if t.len() > MAX_TOKEN || t.chars().any(|ch| ch.is_control()) {
                    return Err(bad(&format!("{label}: the token is not valid")));
                }
                tokens.push((lsp.clone(), Some(t.to_string()).filter(|t| !t.is_empty())));
            }
            custom.push(CustomLsp { name: name.to_string(), lsp });
        }
        let mut lsps: Vec<String> = Vec::new();
        for l in &req.lsps {
            let l = normalize_lsp(l).ok_or_else(|| bad("each LSP must be pubkey@host:port"))?;
            if !builtin.iter().any(|b| b.lsp == l) && !custom.iter().any(|c| c.lsp == l) {
                return Err(bad("pick LSPs from the list"));
            }
            if !lsps.contains(&l) {
                lsps.push(l);
            }
        }
        if req.enabled && lsps.is_empty() {
            return Err(bad("pick at least one LSP"));
        }

        // Tokens first: a choice must never point at a token that is not there yet.
        let previous = self.lsps2_choice().await?;
        for (l, t) in &tokens {
            let name = token_secret(l);
            match t {
                Some(t) => self.secrets.write(&name, t),
                None => self.secrets.remove(&name),
            }
            .map_err(|e| DomainError::Internal(format!("LSP token: {e}")))?;
        }
        for gone in previous.custom.iter().filter(|p| !custom.iter().any(|c| c.lsp == p.lsp)) {
            let _ = self.secrets.remove(&token_secret(&gone.lsp));
        }
        let mut choice = Lsps2Choice {
            enabled: req.enabled,
            lsps,
            trust_0conf: req.trust_0conf,
            custom,
            lsp: None,
            saved_at: previous.saved_at,
        };

        let request = if choice.enabled {
            let mut entries = Vec::new();
            for l in &choice.lsps {
                let mut e = json!({ "lsp": l });
                // The stack's LSP keeps the stack's token (ldk-server has it).
                if choice.custom.iter().any(|c| &c.lsp == l) {
                    if let Some(t) = self
                        .secrets
                        .read(&token_secret(l))
                        .map_err(|e| DomainError::Internal(format!("LSP token: {e}")))?
                    {
                        e["token"] = json!(t.to_string());
                    }
                }
                entries.push(e);
            }
            json!({ "enabled": true, "trust_0conf": choice.trust_0conf, "lsps": entries })
        } else {
            json!({ "enabled": false })
        };
        let path = dir.join(REQUEST_FILE);
        let text =
            serde_json::to_string(&request).map_err(|e| DomainError::Internal(e.to_string()))?;
        let unchanged = std::fs::read_to_string(&path).is_ok_and(|c| c.trim_end() == text);
        if !unchanged {
            // Group-readable: under host-dev Allod runs as another user than ldk-server,
            // which shares the handoff directory's group (setgid there).
            allod_keys::write_file_with_mode(&path, &text, 0o640).map_err(|e| {
                DomainError::Internal(format!("cannot write {}: {e}", path.display()))
            })?;
            choice.saved_at = Some(super::now());
            tracing::info!(
                enabled = choice.enabled,
                lsps = choice.lsps.len(),
                "lightning: LSPS2 LSPs changed; ldk-server restarts"
            );
        }
        let value =
            serde_json::to_string(&choice).map_err(|e| DomainError::Internal(e.to_string()))?;
        self.db.config_set(CONFIG_KEY, &value, false).await?;
        self.lsps2_view().await
    }
}

/// One `opening_fee_params` entry of an LSP's LSPS2 fee menu (bLIP-52 `get_info`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeOffer {
    pub min_fee_msat: u64,
    /// Parts per million of the payment.
    pub proportional: u64,
    /// Unix seconds.
    pub valid_until: i64,
    /// Blocks the LSP keeps the channel open at least.
    pub min_lifetime: u64,
    pub min_payment_size_msat: u64,
    pub max_payment_size_msat: u64,
}

impl FeeOffer {
    fn parse(v: &Value) -> Option<FeeOffer> {
        // Amounts are JSON strings (bLIP-50 uint64), small integers plain numbers.
        let u = |k: &str| v[k].as_str().and_then(|s| s.parse().ok()).or_else(|| v[k].as_u64());
        Some(FeeOffer {
            min_fee_msat: u("min_fee_msat")?,
            proportional: u("proportional")?,
            valid_until: crate::public_name::zeus::parse_time(&v["valid_until"])?,
            min_lifetime: u("min_lifetime").unwrap_or(0),
            min_payment_size_msat: u("min_payment_size_msat")?,
            max_payment_size_msat: u("max_payment_size_msat")?,
        })
    }

    /// The opening fee for a payment, as ldk-node computes it (lightning-liquidity
    /// `compute_opening_fee`); `None` outside the offer's payment sizes.
    pub fn fee_for(&self, amount_msat: u64) -> Option<u64> {
        if amount_msat < self.min_payment_size_msat || amount_msat > self.max_payment_size_msat {
            return None;
        }
        let fee = amount_msat
            .checked_mul(self.proportional)
            .map(|f| f.div_ceil(1_000_000))
            .map(|f| f.max(self.min_fee_msat))?;
        // The LSP takes its fee out of the payment, and what is left must be big enough
        // for a channel that can carry it (the node's reserve, anchors, fees).
        (amount_msat.saturating_sub(fee) >= crate::wallet::JIT_MIN_RECEIVE_MSAT).then_some(fee)
    }
}

/// Offers asked of each LSP (`pubkey@host:port`), and when.
pub type OfferCache = Mutex<HashMap<String, (i64, Result<Vec<FeeOffer>, String>)>>;

/// How long an answer is reused: offers until shortly before they expire (at most five
/// minutes), a failure for a minute.
const OFFERS_KEEP_SECS: i64 = 300;
const FAILURE_KEEP_SECS: i64 = 60;
/// An offer this close to its end is not quoted: the invoice would outlive it.
const OFFER_MARGIN_SECS: i64 = 120;
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// Asks one LSP for its LSPS2 fee menu over a Lightning connection of its own.
async fn ask_offers(lsp: &str, token: Option<String>) -> Result<Vec<FeeOffer>, String> {
    let mut peer = Peer::connect(lsp, ASK_TIMEOUT).await.map_err(|e| e.to_string())?;
    let params = match token {
        Some(t) => json!({ "token": t }),
        None => json!({}),
    };
    let r = peer.request("lsps2.get_info", params, ASK_TIMEOUT).await.map_err(|e| e.to_string())?;
    let menu = r["opening_fee_params_menu"]
        .as_array()
        .ok_or_else(|| "the LSP sent no fee menu".to_string())?;
    Ok(menu.iter().filter_map(FeeOffer::parse).collect())
}

impl DomainService {
    /// The LSPs ldk-server asks for a just-in-time channel: what it reports it runs with,
    /// else the saved choice.
    async fn lsps2_in_use(&self) -> Result<Vec<String>, DomainError> {
        if let Some(a) = self.lsps2_active() {
            return Ok(a.lsps);
        }
        let c = self.lsps2_choice().await?;
        Ok(if c.enabled { c.lsps } else { Vec::new() })
    }

    /// What a just-in-time channel would cost for a payment of `amount_msat`: every LSP in
    /// use is asked for its LSPS2 fee menu (cached), and the cheapest offer that fits the
    /// payment is the one ldk-node will take.
    pub async fn lsps2_quote(&self, amount_msat: u64) -> Result<Value, DomainError> {
        if amount_msat < crate::wallet::JIT_MIN_RECEIVE_MSAT {
            return Ok(json!({
                "amount_msat": amount_msat,
                "cheapest": null,
                "lsps": [],
                "min_amount_msat": crate::wallet::JIT_MIN_RECEIVE_MSAT,
            }));
        }
        let lsps = self.lsps2_in_use().await?;
        let choice = self.lsps2_choice().await?;
        let t = super::now();
        let mut asks = Vec::new();
        for lsp in &lsps {
            let cached = self
                .lsps2_offers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(lsp)
                .filter(|(at, r)| {
                    let keep = match r {
                        Ok(offers) => offers
                            .iter()
                            .map(|o| o.valid_until - OFFER_MARGIN_SECS)
                            .min()
                            .map_or(0, |end| (end - at).min(OFFERS_KEEP_SECS)),
                        Err(_) => FAILURE_KEEP_SECS,
                    };
                    t - at < keep
                })
                .map(|(_, r)| r.clone());
            let token = if choice.custom.iter().any(|c| &c.lsp == lsp) {
                self.secrets.read(&token_secret(lsp)).ok().flatten().map(|t| t.to_string())
            } else {
                None
            };
            let lsp = lsp.clone();
            asks.push(async move {
                match cached {
                    Some(r) => (lsp, r, false),
                    None => {
                        let r = ask_offers(&lsp, token).await;
                        (lsp, r, true)
                    }
                }
            });
        }
        let answers = futures_util::future::join_all(asks).await;

        let names: HashMap<String, String> = self
            .builtin_lsps()
            .into_iter()
            .map(|s| (s.lsp, s.name))
            .chain(choice.custom.iter().map(|c| {
                let name = if c.name.is_empty() { short_lsp(&c.lsp) } else { c.name.clone() };
                (c.lsp.clone(), name)
            }))
            .collect();
        let mut out = Vec::new();
        let mut cheapest: Option<(u64, Value)> = None;
        for (lsp, r, fresh) in answers {
            if fresh {
                self.lsps2_offers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(lsp.clone(), (t, r.clone()));
            }
            let name = names.get(&lsp).cloned().unwrap_or_else(|| "LSP".into());
            let mut entry = json!({ "lsp": lsp, "name": name });
            match r {
                Err(e) => {
                    entry["error"] = json!(e);
                }
                Ok(offers) => {
                    let usable: Vec<&FeeOffer> =
                        offers.iter().filter(|o| o.valid_until - OFFER_MARGIN_SECS > t).collect();
                    entry["min_payment_msat"] =
                        json!(usable.iter().map(|o| o.min_payment_size_msat).min());
                    entry["max_payment_msat"] =
                        json!(usable.iter().map(|o| o.max_payment_size_msat).max());
                    let best = usable
                        .iter()
                        .filter_map(|o| o.fee_for(amount_msat).map(|f| (f, *o)))
                        .min_by_key(|(f, _)| *f);
                    if let Some((fee, o)) = best {
                        entry["fee_msat"] = json!(fee);
                        entry["receive_msat"] = json!(amount_msat - fee);
                        entry["valid_until"] = json!(o.valid_until);
                        entry["min_lifetime"] = json!(o.min_lifetime);
                        entry["proportional"] = json!(o.proportional);
                        entry["min_fee_msat"] = json!(o.min_fee_msat);
                        if cheapest.as_ref().is_none_or(|(f, _)| fee < *f) {
                            cheapest = Some((fee, entry.clone()));
                        }
                    }
                }
            }
            out.push(entry);
        }
        Ok(json!({
            "amount_msat": amount_msat,
            "cheapest": cheapest.map(|(_, e)| e),
            "lsps": out,
            "min_amount_msat": crate::wallet::JIT_MIN_RECEIVE_MSAT,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_fees_as_ldk_node_computes_them() {
        let o = FeeOffer {
            min_fee_msat: 2_500_000,
            proportional: 20_564,
            valid_until: 0,
            min_lifetime: 13_140,
            min_payment_size_msat: 2_501_000,
            max_payment_size_msat: 16_000_000_000,
        };
        // 20,000 sats: 2.0564% is 411,280 msat, under the minimum.
        assert_eq!(o.fee_for(20_000_000), Some(2_500_000));
        // 1M sats: the proportional part, rounded up.
        assert_eq!(o.fee_for(1_000_000_000), Some(20_564_000));
        assert_eq!(o.fee_for(1_000_000_001), Some(20_564_001));
        // Outside the payment sizes.
        assert_eq!(o.fee_for(2_500_000), None);
        assert_eq!(o.fee_for(16_000_000_001), None);
        // Free, from a JSON menu entry (amounts as strings).
        let free = FeeOffer::parse(&json!({"min_fee_msat": "0", "proportional": 0,
            "valid_until": "2026-11-26T22:49:30.695628237Z", "min_lifetime": 100,
            "max_client_to_self_delay": 1024, "min_payment_size_msat": "0",
            "max_payment_size_msat": "100000000000", "promise": "x"}))
        .unwrap();
        assert_eq!((free.fee_for(21_000_000), free.min_lifetime), (Some(0), 100));
        // What is left must be enough for a channel that carries it: 5,000 sats.
        assert_eq!(free.fee_for(2_000_000), None);
        assert_eq!(free.fee_for(5_000_000), Some(0));
        assert_eq!(o.fee_for(7_000_000), None);
        assert_eq!(o.fee_for(7_500_000), Some(2_500_000));
        assert_eq!(free.valid_until, 1_795_733_370);
    }

    #[test]
    fn checks_the_lsp_form() {
        let pk = "02".to_string() + &"AB".repeat(32);
        assert_eq!(
            normalize_lsp(&format!(" {pk}@lsp.example:9735 ")),
            Some(format!("{}@lsp.example:9735", pk.to_ascii_lowercase()))
        );
        assert!(normalize_lsp(&format!("{pk}@[2001:db8::1]:9735")).is_some());
        for bad in [
            "nope".to_string(),
            format!("{pk}@lsp.example"),
            format!("{pk}@lsp.example:0"),
            format!("04{}@h:1", "ab".repeat(32)),
            format!("{pk}@bad host:9735"),
            format!("{pk}@[nope]:9735"),
        ] {
            assert_eq!(normalize_lsp(&bad), None, "{bad}");
        }
    }
}
