//! Paying BIP-353 names (`₿user@domain`, SPEC §11.3): the TXT record at
//! `user.user._bitcoin-payment.domain` holds a `bitcoin:` URI. Allod only trusts it when
//! a DNSSEC proof for it checks out from the root keys, so the DNS server that answered
//! does not have to be trusted.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use dnssec_prover::rr::{Name, RR};
use dnssec_prover::ser::parse_rr_stream;
use dnssec_prover::validation::verify_rr_stream;

/// DNS server used to build DNSSEC proofs when none is configured.
pub const DEFAULT_RESOLVER: &str = "1.1.1.1:53";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Bip353Error {
    /// The name was written with `₿` but has no BIP-353 record.
    #[error("{0} has no BIP-353 payment record")]
    NotFound(String),
    /// A record exists but its DNSSEC proof is missing or does not check out.
    #[error("the payment record of {0} is not signed with DNSSEC, so it cannot be trusted")]
    Unsigned(String),
    /// BIP-353: more than one `bitcoin:` record means the name must not be paid.
    #[error("{0} has more than one payment record")]
    Ambiguous(String),
    #[error("cannot look up {0}: {1}")]
    Lookup(String, String),
}

/// How names are looked up.
#[derive(Debug, Clone)]
pub enum Bip353Resolver {
    /// Plain DNS to see whether a record exists, then a DNSSEC proof built through this
    /// recursive resolver (over TCP) and checked locally.
    Dnssec(SocketAddr),
    /// Fixed records, taken as verified (tests only): `user@domain` → TXT values.
    Fixed(Arc<HashMap<String, Vec<String>>>),
}

impl Default for Bip353Resolver {
    fn default() -> Self {
        Bip353Resolver::Dnssec(DEFAULT_RESOLVER.parse().expect("valid default resolver"))
    }
}

/// A name in the form BIP-353 allows: `user@domain`, optionally prefixed with `₿`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanName {
    pub user: String,
    pub domain: String,
    /// Written with `₿`: a BIP-353 name only, never an LNURL address.
    pub explicit: bool,
}

impl HumanName {
    /// `user@domain` for display and records.
    pub fn address(&self) -> String {
        format!("{}@{}", self.user, self.domain)
    }

    fn dns_name(&self) -> String {
        format!("{}.user._bitcoin-payment.{}.", self.user, self.domain)
    }
}

/// Splits a pasted `₿user@domain` / `user@domain`. Hosts with a port or an IP address
/// cannot carry a BIP-353 record.
pub fn parse_name(input: &str) -> Option<HumanName> {
    let s = input.trim();
    let (explicit, s) = match s.strip_prefix('₿') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let s = s.to_ascii_lowercase();
    let (user, domain) = s.split_once('@')?;
    let domain = domain.trim_end_matches('.');
    let user_ok = !user.is_empty()
        && user.len() <= 63
        && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    let domain_ok = domain.contains('.')
        && domain.len() <= 200
        && domain.parse::<std::net::IpAddr>().is_err()
        && domain.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        });
    (user_ok && domain_ok).then(|| HumanName {
        user: user.to_string(),
        domain: domain.to_string(),
        explicit,
    })
}

/// The `bitcoin:` values among TXT values (case-insensitive scheme).
fn bitcoin_uris(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|v| v.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("bitcoin:")))
        .collect()
}

/// Exactly one `bitcoin:` URI, or the BIP-353 errors.
fn single(name: &HumanName, uris: Vec<String>) -> Result<Option<String>, Bip353Error> {
    match uris.len() {
        0 => Ok(None),
        1 => Ok(uris.into_iter().next()),
        _ => Err(Bip353Error::Ambiguous(name.address())),
    }
}

/// TXT values at `name` through the system resolver, each with its strings joined.
/// `None` when the name does not resolve (or DNS is unreachable).
pub async fn plain_txt(dns_name: &str) -> Option<Vec<String>> {
    use hickory_resolver::proto::rr::{RData, RecordType};
    let resolver = hickory_resolver::Resolver::builder_tokio().ok()?.build().ok()?;
    let lookup = resolver.lookup(dns_name, RecordType::TXT).await.ok()?;
    Some(
        lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::TXT(txt) => Some(
                    txt.txt_data.iter().map(|b| String::from_utf8_lossy(b)).collect::<String>(),
                ),
                _ => None,
            })
            .collect(),
    )
}

/// Checks a DNSSEC proof (an RR stream, RFC 9102 style) for `dns_name` at UNIX time
/// `now` and returns the verified TXT values.
pub fn verify_proof(proof: &[u8], dns_name: &str, now: u64) -> Result<Vec<String>, String> {
    let rrs = parse_rr_stream(proof).map_err(|_| "unreadable proof".to_string())?;
    let verified = verify_rr_stream(&rrs).map_err(|e| format!("{e:?}"))?;
    if now < verified.valid_from || now > verified.expires {
        return Err("the signatures are not valid now".into());
    }
    let name = Name::try_from(dns_name).map_err(|_| "bad name".to_string())?;
    Ok(verified
        .resolve_name(&name)
        .into_iter()
        .filter_map(|rr| match rr {
            RR::Txt(t) => Some(String::from_utf8_lossy(&t.data.as_vec()).into_owned()),
            _ => None,
        })
        .collect())
}

