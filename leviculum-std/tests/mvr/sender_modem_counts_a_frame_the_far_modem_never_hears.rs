//! mvr for the `bench_single_pair_medium_rnode_only` red of the 2026-09-22
//! full hardware run (bench log `hw-vollauf4-b74027aa.log` line 2265,
//! corpus 056e621):
//!
//! ```text
//! VERDICT bench_single_pair_medium_rnode_only RED step 4 (benchmark) failed:
//!   sender -> receiver.probe: sent=19 received=18 pdr=94.7%
//! ```
//!
//! NOT THE MECHANISM THE SIBLING RED HAS. `bench_single_pair_fast` went red
//! in the same run and `burst_continuation_contends_inside_its_own_answer`
//! pins it: the sender's own announce rebroadcast, queued behind the probe as
//! a burst continuation, contends in the window the far end needs to answer.
//! That cannot be this one. The lost exchange here is probe 7, forwarded at
//! 18:18:52.017 and written to the modem at 18:18:52.386, and the sender put
//! nothing else on the air for 9.6 s afterwards or for 1.24 s before —
//! the last announce in the cell was at 18:18:15.473, 37 s earlier
//! (`logs/bench_single_pair_medium_rnode_only_2026-09-22T18-20-07Z.log`).
//! One frame, an idle channel, no second frame to collide with.
//!
//! WHICH LEG WAS LOST. The probe, not the proof. The receiver logged 18
//! `PROOF_GEN` for 19 probes and has no `PKT_RX` and no `LORA_RX` at all for
//! `ph=bf541f49d530283a`: nothing reached its host. The sender's own record
//! of that frame is complete and in order --
//! `PKT_RX(Local) -> PATH_LOOKUP -> PKT_FORWARD -> PKT_TX -> send queue 1
//! packet -> acquisition jitter 360ms -> TX 131 bytes to serial` -- and
//! `port.write_all` + `port.flush` (`interfaces/rnode.rs:1901,1914`) both
//! returned, so the whole frame reached the firmware.
//!
//! WHAT THE TWO MODEMS SAY. The RNode firmware reports `airtime_short` and
//! `channel_load_short` about every 1.16 s, and both are computed from
//! constants this file mirrors. `airtime_short` is what the firmware's own
//! `add_airtime` charged for frames IT transmitted; `channel_load_short` is
//! `local_channel_util + airtime`, where `local_channel_util` is the fraction
//! of the last `DCD_SAMPLES * STATUS_INTERVAL_MS` = 7500 ms in which the
//! 3 ms carrier-detect poll saw `LoRa->dcd()` true
//! (`RNode_Firmware.ino:1451-1459`, `Config.h:176-184`).
//!
//! Across the lost probe the sender's modem stepped `airtime_short`
//! 6.10 -> 7.63 (18:18:52.208 -> 18:18:52.704): exactly one 131-byte SF7
//! frame, and `sx127x::endPacket` busy-waits on TX_DONE with no timeout
//! (`reference/RNode_Firmware/sx127x.cpp:183-195`), so the counter is only
//! reached after the modem reports the packet clocked out.
//!
//! The SF7 cell cannot say what the far modem heard, and this file does not
//! pretend it can: one 131-byte frame is 3.05 pp of that receiver's
//! carrier-detect window, and its report-to-report step in the same 60 s
//! reaches 6.46 pp on its own. The +2.28 observed across the lost frame is
//! inside the instrument's own jitter.
//!
//! THE SIBLING RED THAT DOES SAY IT. `bench_single_pair_slow_ca_rnode_only`
//! went red 10 h later on 648650a3
//! (bench log `hw-vollauf4-648650a31b29d94ce12679d7c27807d7928a5273.log`
//! line 2014, 6 of 7, corpus 9887b3f) with the same shape -- probe lost,
//! sender's host record complete, receiver's host record empty -- on a
//! channel so idle that the instrument is clean: SF10/CR8 makes one probe
//! 650 of the 2500 carrier samples, and the cell's baseline
//! `channel_load_short` is a flat 0.00.
//!
//! ```text
//! 04:26:59.354 sender   TX 131 bytes to serial        <- probe 6, lost
//! 04:27:01.566 sender   CHTM airtime_short=13.00      <- 0.00 before it
//! 04:27:00.235 receiver CHTM channel_load_short=0.00  \
//!   ... 26 consecutive reports, none more than 1.166 s apart ...
//! 04:27:27.572 receiver CHTM channel_load_short=0.00  /
//! 04:27:28.486 sender   TX 131 bytes to serial        <- probe 7, delivered
//! 04:27:29.895 receiver CHTM channel_load_short=13.32
//! 04:27:31.066 receiver CHTM channel_load_short=22.60
//! 04:27:30.729 receiver RX 131 bytes rssi=-43 snr=10
//! ```
//!
//! The delivered probe 29.1 s later, on identical settings, drove the
//! receiver's carrier sampler from 0.00 to 22.60. The lost one drove it
//! nowhere, for 27.3 s -- more than three full sampler windows -- while the
//! firmware loop that feeds that sampler never gapped by more than one report
//! interval, so it was not stalled and the frame was not merely sampled late.
//!
//! WHAT IS THEREFORE ATTRIBUTED. The loss is below the host/modem boundary at
//! both ends. On the sender our stack queued, jittered, wrote and flushed the
//! frame and the modem charged itself the airtime for transmitting it; on the
//! receiver the modem never registered a carrier and never handed a
//! `CMD_DATA` frame up. Nothing our code does sits between those two facts.
//! Two branches remain and this run cannot separate them: the sender's RF
//! chain did not radiate the packet its modem reported as sent, or the
//! receiver's radio was not in RX while it believed it was. A third board
//! listening on the channel decides it, which is a rig measurement, not a
//! desk one.
//!
//! WHAT THIS FILE PINS, so that the attribution stays falsifiable: the
//! arithmetic that makes those two readings evidence. If the mirrored
//! firmware constants or the airtime model drift, these numbers stop matching
//! the four `airtime_short` steps the run recorded, and the attribution has
//! to be re-argued rather than quietly inherited.
//!
//! Sans-hardware, deterministic, milliseconds.

