//! Persistent storage for a board's remote-management allow-list
//! (Codeberg #235/#238).
//!
//! A standalone LNode can serve `rnstransport.remote.management` with the
//! `/status` handler, so `rnstatus -R` and `lnstatus -R` read a board the
//! same way they read a daemon. Who may ask is an identity allow-list, and
//! on a board that list is **only** writable over USB, through the #238
//! control envelope ([`crate::envelope::TYPE_MGMT_ALLOW`]). Nothing that
//! arrives over LoRa or BLE can reach it: the envelope is classified on
//! the transport CDC read path alone (`leviculum_nrf::usb`), and the same
//! bytes handed to the node from a radio interface are dropped in packet
//! parsing — the first byte `0xA4` has the IFAC bit set, and no radio
//! carrier here runs IFAC.
//!
//! Stored form: the same magic + version + checksum envelope the
//! telemetry-target, fixed-position, media-profile, node-name and
//! propagation-config stores use, so a blank (erased, all-`0xFF`) or
//! corrupt region decodes to `None`. The record shares the telemetry
//! flash page with those five (the firmware names the layout in
//! `leviculum_nrf::telemetry`); this module only fixes the record's bytes.
//!
//! Layout (136 bytes, already 4-byte aligned for word-wide flash writes):
//!
//! ```text
//!   0..4    magic "LMGA"
//!   4       format version (0x01)
//!   5       entry count (0..=MGMT_ALLOW_MAX_IDENTITIES), never inferred
//!   6..134  entries, 8 × TRUNCATED_HASHBYTES, zero-filled past the count
//! 134..136  checksum over bytes 0..134
//! ```
//!
//! An explicitly cleared list is stored as a valid record with count 0
//! rather than as an erased region, for the same reason the fixed position
//! is: the store task rewrites the whole record on every save, and
//! "cleared by the operator" and "never set" mean the same thing on the
//! next boot — no remote management at all.
//!
//! # `None` is not "everybody"
//!
//! On the daemon, an empty `remote_management_allowed` list still
//! registers the handler and is then consulted per request
//! (`leviculum-std/src/config.rs:88-96` carries Python's names and
//! Python's semantics). **That semantics is deliberately not copied
//! here.** A daemon runs on a machine with an operator, a config file and
//! a login; a board is handed to somebody and left on a mast. An
//! unattended board that registers a management destination with an empty
//! list is one edit of that list away from answering anybody, and #235
//! forbids exactly that. So on a board an empty or absent list registers
//! **nothing**: no destination, no handler, no announce.
//!
//! [`remote_mgmt_decision`] is that rule as a pure function, because the
//! firmware crate runs no test of its own (41d545d5) and a decision that
//! cannot be exercised on a host is a decision nobody has checked.

extern crate alloc;

use alloc::vec::Vec;

use crate::constants::TRUNCATED_HASHBYTES;

const MAGIC: [u8; 4] = [0x4C, 0x4D, 0x47, 0x41]; // "LMGA"
const FORMAT_VERSION: u8 = 0x01;
const COUNT_OFFSET: usize = 5;
const ENTRIES_OFFSET: usize = 6;

/// How many identities a board's allow-list may hold.
///
/// Eight, and the number is bounded from three sides:
///
/// * **What it is for.** These are the board's *operators*, not its
///   clients — the stations allowed to read its interface counters. A
///   deployment with more than eight management stations for one mast
///   node has a monitoring daemon, and that daemon is one identity.
/// * **Flash.** The record is [`ENCODED_SIZE`] = 136 bytes, inside the
///   256-byte slot spacing of the telemetry page's layout
///   (`leviculum_nrf::telemetry`, whose compile-time assertion is what
///   keeps that true if this bound grows).
/// * **Heap (#388).** The list is copied into the `/status` handler's
///   `RequestPolicy::AllowList` vector: 8 × 16 = 128 bytes of entries
///   plus the vector header, ~152 bytes, against the 6144-byte
///   fragmentation reserve of the boot heap budget
///   (`leviculum_nrf::heap_census::FRAG_RESERVE_BYTES`) — 2.5 % of one
///   term, so no budget term moves and the boot assertion is unaffected.
pub const MGMT_ALLOW_MAX_IDENTITIES: usize = 8;

