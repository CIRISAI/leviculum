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
//! bit 3 its validity, bit 4 the identity hint below, **three bits
//! (5-7) still free**.
//!
//! # The identity hint (#412)
//!
//! The advertisement used to carry nothing about WHO was advertising,
//! so a peer that rotates its address was indistinguishable from a new
//! one and every rotation cost a dial — on the corpus night of
//! 2026-09-14/15 one board spent all seven outgoing links it made on a
//! single rotating phone and never dialled a neighbour board (#412).
//! The post-connect Identity characteristic catches the duplicate, but
//! only after the scarce central slot is already spent.
//!
//! Capability bit 4 ([`CAP_IDENTITY_HINT`]) says the record carries the
//! first [`IDENTITY_HINT_LEN`] bytes of the advertiser's identity hash
//! in bytes 4..8 — exactly the value every log line already prints as
//! `peer=`. [`identity_hint`] derives it,
//! [`manufacturer_data_with_hint`] writes it, and
//! [`crate::peer::PeerAdvertisement::identity_hint`] reads it back.
//!
//! Its reach is the record's reach, and that is our own two stacks. An
//! Android Columba advertises the service UUID alone and carries no
//! capability record at all (6674ae87), so the rotating phone the #412
//! capture is made of yields no hint and is dialled exactly as before.
//! What this closes is a peer of OURS reappearing under a new address;
//! closing the other half needs Columba to carry the hint too, or the
//! dial ledger and role preference #412's design comment proposes.
//!
//! It is a HINT, like the free-slot count, and the same rules bind it.
//! Four bytes collide one time in 2^32, negligible at our node counts,
//! and the only cost of a false match is a skipped dial: the
//! post-connect identity check stays the authority, and nothing durable
//! — no link, no peer record, no route — may be keyed on the hint.
//!
//! Taking bit 4 needs no [`PROTOCOL_VERSION`] bump, for the same two
//! reasons the slot hint did not: Columba never reads the record, and
//! our own parser tests `payload.len() >=` the base length and reads
//! `caps` at a fixed offset, so an older board sees a longer record,
//! takes the `caps` byte it knows and ignores the tail.
//!
//! ## Exposure, decided (#412)
//!
//! The hint makes a node passively identifiable by four bytes to
//! anyone listening, and there is deliberately **no opt-out flag**.
//! That is not new information: the scan response already carries the
//! `LN-<hex8>` device name ([`crate::device_name`]), which is the same
//! identity prefix in ASCII, and every LoRa announce carries the full
//! identity hash in the clear. A flag would therefore buy no privacy
//! while adding a configuration surface and a second code path — so a
//! later reader who wants this reopened has to change the device name
//! and the announce first, not this record.
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
//! | Manufacturer specific data     |  8   | 10    |
//! | **sum**                        |      | **31** |
//!
//! That is the whole PDU: since the identity hint [`ADV_BYTES_USED`]
//! EQUALS [`LEGACY_AD_CAPACITY`], and the test below asserts that
//! equality rather than headroom, so that the day someone spends a
//! byte that is not there the host says so.
//! **The next AD structure does not fit**: it
//! belongs in the scan response — where only the device name lives
//! today, 13 of its own 31 bytes — or the advertisement has to move to
//! extended advertising. Growing this record again is not an option.
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

/// Capability bit 4: bytes 4..8 of the record carry an identity hint
/// (#412) — see the module docs.
///
/// Set by [`manufacturer_data_with_hint`] and by nothing else, so a
/// record can never claim a hint it does not carry. A reader must test
/// this bit AND the record's length before reading the tail: an
/// implementation that set the bit on a four-byte record would
/// otherwise read whatever the next AD structure begins with.
pub const CAP_IDENTITY_HINT: u8 = 1 << 4;

/// Bytes of the identity hash the hint carries: the first four, the
/// prefix every log line already prints as `peer=`.
///
/// Four is what the advertising PDU had left (see the byte budget) and
/// it is enough: one collision in 2^32 at node counts in the tens, and
/// a collision costs one skipped dial, never a wrong link — the
/// post-connect identity check is still the authority.
pub const IDENTITY_HINT_LEN: usize = 4;

/// Length of the manufacturer-data payload WITHOUT the identity hint:
/// company ID (2, little endian) + version (1) + capability bits (1).
///
/// This is the record's floor, not its size: it is what a reader must
/// require before it may read `caps`, which is why the parser tests
/// `payload.len() >= MANUFACTURER_DATA_LEN` rather than `==`. A board
/// that predates #412 advertises exactly this many bytes and is read
/// by current firmware unchanged.
pub const MANUFACTURER_DATA_LEN: usize = 4;

