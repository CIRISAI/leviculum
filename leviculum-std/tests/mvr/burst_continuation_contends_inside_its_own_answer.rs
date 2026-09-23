//! mvr for the `bench_single_pair_fast` red of the 2026-09-22 full hardware
//! run (bench log `hw-vollauf4-b74027aa.log` line 2017, corpus 056e621):
//!
//! ```text
//! VERDICT bench_single_pair_fast RED step 4 (benchmark) failed:
//!   sender -> receiver.probe: sent=34 received=33 pdr=97.1%
//! ```
//!
//! WHAT THE RUN'S OWN RECORDS SAY. No probe was lost. The receiver board's
//! own capture
//! (`logs/bench_single_pair_fast_receiver_debug_2026-09-22T18-12-05Z.log`)
//! holds 34 `[LORA] RX 147 bytes ... dst=dc817b8c` lines, all at rssi -41 and
//! snr 12..15, and the board answered every one of them. Probe 13 is the one
//! the benchmark scored as a timeout, and the board answered it too:
//!
//! ```text
//! t=83523  [LORA] RX 147 bytes rssi=-41 snr=13 dst=dc817b8c
//! t=83528  [LORA_JITTER] wait_ms=144 slot_ms=24
//! t=83682  [LORA_CAD] busy=false attempt=0
//! t=83683  [LORA] TX 115 bytes / [T114_TX_FRAME] first8=0301eef8face7cd8
//! t=83806  [LORA] TX done
//! ```
//!
//! `eef8face` is the probe the sender forwarded at 18:13:34.280. The proof
//! for it went on the air and the sender never saw it: the sender's radio
//! logs no `LORA_RX` at all between 18:13:34.138 and 18:13:45.279, and the
//! next probe is 10.009 s later — the script's whole timeout. One lost
//! proof, on the return leg.
//!
//! WHAT THE SENDER WAS DOING WITH ITS OWN RADIO WHILE IT WAITED:
//!
//! ```text
//! 18:13:34.280  send queue: 1 packets, acquisition jitter 264ms (slot 24ms)
//! 18:13:34.464  send queue: 2 packets (priority insert at 1)   <- announce
//! 18:13:34.553  TX 147 bytes to serial                         <- the probe
//! 18:13:34.617  TX 215 bytes to serial                         <- 64 ms later
//! ```
//!
//! The sender put exactly two non-probe frames on the air in the whole 60 s
//! window, at 18:13:34.617 and 18:13:44.627, and both were announce
//! rebroadcasts queued behind a probe as a burst continuation. Those are also
//! the only two disturbed exchanges in the cell: the first lost its proof,
//! the second is the slowest probe of the run at 991 ms — there the board
//! drew 264 ms of jitter, HEARD the sender's announce inside that listening
//! window (`t=93827 [LORA] RX 215 bytes ... dst=2bcd55a7`) and deferred its
//! proof until after it. The other 32 probes, with nothing queued behind
//! them, all came back in 537..1060 ms. Two occurrences, two anomalies,
//! and the difference between them is one jitter draw.
//!
//! THE MECHANISM THIS FILE PINS. `interfaces/rnode.rs` makes the frame that
//! acquires an idle channel serve the randomised wait `ChannelAccess` draws,
//! and spaces every further frame of the same burst by the fixed
//! `rnode::MIN_SPACING_MS` (50 ms), documented at its definition as the
//! serial-buffer floor and explicitly NOT a CSMA-fair pacing. At this cell's
//! PHY the first frame needs at least 189 ms from serial write to end of air,
//! so the second frame is always already in the modem's queue when the first
//! one stops radiating. Modem and far end are then released to contend by the
//! same event — the end of OUR frame — and each draws a contention window of
//! the same order (48..360 ms). 144 of the 210 reachable draw pairs put our
//! own second frame on the air inside the answer to our first.
//!
//! The modem does contend, and that is measured rather than assumed: the
//! board armed its receiver at t=83528 and listened until t=83675 without
//! hearing anything, so the announce written to the sender's modem at
//! 18:13:34.617 was still not on the air 152 ms after the probe stopped
//! radiating. A firmware that flushed its queue would have been heard there.
//! Contending is exactly the problem — it contends in the one window the
//! answer needs.
//!
//! This is Codeberg #374 ("a relay's own announce burst collides with an
//! incoming LoRa packet, and neither side's carrier sense prevents it") seen
//! from the other end: there the burst deafened the relay to an inbound
//! packet, here it deafens the sender to the proof it is itself waiting for.
//! It is also the same slot-against-airtime arithmetic
//! `two_responders_overlap_inside_one_airtime` pins for two far ends, with
//! the difference that one of the two contenders here is a frame WE hold, so
//! a spacing change can act on it at all.
//!
//! NARROWED, NOT CLOSED, 2026-09-23. `interfaces/rnode.rs::tx_hold` now
//! holds the next frame for the previous one's airtime plus the firmware's
//! DIFS and its longest contention draw — 501 ms at this cell's PHY, not the
//! 971 ms the search below found. That is deliberate: the hold is priced to
//! empty the MODEM's queue, so every frame gets its own CSMA contest instead
//! of riding a flush (Codeberg #36), and it says nothing about the far end's
//! answer window, which opens at the end of our airtime and closes after a
//! draw we cannot see. `the_hold_narrows_the_census_without_closing_it`
//! below measures what is left: 42.2 % of triples, against 68.6 % before
//! the hold and 17.8 % under the wider `compute_spacing_ms`. The mvr for the hold itself is
//! `one_frame_in_the_modem_at_a_time`.
//!
//! No fix was attempted here, and the numbers below say why one could not be
//! picked from a desk: the airtime-aware spacing that already exists and has
//! no caller (`rnode::compute_spacing_ms`, wired in c2eba153 and reverted in
//! 12f99a02) narrows the census from 68.6 % to 17.8 % but does not close it,
//! and the shortest spacing that closes it outright is 971 ms — paid on every
//! burst continuation, against a probe interval of 1500 ms. Which of those
//! the link wants is a rig measurement against the reference, not a sentence.
//!
//! Sans-hardware, deterministic, milliseconds.

