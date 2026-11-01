//! Gossip peer discovery: BOLT 10 DNS seeds on a first start, the best-connected nodes
//! of our own graph afterwards.
//!
//! ldk-node and LDK request a full two-week gossip sync only from the first five peers
//! of a process, so the peers chosen at startup decide how complete the graph gets.
//! With a saved graph we pick large, well-connected nodes from it; DNS seeds (random
//! nodes) only bootstrap an empty graph or refill when the graph offers nothing.

use std::collections::HashSet;
use std::ops::Deref;
use std::str::FromStr;

use lightning::bitcoin::secp256k1::PublicKey;
use lightning::bitcoin::Network;
use lightning::ln::msgs::SocketAddress;
use lightning::routing::gossip::NetworkGraph;
use lightning::util::logger::Logger;
use rand::seq::SliceRandom;
use serde::Serialize;

/// Where a peer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// `ALLOD_GOSSIP_PEERS`: always kept connected.
    Static,
    /// A well-connected node from our own graph.
    Graph,
    /// A BOLT 10 DNS seed answer.
    Dns,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub key: PublicKey,
    /// `host:port`, `[v6]:port` or `<onion>:port`, as [`crate::connect`] takes it.
    pub addr: String,
    pub source: Source,
}

/// Default BOLT 10 seeds per network. Both answered SRV queries in November 2026;
/// `lseed.bitcoinstats.com` and `lseed.darosior.ninja` did not.
pub fn default_seeds(network: Network) -> Vec<String> {
    match network {
        Network::Bitcoin => vec!["nodes.lightning.directory".into(), "nodes.lightning.wiki".into()],
        Network::Testnet => vec!["test.nodes.lightning.directory".into()],
        _ => Vec::new(),
    }
}

