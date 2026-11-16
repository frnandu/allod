//! Serving LNURL-pay for Lightning addresses (SPEC §12): the payRequest (LUD-06/16), the
//! callback's checks (amount, LUD-12 comment, LUD-18 payer data), NIP-57 zap requests
//! and the zap receipt. No HTTP framework here: the API crate maps these to routes.

use std::collections::HashMap;

use nostr::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Longest comment accepted (LUD-12).
pub const COMMENT_MAX: usize = 255;
/// 1 sat.
pub const MIN_SENDABLE_MSAT: u64 = 1_000;
/// 1 BTC.
pub const MAX_SENDABLE_MSAT: u64 = 100_000_000_000;
/// Relays a zap receipt is sent to at most.
pub const MAX_ZAP_RELAYS: usize = 10;
/// Largest encoded LUD-18 payer-data object accepted.
pub const MAX_PAYER_DATA_BYTES: usize = 4 * 1024;
/// Largest NIP-57 request accepted before JSON parsing or signature verification.
pub const MAX_ZAP_REQUEST_BYTES: usize = 16 * 1024;
/// Maximum tags in one NIP-57 request.
pub const MAX_ZAP_TAGS: usize = 64;

/// The metadata string of an address (LUD-06): what the invoice's description hash
/// commits to.
pub fn metadata(address: &str, text: &str) -> String {
    json!([["text/plain", text], ["text/identifier", address]]).to_string()
}

/// The payRequest answered at `/.well-known/lnurlp/{username}`.
pub fn pay_request(callback: &str, metadata: &str, nostr_pubkey: Option<&str>) -> Value {
    let mut v = json!({
        "tag": "payRequest",
        "callback": callback,
        "minSendable": MIN_SENDABLE_MSAT,
        "maxSendable": MAX_SENDABLE_MSAT,
        "metadata": metadata,
        "commentAllowed": COMMENT_MAX,
    });
    if let Some(pk) = nostr_pubkey {
        v["allowsNostr"] = json!(true);
        v["nostrPubkey"] = json!(pk);
    }
    v
}

/// An LNURL error answer.
pub fn error(reason: &str) -> Value {
    json!({"status": "ERROR", "reason": reason})
}

/// A checked callback request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub amount_msat: u64,
    pub comment: Option<String>,
    /// LUD-18 payer data, as sent (JSON).
    pub payer_data: Option<Value>,
    /// The NIP-57 zap request exactly as sent: the invoice commits to these bytes.
    pub zap_request: Option<String>,
}

impl Callback {
    /// What the invoice's description hash is: the zap request for a zap (NIP-57), the
    /// metadata string otherwise (LUD-06).
    pub fn description_hash(&self, metadata: &str) -> String {
        let preimage = self.zap_request.as_deref().unwrap_or(metadata);
        hex::encode(Sha256::digest(preimage.as_bytes()))
    }

    /// The text shown for the payment: the zap's message, else the comment.
    pub fn note(&self) -> Option<String> {
        let zap = self
            .zap_request
            .as_deref()
            .and_then(|z| serde_json::from_str::<Value>(z).ok())
            .and_then(|z| z.get("content").and_then(Value::as_str).map(str::to_string))
            .filter(|c| !c.trim().is_empty());
        zap.or_else(|| self.comment.clone())
    }

    /// The record kept on the transaction (NWC-06 shape: `comment`, `payer_data`,
    /// `nostr`), plus the zap request as sent for the receipt.
    pub fn record(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(c) = &self.comment {
            m.insert("comment".into(), json!(c));
        }
        if let Some(p) = &self.payer_data {
            m.insert("payer_data".into(), p.clone());
        }
        if let Some(z) = &self.zap_request {
            if let Ok(v) = serde_json::from_str::<Value>(z) {
                m.insert("nostr".into(), v);
            }
            m.insert("zap_request".into(), json!(z));
        }
        Value::Object(m)
    }
}

/// Checks the callback's query parameters.
pub fn parse_callback(q: &HashMap<String, String>) -> Result<Callback, String> {
    let amount_msat = q
        .get("amount")
        .and_then(|a| a.parse::<u64>().ok())
        .ok_or_else(|| "amount (msat) is missing".to_string())?;
    if !(MIN_SENDABLE_MSAT..=MAX_SENDABLE_MSAT).contains(&amount_msat) {
        return Err(format!(
            "amount must be between {MIN_SENDABLE_MSAT} and {MAX_SENDABLE_MSAT} msat"
        ));
    }
    let comment = q.get("comment").map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
    if comment.as_ref().is_some_and(|c| c.chars().count() > COMMENT_MAX) {
        return Err(format!("the comment is longer than {COMMENT_MAX} characters"));
    }
    let payer_data = match q.get("payerdata") {
        None => None,
        Some(p) => {
            if p.len() > MAX_PAYER_DATA_BYTES {
                return Err("payerdata is too large".into());
            }
            Some(
                serde_json::from_str::<Value>(p)
                    .ok()
                    .filter(Value::is_object)
                    .ok_or_else(|| "payerdata is not a JSON object".to_string())?,
            )
        }
    };
    let zap_request = match q.get("nostr") {
        None => None,
        Some(z) => {
            check_zap_request(z, amount_msat)?;
            Some(z.clone())
        }
    };
    Ok(Callback { amount_msat, comment, payer_data, zap_request })
}

