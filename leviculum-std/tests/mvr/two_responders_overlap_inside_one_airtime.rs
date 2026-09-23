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
//!
//! The second half of the file (from "What each candidate direction costs")
//! prices the four directions that were proposed instead of a fix, in the
//! same arithmetic and against the same two anchors, so the rig measurement
//! that picks one has a number to beat rather than a paragraph. Nothing
//! there changes channel access either.

use leviculum_channel_access::{
    jitter_slot_ms, ChannelAccess, JITTER_CW_SLOTS, JITTER_DIFS_SLOTS, JITTER_FAST_THRESHOLD_BPS,
    JITTER_SLOT_MAX_MS, JITTER_SLOT_MIN_MS, JITTER_SLOT_SYMBOLS,
};
use leviculum_core::rnode::{airtime_ms_with_preamble, compute_bitrate, derive_preamble_symbols};

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

// ---------------------------------------------------------------------------
// What each candidate direction costs, in the same arithmetic
// ---------------------------------------------------------------------------
//
// Nothing below changes channel access. These tests price the four
// directions the 2026-09-23 order names, in the units the failure is
// measured in: the share of ordered draw pairs that still share the air,
// the median wait a frame pays for it, and what a Python peer on the same
// channel would have to tolerate. Every figure is asserted AND printed, so
// a change to the policy both breaks a test and says by how much:
//
//     cargo test -p leviculum-std --test mvr -- --nocapture two_responders
//
// Two anchors are priced throughout, because the difference between them
// is itself a result. ANCHOR 0 is the order's premise — one frame releases
// both responders, so the two waits start together and their difference is
// the whole separation. ANCHOR 201 is what the 2026-09-22 cell actually
// did: rx_a and rx_b were released by two DIFFERENT probes 201 ms apart,
// and that offset is a free head start the policy can only squander.

/// The anchor offset the cell measured: rx_b was released 201 ms after
/// rx_a (`RX_B_QUEUED_MS - RX_A_QUEUED_MS`).
const MEASURED_ANCHOR_MS: i64 = (RX_B_QUEUED_MS - RX_A_QUEUED_MS) as i64;

/// Python's own first-hop packet timeout at this PHY, in ms:
/// `RNS.Reticulum.MTU * per_byte_latency + DEFAULT_PER_HOP_TIMEOUT`
/// (`Transport.py:2700-2703`), with MTU 500 (`Reticulum.py:93`) and the
/// 6 s per-hop constant (`Reticulum.py:142`). The per-byte latency comes
/// off the interface bitrate, which `RNodeInterface.updateBitrate`
/// (`RNodeInterface.py:693-696`) computes with the formula
/// [`compute_bitrate`] mirrors. This is the budget every wait below is
/// spent out of.
fn python_first_hop_timeout_ms() -> u64 {
    let bitrate = compute_bitrate(SF, CR, BW) as u64;
    500 * 8 * 1000 / bitrate + 6_000
}

/// Ordered draw pairs, and how many of them leave the two answers sharing
/// the air.
///
/// One responder is released at t=0 and the other at `anchor_offset_ms`.
/// Each waits DIFS plus its own draw; DIFS is common and cancels, so the
/// separation is the anchor offset plus the difference of the two draws.
/// They share the air when that separation is shorter than the frame.
fn overlap_census(
    slot_ms: u64,
    cw_slots: u64,
    anchor_offset_ms: i64,
    airtime_ms: u64,
) -> (u64, u64) {
    let mut total = 0;
    let mut overlapping = 0;
    for first in 0..cw_slots as i64 {
        for second in 0..cw_slots as i64 {
            total += 1;
            let separation = anchor_offset_ms + (second - first) * slot_ms as i64;
            if separation.unsigned_abs() < airtime_ms {
                overlapping += 1;
            }
        }
    }
    (total, overlapping)
}

