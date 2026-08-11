//! SX126x interrupt policy: which IRQs the chip is allowed to LATCH, which
//! of the latched ones drive a DIO line, and when a bounded RX window has to
//! be extended because a frame is still on the air.
//!
//! # Why this lives in core and not in the driver
//!
//! The driver is `leviculum-nrf::sx1262`, which cross-compiles to
//! `thumbv7em-none-eabihf` and has no host test target: nothing in it can be
//! observed failing without a board on the bench. The RX-extend guard was
//! written there, shipped, and was dead for its whole life — its precondition
//! could never be true, because the same function configured the chip to not
//! latch the two bits the precondition tests (Codeberg #144). A guard nobody
//! can run a test against is exactly the defect this module exists to make
//! impossible: the decision is a pure function here, the driver holds only
//! SPI.
//!
//! # The latch mask is not a routing mask
//!
//! `SetDioIrqParams` (opcode 0x08) takes four 16-bit masks. The FIRST one is
//! not a DIO routing mask — it gates whether an interrupt is recorded in the
//! IRQ status register at all (datasheet §13.3.1: an IRQ absent from
//! `IrqMask` is never raised, so `GetIrqStatus` never shows it). The three
//! that follow route the already-latched interrupts to DIO1, DIO2 and DIO3.
//!
//! This is why the reference RNode firmware sets the first mask to `0xFFFF`
//! and only the DIO mask narrowly (`sx126x.cpp:632-635`): its carrier-detect
//! path reads PREAMBLE_DET and HEADER_DET back out of the status register
//! (`sx126x.cpp:502-505`) without ever wanting them to wake a pin. Anything
//! that reads a bit back must enable that bit here first.

/// TxDone.
pub const IRQ_TX_DONE: u16 = 0x0001;
/// RxDone.
pub const IRQ_RX_DONE: u16 = 0x0002;
/// PreambleDetected: a LoRa preamble was seen. Read back, never routed.
pub const IRQ_PREAMBLE_DETECTED: u16 = 0x0004;
/// HeaderValid: an explicit header passed its CRC. Read back, never routed.
pub const IRQ_HEADER_VALID: u16 = 0x0010;
/// CrcErr: the payload CRC failed.
pub const IRQ_CRC_ERR: u16 = 0x0040;
/// CadDone.
pub const IRQ_CAD_DONE: u16 = 0x0080;
/// CadDetected.
pub const IRQ_CAD_DETECTED: u16 = 0x0100;
/// Timeout: the RX or TX hardware timer expired.
pub const IRQ_TIMEOUT: u16 = 0x0200;

/// Every interrupt latches. This is the only correct value for the first
/// `SetDioIrqParams` argument in any operation whose code reads a status bit
/// it does not also route to a DIO line, and it costs nothing otherwise: an
/// interrupt that latches but is routed nowhere cannot wake the MCU.
pub const IRQ_LATCH_ALL: u16 = 0xFFFF;

/// Routed to DIO1 during a transmission.
pub const DIO1_TX: u16 = IRQ_TX_DONE | IRQ_TIMEOUT;
/// Routed to DIO1 during a reception.
pub const DIO1_RX: u16 = IRQ_RX_DONE | IRQ_CRC_ERR | IRQ_TIMEOUT;
/// Routed to DIO1 during a channel-activity detection.
pub const DIO1_CAD: u16 = IRQ_CAD_DONE | IRQ_CAD_DETECTED;

/// The bits [`rx_extend_ms`] reads out of the status register. They are
/// never routed to DIO1 — a preamble must not wake the wait, it must only be
/// visible to it afterwards — so they reach the guard through the latch mask
/// or not at all.
pub const RX_EXTEND_INPUTS: u16 = IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID;

/// The eight `SetDioIrqParams` argument bytes, big-endian per mask.
///
/// `latch` is the enable mask (see the module docs); `dio1`, `dio2` and
/// `dio3` route latched interrupts to pins.
pub fn dio_irq_params(latch: u16, dio1: u16, dio2: u16, dio3: u16) -> [u8; 8] {
    [
        (latch >> 8) as u8,
        latch as u8,
        (dio1 >> 8) as u8,
        dio1 as u8,
        (dio2 >> 8) as u8,
        dio2 as u8,
        (dio3 >> 8) as u8,
        dio3 as u8,
    ]
}