/// `ALLOD_GOSSIP_DNS_SEEDS`: empty uses the network default, `none` disables seeding.
pub fn parse_seeds(value: &str, network: Network) -> Vec<String> {
    let value = value.trim();
    if value.is_empty() {
        return default_seeds(network);
    }
    if value.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    value.split([',', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// The node id a seed encodes in its SRV target: `ln1<bech32 pubkey>.<seed>.`
pub fn node_id_from_target(target: &str) -> Option<PublicKey> {
    let label = target.split('.').next()?;
    let (hrp, data) = bech32::decode(label).ok()?;
    if hrp.as_str() != "ln" {
        return None;
    }
    PublicKey::from_slice(&data).ok()
}

/// Queries every seed for SRV records and turns them into candidates. The target host
/// name is resolved later, when connecting.
pub async fn dns_candidates(seeds: &[String]) -> (Vec<Candidate>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    if seeds.is_empty() {
        return (out, errors);
    }
    let resolver = match hickory_resolver::Resolver::builder_tokio() {
        Ok(builder) => builder.build(),
        Err(e) => {
            errors.push(format!("DNS resolver: {e}"));
            return (out, errors);
        }
    };
    for seed in seeds {
        let name = format!("_nodes._tcp.{}.", seed.trim_end_matches('.'));
        match resolver.srv_lookup(name.as_str()).await {
            Ok(lookup) => {
                let before = out.len();
                for srv in lookup.iter() {
                    let target = srv.target().to_utf8();
                    let Some(key) = node_id_from_target(&target) else { continue };
                    out.push(Candidate {
                        key,
                        addr: format!("{}:{}", target.trim_end_matches('.'), srv.port()),
                        source: Source::Dns,
                    });
                }
                tracing::info!(seed, nodes = out.len() - before, "DNS seed answered");
            }
            Err(e) => {
                tracing::warn!(seed, error = %e, "DNS seed did not answer");
                errors.push(format!("{seed}: {e}"));
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.key));
    (out, errors)
}

/// Addresses we can dial, in preference order: IPv4, IPv6, host names, then onion.
fn dialable(addresses: &[SocketAddress], onion: bool) -> Option<String> {
    let rank = |a: &SocketAddress| match a {
        SocketAddress::TcpIpV4 { .. } => Some(0),
        SocketAddress::TcpIpV6 { .. } => Some(1),
        SocketAddress::Hostname { .. } => Some(2),
        SocketAddress::OnionV3 { .. } if onion => Some(3),
        _ => None,
    };
    addresses
        .iter()
        .filter_map(|a| rank(a).map(|r| (r, a)))
        .min_by_key(|(r, _)| *r)
        .map(|(_, a)| a.to_string())
}

/// The `pool` best-connected nodes of the graph that announce a dialable address,
/// shuffled so restarts do not always pick the same five.
pub fn graph_candidates<L: Deref>(
    graph: &NetworkGraph<L>,
    onion: bool,
    pool: usize,
) -> Vec<Candidate>
where
    L::Target: Logger,
{
    let read_only = graph.read_only();
    let mut ranked: Vec<(usize, Candidate)> = read_only
        .nodes()
        .unordered_iter()
        .filter_map(|(id, info)| {
            let announcement = info.announcement_info.as_ref()?;
            let addr = dialable(announcement.addresses(), onion)?;
            let key = PublicKey::from_slice(id.as_slice()).ok()?;
            Some((info.channels.len(), Candidate { key, addr, source: Source::Graph }))
        })
        .collect();
    ranked.sort_by_key(|(channels, _)| std::cmp::Reverse(*channels));
    let mut top: Vec<Candidate> = ranked.into_iter().take(pool).map(|(_, c)| c).collect();
    top.shuffle(&mut rand::thread_rng());
    top
}

/// `ALLOD_GOSSIP_PEERS`: comma-separated `pubkey@host:port`.
pub fn parse_static(list: &str) -> Vec<Candidate> {
    list.split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter_map(|peer| {
            let parsed = peer.split_once('@').and_then(|(key, addr)| {
                Some(Candidate {
                    key: PublicKey::from_str(key).ok()?,
                    addr: addr.to_string(),
                    source: Source::Static,
                })
            });
            if parsed.is_none() {
                tracing::warn!(peer, "ignoring malformed peer, expected pubkey@host:port");
            }
            parsed
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn decodes_seed_targets() {
        // Real answer from nodes.lightning.directory.
        let key = node_id_from_target(
            "ln1qgx7gqwyc3tw7ml7creluu5928ulluh5fvs8ggsus2t9k8fzpe2jy7nuer5.nodes.lightning.directory.",
        )
        .unwrap();
        assert_eq!(key.serialize().len(), 33);
        assert!(node_id_from_target("garbage.nodes.lightning.directory.").is_none());
    }

    #[test]
    fn seeds_default_per_network() {
        assert_eq!(parse_seeds("", Network::Bitcoin).len(), 2);
        assert!(parse_seeds("none", Network::Bitcoin).is_empty());
        assert!(parse_seeds("", Network::Regtest).is_empty());
        assert_eq!(
            parse_seeds("a.example, b.example", Network::Signet),
            ["a.example", "b.example"]
        );
    }

    #[test]
    fn parses_static_peers() {
        let key = "03864ef025fde8fb587d989186ce6a4a186895ee44a926bfc370e2c366597a3f8f";
        let peers = parse_static(&format!("{key}@3.33.236.230:9735, bad, {key}@abc.onion:9735"));
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].addr, "3.33.236.230:9735");
        assert_eq!(peers[1].source, Source::Static);
    }

    #[test]
    fn prefers_clearnet_and_skips_onion_without_proxy() {
        let onion = SocketAddress::from_str(
            "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:9735",
        )
        .unwrap();
        let v4 = SocketAddress::TcpIpV4 { addr: [1, 2, 3, 4], port: 9735 };
        assert_eq!(dialable(&[onion.clone(), v4], false).unwrap(), "1.2.3.4:9735");
        assert!(dialable(std::slice::from_ref(&onion), false).is_none());
        assert!(dialable(&[onion], true).unwrap().ends_with(".onion:9735"));
    }

    #[test]
    fn graph_candidates_need_an_announced_address() {
        struct NoLog;
        impl Logger for NoLog {
            fn log(&self, _: lightning::util::logger::Record) {}
        }
        let graph = NetworkGraph::new(Network::Bitcoin, Arc::new(NoLog));
        assert!(graph_candidates(&graph, false, 10).is_empty());
    }
}