/// The same census when each responder draws TWICE before it keys: once on
/// the host and once inside the modem, which is what an RNode actually
/// does (`RNode_Firmware.ino:1623-1652`, and there is no command to turn
/// it off). Both draws come from the same window, so a wait is
/// `(host + modem)` slots.
fn overlap_census_host_and_modem(
    slot_ms: u64,
    cw_slots: u64,
    anchor_offset_ms: i64,
    airtime_ms: u64,
) -> (u64, u64) {
    let mut total = 0;
    let mut overlapping = 0;
    for first_host in 0..cw_slots as i64 {
        for first_modem in 0..cw_slots as i64 {
            for second_host in 0..cw_slots as i64 {
                for second_modem in 0..cw_slots as i64 {
                    total += 1;
                    let first = first_host + first_modem;
                    let second = second_host + second_modem;
                    let separation = anchor_offset_ms + (second - first) * slot_ms as i64;
                    if separation.unsigned_abs() < airtime_ms {
                        overlapping += 1;
                    }
                }
            }
        }
    }
    (total, overlapping)
}

/// The census with a coarse step on top of the fine draw: each responder
/// additionally defers by `0..coarse_choices` whole `coarse_ms`, which is
/// what "defer a responder by one frame airtime" is once it is made a
/// choice rather than a constant.
fn overlap_census_with_deferral(
    slot_ms: u64,
    cw_slots: u64,
    coarse_ms: u64,
    coarse_choices: u64,
    anchor_offset_ms: i64,
    airtime_ms: u64,
) -> (u64, u64) {
    let mut total = 0;
    let mut overlapping = 0;
    for first_coarse in 0..coarse_choices as i64 {
        for first in 0..cw_slots as i64 {
            for second_coarse in 0..coarse_choices as i64 {
                for second in 0..cw_slots as i64 {
                    total += 1;
                    let separation = anchor_offset_ms
                        + (second - first) * slot_ms as i64
                        + (second_coarse - first_coarse) * coarse_ms as i64;
                    if separation.unsigned_abs() < airtime_ms {
                        overlapping += 1;
                    }
                }
            }
        }
    }
    (total, overlapping)
}

/// Share of a census that overlapped, in percent.
fn pct(overlapping: u64, total: u64) -> f64 {
    100.0 * overlapping as f64 / total as f64
}

/// The median wait a draw of this shape imposes, in ms. The draw is
/// uniform, so its median and its mean are the same value; computed in
/// half-milliseconds so an odd window does not truncate.
fn median_wait_ms(slot_ms: u64, cw_slots: u64) -> f64 {
    (2 * JITTER_DIFS_SLOTS * slot_ms + (cw_slots - 1) * slot_ms) as f64 / 2.0
}

/// The widest wait a draw of this shape imposes, in ms — the figure that
/// has to fit inside a peer's packet timeout.
fn max_wait_ms(slot_ms: u64, cw_slots: u64) -> u64 {
    (JITTER_DIFS_SLOTS + cw_slots - 1) * slot_ms
}

#[test]
fn the_baseline_both_anchors_and_the_head_start_the_policy_threw_away() {
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let cw = JITTER_CW_SLOTS as u64;

    let shared = overlap_census(slot, cw, 0, proof_air);
    let measured = overlap_census(slot, cw, MEASURED_ANCHOR_MS, proof_air);
    assert_eq!(shared, (196, 106));
    assert_eq!(measured, (196, 55));

    // The 201 ms the two probes were apart is longer than the 118 ms frame:
    // had neither responder waited at all, the two proofs could not have
    // met. The draw is what put them back on top of each other, and it does
    // that for 55 of the 196 pairs it can hand out.
    assert!(MEASURED_ANCHOR_MS as u64 > proof_air);
    println!(
        "BASELINE slot={slot}ms cw={cw} frame={proof_air}ms \
         shared_anchor={:.2}% measured_anchor({MEASURED_ANCHOR_MS}ms)={:.2}% \
         median_wait={:.0}ms max_wait={}ms",
        pct(shared.1, shared.0),
        pct(measured.1, measured.0),
        median_wait_ms(slot, cw),
        max_wait_ms(slot, cw),
    );
}