/// Length of the manufacturer-data payload WITH the identity hint —
/// what this node advertises since #412.
pub const MANUFACTURER_DATA_HINT_LEN: usize = MANUFACTURER_DATA_LEN + IDENTITY_HINT_LEN;

/// The manufacturer-specific data payload advertised with `caps`, with
/// no identity hint.
///
/// This is the AD structure's *data*; the length and AD-type bytes in
/// front of it are added by the advertisement builder, so on the wire
/// the structure reads `05 FF FF FF 03 <caps>` — a length field of 5 and
/// [`ad_structure_len`]`(4)` = 6 bytes consumed.
///
/// Since #412 no advertiser of ours emits this form — both stacks build
/// [`manufacturer_data_with_hint`] — but it stays the base the hinted
/// record is defined against, and the tests use it for what a pre-#412
/// board puts on the air.
#[must_use]
pub const fn manufacturer_data(caps: u8) -> [u8; MANUFACTURER_DATA_LEN] {
    let [cid_lo, cid_hi] = COMPANY_ID.to_le_bytes();
    [cid_lo, cid_hi, PROTOCOL_VERSION, caps]
}

/// The first [`IDENTITY_HINT_LEN`] bytes of an identity hash: the value
/// that goes on the air (#412) and the value a reader compares a live
/// link's identity against.
///
/// One function for both ends, so "which four bytes" is stated once.
#[must_use]
pub const fn identity_hint(identity_hash: &[u8; 16]) -> [u8; IDENTITY_HINT_LEN] {
    [
        identity_hash[0],
        identity_hash[1],
        identity_hash[2],
        identity_hash[3],
    ]
}

/// The manufacturer-specific data payload advertised with `caps` and an
/// identity hint (#412).
///
/// [`CAP_IDENTITY_HINT`] is set HERE rather than by the caller: the bit
/// and the four bytes behind it are one fact, and a caller that could
/// set one without the other would put a record on the air that lies
/// about its own length. On the wire the structure reads
/// `09 FF FF FF 03 <caps> <h0> <h1> <h2> <h3>` — a length field of 9
/// and [`ad_structure_len`]`(8)` = 10 bytes consumed.
#[must_use]
pub const fn manufacturer_data_with_hint(
    caps: u8,
    hint: &[u8; IDENTITY_HINT_LEN],
) -> [u8; MANUFACTURER_DATA_HINT_LEN] {
    let [cid_lo, cid_hi] = COMPANY_ID.to_le_bytes();
    [
        cid_lo,
        cid_hi,
        PROTOCOL_VERSION,
        caps | CAP_IDENTITY_HINT,
        hint[0],
        hint[1],
        hint[2],
        hint[3],
    ]
}