/// NIP-57 Appendix D: a valid signature, kind 9734, one `p` tag, relays, at most one
/// `e` tag, and an `amount` tag (when present) equal to the amount.
pub fn check_zap_request(raw: &str, amount_msat: u64) -> Result<(), String> {
    if raw.len() > MAX_ZAP_REQUEST_BYTES {
        return Err("the zap request is too large".into());
    }
    let ev = Event::from_json(raw).map_err(|_| "the zap request is not a Nostr event")?;
    if ev.tags.len() > MAX_ZAP_TAGS {
        return Err("the zap request has too many tags".into());
    }
    if ev.kind != Kind::ZapRequest {
        return Err("the zap request is not kind 9734".into());
    }
    ev.verify().map_err(|_| "the zap request's signature is not valid")?;
    let count = |k: &str| {
        ev.tags.iter().filter(|t| t.as_slice().first().map(String::as_str) == Some(k)).count()
    };
    if count("p") != 1 {
        return Err("the zap request needs exactly one p tag".into());
    }
    if count("e") > 1 {
        return Err("the zap request has more than one e tag".into());
    }
    if zap_relays_from_event(&ev).is_empty() {
        return Err("the zap request names no public wss relays".into());
    }
    if let Some(a) = tag_value(&ev, "amount") {
        if a.parse::<u64>().ok() != Some(amount_msat) {
            return Err("the zap request's amount does not match".into());
        }
    }
    Ok(())
}

fn tag_value(ev: &Event, name: &str) -> Option<String> {
    ev.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.first().map(String::as_str) == Some(name)).then(|| s.get(1).cloned()).flatten()
    })
}

/// The relays a receipt goes to: those of the zap request (deduplicated, at most
/// [`MAX_ZAP_RELAYS`]).
pub fn zap_relays(zap_request: &str) -> Vec<String> {
    let Ok(ev) = Event::from_json(zap_request) else { return Vec::new() };
    zap_relays_from_event(&ev)
}

