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
pub fn rx_extend_ms(flags: u16, bw_hz: u32, sf: u8, cr_denom: u8) -> Option<u64> {
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
        crate::rnode::airtime_ms(
            (crate::rnode::MAX_SINGLE_PAYLOAD + 1) as u32,
            bw_hz,
            sf,
            cr_denom,
        )
        .max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SF10 / 62.5 kHz / 4:5 — the slow end of the lab rig, where a single
    /// frame is seconds of airtime and the guard matters.
    const SLOW: (u32, u8, u8) = (62_500, 10, 5);

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
        let extend = rx_extend_ms(IRQ_PREAMBLE_DETECTED, bw, sf, cr).expect("guard must fire");
        assert_eq!(
            extend,
            crate::rnode::airtime_ms((crate::rnode::MAX_SINGLE_PAYLOAD + 1) as u32, bw, sf, cr)
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
        assert!(rx_extend_ms(IRQ_HEADER_VALID, bw, sf, cr).is_some());
    }

    #[test]
    fn a_concluded_window_is_not_extended() {
        let (bw, sf, cr) = SLOW;
        // RxDone wins even with a preamble still latched from the same frame.
        assert_eq!(
            rx_extend_ms(IRQ_PREAMBLE_DETECTED | IRQ_RX_DONE, bw, sf, cr),
            None
        );
        assert_eq!(
            rx_extend_ms(IRQ_PREAMBLE_DETECTED | IRQ_TIMEOUT, bw, sf, cr),
            None
        );
    }

    #[test]
    fn a_silent_channel_is_not_extended() {
        let (bw, sf, cr) = SLOW;
        assert_eq!(rx_extend_ms(0, bw, sf, cr), None);
        // CAD bits are not evidence of an inbound frame.
        assert_eq!(rx_extend_ms(IRQ_CAD_DETECTED, bw, sf, cr), None);
    }

    #[test]
    fn an_unconfigured_radio_is_not_extended() {
        assert_eq!(rx_extend_ms(IRQ_PREAMBLE_DETECTED, 0, 10, 5), None);
    }

    /// The extension tracks the settings rather than being a constant: the
    /// fast end of the rig is tens of milliseconds where the slow end is
    /// seconds. `max(1)` is a floor against a rounded-to-zero airtime turning
    /// the extension into an immediate re-read; no configuration in the
    /// corpus reaches it, which is why it is asserted and not relied on.
    #[test]
    fn the_extension_tracks_the_settings() {
        let fast = rx_extend_ms(IRQ_PREAMBLE_DETECTED, 500_000, 5, 5).expect("guard must fire");
        let (bw, sf, cr) = SLOW;
        let slow = rx_extend_ms(IRQ_PREAMBLE_DETECTED, bw, sf, cr).expect("guard must fire");
        assert_eq!(fast, 35);
        assert!(slow > 100 * fast, "slow={slow}ms fast={fast}ms");
        assert!(fast >= 1);
    }
}