use leviculum_channel_access::{jitter_slot_ms, JITTER_CW_SLOTS, JITTER_DIFS_SLOTS};
use leviculum_core::rnode::{
    airtime_ms_with_preamble, compute_bitrate, compute_spacing_ms, derive_preamble_symbols,
    CSMA_DIFS_MS, CSMA_MAX_CW_MS, MIN_SPACING_MS,
};

/// The `[radio]` block of `hardware/bench_single_pair_fast.toml`.
const BW: u32 = 250_000;
const SF: u8 = 7;
const CR: u8 = 5;

/// The three frame sizes this cell puts on the air, read off the run's
/// `LORA_TX` / `[LORA] RX` lines: a two-hop probe is 147 bytes, the proof
/// answering it 115, and the announce rebroadcast that rides behind the
/// probe 215.
const PROBE_BYTES: u32 = 147;
const PROOF_BYTES: u32 = 115;
const ANNOUNCE_BYTES: u32 = 215;

/// What the board spends between the end of an inbound frame and the start
/// of its own jitter wait, measured on the failing exchange: `[LORA] RX` at
/// t=83523, `[LORA] TX` at t=83683 with a 144 ms draw, so 16 ms of stack,
/// CAD and key-up. `the_census_does_not_hinge_on_that_16_ms` sweeps it.
const FAR_END_OVERHEAD_MS: u64 = 16;

/// The two jitter draws the board logged in this cell, in ms
/// (`[LORA_JITTER] wait_ms=`). 144 lost its proof, 264 deferred behind the
/// sender's announce and cost the run its slowest probe.
const BOARD_DRAW_LOST_MS: u64 = 144;
const BOARD_DRAW_DEFERRED_MS: u64 = 264;

/// The probe interval the cell paced at (`min probe interval: 1.5s`), which
/// is what any added spacing is spent out of.
const PROBE_INTERVAL_MS: u64 = 1_500;

fn preamble() -> u16 {
    derive_preamble_symbols(SF, CR, BW)
}

fn air(bytes: u32) -> u64 {
    airtime_ms_with_preamble(bytes, BW, SF, CR, preamble())
}

/// Half-open intervals of occupied air.
fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Python's own first-hop packet timeout at this PHY, in ms:
/// `RNS.Reticulum.MTU * per_byte_latency + DEFAULT_PER_HOP_TIMEOUT`
/// (`Transport.py:2700-2703`), MTU 500, 6 s per hop. The budget any added
/// wait is spent out of for a Python peer on the same channel.
fn python_first_hop_timeout_ms() -> u64 {
    let bitrate = compute_bitrate(SF, CR, BW) as u64;
    500 * 8 * 1000 / bitrate + 6_000
}

/// How many contention slots the RNode firmware's own window holds
/// (`CSMA_MAX_CW_MS` at the jitter slot width, 15 here — the cell logged
/// `cw_min=0 cw_max=14`).
fn modem_cw_slots() -> u64 {
    CSMA_MAX_CW_MS / jitter_slot_ms(BW, SF, CR)
}

