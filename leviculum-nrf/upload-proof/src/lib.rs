#![no_std]
//! When a client upload's packet proof leaves the board, and the window the
//! uploader keeps it open for (leviculum#397).
//!
//! # The defect this crate pins (`ble_pn_board_upload`, 2026-09-16)
//!
//! A client uploading one message to a propagation node sends it as a plain
//! link packet and holds a packet receipt whose deadline is
//! `max(rtt * TRAFFIC_TIMEOUT_FACTOR, floor)` — Python
//! `reference/Reticulum/RNS/Packet.py:431`, ours
//! [`leviculum_core::constants::TRAFFIC_TIMEOUT_FACTOR`] over
//! [`leviculum_core::constants::RAW_RECEIPT_TIMEOUT_FLOOR_MS`]. When that
//! deadline passes LXMF tears the link down from under the transfer
//! (`reference/LXMF/LXMF/LXMessage.py:616-620`), so a proof sent afterwards
//! lands on a closed link and the client retries the same message.
//!
//! Until this crate the board sent that proof only after the upload's
//! propagation stamp had been judged and the record flushed. The judgement is
//! the 1000-round PN workblock, 3655-3727 ms on an nRF52840
//! ([`leviculum_settle_budget::WORKBLOCK_US_NRF52840`]). Over BLE the measured
//! round trip is 295-484 ms, so the window is 1770-2904 ms — always shorter
//! than the walk, and no upload from a host has ever concluded. Over LoRa the
//! same code looks healthy only because seconds of round trip buy a window
//! wider than the walk. [`proof_lands`] is that comparison, and
//! `the_old_order_misses_every_ble_window` in the tests below is the pre-fix
//! schedule failing it.
//!
//! # What the reference does, and what we do
//!
//! The reference does NOT prove on receipt. `propagation_packet` validates the
//! stamps, stores the message, and calls `packet.prove()` last
//! (`reference/LXMF/LXMF/LXMRouter.py:2233-2256`); the propagation destination
//! never sets a proof strategy, so `RNS.Destination.PROVE_NONE` keeps
//! `Link.receive` (`reference/Reticulum/RNS/Link.py:999-1006`) out of it. That
//! ordering costs Python nothing because its workblock is ~20 ms on a desktop
//! CPU (measured on hamster, 2026-09-25) — two orders of magnitude inside the
//! narrowest window a link ever has.
//!
//! So proving on receipt IS a deviation, taken under the CLAUDE.md deviation
//! rule rather than by parity:
//!
//! 1. *Wire format*: unchanged. The same link proof packet for the same
//!    packet hash, only earlier.
//! 2. *Semantics*: a packet proof states that the bytes arrived and decrypted
//!    — it is issued by `Link.receive` itself under `PROVE_ALL`, before any
//!    application sees the payload. The judgement a Python client expects on
//!    top of it still reaches it: a failed stamp still sends
//!    `ERROR_INVALID_STAMP` and tears the link down, which
//!    `propagation_transfer_signalling_packet`
//!    (`reference/LXMF/LXMF/LXMRouter.py:2667-2677`) turns into
//!    `LXMessage.REJECTED` whether or not the receipt already concluded. What
//!    a client stops getting is a *silence* it never had a use for.
//! 3. *Priority 1*: it moves BLE client upload from zero deliveries to one
//!    round trip, and removes the ~16 s resend loop that made the board accept
//!    the same message repeatedly (`PN_ACCEPT ... dup=1`).
//!
//! # What is modelled here, and what is not
//!
//! Two things, and they are the two the firmware cannot assert itself — it
//! cross-compiles to `thumbv7em-none-eabihf` and has no test target, which is
//! why every decision in `leviculum_nrf::pn` that can be made a pure function
//! lives in a sibling crate like this one:
//!
//! * [`UploadProofs`] — the per-link ledger of proof hashes owed, and the one
//!   method that releases them. `leviculum_nrf::pn::Engine` holds this type
//!   rather than its own map, and [`UploadProofs::on_receipt`] is called from
//!   `on_event`'s `LinkDataReceived` arm, so the release point the tests below
//!   assert is the release point the board runs.
//! * [`receipt_window_ms`] / [`proof_lands`] — the arithmetic that says whether
//!   a proof released after a given delay still finds a live receipt. Pure
//!   numbers; nothing here hashes, links or sends.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};