#[test]
fn direction_1_the_slot_is_the_references_own_and_only_its_floor_saves_it() {
    // Our derivation against the reference's, recomputed here from the
    // rule rather than read out of the same function: 12 symbol times
    // (Config.h:107), clamped to [24, 100] ms (Config.h:104-105), floor 6
    // at rates above 30 kbps (Utilities.h:1244-1252).
    let symbol_us = (1u64 << SF) * 1_000_000 / BW as u64;
    let twelve_symbols_ms = JITTER_SLOT_SYMBOLS * symbol_us / 1_000;
    let bitrate = compute_bitrate(SF, CR, BW) as u64;
    assert_eq!(symbol_us, 512, "SF7 at BW250 is a 512 us symbol");
    assert_eq!(twelve_symbols_ms, 6, "the reference's unclamped slot here");
    assert_eq!(bitrate, 10_937);
    assert!(
        bitrate <= JITTER_FAST_THRESHOLD_BPS,
        "at BW250/SF7 the modulation is NOT fast, so the 6 ms fast floor \
         does not apply and the 24 ms floor does"
    );
    let reference_slot = twelve_symbols_ms.clamp(JITTER_SLOT_MIN_MS, JITTER_SLOT_MAX_MS);
    assert_eq!(reference_slot, jitter_slot_ms(BW, SF, CR));
    assert_eq!(reference_slot, 24);

    // So there is NO divergence to find at BW250: we derive the reference's
    // slot exactly, and at this PHY the reference's own rule returns its
    // FLOOR. That is the whole answer to "scale the slot to the airtime" —
    // the reference does not scale to airtime anywhere. Its 12 symbols are
    // a fixed fraction of a frame, and a small one: a 115 B proof is 230
    // symbols of air here, so the unclamped rule is a TWENTIETH of the
    // frame and only the floor lifts it to a fifth.
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    assert_eq!(proof_air.div_ceil(twelve_symbols_ms), 20);
    assert_eq!(proof_air.div_ceil(reference_slot), 5);

    let cw = JITTER_CW_SLOTS as u64;
    // Three slots priced: the reference's (24), the reference's own ceiling
    // (100, the widest slot expressible inside its clamp) and one frame
    // (118, which the clamp cannot express at all).
    for slot in [reference_slot, JITTER_SLOT_MAX_MS, proof_air] {
        let shared = overlap_census(slot, cw, 0, proof_air);
        let measured = overlap_census(slot, cw, MEASURED_ANCHOR_MS, proof_air);
        println!(
            "DIRECTION 1 slot={slot}ms cw={cw} shared_anchor={:.2}% \
             measured_anchor={:.2}% median_wait={:.0}ms max_wait={}ms \
             python_budget={:.0}%",
            pct(shared.1, shared.0),
            pct(measured.1, measured.0),
            median_wait_ms(slot, cw),
            max_wait_ms(slot, cw),
            100.0 * max_wait_ms(slot, cw) as f64 / python_first_hop_timeout_ms() as f64,
        );
    }
    assert_eq!(
        overlap_census(JITTER_SLOT_MAX_MS, cw, 0, proof_air),
        (196, 40)
    );
    assert_eq!(overlap_census(proof_air, cw, 0, proof_air), (196, 14));
    // A slot of one frame costs 799 ms of median wait over the 204 ms the
    // policy costs now, and buys 54.08% -> 7.14%.
    assert_eq!(
        median_wait_ms(proof_air, cw) - median_wait_ms(24, cw),
        799.0
    );
}

#[test]
fn direction_1_cannot_reach_five_percent_because_the_ties_are_a_floor() {
    // Whatever the slot, two responders that drew the SAME number of slots
    // key up together: a slot-quantised uniform window of W draws collides
    // with itself once in W, and no slot width touches that. 1/14 is 7.14%,
    // so the 5% the order asks for is out of reach for direction 1 by
    // construction, and the smallest window whose tie rate alone is under
    // 5% is 21 draws.
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let cw = JITTER_CW_SLOTS as u64;
    for slot in [proof_air, proof_air * 2, proof_air * 10, 10_000] {
        let (total, overlapping) = overlap_census(slot, cw, 0, proof_air);
        assert_eq!((total, overlapping), (196, 14), "slot {slot}");
    }
    let tie_floor = pct(cw, cw * cw);
    let smallest_window_under_5pct = (2..).find(|w| pct(*w, w * w) < 5.0).expect("exists");
    assert_eq!(smallest_window_under_5pct, 21);
    println!(
        "DIRECTION 1 FLOOR tie_rate=1/{cw}={tie_floor:.2}% (slot-independent), \
         smallest cw whose ties alone are under 5% = {smallest_window_under_5pct}"
    );
}

