//! mvr for the `bench_dual_pair_fast_rnode_only` red of the 2026-09-22 full
//! hardware run (bench log `hw-vollauf4-b74027aa.log` line 722, corpus 056e621):
//!
//! ```text
//! VERDICT bench_dual_pair_fast_rnode_only RED step 5 (benchmark) failed:
//!   rnode_a -> rx_a.probe: sent=36 received=35 pdr=97.2%
//!   rnode_b -> rx_b.probe: sent=35 received=34 pdr=97.1%
//! ```
//!
//! WHAT THE RUN'S OWN RECORDS SAY, and it is not what the cell header or the
//! register entry predicted. Counted over the cell log
//! (`logs/bench_dual_pair_fast_rnode_only_2026-09-22T17-55-22Z.log`), every
//! one of the 71 probes reached its destination: `PKT_LOCAL` = 36 on rx_a and
//! 35 on rx_b, `PROOF_GEN` and `PROOF_SEND` the same. No probe was lost. Both
//! reds are lost PROOFS on the return leg, and both are ONE event:
//!
//! ```text
//! 17:54:12.290611  rx_a  send queue: 1 packets, acquisition jitter 312ms (slot 24ms)
//! 17:54:12.491640  rx_b  send queue: 1 packets, acquisition jitter  96ms (slot 24ms)
//! 17:54:12.600269  rx_b  LORA_TX iface=rnode_0 len=115
//! 17:54:12.604474  rx_a  LORA_TX iface=rnode_0 len=115
//! ```
//!
//! The two proofs (`683e9a9435b63fc5` for probe `35fe5ba32f87c4be`,
//! `c8bc341c2535356f` for probe `3505c2962fc564da`) were handed to their two
//! modems 4.2 ms apart. Each appears exactly once in the whole run — at its
//! own `PKT_TX`. Nobody ever received either. Those are the only two air
//! losses in the cell, one per leg, from a single collision between the two
//! RECEIVERS.
//!
//! So the loss is not sender-side and not probe-side: it is the two far ends
//! answering into each other. At rnode_b the two receivers arrive 1.5 dB
//! apart (−36.0 vs −37.5 dBm over 34 frames each), so no capture is available
//! there at all.
//!
//! THE MECHANISM THIS FILE PINS. The host's pre-TX wait
//! (`ChannelAccess::acquisition_jitter_ms`, `interfaces/rnode.rs` branch 3) is
//! a blind sleep of DIFS plus a uniform draw over `JITTER_CW_SLOTS` slots. It
//! is quantised in slots, and at the corpus's bench PHY one slot is FIVE
//! TIMES SHORTER than the frame it is separating. Two responders released by
//! the same event therefore overlap in the air for the majority of the
//! reachable draw pairs — separation measured in slots cannot clear a channel
//! held for several slots.
//!
//! `interfaces/rnode.rs::two_peers_released_by_the_same_event_do_not_key_together`
//! asserts only that two peers land at least ONE slot apart, which is 24 ms
//! against a 118 ms frame. It is a real guard against the type-aware bypass
//! returning (#347) and is left alone; what it cannot see is the PHY, and
//! this file supplies it.
//!
//! No fix is attempted here. Widening the window, or deferring a responder by
//! an airtime, is a channel-access design change that has to be measured on
//! the rig against the reference first; this test states the arithmetic the
//! measurement has to beat. The counts below are the CURRENT policy's, so a
//! change to `JITTER_CW_SLOTS`, to the slot derivation or to the preamble
//! moves them and this test says by how much.

use leviculum_channel_access::{jitter_slot_ms, ChannelAccess, JITTER_CW_SLOTS, JITTER_DIFS_SLOTS};
use leviculum_core::rnode::{airtime_ms_with_preamble, derive_preamble_symbols};

/// The `[radio]` block of `hardware/bench_dual_pair_fast_rnode_only.toml`,
/// byte for byte the one `bench_dual_pair_fast` uses.
const BW: u32 = 250_000;
const SF: u8 = 7;
const CR: u8 = 5;

/// The two frame sizes the cell puts on the air, read off the run's
/// `LORA_TX`/`LORA_RX` lines: a probe is 131 bytes, the proof answering it
/// is 115.
const PROBE_BYTES: u32 = 131;
const PROOF_BYTES: u32 = 115;