impl Bip353Resolver {
    /// The verified `bitcoin:` URI of `name`, or `None` when it has no BIP-353 record.
    pub async fn resolve(&self, name: &HumanName) -> Result<Option<String>, Bip353Error> {
        let dns_name = name.dns_name();
        match self {
            Bip353Resolver::Fixed(map) => {
                single(name, bitcoin_uris(map.get(&name.address()).cloned().unwrap_or_default()))
            }
            Bip353Resolver::Dnssec(server) => {
                // Cheap check first: most Lightning addresses are LNURL-only.
                let plain = bitcoin_uris(plain_txt(&dns_name).await.unwrap_or_default());
                if plain.is_empty() {
                    return Ok(None);
                }
                let qname = Name::try_from(dns_name.as_str())
                    .map_err(|_| Bip353Error::Lookup(name.address(), "invalid name".into()))?;
                let (proof, _ttl) = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    dnssec_prover::query::build_txt_proof_async(*server, &qname),
                )
                .await
                .map_err(|_| Bip353Error::Lookup(name.address(), "DNS timed out".into()))?
                .map_err(|e| Bip353Error::Lookup(name.address(), e.to_string()))?;
                let values = verify_proof(&proof, &dns_name, crate::wallet::now() as u64)
                    .map_err(|e| {
                        tracing::info!(name = %name.address(), error = %e, "BIP-353 proof rejected");
                        Bip353Error::Unsigned(name.address())
                    })?;
                match single(name, bitcoin_uris(values))? {
                    Some(uri) => Ok(Some(uri)),
                    // Plain DNS showed a record, but the signed answer has none.
                    None => Err(Bip353Error::Unsigned(name.address())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let n = parse_name(" ₿Alice@Example.COM ").unwrap();
        assert_eq!(
            (n.user.as_str(), n.domain.as_str(), n.explicit),
            ("alice", "example.com", true)
        );
        assert_eq!(n.dns_name(), "alice.user._bitcoin-payment.example.com.");
        assert!(!parse_name("bob@pay.example.com").unwrap().explicit);
        for bad in
            ["alice@localhost", "a@1.2.3.4", "a@host.com:8080", "@x.com", "a b@x.com", "x.com"]
        {
            assert!(parse_name(bad).is_none(), "{bad}");
        }
    }

    #[tokio::test]
    async fn fixed_records() {
        let n = parse_name("a@x.com").unwrap();
        let r = |v: &[&str]| {
            Bip353Resolver::Fixed(Arc::new(HashMap::from([(
                "a@x.com".to_string(),
                v.iter().map(|s| s.to_string()).collect(),
            )])))
        };
        assert_eq!(r(&[]).resolve(&n).await, Ok(None));
        assert_eq!(
            r(&["v=spf1", "BITCOIN:?lno=lno1x"]).resolve(&n).await,
            Ok(Some("BITCOIN:?lno=lno1x".into()))
        );
        assert_eq!(
            r(&["bitcoin:?lno=a", "bitcoin:?lno=b"]).resolve(&n).await,
            Err(Bip353Error::Ambiguous("a@x.com".into()))
        );
    }

    /// A real proof for `matt@mattcorallo.com`, captured at UNIX time 1794072207.
    const PROOF: &[u8] = include_bytes!("testdata/bip353_matt_mattcorallo_com.bin");
    const PROOF_TIME: u64 = 1_794_072_207;
    const PROOF_NAME: &str = "matt.user._bitcoin-payment.mattcorallo.com.";

    #[test]
    fn signed_proof_checks_out() {
        let values = verify_proof(PROOF, PROOF_NAME, PROOF_TIME).unwrap();
        let uris = bitcoin_uris(values);
        assert_eq!(uris.len(), 1, "{uris:?}");
        assert!(uris[0].contains("lno="), "{}", uris[0]);
        // Another name is not covered by this proof.
        assert!(verify_proof(PROOF, "eve.user._bitcoin-payment.mattcorallo.com.", PROOF_TIME)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stale_or_tampered_proofs_fail() {
        assert!(verify_proof(PROOF, PROOF_NAME, PROOF_TIME + 120 * 86_400).is_err(), "expired");
        // Flip one byte of the TXT data (it sits near the end of the proof's first records).
        let at = PROOF.windows(4).position(|w| w == b"lno=").expect("offer in proof") + 10;
        let mut bad = PROOF.to_vec();
        bad[at] ^= 1;
        assert!(verify_proof(&bad, PROOF_NAME, PROOF_TIME).is_err(), "tampered");
    }

    #[test]
    fn bad_proofs_fail() {
        assert!(verify_proof(b"nonsense", "a.user._bitcoin-payment.x.com.", 0).is_err());
        assert!(verify_proof(&[], "a.user._bitcoin-payment.x.com.", 0).is_err());
    }

    /// Live check against a signed name (needs the internet): `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_signed_name() {
        let n = parse_name("₿matt@mattcorallo.com").unwrap();
        let uri = Bip353Resolver::default().resolve(&n).await.unwrap().unwrap();
        assert!(uri.to_ascii_lowercase().starts_with("bitcoin:"), "{uri}");
    }
}