/// When our own second frame reaches the air, relative to the end of the
/// first frame's airtime, for modem contention slot `k`.
fn second_frame_air(queued_at: u64, k: u64) -> (u64, u64) {
    let start = queued_at + CSMA_DIFS_MS + k * jitter_slot_ms(BW, SF, CR);
    (start, start + air(ANNOUNCE_BYTES))
}

/// When the far end's proof reaches the air, same anchor, for jitter draw
/// `j` slots.
fn proof_air(overhead_ms: u64, j: u64) -> (u64, u64) {
    let start = overhead_ms + (JITTER_DIFS_SLOTS + j) * jitter_slot_ms(BW, SF, CR);
    (start, start + air(PROOF_BYTES))
}

/// Ordered (far-end draw, modem draw) pairs, and how many of them leave our
/// own second frame sharing the air with the answer to our first.
fn census(overhead_ms: u64, queued_at: u64) -> (u64, u64) {
    let mut total = 0;
    let mut contending = 0;
    for j in 0..JITTER_CW_SLOTS as u64 {
        for k in 0..modem_cw_slots() {
            total += 1;
            if overlaps(proof_air(overhead_ms, j), second_frame_air(queued_at, k)) {
                contending += 1;
            }
        }
    }
    (total, contending)
}

#[test]
fn the_burst_spacing_is_shorter_than_the_frame_it_is_meant_to_follow() {
    assert_eq!(preamble(), 47, "the RNode preamble derivation at this PHY");
    assert_eq!(air(PROBE_BYTES), 141, "147-byte probe");
    assert_eq!(air(PROOF_BYTES), 118, "115-byte proof");
    assert_eq!(air(ANNOUNCE_BYTES), 190, "215-byte announce");

    // The whole premise in one line: the interface releases the burst's
    // second frame 50 ms after writing the first, and the first cannot have
    // left the air by then under ANY modem draw — the shortest path to the
    // end of its airtime is DIFS plus zero contention slots plus the frame.
    let earliest_air_end = CSMA_DIFS_MS + air(PROBE_BYTES);
    assert_eq!(earliest_air_end, 189);
    assert!(
        MIN_SPACING_MS < earliest_air_end,
        "the burst spacing ({MIN_SPACING_MS}ms) is a serial floor, not a \
         channel separation: the first frame is still radiating at best \
         {}ms later",
        earliest_air_end - MIN_SPACING_MS
    );

    // So the second frame's arrival at the modem carries no information
    // about the draws at all, and the modem contends from the instant our
    // own frame stops — the same instant the far end starts contending to
    // answer it.
    assert_eq!(
        second_frame_air(0, 0).0,
        CSMA_DIFS_MS,
        "queued before the air cleared, so DIFS runs from the air clearing"
    );
}

#[test]
fn most_draw_pairs_put_our_own_second_frame_inside_the_answer() {
    let (total, contending) = census(FAR_END_OVERHEAD_MS, 0);
    assert_eq!(
        (total, contending),
        (210, 144),
        "of the {total} equally likely ordered draw pairs, {contending} put \
         our burst's second frame on the air while the answer to its first \
         is there"
    );

    // Contending is not the same as losing: the far end's wait is a
    // LISTENING window, so when our frame starts first it defers and the
    // exchange only runs late. Both outcomes are in the cell, one each.
    let slot = jitter_slot_ms(BW, SF, CR);
    assert_eq!(BOARD_DRAW_LOST_MS % slot, 0);
    assert_eq!(BOARD_DRAW_DEFERRED_MS % slot, 0);
    assert_eq!(
        (BOARD_DRAW_LOST_MS / slot, BOARD_DRAW_DEFERRED_MS / slot),
        (6, 11),
        "both logged waits are whole slots the policy hands out"
    );
    let draw_max = (JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 - 1) * slot;
    assert_eq!((JITTER_DIFS_SLOTS * slot, draw_max), (48, 360));
    assert!(BOARD_DRAW_DEFERRED_MS <= draw_max);
}

#[test]
fn the_census_does_not_hinge_on_that_16_ms() {
    // The far-end overhead is one measurement off one exchange. If the
    // census only said what it says at exactly 16 ms it would be arithmetic
    // about a rounding error, so sweep it across the whole plausible range
    // — a stack hop is not 48 ms — and report the span.
    let mut low = u64::MAX;
    let mut high = 0;
    for overhead in 0..=48 {
        let (total, contending) = census(overhead, 0);
        assert_eq!(total, 210);
        low = low.min(contending);
        high = high.max(contending);
    }
    assert_eq!(
        (low, high),
        (134, 146),
        "64 % to 70 % of draw pairs contend across the whole range"
    );
}