/// What the cell measured on 2026-09-22, in ms since the second boundary:
/// rx_a queued at .290611 and drew 312 ms, rx_b queued at .491640 and drew
/// 96 ms.
const RX_A_QUEUED_MS: u64 = 290;
const RX_A_DRAW_MS: u64 = 312;
const RX_B_QUEUED_MS: u64 = 491;
const RX_B_DRAW_MS: u64 = 96;

#[test]
fn one_jitter_slot_is_a_fifth_of_the_frame_it_separates() {
    let slot = jitter_slot_ms(BW, SF, CR);
    assert_eq!(slot, 24, "the cell's log reports `slot 24ms` at this PHY");

    // The modem is not on its default preamble: the RNode derivation targets
    // 24 ms of preamble, which is 47 symbols here, and pacing that assumes
    // the default 8 undercounts every frame by 20 ms.
    let preamble = derive_preamble_symbols(SF, CR, BW);
    assert_eq!(preamble, 47);

    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let probe_air = airtime_ms_with_preamble(PROBE_BYTES, BW, SF, CR, preamble);
    assert_eq!(proof_air, 118, "115-byte proof");
    assert_eq!(probe_air, 128, "131-byte probe");

    // This is the whole defect in one line: the unit the draw separates in is
    // far smaller than the unit the channel is occupied in, so "one slot
    // apart" and "clear of each other" are not the same statement.
    assert_eq!(
        proof_air.div_ceil(slot),
        5,
        "a proof holds the channel for {proof_air}ms, {slot}ms to the slot"
    );
    assert_eq!(probe_air.div_ceil(slot), 6, "a probe holds it longer still");
}

#[test]
fn most_draw_pairs_leave_two_responders_overlapping_in_the_air() {
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);

    // Two nodes released by the same frame — which is what happens when a
    // probe lands on one receiver while the other is already contending, and
    // what `rx_a`/`rx_b` were doing. DIFS is common to both and cancels, so
    // the separation is the difference of the two draws alone.
    let draws = JITTER_CW_SLOTS as u64;
    let mut overlapping = 0u64;
    for a in 0..draws {
        for b in 0..draws {
            if a.abs_diff(b) * slot < proof_air {
                overlapping += 1;
            }
        }
    }

    assert_eq!(
        (draws * draws, overlapping),
        (196, 106),
        "of the {} equally likely ordered draw pairs, {overlapping} put the \
         two frames on the air at once",
        draws * draws
    );

    // The span is not the problem and must not be mistaken for it: the window
    // reaches 360 ms, three whole frames. The granularity is.
    assert_eq!(JITTER_DIFS_SLOTS * slot, 48, "shortest wait");
    assert_eq!(
        (JITTER_DIFS_SLOTS + draws - 1) * slot,
        360,
        "longest wait, and it is wider than a frame"
    );
}

#[test]
fn the_measured_pair_of_draws_is_one_the_policy_hands_out() {
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);

    // Replay the two waits the cell logged. rx_a is released 201 ms before
    // rx_b and waits 216 ms longer, so the anchor offset very nearly cancels
    // and the two key-ups land 15 ms apart — the log's 4.2 ms after the two
    // modems' own CSMA had its say.
    let rx_a_keys = RX_A_QUEUED_MS + RX_A_DRAW_MS;
    let rx_b_keys = RX_B_QUEUED_MS + RX_B_DRAW_MS;
    let separation = rx_a_keys.abs_diff(rx_b_keys);
    assert_eq!(separation, 15);
    assert!(
        separation < slot && separation < proof_air,
        "the measured pair is inside one slot ({slot}ms) and far inside one \
         frame ({proof_air}ms): {separation}ms"
    );

    // Both waits have to be reachable draws, or the replay above is arithmetic
    // about nothing. Every slot count the window offers turns up in a seed
    // sweep, which also says the draw is the uniform one the policy claims and
    // not a constant.
    let mut seen = std::collections::BTreeSet::new();
    for seed in 1u32..=4096 {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(BW, SF, CR);
        seen.insert(access.acquisition_jitter_ms());
    }
    let expected: std::collections::BTreeSet<u64> = (0..JITTER_CW_SLOTS as u64)
        .map(|d| (JITTER_DIFS_SLOTS + d) * slot)
        .collect();
    assert_eq!(
        seen, expected,
        "the window must hand out every one of its {JITTER_CW_SLOTS} draws"
    );
    assert!(seen.contains(&RX_A_DRAW_MS) && seen.contains(&RX_B_DRAW_MS));
}