#[test]
fn direction_2_the_window_that_reaches_five_percent_costs_two_seconds() {
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let base = median_wait_ms(slot, JITTER_CW_SLOTS as u64);

    let first_under = |target: f64| -> u64 {
        (2u64..2_000)
            .find(|cw| {
                let (total, overlapping) = overlap_census(slot, *cw, 0, proof_air);
                pct(overlapping, total) < target
            })
            .expect("a wide enough window exists")
    };
    let cw_10 = first_under(10.0);
    let cw_5 = first_under(5.0);
    assert_eq!((cw_10, cw_5), (88, 178));

    // The last window that is still too narrow, so the boundary is pinned
    // from both sides and not just asserted at one point.
    let (total, overlapping) = overlap_census(slot, cw_5 - 1, 0, proof_air);
    assert!(pct(overlapping, total) >= 5.0);

    for cw in [cw_10, cw_5] {
        let shared = overlap_census(slot, cw, 0, proof_air);
        let measured = overlap_census(slot, cw, MEASURED_ANCHOR_MS, proof_air);
        println!(
            "DIRECTION 2 slot={slot}ms cw={cw} shared_anchor={:.2}% \
             measured_anchor={:.2}% median_wait={:.0}ms (+{:.0}ms) max_wait={}ms \
             python_budget={:.0}%",
            pct(shared.1, shared.0),
            pct(measured.1, measured.0),
            median_wait_ms(slot, cw),
            median_wait_ms(slot, cw) - base,
            max_wait_ms(slot, cw),
            100.0 * max_wait_ms(slot, cw) as f64 / python_first_hop_timeout_ms() as f64,
        );
    }
    assert_eq!(median_wait_ms(slot, cw_5) - base, 1968.0);
    assert_eq!(max_wait_ms(slot, cw_5), 4296);
    // Two thirds of a Python peer's whole first-hop packet timeout spent
    // waiting to answer, before the modem has drawn its own window.
    assert_eq!(python_first_hop_timeout_ms(), 6_365);
}

#[test]
fn direction_3_a_deferral_that_is_a_constant_cancels_and_buys_nothing() {
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let cw = JITTER_CW_SLOTS as u64;
    let base = overlap_census(slot, cw, 0, proof_air);

    // "Defer a responder by one frame airtime after a shared release" has
    // to answer which frames count as a shared release, and the interface
    // cannot know: it hears its own inbound frame and nothing about who
    // else heard it. The only observable proxy is "this frame was queued
    // inside one airtime of the last LORA_RX" — and a proof is ALWAYS
    // queued inside one airtime of the frame it proves. So the condition
    // is true for every answer, the deferral is a constant added to both
    // responders, and a constant common to both cancels exactly the way
    // DIFS already does.
    let deferred = overlap_census_with_deferral(slot, cw, proof_air, 1, 0, proof_air);
    assert_eq!(deferred, base, "a constant deferral moved the odds");
    println!(
        "DIRECTION 3 constant: shared_anchor {:.2}% -> {:.2}% for +{proof_air}ms on every \
         answer's RTT",
        pct(base.1, base.0),
        pct(deferred.1, deferred.0),
    );

    // Made a CHOICE instead — defer by a uniform 0 or 1 airtimes on top of
    // the existing draw — it is simply a second, coarser slot, and it
    // inherits the same tie floor: the two responders agree on the coarse
    // step half the time and are back inside the fine window. 54.08% ->
    // 47.70% from a shared anchor, and from the measured 201 ms anchor it
    // goes the WRONG way, 28.06% -> 29.08%, for the same reason the host
    // draw does (see the reference test below): an anchor already clear of
    // the frame can only be spoiled by more spread.
    let coin = overlap_census_with_deferral(slot, cw, proof_air, 2, 0, proof_air);
    assert_eq!(coin, (784, 374));
    let coin_measured =
        overlap_census_with_deferral(slot, cw, proof_air, 2, MEASURED_ANCHOR_MS, proof_air);
    assert_eq!(coin_measured, (784, 228));
    println!(
        "DIRECTION 3 coin: shared_anchor={:.2}% measured_anchor={:.2}% \
         median_wait=+{:.0}ms on every answer",
        pct(coin.1, coin.0),
        pct(coin_measured.1, coin_measured.0),
        proof_air as f64 / 2.0,
    );
}