fn zap_relays_from_event(ev: &Event) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in ev.tags.iter() {
        let s = t.as_slice();
        if s.first().map(String::as_str) != Some("relays") {
            continue;
        }
        for r in &s[1..] {
            let r = r.trim().trim_end_matches('/').to_string();
            if crate::check_egress_url(&r, "wss", None, false).is_ok() && !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out.truncate(MAX_ZAP_RELAYS);
    out
}

/// The zap receipt (kind 9735) for a paid zap: `description` is the zap request exactly
/// as sent, so its hash matches the invoice.
pub fn zap_receipt(
    keys: &Keys,
    zap_request: &str,
    bolt11: &str,
    preimage: Option<&str>,
    paid_at: u64,
) -> Result<Event, String> {
    let ev = Event::from_json(zap_request).map_err(|e| e.to_string())?;
    let mut tags = Vec::new();
    for name in ["p", "e", "a"] {
        if let Some(v) = tag_value(&ev, name) {
            tags.push(Tag::parse([name.to_string(), v]).map_err(|e| e.to_string())?);
        }
    }
    tags.push(Tag::parse(["P".to_string(), ev.pubkey.to_hex()]).map_err(|e| e.to_string())?);
    tags.push(Tag::parse(["bolt11".to_string(), bolt11.to_string()]).map_err(|e| e.to_string())?);
    tags.push(
        Tag::parse(["description".to_string(), zap_request.to_string()])
            .map_err(|e| e.to_string())?,
    );
    if let Some(p) = preimage {
        tags.push(Tag::parse(["preimage".to_string(), p.to_string()]).map_err(|e| e.to_string())?);
    }
    EventBuilder::new(Kind::ZapReceipt, "")
        .tags(tags)
        .custom_created_at(Timestamp::from(paid_at))
        .finalize(keys)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zap(keys: &Keys, amount: Option<u64>, relays: bool) -> String {
        let recipient = Keys::generate().public_key();
        let mut tags = vec![Tag::public_key(recipient)];
        if relays {
            tags.push(
                Tag::parse(["relays", "wss://relay.example", "wss://relay.example/"]).unwrap(),
            );
        }
        if let Some(a) = amount {
            tags.push(Tag::parse(["amount".to_string(), a.to_string()]).unwrap());
        }
        EventBuilder::new(Kind::ZapRequest, "great post")
            .tags(tags)
            .finalize(keys)
            .unwrap()
            .as_json()
    }

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn pay_request_and_metadata() {
        let m = metadata("sats@pay.example", "Sats for sats@pay.example");
        assert_eq!(
            m,
            r#"[["text/plain","Sats for sats@pay.example"],["text/identifier","sats@pay.example"]]"#
        );
        let p = pay_request("https://pay.example/lnurlp/sats/callback", &m, Some("ab"));
        assert_eq!(p["tag"], "payRequest");
        assert_eq!(p["metadata"], m);
        assert_eq!(p["allowsNostr"], true);
        assert!(pay_request("x", &m, None).get("allowsNostr").is_none());
    }

    #[test]
    fn callback_checks() {
        assert!(parse_callback(&q(&[])).is_err());
        assert!(parse_callback(&q(&[("amount", "999")])).is_err());
        assert!(parse_callback(&q(&[("amount", "100000000001")])).is_err());
        let long = "x".repeat(256);
        assert!(parse_callback(&q(&[("amount", "1000"), ("comment", &long)])).is_err());
        assert!(parse_callback(&q(&[("amount", "1000"), ("payerdata", "[1]")])).is_err());
        let c = parse_callback(&q(&[("amount", "21000"), ("comment", " thanks ")])).unwrap();
        assert_eq!(c.comment.as_deref(), Some("thanks"));
        assert_eq!(c.note().as_deref(), Some("thanks"));
        let m = metadata("a@b.c", "x");
        assert_eq!(c.description_hash(&m), hex::encode(Sha256::digest(m.as_bytes())));
        assert_eq!(c.record(), json!({"comment": "thanks"}));

        let payerdata = format!(r#"{{"value":"{}"}}"#, "x".repeat(MAX_PAYER_DATA_BYTES));
        let e = parse_callback(&q(&[("amount", "1000"), ("payerdata", &payerdata)])).unwrap_err();
        assert_eq!(e, "payerdata is too large");

        let oversized_zap = "x".repeat(MAX_ZAP_REQUEST_BYTES + 1);
        let e = parse_callback(&q(&[("amount", "1000"), ("nostr", &oversized_zap)])).unwrap_err();
        assert_eq!(e, "the zap request is too large");
    }

    #[test]
    fn zap_requests_are_checked_and_answered_with_a_receipt() {
        let payer = Keys::generate();
        let z = zap(&payer, Some(21_000), true);
        let c = parse_callback(&q(&[("amount", "21000"), ("nostr", &z)])).unwrap();
        // The hash covers the request exactly as sent.
        assert_eq!(c.description_hash("ignored"), hex::encode(Sha256::digest(z.as_bytes())));
        assert_eq!(c.note().as_deref(), Some("great post"));
        assert_eq!(c.record()["nostr"]["kind"], 9734);
        assert_eq!(c.record()["zap_request"], json!(z));
        assert_eq!(zap_relays(&z), vec!["wss://relay.example".to_string()]);

        // Wrong amount, no relays, a tampered event, a wrong kind.
        assert!(parse_callback(&q(&[("amount", "1000"), ("nostr", &z)])).is_err());
        let no_relays = zap(&payer, None, false);
        assert!(parse_callback(&q(&[("amount", "1000"), ("nostr", &no_relays)])).is_err());
        for relay in [
            "ws://relay.example",
            "wss://127.0.0.1:7777",
            "wss://169.254.169.254",
            "wss://[::1]",
            "wss://[fe80::1]",
            "wss://user:pass@relay.example",
        ] {
            let private = EventBuilder::new(Kind::ZapRequest, "")
                .tags([
                    Tag::public_key(Keys::generate().public_key()),
                    Tag::parse(["relays", relay]).unwrap(),
                ])
                .finalize(&payer)
                .unwrap()
                .as_json();
            assert!(check_zap_request(&private, 1000).is_err(), "{relay}");
            assert!(zap_relays(&private).is_empty(), "{relay}");
        }
        let tampered = z.replace("great post", "changed");
        assert!(parse_callback(&q(&[("amount", "21000"), ("nostr", &tampered)])).is_err());
        let note = EventBuilder::new(Kind::TextNote, "hi").finalize(&payer).unwrap().as_json();
        assert!(parse_callback(&q(&[("amount", "1000"), ("nostr", &note)])).is_err());

        let tags = (0..=MAX_ZAP_TAGS)
            .map(|i| Tag::parse(["x".to_string(), i.to_string()]).unwrap())
            .collect::<Vec<_>>();
        let too_many_tags =
            EventBuilder::new(Kind::ZapRequest, "").tags(tags).finalize(&payer).unwrap().as_json();
        let e = parse_callback(&q(&[("amount", "1000"), ("nostr", &too_many_tags)])).unwrap_err();
        assert_eq!(e, "the zap request has too many tags");

        let zapper = Keys::generate();
        let r = zap_receipt(&zapper, &z, "lnbc1x", Some("00ff"), 1_700_000_000).unwrap();
        assert_eq!(r.kind, Kind::ZapReceipt);
        assert_eq!(r.pubkey, zapper.public_key());
        assert!(r.verify().is_ok());
        let tag = |n: &str| tag_value(&r, n);
        assert_eq!(tag("description").as_deref(), Some(z.as_str()));
        assert_eq!(tag("bolt11").as_deref(), Some("lnbc1x"));
        assert_eq!(tag("preimage").as_deref(), Some("00ff"));
        assert_eq!(tag("P"), Some(payer.public_key().to_hex()));
        assert!(tag("p").is_some());
    }
}
