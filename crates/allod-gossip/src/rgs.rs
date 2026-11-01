//! Rapid Gossip Sync (RGS) snapshot encoder.
//!
//! Writes the version 2 format that `lightning-rapid-gossip-sync` parses
//! (`processing.rs` at the pinned rust-lightning commit). Every snapshot is a full one:
//! the client's `latest_seen` timestamp is ignored, which is fine on a local link and
//! keeps the encoder free of per-client state.

use std::collections::{BTreeMap, HashMap};

use lightning::bitcoin::constants::ChainHash;
use lightning::routing::gossip::{ChannelUpdateInfo, NetworkGraph, NodeId};
use lightning::util::logger::Logger;
use lightning::util::ser::{BigSize, Writeable};
use std::ops::Deref;

/// Updates older than this are not sent: ldk-node would drop the channel as stale anyway.
pub const MAX_UPDATE_AGE_SECS: u64 = 14 * 24 * 3600;

/// What the snapshot may include for one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inclusion {
    /// Send the channel, with its on-chain capacity when known.
    Include { funding_sats: Option<u64> },
    /// Leave the channel out (not verified, closed, or not yet checked).
    Skip,
}

/// Counts describing an encoded snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotStats {
    pub channels: usize,
    pub nodes: usize,
    pub updates: usize,
    pub channels_with_capacity: usize,
}