#[test]
fn direction_4_carrier_sense_re_anchors_the_pair_and_doubles_the_odds() {
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let cw = JITTER_CW_SLOTS as u64;

    // What the interface would have to observe is already in this task:
    // branch 1 deframes CMD_DATA and emits LORA_RX in the SAME
    // `tokio::select!` the send timer lives in, so it can drop or rebuild
    // `send_timer` without any new plumbing. What it CANNOT observe is a
    // busy channel: the host learns of a frame only once the modem has
    // received it whole and pushed it up the serial line, which is an
    // END-OF-FRAME edge, never a carrier. Acting on that edge re-anchors
    // every waiting responder to the same instant — and the anchor offset
    // is the only thing that was keeping the measured pair apart.
    let blind = overlap_census(slot, cw, MEASURED_ANCHOR_MS, proof_air);
    let re_anchored = overlap_census(slot, cw, 0, proof_air);
    assert_eq!((blind, re_anchored), ((196, 55), (196, 106)));
    println!(
        "DIRECTION 4 measured anchor {MEASURED_ANCHOR_MS}ms: blind sleep {:.2}% -> \
         re-anchored on the inbound frame {:.2}% (worse), median_wait unchanged \
         ({:.0}ms), DIFS restart adds {}ms per interruption",
        pct(blind.1, blind.0),
        pct(re_anchored.1, re_anchored.0),
        median_wait_ms(slot, cw),
        JITTER_DIFS_SLOTS * slot,
    );

    // Whether the interruption resumes the remainder (what the nRF
    // firmware does, `leviculum-nrf/src/lora.rs:1597-1625`) or redraws, the
    // separation stays a whole number of slots from a common anchor, so
    // the tie floor applies to both and neither can beat 1/14.
    assert!(pct(re_anchored.1, re_anchored.0) >= pct(cw, cw * cw));
}

#[test]
fn the_reference_is_a_modem_that_already_drew_this_window_once() {
    // Python-RNS adds nothing before the serial write: `process_outgoing`
    // escapes the frame and writes it (`RNodeInterface.py:708-728`). The
    // separation a Python pair gets is the modem's alone — and the modem's
    // is not nothing: `tx_queue_handler` draws `random(cw_min, cw_max)`
    // slots for EVERY queued packet and there is no host command to
    // disable it (`RNode_Firmware.ino:1623-1652`). At band 1 that is the
    // same 0..=13 slots of the same 24 ms our host draws.
    //
    // So "host jitter off" is not "no wait": it is ONE draw of this window
    // instead of two, and the question the deviation rule asks is what the
    // second draw buys.
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let proof_air = airtime_ms_with_preamble(PROOF_BYTES, BW, SF, CR, preamble);
    let cw = JITTER_CW_SLOTS as u64;

    let modem_only_shared = overlap_census(slot, cw, 0, proof_air);
    let both_shared = overlap_census_host_and_modem(slot, cw, 0, proof_air);
    let modem_only_measured = overlap_census(slot, cw, MEASURED_ANCHOR_MS, proof_air);
    let both_measured = overlap_census_host_and_modem(slot, cw, MEASURED_ANCHOR_MS, proof_air);

    assert_eq!(modem_only_shared, (196, 106));
    assert_eq!(both_shared, (38_416, 15_756));
    assert_eq!(modem_only_measured, (196, 55));
    assert_eq!(both_measured, (38_416, 11_150));

    println!(
        "REFERENCE shared_anchor: modem only {:.2}%, host+modem {:.2}% \
         (median wait {:.0}ms -> {:.0}ms)",
        pct(modem_only_shared.1, modem_only_shared.0),
        pct(both_shared.1, both_shared.0),
        median_wait_ms(slot, cw),
        2.0 * median_wait_ms(slot, cw),
    );
    println!(
        "REFERENCE measured_anchor({MEASURED_ANCHOR_MS}ms): modem only {:.2}%, \
         host+modem {:.2}%",
        pct(modem_only_measured.1, modem_only_measured.0),
        pct(both_measured.1, both_measured.0),
    );

    // Two anchors, two signs, and that is the finding. From a SHARED
    // anchor the second draw helps — 54.08% down to 41.01% — because it
    // widens a difference distribution centred on zero. From the anchor
    // the cell actually produced it HURTS: 201 ms is already outside the
    // 118 ms collision band, so every millisecond of extra spread pulls
    // probability back INTO it, 28.06% up to 29.02%. The host draw cannot
    // know which anchor it is on, and it costs 204 ms of median wait on
    // every answer either way.
    //
    // That is the deviation rule's third clause with a number on it: on
    // the exposure this cell produces, the second draw measurably makes
    // Priority 1 slightly worse. "Less, not more" is a direction the rig
    // has to rule out rather than an idea.
    let delta =
        pct(both_measured.1, both_measured.0) - pct(modem_only_measured.1, modem_only_measured.0);
    assert!(
        delta > 0.0,
        "the second draw was expected to cost at the measured anchor, moved {delta:.2} points"
    );
    println!("REFERENCE second draw at the measured anchor: {delta:+.2} points, +204ms median");
}