/// Total encoded size: 6 header + 8 × 16 entries + 2 checksum = 136.
pub const ENCODED_SIZE: usize =
    ENTRIES_OFFSET + MGMT_ALLOW_MAX_IDENTITIES * TRUNCATED_HASHBYTES + CHECKSUM_SIZE;

const CHECKSUM_SIZE: usize = 2;
const CHECKSUM_OFFSET: usize = ENTRIES_OFFSET + MGMT_ALLOW_MAX_IDENTITIES * TRUNCATED_HASHBYTES;

/// Encoded size rounded up to 4-byte alignment (flash writes are
/// word-wide): 136, already aligned.
pub const ENCODED_SIZE_ALIGNED: usize = ENCODED_SIZE.div_ceil(4) * 4;

/// The persisted allow-list: the identity hashes permitted to query
/// `rnstransport.remote.management` `/status` on this board.
///
/// A fixed array plus a count rather than a `Vec`, so the record's bytes
/// and the in-memory form have the same shape and a decode allocates
/// nothing — the board decodes this before the heap has anything else in
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredMgmtAllow {
    entries: [[u8; TRUNCATED_HASHBYTES]; MGMT_ALLOW_MAX_IDENTITIES],
    count: u8,
}

impl StoredMgmtAllow {
    /// The empty list — which on a board means remote management off, not
    /// "open to everybody" (module docs).
    pub const EMPTY: Self = Self {
        entries: [[0u8; TRUNCATED_HASHBYTES]; MGMT_ALLOW_MAX_IDENTITIES],
        count: 0,
    };

    /// Build a list from identity hashes, refusing more than
    /// [`MGMT_ALLOW_MAX_IDENTITIES`].
    ///
    /// Duplicates are dropped: the same identity twice is the same
    /// permission, and keeping it twice would spend a slot an operator
    /// meant for a second station. The order of first appearance is
    /// preserved, so what a host sent is what a report reads back.
    pub fn from_hashes(hashes: &[[u8; TRUNCATED_HASHBYTES]]) -> Option<Self> {
        let mut list = Self::EMPTY;
        for hash in hashes {
            if list.contains(hash) {
                continue;
            }
            if list.count as usize >= MGMT_ALLOW_MAX_IDENTITIES {
                return None;
            }
            list.entries[list.count as usize] = *hash;
            list.count += 1;
        }
        Some(list)
    }

    /// The identities on the list, in the order they were set.
    pub fn hashes(&self) -> &[[u8; TRUNCATED_HASHBYTES]] {
        &self.entries[..self.count as usize]
    }

    /// How many identities are on the list.
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the list permits nobody. On a board that is remote
    /// management off — see [`remote_mgmt_decision`].
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether `hash` is on the list.
    pub fn contains(&self, hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.hashes().iter().any(|entry| entry == hash)
    }
}

/// Same two-byte XOR checksum every record on the page uses:
/// even-indexed bytes into `a`, odd into `b`.
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

/// Encode the allow-list — including the explicit clear
/// ([`StoredMgmtAllow::EMPTY`]) — into a fixed-size buffer for
/// persistent storage.
pub fn encode_mgmt_allow(list: &StoredMgmtAllow) -> [u8; ENCODED_SIZE_ALIGNED] {
    let mut buf = [0u8; ENCODED_SIZE_ALIGNED];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4] = FORMAT_VERSION;
    buf[COUNT_OFFSET] = list.count;
    for (i, hash) in list.hashes().iter().enumerate() {
        let start = ENTRIES_OFFSET + i * TRUNCATED_HASHBYTES;
        buf[start..start + TRUNCATED_HASHBYTES].copy_from_slice(hash);
    }
    let cs = checksum(&buf[..CHECKSUM_OFFSET]);
    buf[CHECKSUM_OFFSET] = cs[0];
    buf[CHECKSUM_OFFSET + 1] = cs[1];
    buf
}