/// The spacing the post-TX hold actually imposes, and what it leaves.
///
/// The hold is `airtime + DIFS + longest draw`, all three at the running
/// PHY (`interfaces/rnode.rs::tx_hold`). It is shorter than
/// `compute_spacing_ms` by that function's flat 100 ms margin and longer by
/// nothing, so the census it leaves is the honest bound on what the hold
/// buys against #374 — as opposed to what it buys against #36, which is the
/// whole of it: with the hold running, the modem's queue never holds two
/// frames, so no frame can leave inside another's preamble because the
/// firmware flushed them together.
#[test]
fn the_hold_narrows_the_census_without_closing_it() {
    let slot = jitter_slot_ms(BW, SF, CR);
    // Exactly the terms `tx_hold` sums, derived here rather than imported:
    // the interface is a different crate's private function, and a second
    // copy of the arithmetic that agrees is the point.
    let hold = air(PROBE_BYTES) + JITTER_DIFS_SLOTS * slot + (JITTER_CW_SLOTS as u64 - 1) * slot;
    assert_eq!(hold, 501);
    assert!(
        hold < compute_spacing_ms(PROBE_BYTES, BW, SF, CR, preamble()),
        "the hold is the same shape without the flat margin"
    );

    let mut total = 0;
    let mut contending = 0;
    for m in 0..modem_cw_slots() {
        let first_frame_air_end = CSMA_DIFS_MS + m * slot + air(PROBE_BYTES);
        let queued_at = hold.saturating_sub(first_frame_air_end);
        let (sub_total, sub_contending) = census(FAR_END_OVERHEAD_MS, queued_at);
        total += sub_total;
        contending += sub_contending;
    }
    println!(
        "HOLD_CENSUS hold_ms={hold} triples={total} contending={contending} \
         pct={:.1}",
        100.0 * contending as f64 / total as f64
    );
    assert_eq!(
        (total, contending),
        (3150, 1328),
        "42.2 % of triples still contend — MORE than the 17.8 % the wider \
         `compute_spacing_ms` leaves, because the hold is 148 ms shorter. \
         The hold is priced to empty the modem's queue, not to clear the far \
         end's answer window, and what it leaves is what the rig series \
         measures: our second frame still draws against the answer to our \
         first, it just no longer leaves deaf behind it"
    );
}

#[test]
fn airtime_aware_spacing_narrows_the_census_without_closing_it() {
    let spacing = compute_spacing_ms(PROBE_BYTES, BW, SF, CR, preamble());
    assert_eq!(
        spacing,
        air(PROBE_BYTES) + CSMA_DIFS_MS + CSMA_MAX_CW_MS + 100,
        "airtime + DIFS + the whole contention window + margin"
    );
    assert_eq!(spacing, 649);

    // Now the arrival of the second frame DOES depend on the draws: the
    // spacing runs from our serial write, while the answer window runs from
    // the end of our airtime, and what separates those two anchors is the
    // modem's own draw for the first frame. So the census gains a dimension.
    let slot = jitter_slot_ms(BW, SF, CR);
    let mut total = 0;
    let mut contending = 0;
    for m in 0..modem_cw_slots() {
        let first_frame_air_end = CSMA_DIFS_MS + m * slot + air(PROBE_BYTES);
        let queued_at = spacing.saturating_sub(first_frame_air_end);
        let (sub_total, sub_contending) = census(FAR_END_OVERHEAD_MS, queued_at);
        total += sub_total;
        contending += sub_contending;
    }
    assert_eq!(
        (total, contending),
        (3150, 559),
        "17.8 % of triples still contend: a spacing measured from the serial \
         write cannot cover a window that opens at the end of the airtime"
    );

    // What WOULD close it, and what it costs. Searched rather than derived,
    // because the answer is the worst case over three draws at once.
    let closes = (0u64..3_000)
        .find(|&s| {
            (0..modem_cw_slots()).all(|m| {
                let air_end = CSMA_DIFS_MS + m * slot + air(PROBE_BYTES);
                census(FAR_END_OVERHEAD_MS, s.saturating_sub(air_end)).1 == 0
            })
        })
        .expect("some spacing clears a bounded window");
    assert_eq!(closes, 971);
    assert!(
        closes > PROBE_INTERVAL_MS / 2,
        "{closes}ms of spacing is {:.0}% of this cell's {PROBE_INTERVAL_MS}ms \
         probe interval, and {:.0}% of a Python peer's first-hop packet \
         timeout ({}ms) — both of which a burst continuation would pay",
        100.0 * closes as f64 / PROBE_INTERVAL_MS as f64,
        100.0 * closes as f64 / python_first_hop_timeout_ms() as f64,
        python_first_hop_timeout_ms(),
    );
}
