//! What this node puts in its connectable advertisement, and how many
//! of the 31 legacy bytes that costs.
//!
//! The advertisement is the only thing a Columba peer sees before it
//! decides whether to connect, so a capability it has to know *before*
//! connecting belongs here and nowhere else. Today there is exactly one:
//! `PERIPHERAL_ONLY` (BLE protocol v0.3.0).
//!
//! # Why the flag exists
//!
//! Columba v2.2 breaks the "who initiates?" tie between two nodes that
//! can both see each other by comparing BLE addresses: the **lower**
//! address connects, the higher one waits (v2.2 §"Connection Direction
//! (MAC Sorting)": `if my_mac_int < peer_mac_int: connect_to_peer()`;
//! an earlier revision of this comment had the direction backwards). A
//! node that has no central role cannot honour that rule — when its own
//! address sorts below the peer's, the peer waits for it to initiate
//! and it never does, so the link is never made and neither side
//! reports an error. Which way a peer's (random, rotating) address
//! sorts against a mid-range address re-flips on rotation, so the hole
//! shows up as an intermittent "sometimes it just doesn't connect".
//! The full decision rule, and what rotation can and cannot disturb,
//! lives in [`crate::peer`].
//!
//! `PERIPHERAL_ONLY` closes it by saying so in the advertisement: a peer
//! that sees the bit skips the sort and initiates unconditionally. The
//! bit is *cleared* in phase B, when the firmware actually gains the
//! central role and can hold up its end of the sort (#255).
//!
//! # Free peripheral slots (#375 item 3)
//!
//! A searching board used to pick its target blind: it could not tell a
//! peer with three free incoming slots from one with its last one free,
//! so several searchers raced into the same board and all but one were
//! refused. Capability bits 1 to 3 close that: bits 1-2 carry the
//! advertiser's free-slot count (0 to [`PERIPH_SLOTS`]) and bit 3 says
//! the count is there at all. [`with_free_slots`] writes them,
//! [`free_slots`] reads them back, and [`crate::window`] is the only
//! consumer — the count refines which eligible peer is dialled first
//! and nothing else. It is a HINT: it can be stale by the time a
//! dialler acts on it, so the duplicate and refusal paths never look at
//! it and a board that advertised a free slot and has none by the time
//! the connection lands refuses exactly as before.
//!
//! Taking the bits needs no version bump, because neither reader can
//! misread them:
//!
//! - **Columba does not read the record at all.** `BleScanner.performScan`
//!   filters on the service UUID and never touches manufacturer-specific
//!   data (`rns-host/.../ble/client/BleScanner.kt:257`); nothing in the
//!   Columba tree calls `getManufacturerSpecificData`.
//! - **Our own older firmware masks.** [`crate::peer::should_initiate`]
//!   tests `caps & CAP_PERIPHERAL_ONLY != 0`, never `caps == 0`, so a
//!   board predating this change ignores the new bits instead of
//!   misreading them.
//!
//! Bit 3 is what keeps an OLD record honest in the other direction: a
//! v0.3.0 board advertising `caps = 0x01` has bits 1-2 clear, and
//! without a validity bit that would read as "zero free slots" — the
//! worst possible answer, since it would sort a perfectly free board
//! last. With bit 3 clear it reads as "no slot information" and keeps
//! today's behaviour. Bit budget: bit 0 the flag, bits 1-2 the count,
//! bit 3 its validity, **four bits (4-7) still free**.
//!
//! # The byte budget
//!
//! A legacy advertising PDU carries 31 bytes of AD structures, and every
//! structure costs 2 bytes of overhead (a length byte and an AD-type
//! byte) on top of its data — see [`ad_structure_len`]. Ours:
//!
//! | AD structure                   | data | total |
//! |--------------------------------|------|-------|
//! | Flags                          |  1   |  3    |
//! | Complete 128-bit service UUIDs | 16   | 18    |
//! | Manufacturer specific data     |  4   |  6    |
//! | **sum**                        |      | **27** |
//!
//! The `LN-<hex8>` device name ([`crate::device_name`]) does **not**
//! compete for these bytes: it goes in the scan response, a second
//! 31-byte PDU (`ble_task`'s `ScannableUndirected` advertisement carries
//! `adv_data` and `scan_data` separately). [`ADV_BYTES_USED`] and the
//! tests below hold the sum against the limit, so the next AD structure
//! anyone adds either fits or fails on the host instead of at
//! `LegacyAdvertisementBuilder::build`'s panic on a board.

/// Bytes of AD structures a legacy advertising or scan-response PDU can
/// carry (`BLE_GAP_ADV_SET_DATA_SIZE_MAX`).
pub const LEGACY_AD_CAPACITY: usize = 31;

/// Total size of an AD structure carrying `data_len` bytes: the data
/// plus a length byte and an AD-type byte.
#[must_use]
pub const fn ad_structure_len(data_len: usize) -> usize {
    data_len + 2
}