/// Decode the allow-list from a persistent storage buffer.
///
/// `None` for a blank region (erased flash reads as `0xFF`), a wrong
/// magic or version, a count past [`MGMT_ALLOW_MAX_IDENTITIES`], or a
/// checksum mismatch. Every one of those means "no stored list", and on a
/// board that means remote management off — which is today's behaviour and
/// the safe direction: a torn write can cost an operator their remote
/// status, it can never hand it to somebody else.
///
/// A valid record with count 0 decodes to [`StoredMgmtAllow::EMPTY`],
/// *not* to `None`. The two are the same decision
/// ([`remote_mgmt_decision`]) and are kept distinct anyway, so a host
/// reading the record back can tell "an operator cleared this" from
/// "nothing was ever written".
pub fn decode_mgmt_allow(buf: &[u8]) -> Option<StoredMgmtAllow> {
    if buf.len() < ENCODED_SIZE {
        return None;
    }
    if buf[0] == 0xFF {
        return None; // erased flash
    }
    if buf[0..4] != MAGIC {
        return None;
    }
    if buf[4] != FORMAT_VERSION {
        return None;
    }
    let stored = [buf[CHECKSUM_OFFSET], buf[CHECKSUM_OFFSET + 1]];
    if stored != checksum(&buf[..CHECKSUM_OFFSET]) {
        return None;
    }
    let count = buf[COUNT_OFFSET];
    if count as usize > MGMT_ALLOW_MAX_IDENTITIES {
        return None;
    }
    let mut list = StoredMgmtAllow::EMPTY;
    list.count = count;
    for i in 0..count as usize {
        let start = ENTRIES_OFFSET + i * TRUNCATED_HASHBYTES;
        list.entries[i].copy_from_slice(&buf[start..start + TRUNCATED_HASHBYTES]);
    }
    Some(list)
}

/// What a board does about remote management, given what its allow-list
/// record decoded to.
///
/// The whole of the board's deviation from the daemon's Python semantics,
/// in one pure function so a host test can state it (41d545d5): an absent
/// **or empty** list registers nothing at all. The daemon registers the
/// handler and consults an empty list per request; a board must not,
/// because a board is unattended and a registered management destination
/// with nobody on the list is an announce advertising a door.
///
/// `Enabled` carries the vector `NodeCoreBuilder::remote_management`
/// wants, so the caller neither re-derives the list nor decides anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMgmtDecision {
    /// Register no destination, no handler and no announce.
    Disabled,
    /// Register `rnstransport.remote.management` with this allow-list,
    /// which is guaranteed non-empty.
    Enabled(Vec<[u8; TRUNCATED_HASHBYTES]>),
}