/// Builds a full RGS v2 snapshot of `graph`. `decide` picks which channels go in.
pub fn encode<L: Deref>(
    graph: &NetworkGraph<L>,
    chain_hash: ChainHash,
    now_unix: u64,
    mut decide: impl FnMut(u64) -> Inclusion,
) -> (Vec<u8>, SnapshotStats)
where
    L::Target: Logger,
{
    let read_only = graph.read_only();
    let min_update = now_unix.saturating_sub(MAX_UPDATE_AGE_SECS);
    let fresh = |info: &Option<ChannelUpdateInfo>| -> Option<ChannelUpdateInfo> {
        info.as_ref().filter(|u| u64::from(u.last_update) >= min_update).cloned()
    };

    struct Chan {
        scid: u64,
        features: lightning::types::features::ChannelFeatures,
        node_one: NodeId,
        node_two: NodeId,
        funding_sats: Option<u64>,
        updates: [Option<ChannelUpdateInfo>; 2],
    }

    // Ordered by scid: announcements and updates are delta-encoded.
    let mut chans: BTreeMap<u64, Chan> = BTreeMap::new();
    for (scid, info) in read_only.channels().unordered_iter() {
        let Inclusion::Include { funding_sats } = decide(*scid) else { continue };
        let updates = [fresh(&info.one_to_two), fresh(&info.two_to_one)];
        if updates.iter().all(Option::is_none) {
            continue;
        }
        chans.insert(
            *scid,
            Chan {
                scid: *scid,
                features: info.features.clone(),
                node_one: info.node_one,
                node_two: info.node_two,
                funding_sats,
                updates,
            },
        );
    }

    let mut node_ids: Vec<NodeId> =
        chans.values().flat_map(|c| [c.node_one, c.node_two]).collect::<Vec<_>>();
    node_ids.sort();
    node_ids.dedup();
    let node_index: HashMap<NodeId, u64> =
        node_ids.iter().enumerate().map(|(i, id)| (*id, i as u64)).collect();

    let mut out = Vec::with_capacity(chans.len() * 64);
    out.extend_from_slice(b"LDK");
    write(&mut out, &2u8);
    write(&mut out, &chain_hash);
    write(&mut out, &u32::try_from(now_unix).unwrap_or(u32::MAX));

    // No default node features: each node that has an announcement carries its own.
    write(&mut out, &0u8);
    write(&mut out, &(node_ids.len() as u32));
    for id in &node_ids {
        let mut key = *id.as_slice().first_chunk::<33>().expect("node ids are 33 bytes");
        let announcement = read_only.node(id).and_then(|n| n.announcement_info.clone());
        let mut addresses = Vec::new();
        if let Some(info) = &announcement {
            for address in info.addresses().iter().take(u8::MAX as usize) {
                let encoded = address.encode();
                if let Ok(len) = u8::try_from(encoded.len()) {
                    addresses.push((len, encoded));
                }
            }
        }
        // Byte 0 keeps the key parity in bits 0-1 and carries the detail flags above them.
        if !addresses.is_empty() {
            key[0] |= 1 << 2;
        }
        if announcement.is_some() {
            key[0] |= 0b111 << 3; // features follow explicitly
        }
        out.extend_from_slice(&key);
        if !addresses.is_empty() {
            write(&mut out, &(addresses.len() as u8));
            for (len, encoded) in &addresses {
                write(&mut out, len);
                out.extend_from_slice(encoded);
            }
        }
        if let Some(info) = &announcement {
            write(&mut out, info.features());
        }
    }

    let mut stats = SnapshotStats { nodes: node_ids.len(), ..Default::default() };
    write(&mut out, &(chans.len() as u32));
    let mut previous = 0u64;
    for chan in chans.values() {
        write(&mut out, &chan.features);
        write(&mut out, &BigSize(chan.scid - previous));
        previous = chan.scid;
        write(&mut out, &BigSize(node_index[&chan.node_one]));
        let mut second = node_index[&chan.node_two];
        if let Some(sats) = chan.funding_sats {
            second |= 1 << 63;
            stats.channels_with_capacity += 1;
            write(&mut out, &BigSize(second));
            let mut extra = Vec::new();
            write(&mut extra, &BigSize(sats));
            write(&mut out, &extra);
        } else {
            write(&mut out, &BigSize(second));
        }
    }
    stats.channels = chans.len();

    let updates: Vec<(u64, u8, &ChannelUpdateInfo)> = chans
        .values()
        .flat_map(|c| {
            c.updates
                .iter()
                .enumerate()
                .filter_map(move |(dir, u)| u.as_ref().map(|u| (c.scid, dir as u8, u)))
        })
        .collect();
    stats.updates = updates.len();
    write(&mut out, &(updates.len() as u32));
    if updates.is_empty() {
        return (out, stats);
    }

    // The most common value of each field is sent once and omitted from the updates.
    let cltv = mode(updates.iter().map(|(_, _, u)| u.cltv_expiry_delta));
    let htlc_min = mode(updates.iter().map(|(_, _, u)| u.htlc_minimum_msat));
    let fee_base = mode(updates.iter().map(|(_, _, u)| u.fees.base_msat));
    let fee_ppm = mode(updates.iter().map(|(_, _, u)| u.fees.proportional_millionths));
    let htlc_max = mode(updates.iter().map(|(_, _, u)| u.htlc_maximum_msat));
    write(&mut out, &cltv);
    write(&mut out, &htlc_min);
    write(&mut out, &fee_base);
    write(&mut out, &fee_ppm);
    write(&mut out, &htlc_max);

    let mut previous = 0u64;
    for (scid, direction, update) in updates {
        write(&mut out, &BigSize(scid - previous));
        previous = scid;
        let mut flags = direction;
        if !update.enabled {
            flags |= 1 << 1;
        }
        if update.cltv_expiry_delta != cltv {
            flags |= 0b0100_0000;
        }
        if update.htlc_minimum_msat != htlc_min {
            flags |= 0b0010_0000;
        }
        if update.fees.base_msat != fee_base {
            flags |= 0b0001_0000;
        }
        if update.fees.proportional_millionths != fee_ppm {
            flags |= 0b0000_1000;
        }
        if update.htlc_maximum_msat != htlc_max {
            flags |= 0b0000_0100;
        }
        write(&mut out, &flags);
        if flags & 0b0100_0000 != 0 {
            write(&mut out, &update.cltv_expiry_delta);
        }
        if flags & 0b0010_0000 != 0 {
            write(&mut out, &update.htlc_minimum_msat);
        }
        if flags & 0b0001_0000 != 0 {
            write(&mut out, &update.fees.base_msat);
        }
        if flags & 0b0000_1000 != 0 {
            write(&mut out, &update.fees.proportional_millionths);
        }
        if flags & 0b0000_0100 != 0 {
            write(&mut out, &update.htlc_maximum_msat);
        }
    }
    (out, stats)
}

fn write<T: Writeable>(out: &mut Vec<u8>, value: &T) {
    value.write(out).expect("writing to a Vec cannot fail");
}