use leviculum_core::rnode::{airtime_ms_with_preamble, derive_preamble_symbols};

/// `[radio]` of `hardware/bench_single_pair_medium_rnode_only.toml`.
const MEDIUM_BW: u32 = 125_000;
const MEDIUM_SF: u8 = 7;
const MEDIUM_CR: u8 = 5;

/// `[radio]` of `hardware/bench_single_pair_slow_ca_rnode_only.toml`.
const SLOW_BW: u32 = 125_000;
const SLOW_SF: u8 = 10;
const SLOW_CR: u8 = 8;

/// What the firmware charges itself for, in bytes: our 131-byte probe or
/// 115-byte proof plus the one header byte `transmit()` writes ahead of the
/// payload (`RNode_Firmware.ino:724`).
const PROBE_WRITTEN: u32 = 132;
const PROOF_WRITTEN: u32 = 116;

// ---- RNode firmware constants, mirrored with their source lines. ----

/// `STATUS_INTERVAL_MS` (`Config.h:176`): the carrier-detect poll period.
const STATUS_INTERVAL_MS: u64 = 3;

/// `DCD_SAMPLES` (`Config.h:178`): how many of those polls the channel
/// utilisation is averaged over.
const DCD_SAMPLES: u64 = 2500;

/// `AIRTIME_BINLEN_MS` (`Config.h:183`), which is also the carrier sampler's
/// window: 7500 ms.
const AIRTIME_BINLEN_MS: u64 = STATUS_INTERVAL_MS * DCD_SAMPLES;

/// `PHY_HEADER_LORA_SYMBOLS` / `PHY_CRC_LORA_BITS` (`Config.h:82-83`).
const PHY_HEADER_LORA_SYMBOLS: f64 = 20.0;
const PHY_CRC_LORA_BITS: f64 = 16.0;

/// The airtime the firmware's `add_airtime` charges for one transmitted
/// frame, in ms (`RNode_Firmware.ino:654-665`, SX1276 branch — both benches
/// run on T-Beams). Deliberately NOT our own `airtime_ms_with_preamble`: the
/// point is to reproduce the counter the run recorded, not our model of the
/// air.
fn firmware_airtime_cost_ms(written: u32, bw: u32, sf: u8, cr: u8, preamble: u16) -> f64 {
    let symbol_time_ms = (1u64 << sf) as f64 / bw as f64 * 1000.0;
    // `lora_low_datarate` is set above a 16 ms symbol time
    // (`Utilities.h`, setSpreadingFactor path); neither bench reaches it.
    let ldr = if symbol_time_ms > 16.0 { 1.0 } else { 0.0 };

    let mut symbols =
        8.0 * written as f64 + PHY_CRC_LORA_BITS - 4.0 * sf as f64 + 8.0 + PHY_HEADER_LORA_SYMBOLS;
    symbols /= 4.0 * (sf as f64 - 2.0 * ldr);
    symbols *= cr as f64;
    symbols += preamble as f64 + 0.25 + 8.0;
    symbols * symbol_time_ms
}