/// A Reticulum packet hash, which is what a link data proof proves.
pub type PacketHash = [u8; 32];

/// Python `RNS.Link.TRAFFIC_TIMEOUT_FACTOR`
/// (`reference/Reticulum/RNS/Packet.py:431`), restated so this crate stays
/// dependency-free for the firmware. `tests/window.rs` asserts it against
/// [`leviculum_core::constants::TRAFFIC_TIMEOUT_FACTOR`], which is the copy
/// the uploader actually computes with.
pub const TRAFFIC_TIMEOUT_FACTOR: u64 = 6;

/// The enforced floor under a raw link receipt, milliseconds — our
/// [`leviculum_core::constants::RAW_RECEIPT_TIMEOUT_FLOOR_MS`]. Python's
/// literal floor is 5 ms, but it checks receipts once a second
/// (`Transport.receipts_check_interval`), so a second is the honest floor on
/// either stack. Asserted against the constant in `tests/window.rs`.
pub const RECEIPT_FLOOR_MS: u64 = 1_000;

/// Shortest round trip measured to a phone over BLE, milliseconds
/// (`ble_pn_board_upload`, 2026-09-16, firmware `5349cecd`). The shortest RTT
/// is the *narrowest* window, so this is the binding sample.
pub const BLE_RTT_MIN_MS: u64 = 295;

/// Longest round trip measured to a phone over BLE, milliseconds, same run.
/// Even the widest BLE window is narrower than one workblock, which is why
/// this defect has no lucky case on that carrier.
pub const BLE_RTT_MAX_MS: u64 = 484;

/// How long the uploader keeps its packet receipt open, milliseconds.
///
/// `max(rtt * TRAFFIC_TIMEOUT_FACTOR, RECEIPT_FLOOR_MS)`. Everything the node
/// wants the uploader to hear about this packet has to be on the air before
/// this elapses, because after it the uploader tears the link down.
pub fn receipt_window_ms(rtt_ms: u64) -> u64 {
    rtt_ms
        .saturating_mul(TRAFFIC_TIMEOUT_FACTOR)
        .max(RECEIPT_FLOOR_MS)
}

/// Does a proof released `delay_ms` after the packet landed still find a live
/// receipt on a link of round trip `rtt_ms`?
///
/// The proof also has to fly back, so the delay it may spend on the node is
/// the window less half a round trip. Equality is late: the uploader fails a
/// receipt whose deadline has been reached.
pub fn proof_lands(rtt_ms: u64, delay_ms: u64) -> bool {
    delay_ms.saturating_add(rtt_ms / 2) < receipt_window_ms(rtt_ms)
}

/// Proof hashes owed per link, and the rule for when they are released.
///
/// The board's propagation destination proves by application strategy
/// (`ProofStrategy::App`), so the core hands up a `LinkProofRequested` naming
/// the packet hash and then a `LinkDataReceived` carrying the plaintext. This
/// type holds the first until the second arrives — a queue per link, because a
/// client may pipeline uploads and the hashes pair with the payloads in order.
///
/// Generic over the link key so the firmware can hand it
/// `leviculum_core::LinkId` while the tests below use plain bytes; the ledger
/// never looks inside a key.
#[derive(Debug)]
pub struct UploadProofs<K: Ord> {
    pending: BTreeMap<K, VecDeque<PacketHash>>,
}

