//! Parsing what the owner pastes into "Send" (SPEC §11.3): BOLT11, BOLT12 offers,
//! BIP-321 URIs, Lightning addresses and LNURL (resolved by `allod-lnurl`).

use super::WalletError;

/// One payment instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    Bolt11(String),
    Bolt12(String),
    Onchain {
        address: String,
    },
    /// A Lightning address (LUD-16) or `lnurl1…` (LUD-06), as given.
    Lnurl(String),
}

/// Parsed input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// Chosen instruction (BOLT12 preferred, then BOLT11, then on-chain).
    pub instruction: Instruction,
    /// `amount=` from a BIP-321 URI, in msat.
    pub uri_amount_msat: Option<u64>,
    /// The URI's BOLT11 invoice when a BOLT12 offer was chosen over it: the fallback for
    /// a node that cannot pay offers.
    pub bolt11_fallback: Option<String>,
}

fn btc_to_msat(v: &str) -> Result<u64, WalletError> {
    let bad = || WalletError::BadRequest(format!("invalid amount in URI: {v}"));
    let (int, frac) = v.split_once('.').unwrap_or((v, ""));
    if frac.len() > 11 || (int.is_empty() && frac.is_empty()) {
        return Err(bad());
    }
    let int: u64 = if int.is_empty() { 0 } else { int.parse().map_err(|_| bad())? };
    let frac: u64 = format!("{frac:0<11}").parse().map_err(|_| bad())?;
    if int > 21_000_000 {
        return Err(bad());
    }
    Ok(int * 100_000_000_000 + frac)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn classify(s: &str) -> Option<Instruction> {
    let l = s.to_ascii_lowercase();
    if l.starts_with("lno1") {
        Some(Instruction::Bolt12(l))
    } else if l.starts_with("lnbc")
        || l.starts_with("lntb")
        || l.starts_with("lntbs")
        || l.starts_with("lnbcrt")
    {
        Some(Instruction::Bolt11(l))
    } else {
        None
    }
}

/// Parses a pasted payment string.
pub fn parse(input: &str) -> Result<Parsed, WalletError> {
    let s = input.trim();
    let s = s.strip_prefix("lightning:").or_else(|| s.strip_prefix("LIGHTNING:")).unwrap_or(s);
    if let Some(i) = classify(s) {
        return Ok(Parsed { instruction: i, uri_amount_msat: None, bolt11_fallback: None });
    }
    let lower = s.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("bitcoin:").map(|_| &s[8..]) {
        let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut bolt11 = None;
        let mut bolt12 = None;
        let mut amount = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = k.to_ascii_lowercase();
            let v = percent_decode(v);
            match k.as_str() {
                "lightning" => bolt11 = classify(&v),
                "lno" => bolt12 = classify(&v),
                "amount" => amount = Some(btc_to_msat(&v)?),
                other if other.starts_with("req-") => {
                    return Err(WalletError::BadRequest(format!(
                        "unsupported required parameter: {other}"
                    )))
                }
                _ => {}
            }
        }
        let bolt11_fallback = match (&bolt12, &bolt11) {
            (Some(_), Some(Instruction::Bolt11(inv))) => Some(inv.clone()),
            _ => None,
        };
        let instruction = bolt12
            .or(bolt11)
            .or_else(|| {
                (!address.is_empty()).then(|| Instruction::Onchain { address: address.to_string() })
            })
            .ok_or_else(|| WalletError::BadRequest("no payment instruction in URI".into()))?;
        return Ok(Parsed { instruction, uri_amount_msat: amount, bolt11_fallback });
    }
    if allod_lnurl::is_lnurl_target(s) {
        return Ok(Parsed {
            instruction: Instruction::Lnurl(s.to_string()),
            uri_amount_msat: None,
            bolt11_fallback: None,
        });
    }
    Err(WalletError::BadRequest("not a Lightning invoice, offer or bitcoin: URI".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inputs() {
        assert_eq!(parse("LNBC1ABC").unwrap().instruction, Instruction::Bolt11("lnbc1abc".into()));
        assert_eq!(
            parse("lightning:lno1xyz").unwrap().instruction,
            Instruction::Bolt12("lno1xyz".into())
        );
        let p = parse("bitcoin:?lightning=lnbcrt1x&lno=lno1y&amount=0.00001").unwrap();
        assert_eq!(p.instruction, Instruction::Bolt12("lno1y".into()));
        assert_eq!(p.uri_amount_msat, Some(1_000_000));
        let p = parse("BITCOIN:BCRT1QADDR?amount=1").unwrap();
        assert_eq!(p.instruction, Instruction::Onchain { address: "BCRT1QADDR".into() });
        assert_eq!(p.uri_amount_msat, Some(100_000_000_000));
        assert!(parse("bitcoin:?lightning=lnbc1&req-pop=x").is_err());
        assert!(parse("bitcoin:?amount=0.123456789012").is_err());
        assert_eq!(
            parse("me@example.com").unwrap().instruction,
            Instruction::Lnurl("me@example.com".into())
        );
        assert!(matches!(parse("₿me@example.com").unwrap().instruction, Instruction::Lnurl(_)));
        assert!(parse("hello").is_err());
    }
}