/// Bluetooth SIG company identifier `0xFFFF`, reserved for internal and
/// interoperability testing.
///
/// The Columba protocol uses it deliberately: neither project holds an
/// assigned identifier, and `0xFFFF` is the value the SIG set aside for
/// exactly this. It is *not* a placeholder to be replaced later — a
/// change here is a wire-format change on both sides.
pub const COMPANY_ID: u16 = 0xFFFF;

/// Version byte of the advertised capability record: BLE protocol
/// v0.3.0, the version that introduced it.
///
/// A peer that does not recognise the version must ignore the record
/// rather than guess at the bits behind it, which is why the version
/// leads the payload instead of trailing it.
pub const PROTOCOL_VERSION: u8 = 0x03;

/// Capability bit 0: this node has no BLE central role, so the v2.2
/// address-sort rule does not apply to it — connect regardless of how
/// the addresses compare.
pub const CAP_PERIPHERAL_ONLY: u8 = 1 << 0;

/// Incoming (peripheral) link slots a board offers: the firmware's
/// `ble::PERIPH_LINKS` (#372), restated here because the record's
/// encoding is bounded by it and the record lives in this crate. The
/// firmware asserts the two agree at compile time.
pub const PERIPH_SLOTS: u8 = 3;

/// Capability bits 1-2: how many incoming link slots the advertiser
/// still has free, 0 to [`PERIPH_SLOTS`] — two bits, exactly the range
/// [`PERIPH_SLOTS`] allows.
pub const CAP_FREE_SLOTS_MASK: u8 = 0b0000_0110;

/// Where [`CAP_FREE_SLOTS_MASK`] sits.
const CAP_FREE_SLOTS_SHIFT: u32 = 1;

/// Capability bit 3: the advertiser filled in [`CAP_FREE_SLOTS_MASK`].
///
/// Without it a pre-#375 record (`caps = 0x01`, bits 1-2 clear) would
/// read as "zero free slots" instead of "no slot information" — see the
/// module docs.
pub const CAP_FREE_SLOTS_VALID: u8 = 1 << 3;

/// Put a free-slot count into a capability byte, leaving every other
/// bit — [`CAP_PERIPHERAL_ONLY`] above all — as it was.
///
/// A count above [`PERIPH_SLOTS`] saturates rather than wrapping into
/// the neighbouring bits: the caller's arithmetic is not this record's
/// business, and a truncated count that silently corrupted bit 3 would
/// be a wire bug.
#[must_use]
pub const fn with_free_slots(caps: u8, free: u8) -> u8 {
    let free = if free > PERIPH_SLOTS {
        PERIPH_SLOTS
    } else {
        free
    };
    (caps & !(CAP_FREE_SLOTS_MASK | CAP_FREE_SLOTS_VALID))
        | CAP_FREE_SLOTS_VALID
        | (free << CAP_FREE_SLOTS_SHIFT)
}

/// Read a free-slot count back out. `None` is "this advertiser said
/// nothing about its slots" — an older board, another implementation,
/// or one that chose not to — and is never "zero slots".
#[must_use]
pub const fn free_slots(caps: u8) -> Option<u8> {
    if caps & CAP_FREE_SLOTS_VALID == 0 {
        None
    } else {
        Some((caps & CAP_FREE_SLOTS_MASK) >> CAP_FREE_SLOTS_SHIFT)
    }
}

/// Length of the manufacturer-data payload: company ID (2, little
/// endian) + version (1) + capability bits (1).
pub const MANUFACTURER_DATA_LEN: usize = 4;

/// The manufacturer-specific data payload advertised with `caps`.
///
/// This is the AD structure's *data*; the length and AD-type bytes in
/// front of it are added by the advertisement builder, so on the wire
/// the structure reads `05 FF FF FF 03 <caps>` — a length field of 5 and
/// [`ad_structure_len`]`(4)` = 6 bytes consumed.
#[must_use]
pub const fn manufacturer_data(caps: u8) -> [u8; MANUFACTURER_DATA_LEN] {
    let [cid_lo, cid_hi] = COMPANY_ID.to_le_bytes();
    [cid_lo, cid_hi, PROTOCOL_VERSION, caps]
}