/// Bytes of the advertising PDU the firmware's three AD structures use.
/// Held against [`LEGACY_AD_CAPACITY`] by the tests below and asserted
/// again at build time in `ble::columba`.
///
/// Since #412 this is the capacity exactly, not a fraction of it: the
/// manufacturer record carries the identity hint, so the budget is
/// spent and the next AD structure has to go in the scan response or
/// wait for extended advertising.
pub const ADV_BYTES_USED: usize =
    ad_structure_len(1) + ad_structure_len(16) + ad_structure_len(MANUFACTURER_DATA_HINT_LEN);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEVICE_NAME_LEN;

    #[test]
    fn the_advertisement_fills_the_legacy_pdu_to_the_last_byte() {
        assert_eq!(ADV_BYTES_USED, 31);
        // Both constants: a const block fails the build, not one test run.
        const { assert!(ADV_BYTES_USED <= LEGACY_AD_CAPACITY) };
        // Equality, not headroom: #412's identity hint spent the four
        // bytes that were left. The NEXT field goes in the scan
        // response (13 of its 31 bytes used) or needs extended
        // advertising — it cannot go here.
        assert_eq!(
            ADV_BYTES_USED, LEGACY_AD_CAPACITY,
            "the advertising PDU is full"
        );
    }

    #[test]
    fn the_capability_record_costs_ten_of_them() {
        // The AD *length field* is the type byte plus the data bytes;
        // the structure on the wire is one byte more.
        let structure = ad_structure_len(MANUFACTURER_DATA_HINT_LEN);
        assert_eq!(structure, 10);
        assert_eq!(
            MANUFACTURER_DATA_HINT_LEN + 1,
            9,
            "the length field's value"
        );
        assert_eq!(ADV_BYTES_USED - structure, 21, "the two fixed structures");
        // What it cost before the hint, and what the four bytes bought.
        assert_eq!(ad_structure_len(MANUFACTURER_DATA_LEN), 6);
        assert_eq!(structure - ad_structure_len(MANUFACTURER_DATA_LEN), 4);
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
    fn the_record_uses_five_of_the_eight_capability_bits() {
        // The budget the module docs state, held here so it cannot rot:
        // bit 0 the flag, 1-2 the count, 3 its validity, 4 the identity
        // hint, 5-7 unclaimed.
        let claimed =
            CAP_PERIPHERAL_ONLY | CAP_FREE_SLOTS_MASK | CAP_FREE_SLOTS_VALID | CAP_IDENTITY_HINT;
        assert_eq!(claimed, 0b0001_1111);
        assert_eq!(claimed.count_zeros(), 3, "bits left for the next batch");
        // Two bits is exactly the range PERIPH_SLOTS needs.
        assert_eq!(CAP_FREE_SLOTS_MASK >> CAP_FREE_SLOTS_SHIFT, PERIPH_SLOTS);
        // And the hint bit is disjoint from every bit before it, so
        // setting it cannot be read as a slot count or as the flag.
        assert_eq!(
            CAP_IDENTITY_HINT & (CAP_PERIPHERAL_ONLY | CAP_FREE_SLOTS_MASK | CAP_FREE_SLOTS_VALID),
            0
        );
    }

    /// Everything the hinted record has to be, in one place: the layout,
    /// the bit that announces it, and the fact that neither half
    /// disturbs what was already in the byte.
    #[test]
    fn the_hinted_record_is_the_old_one_plus_four_bytes_and_a_bit() {
        const ID: [u8; 16] = [
            0xb2, 0xa8, 0xbe, 0xa1, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
            0xbb, 0xcc,
        ];
        let hint = identity_hint(&ID);
        assert_eq!(hint, [0xb2, 0xa8, 0xbe, 0xa1], "the log lines' peer= value");

        let record = manufacturer_data_with_hint(CAP_PERIPHERAL_ONLY, &hint);
        assert_eq!(
            record,
            [
                0xFF,
                0xFF,
                0x03,
                0x01 | CAP_IDENTITY_HINT,
                0xb2,
                0xa8,
                0xbe,
                0xa1
            ]
        );
        assert_eq!(record.len(), MANUFACTURER_DATA_HINT_LEN);
        // The first four bytes are byte-for-byte the un-hinted record
        // with one more bit set: an older reader takes exactly these
        // and ignores the rest.
        assert_eq!(
            record[..MANUFACTURER_DATA_LEN],
            manufacturer_data(CAP_PERIPHERAL_ONLY | CAP_IDENTITY_HINT)
        );
        assert_eq!(&record[MANUFACTURER_DATA_LEN..], &hint);
    }

    #[test]
    fn the_hint_bit_is_never_the_callers_to_set_and_never_disturbs_the_rest() {
        // The builder owns the bit, so a record claiming a hint it does
        // not carry cannot be constructed through this module.
        for caps in 0..=255u8 {
            let record = manufacturer_data_with_hint(caps, &[0; IDENTITY_HINT_LEN]);
            assert_ne!(record[3] & CAP_IDENTITY_HINT, 0, "the bit is always set");
            assert_eq!(
                record[3] & !CAP_IDENTITY_HINT,
                caps & !CAP_IDENTITY_HINT,
                "no other bit moved"
            );
            // Free slots and the flag survive it, both directions.
            assert_eq!(free_slots(record[3]), free_slots(caps));
            assert_eq!(record[3] & CAP_PERIPHERAL_ONLY, caps & CAP_PERIPHERAL_ONLY);
        }
    }

    #[test]
    fn every_hint_value_rides_the_record_unchanged() {
        // Including the bytes a naive framing would trip over: a zero
        // hint, an all-ones hint, and one that looks like a length byte.
        for hint in [
            [0x00, 0x00, 0x00, 0x00],
            [0xFF, 0xFF, 0xFF, 0xFF],
            [0x05, 0xFF, 0xFF, 0xFF],
            [0xde, 0xad, 0xbe, 0xef],
        ] {
            let record = manufacturer_data_with_hint(with_free_slots(0, 2), &hint);
            assert_eq!(&record[MANUFACTURER_DATA_LEN..], &hint);
            assert_eq!(free_slots(record[3]), Some(2));
        }
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