/// `SetDioIrqParams` arguments for a transmission.
pub fn tx_irq_params() -> [u8; 8] {
    dio_irq_params(IRQ_LATCH_ALL, DIO1_TX, 0, 0)
}

/// `SetDioIrqParams` arguments for a reception.
///
/// Everything latches so the RX-extend guard can read PreambleDetected and
/// HeaderValid back; only [`DIO1_RX`] wakes the pin.
pub fn rx_irq_params() -> [u8; 8] {
    dio_irq_params(IRQ_LATCH_ALL, DIO1_RX, 0, 0)
}

/// `SetDioIrqParams` arguments for a channel-activity detection.
pub fn cad_irq_params() -> [u8; 8] {
    dio_irq_params(IRQ_LATCH_ALL, DIO1_CAD, 0, 0)
}

/// How much longer to wait for a reception that is still in progress, or
/// `None` to conclude the RX window now.
///
/// The software wait around a bounded RX window is sized from the window,
/// but `SetStopRxTimerOnPreambleDetect` means the window bounds the PREAMBLE
/// wait only: once a preamble arrives the hardware timer stops and the chip
/// receives to the end of the frame however long that takes. At slow
/// spreading factors a single frame is seconds of airtime, so the software
/// wait can expire with the frame still arriving. Concluding there — the
/// pre-#144 behaviour — puts the radio in standby mid-frame and drops a
/// packet that was about to be delivered whole.
///
/// So: if neither RxDone nor Timeout has fired but a preamble or header did
/// latch, the reception is live and gets one more max-single-frame airtime.
/// One extension only, so a preamble from a frame that never completes still
/// terminates the window.
///
/// `bw_hz` is zero before `configure_lora` has run, which is the one state
/// in which the airtime is not computable; the window simply concludes.
///
/// `preamble_symbols` is the programmed preamble of the link (both ends run
/// the same derived value): the frame still on the air carries it, so an
/// extension sized from the preamble-8 formula would expire up to
/// (preamble-8)*t_sym before the frame ends — 328 ms at SF12/BW125.
pub fn rx_extend_ms(
    flags: u16,
    bw_hz: u32,
    sf: u8,
    cr_denom: u8,
    preamble_symbols: u16,
) -> Option<u64> {
    if bw_hz == 0 {
        return None;
    }
    if flags & (IRQ_RX_DONE | IRQ_TIMEOUT) != 0 {
        return None;
    }
    if flags & RX_EXTEND_INPUTS == 0 {
        return None;
    }
    Some(
        crate::rnode::airtime_ms_with_preamble(
            (crate::rnode::MAX_SINGLE_PAYLOAD + 1) as u32,
            bw_hz,
            sf,
            cr_denom,
            preamble_symbols,
        )
        .max(1),
    )
}

/// Slack added on top of a computed on-air time when sizing the software
/// timeout around a started radio operation (TX completion, CAD completion).
///
/// It has to absorb only what is not airtime once the operation has been
/// keyed: TCXO start (~4 ms, the `SetDIO3AsTcxoCtrl` 0x0000FF timeout), PA
/// ramp (200 µs), the SPI+BUSY cost of the surrounding commands
/// (sub-millisecond each), and executor wake latency after the DIO1 edge.
/// CSMA waiting is out of scope: DIFS and contention backoff complete before
/// the driver enters `transmit()`. 250 ms is an order of magnitude above the
/// sum of those latencies while staying small against one slow-SF frame.
pub const SOFT_TIMEOUT_SLACK_MS: u64 = 250;

/// Bounded software wait when `bw_hz` is 0 (no `configure_lora` yet), the one
/// state in which no airtime is computable. An unconfigured radio cannot have
/// a frame on the air, so nothing real is aborted; this only bounds the wait
/// for a chip that will never raise the IRQ.
pub const UNCONFIGURED_WAIT_MS: u32 = 1_000;