/// Apply the rule above to a decoded record. See [`RemoteMgmtDecision`].
pub fn remote_mgmt_decision(stored: Option<&StoredMgmtAllow>) -> RemoteMgmtDecision {
    match stored {
        Some(list) if !list.is_empty() => RemoteMgmtDecision::Enabled(list.hashes().to_vec()),
        _ => RemoteMgmtDecision::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn hash(seed: u8) -> [u8; TRUNCATED_HASHBYTES] {
        [seed; TRUNCATED_HASHBYTES]
    }

    #[test]
    fn a_list_round_trips() {
        for n in 0..=MGMT_ALLOW_MAX_IDENTITIES {
            let hashes: Vec<_> = (0..n).map(|i| hash(i as u8 + 1)).collect();
            let list = StoredMgmtAllow::from_hashes(&hashes).expect("n is inside the bound");
            assert_eq!(list.len(), n);
            let decoded = decode_mgmt_allow(&encode_mgmt_allow(&list));
            assert_eq!(decoded, Some(list), "n={n}");
            assert_eq!(decoded.unwrap().hashes(), hashes.as_slice(), "n={n}");
        }
    }

    #[test]
    fn a_blank_or_zeroed_region_is_no_record() {
        assert!(decode_mgmt_allow(&[0xFF; ENCODED_SIZE_ALIGNED]).is_none());
        assert!(decode_mgmt_allow(&[0x00; ENCODED_SIZE_ALIGNED]).is_none());
    }

    #[test]
    fn a_short_buffer_is_no_record() {
        let encoded = encode_mgmt_allow(&StoredMgmtAllow::from_hashes(&[hash(7)]).unwrap());
        assert!(decode_mgmt_allow(&encoded[..ENCODED_SIZE - 1]).is_none());
    }

    #[test]
    fn a_bit_flip_anywhere_is_no_record() {
        // Every byte, not a sample: a torn write that landed inside an
        // entry must not become a list with one wrong identity on it.
        let list = StoredMgmtAllow::from_hashes(&[hash(1), hash(2), hash(3)]).unwrap();
        for i in 0..ENCODED_SIZE {
            let mut buf = encode_mgmt_allow(&list);
            buf[i] ^= 0x01;
            assert!(
                decode_mgmt_allow(&buf).is_none(),
                "bit flip at byte {i} was accepted"
            );
        }
    }

    #[test]
    fn a_count_past_the_bound_is_no_record() {
        // A record whose count claims more entries than the layout holds
        // would read past its own checksum. Re-checksummed so it is the
        // count alone that refuses it.
        let mut buf = encode_mgmt_allow(&StoredMgmtAllow::EMPTY);
        buf[COUNT_OFFSET] = MGMT_ALLOW_MAX_IDENTITIES as u8 + 1;
        let cs = checksum(&buf[..CHECKSUM_OFFSET]);
        buf[CHECKSUM_OFFSET] = cs[0];
        buf[CHECKSUM_OFFSET + 1] = cs[1];
        assert!(decode_mgmt_allow(&buf).is_none());
    }

    #[test]
    fn a_future_format_version_is_no_record() {
        let mut buf = encode_mgmt_allow(&StoredMgmtAllow::EMPTY);
        buf[4] = FORMAT_VERSION + 1;
        let cs = checksum(&buf[..CHECKSUM_OFFSET]);
        buf[CHECKSUM_OFFSET] = cs[0];
        buf[CHECKSUM_OFFSET + 1] = cs[1];
        assert!(decode_mgmt_allow(&buf).is_none());
    }

    #[test]
    fn the_explicit_clear_is_a_record_and_not_an_erased_region() {
        // Both decide the same thing, and both must still be readable as
        // themselves: an operator who cleared the list wants to see that.
        let cleared = decode_mgmt_allow(&encode_mgmt_allow(&StoredMgmtAllow::EMPTY));
        assert_eq!(cleared, Some(StoredMgmtAllow::EMPTY));
        assert!(cleared.unwrap().is_empty());
        assert!(decode_mgmt_allow(&[0xFF; ENCODED_SIZE_ALIGNED]).is_none());
    }

    #[test]
    fn one_identity_past_the_bound_is_refused_rather_than_truncated() {
        let hashes: Vec<_> = (0..=MGMT_ALLOW_MAX_IDENTITIES)
            .map(|i| hash(i as u8 + 1))
            .collect();
        assert!(StoredMgmtAllow::from_hashes(&hashes).is_none());
    }

    #[test]
    fn a_repeated_identity_does_not_spend_a_second_slot() {
        let list = StoredMgmtAllow::from_hashes(&[hash(1), hash(1), hash(2)]).unwrap();
        assert_eq!(list.hashes(), &[hash(1), hash(2)]);
        // And the bound counts distinct identities, so a host that sent
        // the same hash nine times is not refused for it.
        let repeated = vec![hash(3); MGMT_ALLOW_MAX_IDENTITIES + 1];
        assert_eq!(
            StoredMgmtAllow::from_hashes(&repeated).map(|l| l.len()),
            Some(1)
        );
    }

    #[test]
    fn an_absent_record_registers_nothing() {
        // The board's whole deviation from the daemon: no record is not
        // "no restriction", it is no remote management.
        assert_eq!(remote_mgmt_decision(None), RemoteMgmtDecision::Disabled);
    }

    #[test]
    fn an_empty_list_registers_nothing_either() {
        // Where the daemon registers the handler and consults an empty
        // list per request (`leviculum-std/src/config.rs:88-96`), a board
        // registers no destination at all.
        assert_eq!(
            remote_mgmt_decision(Some(&StoredMgmtAllow::EMPTY)),
            RemoteMgmtDecision::Disabled
        );
    }

    #[test]
    fn a_non_empty_list_is_handed_over_verbatim() {
        let list = StoredMgmtAllow::from_hashes(&[hash(9), hash(4)]).unwrap();
        assert_eq!(
            remote_mgmt_decision(Some(&list)),
            RemoteMgmtDecision::Enabled(vec![hash(9), hash(4)])
        );
    }

    #[test]
    fn the_encoded_size_fits_the_pages_slot_spacing() {
        // The page layout in `leviculum_nrf::telemetry` spaces records
        // 0x100 apart and asserts it at compile time; this is the same
        // number stated where the record is defined.
        assert_eq!(ENCODED_SIZE, 136);
        assert_eq!(ENCODED_SIZE_ALIGNED, 136);
        // A const block, so growing the bound past the slot fails the
        // BUILD rather than a test run — the same discipline the firmware's
        // page-layout assertion uses.
        const SLOT_SPACING: usize = 0x100;
        const _: () = assert!(ENCODED_SIZE_ALIGNED <= SLOT_SPACING);
    }
}
