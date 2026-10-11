//! The per-packet radio readings the `packet_rssi` / `packet_snr` /
//! `packet_q` verbs answer from (D6 of
//! `docs/src/audits/2026-10-11-shared-instance-rpc.md`).
//!
//! Python stamps every packet that arrives on an interface carrying
//! `r_stat_rssi` / `r_stat_snr` / `r_stat_q` and keeps the last
//! `LOCAL_CLIENT_CACHE_MAXSIZE` readings keyed by the full packet hash, so a
//! local client can ask after the fact
//! (`reference/Reticulum/RNS/Transport.py:165-168`, filled at `:1458-1479`).
//! `rnprobe` asks for its proof packet's readings
//! (`reference/Reticulum/RNS/Utilities/rnprobe.py:166-178`). Only the
//! interface knows its medium has readings; it hands them over with the
//! packet ([`crate::interfaces::IncomingPacket::radio`]), and the driver
//! records them here under the hash the core will compute for the same bytes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::sync_ext::MutexRecover;

/// Readings kept, oldest evicted first (Python `LOCAL_CLIENT_CACHE_MAXSIZE`,
/// `reference/Reticulum/RNS/Transport.py:168`).
pub(crate) const PACKET_RADIO_CACHE_MAXSIZE: usize = 512;

/// One received packet's radio readings, in the units Python serves: RSSI in
/// dBm, SNR in dB, link quality in percent.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct PacketRadioStats {
    pub rssi: Option<i16>,
    pub snr: Option<f64>,
    pub q: Option<f64>,
}

impl PacketRadioStats {
    fn is_empty(&self) -> bool {
        self.rssi.is_none() && self.snr.is_none() && self.q.is_none()
    }
}

/// Shared between the event loop, which records, and the RPC server, which
/// answers. A leaf lock: nothing else is taken while it is held.
pub(crate) type SharedPacketRadioCache = Arc<Mutex<PacketRadioCache>>;

/// Packet hash to radio readings, bounded at [`PACKET_RADIO_CACHE_MAXSIZE`].
#[derive(Debug, Default)]
pub(crate) struct PacketRadioCache {
    entries: VecDeque<([u8; 32], PacketRadioStats)>,
}

impl PacketRadioCache {
    /// Record a packet's readings; a packet without any is not recorded.
    pub(crate) fn record(&mut self, packet_hash: [u8; 32], stats: PacketRadioStats) {
        if stats.is_empty() {
            return;
        }
        self.entries.push_back((packet_hash, stats));
        while self.entries.len() > PACKET_RADIO_CACHE_MAXSIZE {
            self.entries.pop_front();
        }
    }

    /// The readings for a packet hash. Like Python's lookup
    /// (`reference/Reticulum/RNS/Reticulum.py:1656-1658`), the oldest match
    /// wins when the same packet was heard twice.
    pub(crate) fn get(&self, packet_hash: &[u8]) -> Option<PacketRadioStats> {
        self.entries
            .iter()
            .find(|(hash, _)| hash.as_slice() == packet_hash)
            .map(|(_, stats)| *stats)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// A fresh, empty shared cache.
pub(crate) fn shared() -> SharedPacketRadioCache {
    Arc::new(Mutex::new(PacketRadioCache::default()))
}

/// Look a hash up in a shared cache.
pub(crate) fn lookup(
    cache: &SharedPacketRadioCache,
    packet_hash: &[u8],
) -> Option<PacketRadioStats> {
    cache.lock_recover().get(packet_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(rssi: i16) -> PacketRadioStats {
        PacketRadioStats {
            rssi: Some(rssi),
            snr: Some(5.5),
            q: Some(80.0),
        }
    }

    #[test]
    fn evicts_the_oldest_beyond_the_reference_cap() {
        let mut cache = PacketRadioCache::default();
        for i in 0..(PACKET_RADIO_CACHE_MAXSIZE + 1) {
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            cache.record(hash, stats(-(i as i16 % 100)));
        }
        assert_eq!(cache.len(), PACKET_RADIO_CACHE_MAXSIZE);
        assert_eq!(cache.get(&[0u8; 32]), None, "the first reading is evicted");
        let mut second = [0u8; 32];
        second[..8].copy_from_slice(&1u64.to_be_bytes());
        assert_eq!(cache.get(&second), Some(stats(-1)));
    }

    #[test]
    fn oldest_match_wins_and_empty_readings_are_not_kept() {
        let mut cache = PacketRadioCache::default();
        let hash = [7u8; 32];
        cache.record(hash, PacketRadioStats::default());
        assert_eq!(cache.len(), 0);
        cache.record(hash, stats(-60));
        cache.record(hash, stats(-70));
        assert_eq!(cache.get(&hash), Some(stats(-60)));
        assert_eq!(cache.get(&[8u8; 32]), None);
        assert_eq!(cache.get(&hash[..16]), None, "a truncated hash is no key");
    }
}