/// `airtime` as the host reads it in `airtime_short`: the current and
/// previous bin over two bin lengths (`RNode_Firmware.ino:698`), in percent.
///
/// The bins are `uint16_t` (`Config.h:186`) and `add_airtime` adds a float
/// into them (`RNode_Firmware.ino:687`), so the charge is truncated to whole
/// milliseconds before it is ever divided. Modelling that costs one line and
/// buys 0.005 pp of agreement with the run on two of the four frames.
fn short_airtime_pct(cost_ms: f64) -> f64 {
    let charged_ms = cost_ms as u16;
    charged_ms as f64 / (2.0 * AIRTIME_BINLEN_MS as f64) * 100.0
}

/// How many of the sampler's 2500 slots a frame of this cost occupies.
fn dcd_samples_occupied(cost_ms: f64) -> u64 {
    (cost_ms / STATUS_INTERVAL_MS as f64) as u64
}

/// What `local_channel_util` would read for a frame of this cost, in percent
/// — the number `channel_load_short` carries when the modem is not itself
/// transmitting.
fn channel_util_pct(cost_ms: f64) -> f64 {
    cost_ms / AIRTIME_BINLEN_MS as f64 * 100.0
}

fn medium_preamble() -> u16 {
    derive_preamble_symbols(MEDIUM_SF, MEDIUM_CR, MEDIUM_BW)
}

fn slow_preamble() -> u16 {
    derive_preamble_symbols(SLOW_SF, SLOW_CR, SLOW_BW)
}

/// The host reads `airtime_short` as `(uint16_t)(airtime*100*100)`
/// (`Utilities.h:961`) — a floor, not a rounding — so a recorded value sits
/// up to one quantum of 0.01 pp below the true one. A model that claims to
/// explain a step has to land inside that quantum.
const REPORT_QUANTUM_PCT: f64 = 0.01;

fn assert_pct(observed: f64, modelled: f64, what: &str) {
    assert!(
        (observed - modelled).abs() < REPORT_QUANTUM_PCT,
        "{what}: the run recorded {observed:.2}%, the model says {modelled:.4}%"
    );
}

/// Every `airtime_short` step the two cells recorded, reproduced from the
/// firmware's own cost function. Four frames, two PHYs, two sizes: if this
/// holds, "the sender's modem charged itself for exactly one 131-byte frame"
/// is a reading and not an inference.
#[test]
fn the_sender_modem_counted_exactly_one_frame_of_airtime() {
    assert_eq!(medium_preamble(), 24, "SF7/CR5/BW125 preamble symbols");
    assert_eq!(slow_preamble(), 18, "SF10/CR8/BW125 preamble symbols");

    // bench_single_pair_medium_rnode_only, 18:18:52.208 -> 18:18:52.704,
    // the step across the LOST probe.
    assert_pct(
        1.53,
        short_airtime_pct(firmware_airtime_cost_ms(
            PROBE_WRITTEN,
            MEDIUM_BW,
            MEDIUM_SF,
            MEDIUM_CR,
            medium_preamble(),
        )),
        "SF7 probe: sender airtime_short 6.10 -> 7.63",
    );

    // Same cell, the receiver's own proof: 4.10 -> 5.46 at 18:18:50.766.
    assert_pct(
        1.36,
        short_airtime_pct(firmware_airtime_cost_ms(
            PROOF_WRITTEN,
            MEDIUM_BW,
            MEDIUM_SF,
            MEDIUM_CR,
            medium_preamble(),
        )),
        "SF7 proof: receiver airtime_short 4.10 -> 5.46",
    );

    // bench_single_pair_slow_ca_rnode_only, 04:27:01.566: 0.00 -> 13.00,
    // the step across ITS lost probe.
    assert_pct(
        13.00,
        short_airtime_pct(firmware_airtime_cost_ms(
            PROBE_WRITTEN,
            SLOW_BW,
            SLOW_SF,
            SLOW_CR,
            slow_preamble(),
        )),
        "SF10 probe: sender airtime_short 0.00 -> 13.00",
    );

    // Same cell, the receiver's proof for the NEXT probe: 0.00 -> 11.61.
    assert_pct(
        11.61,
        short_airtime_pct(firmware_airtime_cost_ms(
            PROOF_WRITTEN,
            SLOW_BW,
            SLOW_SF,
            SLOW_CR,
            slow_preamble(),
        )),
        "SF10 proof: receiver airtime_short 0.00 -> 11.61",
    );
}