/// Software timeout for one transmitted frame: its on-air time at the live
/// modulation plus [`SOFT_TIMEOUT_SLACK_MS`].
///
/// Replaces a fixed 5000 ms that predated SF12 support: a 184-byte announce
/// at SF12/BW125/CR4:8/preamble-18 is 10.69 s of airtime, so the driver's
/// expired wait put the chip in standby mid-air and no such frame was ever
/// completed — same family as the fixed 500 ms RX abort (#144).
pub fn tx_timeout_ms(
    frame_len: u32,
    bw_hz: u32,
    sf: u8,
    cr_denom: u8,
    preamble_symbols: u16,
) -> u32 {
    if bw_hz == 0 {
        return UNCONFIGURED_WAIT_MS;
    }
    let air =
        crate::rnode::airtime_ms_with_preamble(frame_len, bw_hz, sf, cr_denom, preamble_symbols);
    (air + SOFT_TIMEOUT_SLACK_MS).min(u32::MAX as u64) as u32
}

/// Software timeout for one channel-activity detection: the programmed
/// listening symbols plus the chip's processing tail (about half a symbol,
/// rounded up to one), plus [`SOFT_TIMEOUT_SLACK_MS`].
///
/// Replaces a per-SF millisecond table that assumed BW125: symbol time
/// doubles with every halving of bandwidth, so at SF12/BW31.25 an 8-symbol
/// CAD (1.05 s) overran the table's 800 ms and was aborted on every attempt.
pub fn cad_timeout_ms(bw_hz: u32, sf: u8, cad_symbols: u8) -> u32 {
    if bw_hz == 0 {
        return UNCONFIGURED_WAIT_MS;
    }
    let t_sym_us = (1u64 << sf) * 1_000_000 / bw_hz as u64;
    let cad_us = (cad_symbols as u64 + 1) * t_sym_us;
    (cad_us.div_ceil(1_000) + SOFT_TIMEOUT_SLACK_MS).min(u32::MAX as u64) as u32
}

/// Whether `SetModulationParams` must enable the low-data-rate optimisation
/// for the given modulation.
///
/// LDRO is keyed to symbol duration, not to a spreading-factor/bandwidth-code
/// pair: the SX126x needs it when a symbol stretches past ~16 ms. Both sides
/// of a link must agree — an LDRO mismatch between ends kills decoding
/// entirely — so this replicates the reference RNode firmware's decision
/// bit for bit (`sx126x.cpp:725-729 handleLowDataRate()`, identical in
/// `sx127x.cpp:457-467`):
///
/// ```c
/// if ( long( (1<<_sf) / (getSignalBandwidth()/1000)) > 16)
/// ```
///
/// i.e. integer-millisecond symbol duration strictly above 16. The integer
/// truncation is part of the wire contract: SF11/BW125 and SF12/BW250 sit at
/// exactly 16.384 ms and the reference (and therefore every RNode peer)
/// runs them with LDRO OFF, while the narrow interleaved SX1262 bandwidths
/// (10.42/20.83/41.67 kHz) push even mid spreading factors far past the
/// threshold. `bw_hz` comes from the chip's bandwidth-code table
/// (`bw_code_to_hz`); the quotient `bw_hz / 1000` is identical for the
/// reference's rounded table values, so the decisions coincide on the whole
/// code domain. Predicates over the raw SX1262 register code cannot express
/// this: the code space is not monotonic in bandwidth (0x08-0x0A are the
/// narrowest bandwidths but the numerically largest codes).
///
/// `bw_hz` of 0 (unconfigured, or an unknown code) reports no LDRO.
pub fn ldro_enabled(bw_hz: u32, sf: u8) -> bool {
    let bw_khz = bw_hz as u64 / 1000;
    if bw_khz == 0 {
        return false;
    }
    (1u64 << sf) / bw_khz > 16
}

/// Output powers the high-power PA has a documented `SetPaConfig` setting for
/// (datasheet Table 13-21), ascending.
///
/// These are the only four points the driver can program. Everything else is
/// an approximation, and the whole reason this table is public is that the
/// approximation used to happen silently: `configure_lora` matched 22/20/17
/// and sent a fourth arm's 14 dBm setting for every other value, so a
/// configured 21, 18 or 15 dBm — all of which pass `rnode::validate_config` —
/// transmitted 14 dBm with nothing said.
pub const PA_PROFILES_DBM: [i8; 4] = [14, 17, 20, 22];