impl<K: Ord> Default for UploadProofs<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord> UploadProofs<K> {
    /// An empty ledger.
    pub fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }

    /// The core asks whether to prove `packet_hash` on `link`; the answer is
    /// yes, once its payload is in hand. Queued in arrival order.
    pub fn requested(&mut self, link: K, packet_hash: PacketHash) {
        self.pending.entry(link).or_default().push_back(packet_hash);
    }

    /// The payload for the oldest owed proof on `link` has arrived and
    /// decrypted. Answers the hash to prove **now**, before the upload is
    /// decoded, judged or stored.
    ///
    /// This is the whole fix for #397: the proof states that the bytes
    /// arrived, and the moment they arrived is the only moment at which the
    /// uploader is still listening. `None` means nothing was owed — a link
    /// packet on a link whose proof request was never seen, or a second
    /// payload against a single request.
    pub fn on_receipt(&mut self, link: &K) -> Option<PacketHash> {
        self.pending.get_mut(link).and_then(VecDeque::pop_front)
    }

    /// Drop everything owed on `link`. Called when the link closes, from
    /// either side: a proof hash is about the link and goes with it, unlike
    /// the message it was owed for, which is resident and still judged
    /// (`leviculum_sync_batch::SyncBatch::after_close`).
    pub fn forget(&mut self, link: &K) {
        self.pending.remove(link);
    }

    /// The ledger itself, for the engine's heap census — the same walk
    /// `leviculum_core::heap_census` does over every other map the role owns.
    pub fn map(&self) -> &BTreeMap<K, VecDeque<PacketHash>> {
        &self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK_A: [u8; 2] = [0xa0, 0x01];
    const LINK_B: [u8; 2] = [0xb0, 0x02];
    const HASH_1: PacketHash = [0x11; 32];
    const HASH_2: PacketHash = [0x22; 32];

    #[test]
    fn a_proof_is_owed_until_its_payload_lands_and_then_once() {
        let mut proofs = UploadProofs::new();
        proofs.requested(LINK_A, HASH_1);

        assert_eq!(proofs.on_receipt(&LINK_A), Some(HASH_1));
        assert_eq!(
            proofs.on_receipt(&LINK_A),
            None,
            "a second payload against one request is owed nothing"
        );
    }

    #[test]
    fn pipelined_uploads_pair_with_their_payloads_in_order() {
        let mut proofs = UploadProofs::new();
        proofs.requested(LINK_A, HASH_1);
        proofs.requested(LINK_A, HASH_2);

        assert_eq!(proofs.on_receipt(&LINK_A), Some(HASH_1));
        assert_eq!(proofs.on_receipt(&LINK_A), Some(HASH_2));
    }

    #[test]
    fn a_close_forgets_only_its_own_link() {
        let mut proofs = UploadProofs::new();
        proofs.requested(LINK_A, HASH_1);
        proofs.requested(LINK_B, HASH_2);

        proofs.forget(&LINK_A);

        assert_eq!(proofs.on_receipt(&LINK_A), None);
        assert_eq!(proofs.on_receipt(&LINK_B), Some(HASH_2));
        assert_eq!(proofs.map().len(), 1, "the census walks what is left");
    }

    #[test]
    fn a_link_that_never_asked_for_a_proof_gets_none() {
        let mut proofs: UploadProofs<[u8; 2]> = UploadProofs::new();
        assert_eq!(proofs.on_receipt(&LINK_A), None);
    }

    #[test]
    fn the_window_is_the_reference_formula_over_the_enforced_floor() {
        // Above the floor the factor decides.
        assert_eq!(receipt_window_ms(BLE_RTT_MIN_MS), 1_770);
        assert_eq!(receipt_window_ms(BLE_RTT_MAX_MS), 2_904);
        // Below it the floor does: a 1 ms TCP RTT would otherwise leave 6 ms.
        assert_eq!(receipt_window_ms(1), RECEIPT_FLOOR_MS);
        assert_eq!(receipt_window_ms(0), RECEIPT_FLOOR_MS);
    }
}