/// periculum's slack between a frame leaving the air and the log line that
/// reports it (`default_census_tolerance_ms`, `periculum/src/topology.rs`),
/// and the only knob a cell can turn from its `[[steps]]` block.
const CENSUS_TOLERANCE_MS: u64 = 50;

#[test]
fn the_collision_census_window_is_narrower_than_the_wait_it_would_be_counting() {
    // The instrument the rig A/B would read — `collision_census`'
    // `responder_releases` / `responder_overlaps` — counts a responder pair
    // only when BOTH answers key the air inside one inbound airtime of that
    // frame, plus the tolerance (`Census::responder_pairs`,
    // `periculum/src/contention.rs:425-461`). At this PHY that window is a
    // probe airtime plus 50 ms.
    //
    // The wait it would be counting is longer than the window. An answer
    // keys the air after the host's draw AND the modem's own: two DIFS and
    // two draws, 96 ms at the very least and 720 ms at the most. Only the
    // smallest few combinations land inside the window at all, and they are
    // exactly the ones that were going to overlap anyway — so the census
    // reports "releases" it can see, all of which overlap, and misses the
    // rest, including the event this file is about.
    let slot = jitter_slot_ms(BW, SF, CR);
    let preamble = derive_preamble_symbols(SF, CR, BW);
    let probe_air = airtime_ms_with_preamble(PROBE_BYTES, BW, SF, CR, preamble);
    let cw = JITTER_CW_SLOTS as u64;
    let window_ms = probe_air + CENSUS_TOLERANCE_MS;
    assert_eq!(window_ms, 178);

    let key_up_ms = |host: u64, modem: u64| (2 * JITTER_DIFS_SLOTS + host + modem) * slot;
    assert_eq!(key_up_ms(0, 0), 96, "the shortest wait to the air");
    assert_eq!(key_up_ms(cw - 1, cw - 1), 720, "the longest");

    let visible = (0..cw)
        .flat_map(|host| (0..cw).map(move |modem| key_up_ms(host, modem)))
        .filter(|key_up| *key_up <= window_ms)
        .count() as u64;
    assert_eq!(visible, 10, "of {} draw combinations", cw * cw);

    // Both responders have to be inside it for the pair to be counted.
    let pair_visible = visible * visible;
    let pair_total = cw * cw * cw * cw;
    assert_eq!((pair_visible, pair_total), (100, 38_416));
    println!(
        "CENSUS window={window_ms}ms (probe airtime {probe_air}ms + {CENSUS_TOLERANCE_MS}ms \
         tolerance) vs key-up {}..{}ms: one responder visible in {:.2}% of draws, a PAIR in \
         {:.2}%",
        key_up_ms(0, 0),
        key_up_ms(cw - 1, cw - 1),
        pct(visible, cw * cw),
        pct(pair_visible, pair_total),
    );

    // The measured collision keyed 314 ms after its probe was queued, so it
    // is outside the window by a factor of nearly two: the census as it
    // stands would have reported `responder_releases=0` for the very run
    // that produced this file.
    let measured_key_up = RX_A_DRAW_MS;
    assert!(
        measured_key_up > window_ms,
        "rx_a's own host draw alone ({measured_key_up}ms) already exceeds the census \
         window ({window_ms}ms), before its modem drew anything"
    );
    println!(
        "CENSUS the measured event: rx_a's host draw alone is {measured_key_up}ms, \
         {}ms past the window",
        measured_key_up - window_ms
    );
}
