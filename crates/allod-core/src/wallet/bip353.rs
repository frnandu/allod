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
    let resolver = hickory_resolver::Resolver::builder_tokio().ok()?.build().ok()?;
    txt_values(&resolver, dns_name).await
}

async fn txt_values(
    resolver: &hickory_resolver::TokioResolver,
    dns_name: &str,
) -> Option<Vec<String>> {
    use hickory_resolver::proto::rr::{RData, RecordType};
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

/// What DNS says about a domain, whatever records it has (SPEC §12.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ZoneHealth {
    /// Name servers answer for the domain's zone (the closest ancestor with NS records).
    pub dns: bool,
    /// The zone is signed: its parent publishes a DS record for it (BIP-353 needs it).
    pub dnssec: bool,
}

/// A resolver that asks these servers only, without a cache.
fn direct_resolver(
    ips: Vec<std::net::IpAddr>,
    port: u16,
) -> Option<hickory_resolver::TokioResolver> {
    use hickory_resolver::config::{NameServerConfig, ResolverConfig};
    let config = ResolverConfig::from_parts(
        None,
        vec![],
        ips.into_iter()
            .map(|ip| {
                let mut ns = NameServerConfig::udp_and_tcp(ip);
                for c in ns.connections.iter_mut() {
                    c.port = port;
                }
                ns
            })
            .collect(),
    );
    let mut builder = hickory_resolver::TokioResolver::builder_with_config(
        config,
        hickory_resolver::net::runtime::TokioRuntimeProvider::default(),
    );
    builder.options_mut().cache_size = 0;
    builder.build().ok()
}

/// The zone `name` belongs to (the closest ancestor with NS records) and a resolver for
/// its own name servers. A recursive resolver may remember "no such name" for the
/// zone's negative TTL; the zone's servers answer as soon as a record exists.
async fn zone_servers(name: &str) -> Option<(String, hickory_resolver::TokioResolver)> {
    use hickory_resolver::proto::rr::{RData, RecordType};
    let system = hickory_resolver::Resolver::builder_tokio().ok()?.build().ok()?;
    let labels: Vec<&str> = name.trim_end_matches('.').split('.').collect();
    for i in 0..labels.len().saturating_sub(1) {
        let zone = format!("{}.", labels[i..].join("."));
        let Ok(ns) = system.lookup(zone.clone(), RecordType::NS).await else { continue };
        let mut ips = Vec::new();
        for r in ns.answers() {
            if let RData::NS(host) = &r.data {
                if let Ok(addrs) = system.lookup_ip(host.0.clone()).await {
                    ips.extend(addrs.iter().filter(|ip| ip.is_ipv4()).take(1));
                }
            }
            if ips.len() >= 2 {
                break;
            }
        }
        if !ips.is_empty() {
            return Some((zone, direct_resolver(ips, 53)?));
        }
    }
    None
}

/// Checks that `name`'s zone exists (its own servers answer) and is signed: the DS
/// record is asked from `validating` (Allod's DNSSEC resolver, e.g. 1.1.1.1:53) and
/// Google's, never the system stub, which may drop DS answers (systemd-resolved).
pub async fn zone_health(name: &str, validating: Option<std::net::SocketAddr>) -> ZoneHealth {
    use hickory_resolver::proto::rr::RecordType;
    let Some((zone, own)) = zone_servers(name).await else { return ZoneHealth::default() };
    let dns = own
        .lookup(zone.clone(), RecordType::NS)
        .await
        .is_ok_and(|l| l.answers().iter().any(|r| r.record_type() == RecordType::NS));
    let mut resolvers: Vec<(std::net::IpAddr, u16)> =
        validating.map(|a| (a.ip(), a.port())).into_iter().collect();
    resolvers.push(([8, 8, 8, 8].into(), 53));
    let mut dnssec = false;
    for (ip, port) in resolvers {
        let Some(r) = direct_resolver(vec![ip], port) else { continue };
        if r.lookup(zone.clone(), RecordType::DS)
            .await
            .is_ok_and(|l| l.answers().iter().any(|r| r.record_type() == RecordType::DS))
        {
            dnssec = true;
            break;
        }
    }
    ZoneHealth { dns, dnssec }
}