/// The load-bearing step of the attribution: at SF10 the far modem's carrier
/// sampler had the resolution to see the frame many times over, so 26
/// consecutive zeros are a measurement of silence and not a blind spot.
#[test]
fn the_far_modem_carrier_sampler_could_not_have_missed_the_slow_frame() {
    let cost = firmware_airtime_cost_ms(PROBE_WRITTEN, SLOW_BW, SLOW_SF, SLOW_CR, slow_preamble());

    let occupied = dcd_samples_occupied(cost);
    assert_eq!(occupied, 650, "carrier-detect polls the frame spans");
    assert!(
        occupied > DCD_SAMPLES / 10,
        "the frame fills {occupied} of {DCD_SAMPLES} slots — over a tenth of \
         the whole window"
    );

    // One poll is the quantum the reading is built from; the frame is three
    // orders of magnitude above it.
    let one_sample_pct = 100.0 / DCD_SAMPLES as f64;
    assert!((one_sample_pct - 0.04).abs() < 1e-9);
    let modelled = channel_util_pct(cost);
    assert!(
        (modelled - 26.02).abs() < 0.01,
        "a heard SF10 probe reads {modelled:.2}% of the sampler window"
    );

    // And that is what the DELIVERED probe 29.1 s later actually produced:
    // 0.00 -> 13.32 -> 22.60, the 22.60 sampled while the frame was still
    // radiating. The lost probe produced 0.00, 26 reports running.
    let observed_on_the_delivered_probe = 22.60;
    assert!(
        observed_on_the_delivered_probe > modelled * 0.8,
        "the delivered probe drove the sampler to {observed_on_the_delivered_probe}%, \
         within a sampling phase of the modelled {modelled:.2}%"
    );

    // 26 reports at no more than 1.166 s apart is 27.3 s of continuous
    // sampling, more than three full windows. A frame cannot be sampled
    // that late.
    let reports = 26u64;
    let worst_gap_ms = 1_166u64;
    assert!(
        reports * worst_gap_ms > 3 * AIRTIME_BINLEN_MS,
        "the zero run covers {} ms, against a {AIRTIME_BINLEN_MS} ms window",
        reports * worst_gap_ms
    );
}

/// The honest negative. The SF7 cell whose red opened this cannot decide what
/// its far modem heard, and nothing here should be read as if it could.
#[test]
fn the_sf7_cell_cannot_decide_what_its_far_modem_heard() {
    let cost = firmware_airtime_cost_ms(
        PROBE_WRITTEN,
        MEDIUM_BW,
        MEDIUM_SF,
        MEDIUM_CR,
        medium_preamble(),
    );
    let modelled = channel_util_pct(cost);
    assert!(
        (modelled - 3.05).abs() < 0.01,
        "one SF7 probe is {modelled:.2}% of the sampler window"
    );

    // The receiver's own report-to-report step in the same 60 s window, with
    // no change in traffic: 12.98 -> 6.52 at 18:18:56.645.
    let worst_quiet_step = 12.98 - 6.52;
    assert!(
        worst_quiet_step > modelled,
        "the cell's own instrument moves {worst_quiet_step:.2} pp between \
         reports, more than the {modelled:.2} pp a frame contributes — the \
         +2.28 across the lost probe decides nothing"
    );
}

/// Our own airtime model is not the firmware's cost model, and the gap is
/// worth having pinned: anything that prices a wait against a frame's air
/// (`interfaces/rnode.rs::tx_hold`, which since 2026-09-23 holds the next
/// frame for the previous one's airtime, and `compute_spacing_ms`) is using
/// the longer of the two. For a hold that is the safe direction — it waits
/// past the end of the air rather than into it — and the margin is 66 ms on
/// an SF10 probe, 3 % of the frame.
#[test]
fn our_airtime_model_runs_above_the_firmware_cost_model() {
    let ours_slow =
        airtime_ms_with_preamble(PROBE_WRITTEN, SLOW_BW, SLOW_SF, SLOW_CR, slow_preamble());
    let firmware_slow =
        firmware_airtime_cost_ms(PROBE_WRITTEN, SLOW_BW, SLOW_SF, SLOW_CR, slow_preamble());
    assert_eq!(ours_slow, 2018, "our SF10 probe airtime");
    assert!(
        (firmware_slow - 1952.0).abs() < 1.0,
        "the firmware charges {firmware_slow:.0} ms for the same frame"
    );

    let ours_medium = airtime_ms_with_preamble(
        PROBE_WRITTEN,
        MEDIUM_BW,
        MEDIUM_SF,
        MEDIUM_CR,
        medium_preamble(),
    );
    let firmware_medium = firmware_airtime_cost_ms(
        PROBE_WRITTEN,
        MEDIUM_BW,
        MEDIUM_SF,
        MEDIUM_CR,
        medium_preamble(),
    );
    assert_eq!(ours_medium, 237, "our SF7 probe airtime");
    assert!(
        (firmware_medium - 229.0).abs() < 1.0,
        "the firmware charges {firmware_medium:.0} ms for the same frame"
    );

    // Ours is the conservative side at both PHYs: it rounds the payload
    // symbol count up where the firmware carries the fraction, so a spacing
    // computed from it never under-waits a frame the modem is still sending.
    assert!(ours_slow as f64 > firmware_slow);
    assert!(ours_medium as f64 > firmware_medium);
}
