//! Persistent storage for a board's propagation-node costs (Codeberg
//! #384): the stamp cost it announces to clients and the peering cost it
//! requires of `/offer` peers.
//!
//! A host sets them over the #238 control envelope
//! ([`crate::envelope::TYPE_PN_CONFIG`], `lnflash --stamp-cost` /
//! `--peering-cost`). The record shares the telemetry flash page (the
//! firmware names the layout in `leviculum_nrf::telemetry`); this module
//! only fixes the record's bytes, in the same magic + version + checksum
//! envelope as its four neighbours.
//!
//! Layout (9 bytes, padded to 12 for word-wide flash writes):
//!
//! ```text
//!  0..4   magic "LPNC"
//!  4      format version (0x01)
//!  5      stamp cost
//!  6      peering cost
//!  7..9   checksum over bytes 0..7
//! ```
//!
//! **`None` is not "costs of zero".** It is "no record", and the caller's
//! answer is the Lead's compatibility defaults ([`DEFAULT_STAMP_COST`],
//! [`DEFAULT_PEERING_COST`]) — the values under which a stock Python peer
//! both forwards our stored messages and syncs toward us.

const MAGIC: [u8; 4] = [0x4C, 0x50, 0x4E, 0x43]; // "LPNC"
const FORMAT_VERSION: u8 = 0x01;
const STAMP_OFFSET: usize = 5;
const PEERING_OFFSET: usize = 6;
const CHECKSUM_OFFSET: usize = 7;
const CHECKSUM_SIZE: usize = 2;

/// Total encoded size: 7 header+payload + 2 checksum = 9 bytes.
pub const ENCODED_SIZE: usize = CHECKSUM_OFFSET + CHECKSUM_SIZE;

/// Encoded size rounded up to 4-byte alignment (flash writes are
/// word-wide): 12.
pub const ENCODED_SIZE_ALIGNED: usize = ENCODED_SIZE.div_ceil(4) * 4;

/// Default announced stamp cost: 13 (Lead, 2026-09-12, compatibility).
///
/// A stock Python peer forwards only messages whose stored stamp value
/// satisfies its own cost minus flexibility, filtered on the offering
/// side (`reference/LXMF/LXMF/LXMPeer.py:331`, `:340`), and 13 is the
/// reference's own announce floor (`PROPAGATION_COST_MIN`,
/// `reference/LXMF/LXMF/LXMRouter.py:52`), so a store accepted at this
/// cost clears every default peer's filter with the full flexibility
/// margin.
pub const DEFAULT_STAMP_COST: u8 = 13;

/// Default announced peering cost: 1 (Lead, 2026-09-12, compatibility).
///
/// Not 0, although 0 is wire-legal: `LXMPeer.peering_key_ready`
/// short-circuits false on a falsy cost
/// (`reference/LXMF/LXMF/LXMPeer.py:228`), so a stock peer never syncs
/// toward a node announcing peering cost 0. One bit of work is the
/// cheapest cost a stock peer will act on.
pub const DEFAULT_PEERING_COST: u8 = 1;

/// The two persisted costs. Distinct from
/// [`crate::envelope::PnConfigWire`], whose fields may carry the keep
/// sentinel; what is on the page is always resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredPnConfig {
    pub stamp_cost: u8,
    pub peering_cost: u8,
}

impl StoredPnConfig {
    /// The Lead's defaults — what a board with no record runs.
    pub const DEFAULT: Self = Self {
        stamp_cost: DEFAULT_STAMP_COST,
        peering_cost: DEFAULT_PEERING_COST,
    };
}

/// Same two-byte XOR checksum every record on the page uses.
fn checksum(data: &[u8]) -> [u8; 2] {
    let mut a: u8 = 0;
    let mut b: u8 = 0;
    for (i, &byte) in data.iter().enumerate() {
        if i % 2 == 0 {
            a ^= byte;
        } else {
            b ^= byte;
        }
    }
    [a, b]
}

/// Encode the costs into a fixed-size buffer for persistent storage.
pub fn encode_pn_config(config: &StoredPnConfig) -> [u8; ENCODED_SIZE_ALIGNED] {
    let mut buf = [0u8; ENCODED_SIZE_ALIGNED];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4] = FORMAT_VERSION;
    buf[STAMP_OFFSET] = config.stamp_cost;
    buf[PEERING_OFFSET] = config.peering_cost;
    let cs = checksum(&buf[..CHECKSUM_OFFSET]);
    buf[CHECKSUM_OFFSET] = cs[0];
    buf[CHECKSUM_OFFSET + 1] = cs[1];
    buf
}

/// Decode the costs from a persistent storage buffer.
///
/// `None` for a blank region, wrong magic or version, checksum mismatch,
/// or a cost of 255 — the value no stamp search can satisfy
/// (`leviculum-lxmf/src/stamp.rs`), which no valid writer produces.
pub fn decode_pn_config(buf: &[u8]) -> Option<StoredPnConfig> {
    if buf.len() < ENCODED_SIZE {
        return None;
    }
    if buf[0] == 0xFF || buf[0..4] != MAGIC || buf[4] != FORMAT_VERSION {
        return None;
    }
    let stored = [buf[CHECKSUM_OFFSET], buf[CHECKSUM_OFFSET + 1]];
    if stored != checksum(&buf[..CHECKSUM_OFFSET]) {
        return None;
    }
    if buf[STAMP_OFFSET] == 0xFF || buf[PEERING_OFFSET] == 0xFF {
        return None;
    }
    Some(StoredPnConfig {
        stamp_cost: buf[STAMP_OFFSET],
        peering_cost: buf[PEERING_OFFSET],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_costs_round_trip() {
        for stamp in [0u8, 1, 13, 26, 254] {
            for peering in [0u8, 1, 18, 254] {
                let config = StoredPnConfig {
                    stamp_cost: stamp,
                    peering_cost: peering,
                };
                assert_eq!(decode_pn_config(&encode_pn_config(&config)), Some(config));
            }
        }
    }

    #[test]
    fn a_blank_or_zeroed_region_is_no_record() {
        assert!(decode_pn_config(&[0xFF; ENCODED_SIZE_ALIGNED]).is_none());
        assert!(decode_pn_config(&[0x00; ENCODED_SIZE_ALIGNED]).is_none());
    }

    #[test]
    fn a_bit_flip_anywhere_is_no_record() {
        for i in 0..ENCODED_SIZE {
            let mut buf = encode_pn_config(&StoredPnConfig::DEFAULT);
            buf[i] ^= 0x01;
            assert!(
                decode_pn_config(&buf).is_none(),
                "bit flip at byte {i} was accepted"
            );
        }
    }

    #[test]
    fn the_unminable_cost_is_no_record() {
        // 255 doubles as the wire's keep sentinel and as the one cost
        // stamp generation refuses; a record carrying it is not ours.
        let mut buf = encode_pn_config(&StoredPnConfig::DEFAULT);
        buf[STAMP_OFFSET] = 0xFF;
        let cs = checksum(&buf[..CHECKSUM_OFFSET]);
        buf[CHECKSUM_OFFSET] = cs[0];
        buf[CHECKSUM_OFFSET + 1] = cs[1];
        assert!(decode_pn_config(&buf).is_none());
    }

    #[test]
    fn the_encoded_size_is_word_aligned() {
        assert_eq!(ENCODED_SIZE, 9);
        assert_eq!(ENCODED_SIZE_ALIGNED, 12);
    }
}