/// The IPv4 addresses `name` has, asked from its zone's own servers.
pub async fn authoritative_a(name: &str) -> Option<Vec<String>> {
    use hickory_resolver::proto::rr::RecordType;
    let (_, own) = zone_servers(name).await?;
    let l = own.lookup(format!("{}.", name.trim_end_matches('.')), RecordType::A).await.ok()?;
    Some(
        l.answers()
            .iter()
            .filter_map(|r| match &r.data {
                hickory_resolver::proto::rr::RData::A(a) => Some(a.0.to_string()),
                _ => None,
            })
            .collect(),
    )
}

/// TXT values at `dns_name` asked straight from `zone`'s own name servers. Falls back to
/// the system resolver when the servers cannot be found.
pub async fn authoritative_txt(dns_name: &str, zone: &str) -> Option<Vec<String>> {
    match zone_servers(zone).await {
        Some((_, own)) => txt_values(&own, dns_name).await,
        None => {
            let system = hickory_resolver::Resolver::builder_tokio().ok()?.build().ok()?;
            txt_values(&system, dns_name).await
        }
    }
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

    /// A real proof for `matt@mattcorallo.com` (RR stream, hex), captured at UNIX time
    /// 1794072207.
    const PROOF_HEX: &str = concat!(
        "046d6174740475736572105f626974636f696e2d7061796d656e740b6d617474636f72616c6c6f03636f6d0000100001",
        "00000e10004b4a6173206c6f6e6720617320697420646f65736e2774207374617274207769746820626974636f696e3a",
        "2c206f74686572207265636f7264732073686f756c642062652069676e6f726564046d6174740475736572105f626974",
        "636f696e2d7061796d656e740b6d617474636f72616c6c6f03636f6d000010000100000e1001ecff626974636f696e3a",
        "626331717a7477793678656e337a647474377a3076726761706d6a74667a3861636a6b6670356670376c3f6c6e6f3d6c",
        "6e6f317a7235717975677167736b726b37306b716d7571377633646e7232666e6d68756b7073396e386875743438766b",
        "7170716e736b743273767371776a616b70376b36707968746b7578773779326b716d73786c777275687a7176307a736e",
        "686839713374397868783339737563367173723037656b6d3565736479756d307736366d6e7838766471757776703764",
        "70356a70376a337635637036616a3077333239666e6b7171763630713936737a356e6b72633572393571666678303032",
        "7135337471646beb3878396d32746d7438356a74706d63796376666e727078336c723435683267376e61337365633778",
        "67756374667a7a636d386a6a71746a3579613237746536306a303376707430767139746d326e3979786c32686e67666e",
        "6d79676573613235733475347a6c78657771707670393478743772757234726878756e776b74686b39766c79336c6d35",
        "6868307071763461796d6371656a6c6773736e6c707a776c6767796b6b616a7037796a73356a76723261676b79797063",
        "646c6a323830637934366a70796e73657a72636a326b7761326c797238787664366c666b706834787278746b32786333",
        "6c7071046d6174740475736572105f626974636f696e2d7061796d656e740b6d617474636f72616c6c6f03636f6d0000",
        "2e000100000e10006300100d0500000e106aff39716aecaf599e100b6d617474636f72616c6c6f03636f6d001f331196",
        "5c21e675537ecbd85ea7133b06b684ba1d230669df36b8a1281a282721fec37c5624506f5b3fc0eb932fcd0b5f92eca4",
        "1c8605ce101ddc3163b86fba0b6d617474636f72616c6c6f03636f6d000030000100002a3000440100030dceb23a7af6",
        "4b39a6e0b1b720c65263b141346d2936fd98a007009662a9436ea557530510ceec4c9465f0e7ce54ca99d66229695cca",
        "db5b1db2569de428cd4cfa0b6d617474636f72616c6c6f03636f6d000030000100002a3000440101030dc8ddab89616f",
        "09312505c8732f2b7453546309c5d6f9ebd172aec0a79114ea039a72578e5dfb78f47da8403711a8ae82fdd2433db679",
        "d840790b55a52180d8e10b6d617474636f72616c6c6f03636f6d00002e000100002a30006300300d0200093a806b00a7",
        "176aee1cff23490b6d617474636f72616c6c6f03636f6d006b8946652c16de9dcca701fc50825f0adafec582e257fc54",
        "4e1f93d05a6ae7534c4d7a257fdb84acc8354c6c648e7689bccf119fc5631775752bf3989989d5580b6d617474636f72",
        "616c6c6f03636f6d00002b000100015180002423490d02282511c1378832188575a172f29a89c09ac28c826fc4fe7853",
        "4d4c6df5eed2f00b6d617474636f72616c6c6f03636f6d00002e0001000151800057002b0d02000151806af3d7116aea",
        "8c29ff4003636f6d009cd740a120a56d56af6307549b529c1a361880a32184d18677141a65b8cc918d93b3e4c6f54369",
        "ccd8283ab242ad6f2bd6aede39339966f9959b8d974b1a602103636f6d00003000010000081900440100030dbc12cbab",
        "b74ff41a06a6cb7f8c33c74a8cb5307118a178208f0fbbbf6ca3ccb59983a09a75573868851217654eda8d069b478020",
        "080eb7cb1e667cc53840c17103636f6d00003000010000081900440101030db71f0465101ddbe2bf0c9455d12fa16c1c",
        "da44f4bf1ba2553418ad1f3aa9b06973f21b84eb532cf4035ee8d4832ca26d89306a7d32560c0cb0129d450ac1083503",
        "636f6d00002e000100000819005700300d01000151806afdbe0b6ae9f65f4d0603636f6d00ac4e06e6d9c13c8c733814",
        "7d60d2da407de9e41d616b7beea5766172a11791c7ce31410801afe429b4ef06b3f217f937be56e02e5348d8f40f7ef2",
        "2030f1281f03636f6d00002b000100011b4f00244d060d028acbb0cd28f41250a80a491389424d341522d946b0da0c02",
        "91f2d3d771d7805a03636f6d00002e000100011b4f0113002b0801000151806affd3d06aeea240223b00abb727bfebb5",
        "364fcb6251ec1eebdd0e0ce7640fcf9ae8cb12c634b719dca742e82c85c34a129f48b9a6be61da237a7930adc8117ae5",
        "6a8404e8459e6b785334c92013d5eb2b7a4556bee06168466580a16b31602f51f0c38292966c821e94ee7618b4bf97e3",
        "d49402560ee6f70ad4ac96403f913c9428cc47542bfc12e956fc87b53a4b5028fcfb0cfcc5ac8b5bdfed7ec02369fce4",
        "6313b32333f6d43f056ab528f11d82d66a7d74e2e01883e125fefab0b0aeeb4285b9641057562964ce9939da8cacff64",
        "959e51b8f44dfc93eada75fb64695ce367cb96e6f6b27d11d033bb041b968553bf76d4cbc081ef0020a423f4092d9c3c",
        "12f0094db9b6268a8e9a0000300001000008c401080100030803010001ca2b37135cf6dd2e7b565e9cf8d5311ba3950d",
        "982c1274a51ff85cc8c76f2f6faf4ddc4711522a75a9c5e8393cbdfce03f557aa752d5b8eb1b24ec1eda1a8332373625",
        "c577cb231cc3a7671bb9a69ffe501220eed6ac62054c31d52233562efdcfaddf4775430fefe8d60b3673a7cb04eee87b",
        "9487b635b1946afc665caf12c43571d0a322a93ab3abf11dfea9deb02baf48281f3464166d917f6b744a8a264fb26178",
        "c4673674ce4005b6198d802ade02853d3d5954eba7723ac68b8dfdf02949549bd48d38ab3eb4ae8bb929916caebed53f",
        "ed3da45a301484bafd86eaf5df9c9d6ec8c6c6beda519ff60372f4e3ae9e83cc4e97c85e3841dc09de82b1e1e9000030",
        "0001000008c401080101030803010001acffb409bcc939f831f7a1e5ec88f7a59255ec53040be432027390a4ce896d6f",
        "9086f3c5e177fbfe118163aaec7af1462c47945944c4e2c026be5e98bbcded25978272e1e3e079c5094d573f0e83c92f",
        "02b32d3513b1550b826929c80dd0f92cac966d17769fd5867b647c3f38029abdc48152eb8f207159ecc5d232c7c1537c",
        "79f4b7ac28ff11682f21681bf6d6aba555032bf6f9f036beb2aaa5b3778d6eebfba6bf9ea191be4ab0caea759e2f773a",
        "1f9029c73ecb8d5735b9321db085f1b8e2d8038fe2941992548cee0d67dd4547e11dd63af9c9fc1c5466fb684cf009d7",
        "197c2cf79e792ab501e6a8a1ca519af2cb9b5f6367e94c0d47502451357be1b50000300001000008c401080101030803",
        "010001af7a8deba49d995a792aefc80263e991efdbc86138a931deb2c65d5682eab5d3b03738e3dfdc89d96da64c86c0",
        "224d9ce02514d285da3068b19054e5e787b2969058e98e12566c8c808c40c0b769e1db1a24a1bd9b31e303184a31fc7b",
        "b56b85bbba8abc02cd5040a444a36d47695969849e16ad856bb58e8fac8855224400319bdab224d83fc0e66aab32ff74",
        "bfeaf0f91c454e6850a1295207bbd4cdde8f6ffb08faa9755c2e3284efa01f99393e18786cb132f1e66ebc6517318e1c",
        "e8a3b7337ebb54d035ab57d9706ecd9350d4afacd825e43c8668eece89819caf6817af62dc4fbd82f0e33f6647b2b6bd",
        "a175f14607f59f4635451e6b27df282ef73d8700002e0001000008c40113003008000002a3006b00df006ae52f809728",
        "005059096217b1ab96607ef3fea08cf6594250805f1b856f2cb772e75ed2013f4791712417ad883bfa5bb1c10620fb78",
        "a2655dbe0493dc3667ffa43bbeb8d3daa659073c92d787d592d2811564a50d93fd309370345cbe543924dd3d69ff2284",
        "d65b1c86c6e9feb9d57b79142a56946f49d94115aa8f5920c80e45f441021fc9eef64c1e2db0f0670368960f27ba535e",
        "7b11c3c174d78abf871f8757003a578f21ac4bb3536834722efd9aea118e7333e18d27867bd936dad3d8d88f3ddac814",
        "8f5629c8eb839838e46a66c62db8b5d4ac1743e9459f57c2e37f76f43067c1b3b068658009c55c8daf9090175dc22f0f",
        "b8c6b6ccb2641675306ace9e6212d07d54",
    );
    const PROOF_TIME: u64 = 1_794_072_207;
    const PROOF_NAME: &str = "matt.user._bitcoin-payment.mattcorallo.com.";

    fn proof() -> Vec<u8> {
        hex::decode(PROOF_HEX).unwrap()
    }

    #[test]
    fn signed_proof_checks_out() {
        let proof = proof();
        let values = verify_proof(&proof, PROOF_NAME, PROOF_TIME).unwrap();
        let uris = bitcoin_uris(values);
        assert_eq!(uris.len(), 1, "{uris:?}");
        assert!(uris[0].contains("lno="), "{}", uris[0]);
        // Another name is not covered by this proof.
        assert!(verify_proof(&proof, "eve.user._bitcoin-payment.mattcorallo.com.", PROOF_TIME)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stale_or_tampered_proofs_fail() {
        let proof = proof();
        assert!(verify_proof(&proof, PROOF_NAME, PROOF_TIME + 120 * 86_400).is_err(), "expired");
        // Flip one byte of the TXT data (it sits near the end of the proof's first records).
        let at = proof.windows(4).position(|w| w == b"lno=").expect("offer in proof") + 10;
        let mut bad = proof.clone();
        bad[at] ^= 1;
        assert!(verify_proof(&bad, PROOF_NAME, PROOF_TIME).is_err(), "tampered");
    }

    #[test]
    fn bad_proofs_fail() {
        assert!(verify_proof(b"nonsense", "a.user._bitcoin-payment.x.com.", 0).is_err());
        assert!(verify_proof(&[], "a.user._bitcoin-payment.x.com.", 0).is_err());
    }

    /// Live check of the direct lookup (needs the internet): `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_authoritative_lookup() {
        let v = authoritative_txt("matt.user._bitcoin-payment.mattcorallo.com.", "mattcorallo.com")
            .await
            .unwrap();
        assert!(v.iter().any(|v| v.starts_with("bitcoin:")), "{v:?}");
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