fn mode<T: Copy + Eq + std::hash::Hash + Default>(values: impl Iterator<Item = T>) -> T {
    let mut counts: HashMap<T, usize> = HashMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(v, _)| v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightning::bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    use lightning::bitcoin::Network;
    use lightning::ln::msgs::{SocketAddress, UnsignedChannelUpdate, UnsignedNodeAnnouncement};
    use lightning::routing::gossip::NodeAlias;
    use lightning::types::features::{ChannelFeatures, NodeFeatures};
    use lightning_rapid_gossip_sync::RapidGossipSync;
    use std::sync::Arc;

    struct NoLog;
    impl Logger for NoLog {
        fn log(&self, _record: lightning::util::logger::Record) {}
    }

    fn node(i: u8) -> NodeId {
        let secp = Secp256k1::new();
        let key = SecretKey::from_slice(&[i; 32]).unwrap();
        NodeId::from_pubkey(&PublicKey::from_secret_key(&secp, &key))
    }

    fn update(
        scid: u64,
        direction: u8,
        ts: u32,
        fee_ppm: u32,
        disabled: bool,
    ) -> UnsignedChannelUpdate {
        UnsignedChannelUpdate {
            chain_hash: ChainHash::using_genesis_block(Network::Bitcoin),
            short_channel_id: scid,
            timestamp: ts,
            message_flags: 1,
            channel_flags: direction | if disabled { 2 } else { 0 },
            cltv_expiry_delta: 144,
            htlc_minimum_msat: 1000,
            htlc_maximum_msat: 990_000_000,
            fee_base_msat: 1000,
            fee_proportional_millionths: fee_ppm,
            excess_data: Vec::new(),
        }
    }

    #[test]
    fn snapshot_round_trips_through_ldk_rgs_parser() {
        let real_now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        // Encoded five days from now: updates from an hour ago stay fresh, one from ten
        // days ago is then older than two weeks and must be left out.
        let now = real_now + 5 * 86400;
        let ts = (real_now - 3600) as u32;
        let chain = ChainHash::using_genesis_block(Network::Bitcoin);
        let source = NetworkGraph::new(Network::Bitcoin, Arc::new(NoLog));
        let (a, b, c) = (node(1), node(2), node(3));
        let scid_ab = (800_000u64 << 40) | (5 << 16) | 1;
        let scid_bc = (800_100u64 << 40) | (7 << 16);
        let scid_skip = (800_200u64 << 40) | (1 << 16);
        let scid_stale = (800_300u64 << 40) | (2 << 16);
        for (scid, x, y) in
            [(scid_ab, a, b), (scid_bc, b, c), (scid_skip, a, c), (scid_stale, a, c)]
        {
            source
                .add_channel_from_partial_announcement(
                    scid,
                    None,
                    u64::from(ts),
                    ChannelFeatures::empty(),
                    x,
                    y,
                )
                .unwrap();
        }
        source.update_channel_unsigned(&update(scid_ab, 0, ts, 100, false)).unwrap();
        source.update_channel_unsigned(&update(scid_ab, 1, ts, 2500, true)).unwrap();
        source.update_channel_unsigned(&update(scid_bc, 1, ts, 100, false)).unwrap();
        source.update_channel_unsigned(&update(scid_skip, 0, ts, 100, false)).unwrap();
        // Last updated ten days before the graph clock: stale by snapshot time.
        source
            .update_channel_unsigned(&update(
                scid_stale,
                0,
                (real_now - 10 * 86400) as u32,
                1,
                false,
            ))
            .unwrap();
        let address = SocketAddress::TcpIpV4 { addr: [1, 2, 3, 4], port: 9735 };
        source
            .update_node_from_unsigned_announcement(&UnsignedNodeAnnouncement {
                features: NodeFeatures::empty(),
                timestamp: ts,
                node_id: b,
                rgb: [0; 3],
                alias: NodeAlias([0; 32]),
                addresses: vec![address.clone()],
                excess_address_data: Vec::new(),
                excess_data: Vec::new(),
            })
            .unwrap();

        let (bytes, stats) = encode(&source, chain, now, |scid| {
            if scid == scid_skip {
                Inclusion::Skip
            } else if scid == scid_ab {
                Inclusion::Include { funding_sats: Some(1_000_000) }
            } else {
                Inclusion::Include { funding_sats: None }
            }
        });
        assert_eq!(stats.channels, 2);
        assert_eq!(stats.nodes, 3);
        assert_eq!(stats.updates, 3);
        assert_eq!(stats.channels_with_capacity, 1);

        let target = Arc::new(NetworkGraph::new(Network::Bitcoin, Arc::new(NoLog)));
        let rgs = RapidGossipSync::new(Arc::clone(&target), Arc::new(NoLog));
        let latest = rgs.update_network_graph_no_std(&bytes, Some(now)).unwrap();
        assert_eq!(u64::from(latest), now);

        let graph = target.read_only();
        assert_eq!(graph.channels().len(), 2);
        assert!(graph.channel(scid_skip).is_none());
        assert!(graph.channel(scid_stale).is_none());
        let ab = graph.channel(scid_ab).unwrap();
        assert_eq!(ab.capacity_sats, Some(1_000_000));
        assert_eq!((ab.node_one, ab.node_two), (a, b));
        assert_eq!(ab.one_to_two.as_ref().unwrap().fees.proportional_millionths, 100);
        let back = ab.two_to_one.as_ref().unwrap();
        assert_eq!(back.fees.proportional_millionths, 2500);
        assert!(!back.enabled);
        let bc = graph.channel(scid_bc).unwrap();
        assert_eq!(bc.capacity_sats, None);
        assert!(bc.one_to_two.is_none());
        assert_eq!(bc.two_to_one.as_ref().unwrap().cltv_expiry_delta, 144);
        let b_info = graph.node(&b).unwrap().announcement_info.as_ref().unwrap();
        assert_eq!(b_info.addresses(), &[address]);
    }
}