/// Which PA profile a requested output power is programmed as.
///
/// The requested value is rounded **down** to the nearest profile: never
/// transmit above what the operator asked for, because the margin between a
/// configured power and the regulatory ceiling is the operator's to spend.
///
/// A request below the lowest profile has nothing to round down to. The PA
/// setting saturates at [`PA_PROFILES_DBM`]`[0]` (14 dBm) and the caller is
/// expected to log the substitution — the driver programs `SetTxParams` at the
/// profile's power, so sub-14 dBm requests are not reachable by profile
/// selection alone and would need the `SetTxParams` power field driven from
/// the request instead. `lnflash` accepts -9..=22 dBm
/// (`lnflash/src/radio.rs:79`), so this case is reachable from a real flash,
/// and it is the one direction in which the programmed power exceeds the
/// request. Saying so beats the silent 14 dBm that preceded it.
pub fn pa_profile_dbm(requested_dbm: i8) -> i8 {
    let mut chosen = PA_PROFILES_DBM[0];
    for profile in PA_PROFILES_DBM {
        if profile <= requested_dbm {
            chosen = profile;
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SF10 / 62.5 kHz / 4:5 — the slow end of the lab rig, where a single
    /// frame is seconds of airtime and the guard matters.
    const SLOW: (u32, u8, u8) = (62_500, 10, 5);
    /// The rig's derived programmed preamble at the SLOW profile
    /// (`rnode::derive_preamble_symbols(10, 5, 62_500)`).
    const SLOW_PREAMBLE: u16 = 18;

    fn latch_mask(params: [u8; 8]) -> u16 {
        u16::from_be_bytes([params[0], params[1]])
    }

    fn dio1_mask(params: [u8; 8]) -> u16 {
        u16::from_be_bytes([params[2], params[3]])
    }

    /// The reachability test. It is the whole point of the module: the guard
    /// reads PreambleDetected and HeaderValid, so the reception's latch mask
    /// has to admit them. Against the pre-#144 mask (`DIO1_RX` in both
    /// positions) this fails, which is the first time that guard's
    /// precondition was ever checked for being satisfiable at all.
    #[test]
    fn rx_latch_mask_admits_what_the_extend_guard_reads() {
        let latch = latch_mask(rx_irq_params());
        assert_eq!(
            latch & RX_EXTEND_INPUTS,
            RX_EXTEND_INPUTS,
            "rx latch mask {latch:#06x} drops bits {RX_EXTEND_INPUTS:#06x}, \
             which rx_extend_ms tests: the guard could never fire"
        );
    }

    /// The other half of the same fact: those bits must not wake DIO1. If
    /// they did, the software wait would return on the preamble instead of
    /// on the frame, and the extension would be measuring the wrong thing.
    #[test]
    fn rx_extend_inputs_are_not_routed_to_dio1() {
        assert_eq!(dio1_mask(rx_irq_params()) & RX_EXTEND_INPUTS, 0);
        assert_eq!(dio1_mask(rx_irq_params()), DIO1_RX);
    }

    #[test]
    fn tx_and_cad_latch_everything_and_route_narrowly() {
        assert_eq!(latch_mask(tx_irq_params()), IRQ_LATCH_ALL);
        assert_eq!(dio1_mask(tx_irq_params()), DIO1_TX);
        assert_eq!(latch_mask(cad_irq_params()), IRQ_LATCH_ALL);
        assert_eq!(dio1_mask(cad_irq_params()), DIO1_CAD);
    }

    #[test]
    fn dio2_and_dio3_are_never_routed() {
        for params in [tx_irq_params(), rx_irq_params(), cad_irq_params()] {
            assert_eq!(&params[4..8], &[0, 0, 0, 0]);
        }
    }

    #[test]
    fn params_are_big_endian_per_mask() {
        assert_eq!(
            dio_irq_params(0x1234, 0x5678, 0x9abc, 0xdef0),
            [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]
        );
    }

    /// The guard firing, observed: a preamble latched, nothing concluded the
    /// window, so the wait is extended by one max-single-frame airtime.
    #[test]
    fn preamble_alone_extends_the_window() {
        let (bw, sf, cr) = SLOW;
        let extend = rx_extend_ms(IRQ_PREAMBLE_DETECTED, bw, sf, cr, SLOW_PREAMBLE)
            .expect("guard must fire");
        assert_eq!(
            extend,
            crate::rnode::airtime_ms_with_preamble(
                (crate::rnode::MAX_SINGLE_PAYLOAD + 1) as u32,
                bw,
                sf,
                cr,
                SLOW_PREAMBLE
            )
        );
        // Worth stating as a number: at SF10/62.5k this is seconds, which is
        // why expiring the software wait mid-frame lost whole packets.
        assert!(
            extend > 1_000,
            "expected a multi-second frame, got {extend}ms"
        );
    }

    #[test]
    fn header_alone_extends_the_window() {
        let (bw, sf, cr) = SLOW;
        assert!(rx_extend_ms(IRQ_HEADER_VALID, bw, sf, cr, SLOW_PREAMBLE).is_some());
    }

    #[test]
    fn a_concluded_window_is_not_extended() {
        let (bw, sf, cr) = SLOW;
        // RxDone wins even with a preamble still latched from the same frame.
        assert_eq!(
            rx_extend_ms(
                IRQ_PREAMBLE_DETECTED | IRQ_RX_DONE,
                bw,
                sf,
                cr,
                SLOW_PREAMBLE
            ),
            None
        );
        assert_eq!(
            rx_extend_ms(
                IRQ_PREAMBLE_DETECTED | IRQ_TIMEOUT,
                bw,
                sf,
                cr,
                SLOW_PREAMBLE
            ),
            None
        );
    }

    #[test]
    fn a_silent_channel_is_not_extended() {
        let (bw, sf, cr) = SLOW;
        assert_eq!(rx_extend_ms(0, bw, sf, cr, SLOW_PREAMBLE), None);
        // CAD bits are not evidence of an inbound frame.
        assert_eq!(
            rx_extend_ms(IRQ_CAD_DETECTED, bw, sf, cr, SLOW_PREAMBLE),
            None
        );
    }

    #[test]
    fn an_unconfigured_radio_is_not_extended() {
        assert_eq!(
            rx_extend_ms(IRQ_PREAMBLE_DETECTED, 0, 10, 5, SLOW_PREAMBLE),
            None
        );
    }

    /// The defect instance: a 184-byte announce at SF12/BW125/CR4:8 with the
    /// rig's programmed 18-symbol preamble is 10.69 s on the air. The fixed
    /// 5000 ms wait aborted every one of them (six 2026-07-30 captures, 10/10
    /// `TX err frame 0: Timeout`, zero `TX done`). The timeout must cover the
    /// frame, with nothing on top but the stated slack.
    #[test]
    fn tx_timeout_covers_the_sf12_announce() {
        let air = crate::rnode::airtime_ms_with_preamble(184, 125_000, 12, 8, 18);
        assert!(air >= 10_690, "premise drifted: announce airtime {air}ms");
        assert_eq!(
            tx_timeout_ms(184, 125_000, 12, 8, 18) as u64,
            air + SOFT_TIMEOUT_SLACK_MS
        );
    }

    /// The other direction: at SF7 the same frame is ~308 ms, and the timeout
    /// must not smuggle in a multi-second wait that would mask a wedged radio.
    #[test]
    fn tx_timeout_stays_sane_at_sf7() {
        let t = tx_timeout_ms(184, 125_000, 7, 5, 18);
        assert!(t < 1_000, "wedged-radio detection took {t}ms");
    }

    /// Why the defect only ever surfaced at SF12: the largest SF10 rig frame
    /// (131 B) is ~2 s of airtime, comfortably inside the old fixed 5000 ms.
    #[test]
    fn sf10_frames_fit_the_old_fixed_timeout() {
        let air = crate::rnode::airtime_ms_with_preamble(131, 125_000, 10, 8, 18);
        assert!(
            air > 1_000 && air < 5_000,
            "boundary premise drifted: {air}ms"
        );
        assert!(tx_timeout_ms(131, 125_000, 10, 8, 18) as u64 >= air + SOFT_TIMEOUT_SLACK_MS);
    }

    /// The programmed preamble is charged: 10 extra symbols at SF12/BW125 are
    /// 327 ms, more than the whole slack, so a preamble-blind timeout would
    /// already have spent its margin before the payload started.
    #[test]
    fn tx_timeout_charges_the_programmed_preamble() {
        let pre18 = tx_timeout_ms(184, 125_000, 12, 8, 18) as u64;
        let pre8 = tx_timeout_ms(184, 125_000, 12, 8, 8) as u64;
        assert!(pre18 - pre8 >= 327, "preamble delta {}ms", pre18 - pre8);
    }

    #[test]
    fn an_unconfigured_radio_gets_a_bounded_wait() {
        assert_eq!(tx_timeout_ms(184, 0, 12, 8, 18), UNCONFIGURED_WAIT_MS);
        assert_eq!(cad_timeout_ms(0, 12, 8), UNCONFIGURED_WAIT_MS);
    }

    /// CAD listening time is symbols × symbol time, so it scales with SF AND
    /// bandwidth. The table this replaces assumed BW125: 8 CAD symbols at
    /// SF12/BW31.25 are 1.05 s against the table's 800 ms.
    #[test]
    fn cad_timeout_tracks_symbol_time() {
        let narrow = cad_timeout_ms(31_250, 12, 8) as u64;
        assert!(
            narrow >= 1_049 + SOFT_TIMEOUT_SLACK_MS,
            "SF12/BW31.25 CAD window {narrow}ms cannot cover 8 symbols"
        );
        // At the profile the table was built for, the derived value stays at
        // or under the old entry (295 ms of symbols + slack vs 800).
        let sf12_bw125 = cad_timeout_ms(125_000, 12, 8) as u64;
        assert!(
            sf12_bw125 <= 800,
            "SF12/BW125 grew past the old table: {sf12_bw125}ms"
        );
        // Fast SF: symbols are ~1 ms each, the slack dominates, and a wedged
        // radio is still detected in well under a second.
        let sf7 = cad_timeout_ms(125_000, 7, 4) as u64;
        assert!(sf7 < 1_000, "SF7 CAD window {sf7}ms");
    }

    /// The extension tracks the settings rather than being a constant: the
    /// fast end of the rig is tens of milliseconds where the slow end is
    /// seconds. `max(1)` is a floor against a rounded-to-zero airtime turning
    /// the extension into an immediate re-read; no configuration in the
    /// corpus reaches it, which is why it is asserted and not relied on.
    #[test]
    fn the_extension_tracks_the_settings() {
        // 94 is the derived programmed preamble at SF5 — 98.25 on-air symbols
        // (6.3 ms at BW500), on top of the ~34 ms max-frame payload.
        let fast = rx_extend_ms(IRQ_PREAMBLE_DETECTED, 500_000, 5, 5, 94).expect("guard must fire");
        let (bw, sf, cr) = SLOW;
        let slow = rx_extend_ms(IRQ_PREAMBLE_DETECTED, bw, sf, cr, SLOW_PREAMBLE)
            .expect("guard must fire");
        assert_eq!(fast, 41);
        assert!(slow > 100 * fast, "slow={slow}ms fast={fast}ms");
        assert!(fast >= 1);
    }

    /// The defect instance (#150): the SX1262 interleaved bandwidth codes
    /// 0x08/0x09/0x0A are 10.42/20.83/41.67 kHz — the narrowest bandwidths
    /// in the table, where an SF11/12 symbol is 49-393 ms, far above the
    /// 16 ms LDRO threshold. The old predicate compared the register code
    /// (`bw <= 0x04`) as if the code space were monotonic in bandwidth, so
    /// exactly these configurations ran with LDRO off while any conforming
    /// peer (the reference firmware derives LDRO from symbol duration) has
    /// it on — and an LDRO mismatch between ends kills decoding entirely.
    #[test]
    fn ldro_covers_the_interleaved_bandwidth_codes() {
        for bw_hz in [10_420u32, 20_830, 41_670] {
            for sf in [11u8, 12] {
                assert!(
                    ldro_enabled(bw_hz, sf),
                    "SF{sf}/BW{bw_hz}: symbol far above 16 ms, LDRO must be on"
                );
            }
        }
    }

    /// The regime the old predicate got right must stay put: SF12/BW125 is
    /// LDRO-on, SF10/BW125 off.
    #[test]
    fn ldro_bw125_regime_is_unchanged() {
        assert!(ldro_enabled(125_000, 12), "SF12/BW125 must stay LDRO-on");
        assert!(!ldro_enabled(125_000, 10), "SF10/BW125 must stay LDRO-off");
    }

    /// The full decision matches the reference firmware on every bandwidth
    /// code in the SX1262 table (`long((1<<sf)/(bw/1000)) > 16`,
    /// RNode_Firmware sx126x.cpp:725-729). The interesting members:
    /// SF11/BW125 and SF12/BW250 sit at exactly 16 integer-ms and the
    /// reference runs them LDRO-OFF (matching every deployed RNode peer),
    /// while narrow bandwidths need LDRO well below SF11 — down to SF7 at
    /// 7.81 kHz.
    #[test]
    fn ldro_matches_the_reference_decision_on_the_whole_code_table() {
        // (bw_hz, reference getSignalBandwidth() value)
        const BW_TABLE: [(u32, u32); 10] = [
            (7_810, 7_800),
            (10_420, 10_400),
            (15_630, 15_600),
            (20_830, 20_800),
            (31_250, 31_250),
            (41_670, 41_700),
            (62_500, 62_500),
            (125_000, 125_000),
            (250_000, 250_000),
            (500_000, 500_000),
        ];
        for (bw_hz, ref_bw) in BW_TABLE {
            for sf in 5u8..=12 {
                let reference = (1u64 << sf) / (ref_bw as u64 / 1000) > 16;
                assert_eq!(
                    ldro_enabled(bw_hz, sf),
                    reference,
                    "SF{sf}/BW{bw_hz} disagrees with the reference firmware"
                );
            }
        }
        // The boundary members, stated as facts rather than derived:
        assert!(!ldro_enabled(125_000, 11), "SF11/BW125 is reference-OFF");
        assert!(!ldro_enabled(250_000, 12), "SF12/BW250 is reference-OFF");
        assert!(ldro_enabled(7_810, 7), "SF7/BW7.81k is reference-ON");
    }

    /// Unconfigured (bw 0) and sub-kHz values report no LDRO instead of
    /// dividing by zero.
    #[test]
    fn ldro_unconfigured_is_off() {
        assert!(!ldro_enabled(0, 12));
        assert!(!ldro_enabled(999, 12));
    }

    /// The extension charges the programmed preamble of the frame still on
    /// the air: at the SLOW profile the derived 18 symbols are 10 symbols
    /// (164 ms) more than the modem-default 8 the old formula assumed.
    #[test]
    fn the_extension_charges_the_programmed_preamble() {
        let (bw, sf, cr) = SLOW;
        let pre18 = rx_extend_ms(IRQ_PREAMBLE_DETECTED, bw, sf, cr, SLOW_PREAMBLE)
            .expect("guard must fire");
        let pre8 = rx_extend_ms(IRQ_PREAMBLE_DETECTED, bw, sf, cr, 8).expect("guard must fire");
        assert!(pre18 - pre8 >= 163, "preamble delta {}ms", pre18 - pre8);
    }

    /// A request that is one of the four documented settings is programmed
    /// as asked, with nothing to log.
    #[test]
    fn a_supported_pa_profile_is_programmed_as_asked() {
        for profile in PA_PROFILES_DBM {
            assert_eq!(pa_profile_dbm(profile), profile, "profile {profile}");
        }
    }

    /// The defect: 21, 18 and 15 dBm all pass `rnode::validate_config`, all
    /// fell through the driver's `_` arm, and all transmitted 14 dBm in
    /// silence. Each now lands on the profile below it — never above, the
    /// margin to the regulatory ceiling is the operator's to spend.
    #[test]
    fn an_unsupported_pa_value_rounds_down_never_up() {
        assert_eq!(pa_profile_dbm(21), 20);
        assert_eq!(pa_profile_dbm(19), 17);
        assert_eq!(pa_profile_dbm(18), 17);
        assert_eq!(pa_profile_dbm(16), 14);
        assert_eq!(pa_profile_dbm(15), 14);
        // Above the top profile there is nothing higher to round to.
        assert_eq!(pa_profile_dbm(37), 22);
        for requested in -9i8..=37 {
            assert!(
                pa_profile_dbm(requested) <= requested.max(PA_PROFILES_DBM[0]),
                "{requested} dBm was rounded up past its own request"
            );
        }
    }

    /// Below the lowest profile there is nothing to round down to: the PA
    /// setting saturates at 14 dBm and the caller logs it. This is the one
    /// direction in which the programmed power exceeds the request, and it
    /// is reachable — `lnflash` accepts -9 dBm.
    #[test]
    fn a_request_below_the_lowest_pa_profile_saturates_at_it() {
        assert_eq!(pa_profile_dbm(13), PA_PROFILES_DBM[0]);
        assert_eq!(pa_profile_dbm(0), 14);
        assert_eq!(pa_profile_dbm(-9), 14);
        assert_eq!(pa_profile_dbm(i8::MIN), 14);
    }
}