/// Bytes of the advertising PDU the firmware's three AD structures use.
/// Held against [`LEGACY_AD_CAPACITY`] by the tests below and asserted
/// again at build time in `ble::columba`.
pub const ADV_BYTES_USED: usize =
    ad_structure_len(1) + ad_structure_len(16) + ad_structure_len(MANUFACTURER_DATA_LEN);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEVICE_NAME_LEN;

    #[test]
    fn the_advertisement_fits_the_legacy_pdu_with_room_to_spare() {
        assert_eq!(ADV_BYTES_USED, 27);
        // Both constants: a const block fails the build, not one test run.
        const { assert!(ADV_BYTES_USED <= LEGACY_AD_CAPACITY) };
        assert_eq!(LEGACY_AD_CAPACITY - ADV_BYTES_USED, 4, "bytes left over");
    }

    #[test]
    fn the_capability_record_costs_six_of_them() {
        // The reviewer's "+5" is the AD *length field* (type byte plus
        // four data bytes); the structure on the wire is one byte more.
        let structure = ad_structure_len(MANUFACTURER_DATA_LEN);
        assert_eq!(structure, 6);
        assert_eq!(MANUFACTURER_DATA_LEN + 1, 5, "the length field's value");
        assert_eq!(ADV_BYTES_USED - structure, 21, "adv before this batch");
    }

    #[test]
    fn the_payload_is_company_id_le_then_version_then_caps() {
        assert_eq!(
            manufacturer_data(CAP_PERIPHERAL_ONLY),
            [0xFF, 0xFF, 0x03, 0x01]
        );
    }

    #[test]
    fn clearing_the_flag_leaves_the_record_and_its_size_alone() {
        // Phase B clears bit 0 rather than dropping the record: a peer
        // that sees no manufacturer data at all cannot tell a v0.3.0
        // node that is central-capable from a pre-v0.3.0 node that never
        // spoke the version, and would sort against the wrong rule.
        let central_capable = manufacturer_data(0);
        assert_eq!(central_capable, [0xFF, 0xFF, 0x03, 0x00]);
        assert_eq!(central_capable[3] & CAP_PERIPHERAL_ONLY, 0);
        assert_eq!(central_capable.len(), MANUFACTURER_DATA_LEN);
    }

    #[test]
    fn the_scan_response_carries_the_name_and_does_not_compete() {
        // Both PDUs are 31 bytes and they are separate; the name's cost
        // is charged to the scan response, which is why the adv budget
        // above does not include it.
        let scan_response = ad_structure_len(DEVICE_NAME_LEN);
        assert_eq!(scan_response, 13);
        assert!(scan_response <= LEGACY_AD_CAPACITY);
    }

    #[test]
    fn every_free_slot_count_round_trips_with_the_flag_either_way() {
        // The two live in the same byte and must not touch each other:
        // the count survives the flag, the flag survives the count.
        for free in 0..=PERIPH_SLOTS {
            for base in [0u8, CAP_PERIPHERAL_ONLY] {
                let caps = with_free_slots(base, free);
                assert_eq!(free_slots(caps), Some(free), "count round-trip");
                assert_eq!(
                    caps & CAP_PERIPHERAL_ONLY,
                    base,
                    "the free-slot bits disturbed bit 0"
                );
                // And the payload carries it unchanged, as one byte.
                assert_eq!(manufacturer_data(caps)[3], caps);
            }
        }
    }

    #[test]
    fn a_record_without_the_validity_bit_says_nothing_about_slots() {
        // What a board predating #375 item 3 advertises: bit 0 only.
        assert_eq!(free_slots(CAP_PERIPHERAL_ONLY), None, "not zero slots");
        assert_eq!(free_slots(0), None, "nor is a cleared record");
        // Even with the count bits set: without bit 3 they are not ours
        // to read (a future implementation may mean something else by
        // them), so the answer is still "no information".
        assert_eq!(free_slots(CAP_FREE_SLOTS_MASK), None);
        // Zero free slots is a statement, and a distinct one.
        assert_eq!(free_slots(with_free_slots(0, 0)), Some(0));
    }

    #[test]
    fn an_over_large_count_saturates_instead_of_corrupting_its_neighbours() {
        let caps = with_free_slots(CAP_PERIPHERAL_ONLY, 7);
        assert_eq!(free_slots(caps), Some(PERIPH_SLOTS));
        assert_eq!(caps & CAP_PERIPHERAL_ONLY, CAP_PERIPHERAL_ONLY);
        assert_eq!(caps & 0xF0, 0, "bits 4-7 are still free");
    }

    #[test]
    fn the_record_uses_four_of_the_eight_capability_bits() {
        // The budget the module docs state, held here so it cannot rot:
        // bit 0 the flag, 1-2 the count, 3 its validity, 4-7 unclaimed.
        let claimed = CAP_PERIPHERAL_ONLY | CAP_FREE_SLOTS_MASK | CAP_FREE_SLOTS_VALID;
        assert_eq!(claimed, 0b0000_1111);
        assert_eq!(claimed.count_zeros(), 4, "bits left for the next batch");
        // Two bits is exactly the range PERIPH_SLOTS needs.
        assert_eq!(CAP_FREE_SLOTS_MASK >> CAP_FREE_SLOTS_SHIFT, PERIPH_SLOTS);
    }

    #[test]
    fn every_capability_byte_round_trips_into_the_payload() {
        for caps in 0..=255u8 {
            let data = manufacturer_data(caps);
            assert_eq!(u16::from_le_bytes([data[0], data[1]]), COMPANY_ID);
            assert_eq!(data[2], PROTOCOL_VERSION);
            assert_eq!(data[3], caps);
        }
    }
}
