//! Defer-or-proceed for the LoRa RX→TX turnaround (Codeberg #344).
//!
//! # The rule
//!
//! **A reception already in progress is not abandoned for a transmission we
//! want to start.**
//!
//! The firmware's idle path parks in continuous RX and races that future
//! against the outgoing queue. When the queue wins, the RX future is dropped
//! and the chip is put in `STBY_RC` before CSMA and TX. If a frame was on the
//! air at that moment it is lost — and the CAD that follows cannot notice,
//! because CAD runs *after* standby, i.e. from a state in which the radio can
//! no longer hear the frame it is about to talk over.
//!
//! Measured on the rig: a sender emitting an announce and then a report a
//! median of 14 ms later got the announce through 22/22 times and the report
//! 1-2/22. The relays turned around to forward the announce while the report
//! was still on the air.
//!
//! # The shape, from the reference
//!
//! RNode firmware v1.86 asks "is the medium free" **while still in RX**:
//! `medium_free()` (`RNode_Firmware.ino:1349-1353`) returns `!dcd`, and `dcd`
//! on SX1262 (`sx126x.cpp:494-520`) is read straight out of the IRQ status
//! register — `IRQ_HEADER_DET` or `IRQ_PREAMBLE_DET` means carrier detected.
//! A bare preamble that never grows a header is bounded there:
//!
//! ```c
//! if (now - preamble_detected_at > lora_preamble_time_ms + lora_header_time_ms) {
//!   preamble_detected_at = 0;
//!   if (!header_detected) { false_preamble_detected = true; }
//! ```
//!
//! We copy the shape, not the code: read the same two bits before standby,
//! and bound the wait the same way — a bare preamble by the reference's
//! false-preamble bound, a decoded header by one whole maximum-size frame.
//!
//! # Why it is a crate and not a branch in the driver
//!
//! The firmware crate only builds for `thumbv7em-none-eabihf`; anything
//! tested only there is tested nowhere. The RX-extend guard was written
//! inside the driver and was dead for its whole life because nothing could
//! run a test against it (#144). This decision is pure — IRQ flags plus
//! elapsed milliseconds in, defer-or-proceed out — so it lives here next to
//! `leviculum-queue-budget` and `leviculum-sd-policy`, and the driver holds
//! only SPI and a clock.
//!
//! # Starvation
//!
//! Every bound is finite and derived from the live modulation, and
//! [`turnaround_wait_ms`] takes the **cumulative** elapsed time of the
//! deferral, not a per-call budget. The wait therefore expires whether or not
//! the channel ever goes quiet, and a preamble that never completes costs at
//! most [`false_preamble_bound_ms`] once — the caller clears the latched bit
//! before transmitting, so the same stale preamble cannot defer a second
//! turnaround.
#![cfg_attr(not(test), no_std)]

use leviculum_core::rnode::{airtime_ms_with_preamble, MAX_SINGLE_PAYLOAD};
use leviculum_core::sx126x::{IRQ_HEADER_VALID, IRQ_PREAMBLE_DETECTED, IRQ_RX_DONE, IRQ_TIMEOUT};

/// Length of the LoRa PHY header in symbols.
///
/// Reference: `RNode_Firmware/Config.h:82`,
/// `#define PHY_HEADER_LORA_SYMBOLS 20`, used there for exactly this purpose
/// (`Utilities.h:1260`, `lora_header_time_ms`).
pub const PHY_HEADER_LORA_SYMBOLS: u64 = 20;

/// Spreading factors the SX1262 supports, and therefore the only ones whose
/// symbol time is meaningful here.
const SF_RANGE: core::ops::RangeInclusive<u8> = 5..=12;
/// Coding-rate denominators, 4/5 through 4/8.
const CR_RANGE: core::ops::RangeInclusive<u8> = 5..=8;

/// The live modulation, as the driver has it programmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modulation {
    /// Signal bandwidth in Hz. Zero before `configure_lora` has run.
    pub bw_hz: u32,
    /// Spreading factor.
    pub sf: u8,
    /// Coding-rate denominator, 5-8.
    pub cr_denom: u8,
    /// Programmed preamble length in symbols. Both ends of a link run the
    /// same derived value, so the frame on the air carries this one.
    pub preamble_symbols: u16,
}

impl Modulation {
    /// Whether the parameters describe a modulation whose timings exist.
    ///
    /// `bw_hz == 0` is the pre-`configure_lora` state; the spreading factor
    /// is range-checked because the symbol time shifts by it.
    pub fn is_configured(&self) -> bool {
        self.bw_hz != 0 && SF_RANGE.contains(&self.sf) && CR_RANGE.contains(&self.cr_denom)
    }

    /// Symbol time in microseconds, `2^SF / BW`.
    fn symbol_time_us(&self) -> u64 {
        (1u64 << self.sf) * 1_000_000 / self.bw_hz as u64
    }

    /// Time on air of the programmed preamble, rounded up to the millisecond.
    ///
    /// The reference's `lora_preamble_time_ms`
    /// (`Utilities.h:1259`): `ceil(lora_preamble_symbols * lora_symbol_time_ms)`.
    /// The 4.25-symbol sync tail is deliberately not charged here — it is not
    /// charged there either, and the bound this feeds is already generous by
    /// a whole preamble.
    pub fn preamble_time_ms(&self) -> u64 {
        (self.preamble_symbols as u64 * self.symbol_time_us()).div_ceil(1_000)
    }

    /// Time on air of the PHY header, rounded up to the millisecond.
    ///
    /// The reference's `lora_header_time_ms` (`Utilities.h:1260`).
    pub fn header_time_ms(&self) -> u64 {
        (PHY_HEADER_LORA_SYMBOLS * self.symbol_time_us()).div_ceil(1_000)
    }
}

/// How long a bare preamble is allowed to hold a transmission off.
///
/// `preamble time + header time`, the reference's false-preamble bound
/// (`sx126x.cpp:508`). A real frame reaches `HeaderValid` inside this window
/// even if the preamble started the instant before it was noticed, because
/// the whole programmed preamble is charged from *now*; anything still
/// showing only a preamble afterwards was noise, and noise does not get to
/// keep the radio silent.
pub fn false_preamble_bound_ms(m: Modulation) -> u64 {
    m.preamble_time_ms() + m.header_time_ms()
}

/// How long a frame whose header has decoded is allowed to hold a
/// transmission off: the on-air time of one maximum-size frame at the live
/// modulation.
///
/// This is the same quantity [`leviculum_core::sx126x::rx_extend_ms`] uses,
/// and for the same reason: it is the longest a legal frame on this link can
/// still be arriving. The header carries the true payload length, but
/// `GetRxBufferStatus` is only valid after `RxDone`, so the bound has to
/// assume the largest frame the split codec can put on the air —
/// [`MAX_SINGLE_PAYLOAD`] plus its one-byte flag header.
///
/// It over-covers by however much of the preamble has already gone by, which
/// is at most [`Modulation::preamble_time_ms`]. Subtracting that would need
/// the preamble's start time, which the latched IRQ bit does not carry.
pub fn frame_bound_ms(m: Modulation) -> u64 {
    airtime_ms_with_preamble(
        (MAX_SINGLE_PAYLOAD + 1) as u32,
        m.bw_hz,
        m.sf,
        m.cr_denom,
        m.preamble_symbols,
    )
}

/// The bound the current IRQ status earns, or `None` to transmit now.
///
/// - `RxDone` or `Timeout` latched: the reception already concluded, there is
///   nothing in progress to protect.
/// - `HeaderValid`: a real frame, bounded by [`frame_bound_ms`].
/// - `PreambleDetected` alone: possibly noise, bounded by
///   [`false_preamble_bound_ms`].
/// - neither: the channel is quiet. **This is the common case and it must
///   stay free** — a turnaround that deferred here would halve throughput.
pub fn turnaround_bound_ms(flags: u16, m: Modulation) -> Option<u64> {
    if !m.is_configured() {
        return None;
    }
    if flags & (IRQ_RX_DONE | IRQ_TIMEOUT) != 0 {
        return None;
    }
    if flags & IRQ_HEADER_VALID != 0 {
        return Some(frame_bound_ms(m));
    }
    if flags & IRQ_PREAMBLE_DETECTED != 0 {
        return Some(false_preamble_bound_ms(m));
    }
    None
}

/// How much longer to keep listening before the turnaround, or `None` to
/// stop deferring and transmit.
///
/// `elapsed_ms` is the time spent in *this* deferral so far, cumulative
/// across calls. Because the bound is compared against the cumulative figure
/// rather than reset per call, a caller that loops on this function is
/// bounded by [`frame_bound_ms`] no matter how the flags evolve: the two
/// bounds are constants of the modulation and the IRQ bits only ever
/// accumulate.
pub fn turnaround_wait_ms(flags: u16, elapsed_ms: u64, m: Modulation) -> Option<u64> {
    let bound = turnaround_bound_ms(flags, m)?;
    match bound.saturating_sub(elapsed_ms) {
        0 => None,
        remaining => Some(remaining),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::sx126x::{
        rx_irq_params, IRQ_CAD_DETECTED, IRQ_CAD_DONE, IRQ_CRC_ERR, RX_EXTEND_INPUTS,
    };

    /// SF7/BW125/CR4:5 with the preamble the RNode derivation picks there
    /// (`derive_preamble_symbols(7, 5, 125_000) == 24`).
    const FAST: Modulation = Modulation {
        bw_hz: 125_000,
        sf: 7,
        cr_denom: 5,
        preamble_symbols: 24,
    };

    /// SF12/BW125/CR4:5, same derivation (`== 18`).
    const SLOW: Modulation = Modulation {
        bw_hz: 125_000,
        sf: 12,
        cr_denom: 5,
        preamble_symbols: 18,
    };

    #[test]
    fn test_constants_match_the_shipped_preamble_derivation() {
        assert_eq!(
            leviculum_core::rnode::derive_preamble_symbols(FAST.sf, FAST.cr_denom, FAST.bw_hz),
            FAST.preamble_symbols
        );
        assert_eq!(
            leviculum_core::rnode::derive_preamble_symbols(SLOW.sf, SLOW.cr_denom, SLOW.bw_hz),
            SLOW.preamble_symbols
        );
    }

    // -- the decision ------------------------------------------------------

    #[test]
    fn preamble_detected_defers() {
        assert_eq!(
            turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, 0, FAST),
            Some(false_preamble_bound_ms(FAST))
        );
        assert_eq!(
            turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, 0, SLOW),
            Some(false_preamble_bound_ms(SLOW))
        );
    }

    #[test]
    fn header_valid_defers_for_a_whole_frame() {
        assert_eq!(
            turnaround_wait_ms(IRQ_HEADER_VALID, 0, FAST),
            Some(frame_bound_ms(FAST))
        );
        // Header wins over the preamble bit latched by the same frame.
        assert_eq!(
            turnaround_wait_ms(IRQ_HEADER_VALID | IRQ_PREAMBLE_DETECTED, 0, SLOW),
            Some(frame_bound_ms(SLOW))
        );
    }

    /// **Control.** Today's behaviour, bit for bit: nothing detected, no
    /// wait, straight to standby and CSMA. A fix that deferred here would
    /// pass every other test in this file and halve our throughput.
    #[test]
    fn quiet_channel_proceeds_immediately() {
        for m in [FAST, SLOW] {
            assert_eq!(turnaround_wait_ms(0, 0, m), None);
            // Bits that are not a reception in progress do not defer either.
            assert_eq!(
                turnaround_wait_ms(IRQ_CAD_DONE | IRQ_CAD_DETECTED | IRQ_CRC_ERR, 0, m),
                None
            );
        }
    }

    #[test]
    fn a_concluded_reception_proceeds() {
        for flag in [IRQ_RX_DONE, IRQ_TIMEOUT] {
            assert_eq!(
                turnaround_wait_ms(flag | IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID, 0, SLOW),
                None,
                "flag {flag:#06x} concluded the window; nothing is in progress"
            );
        }
    }

    #[test]
    fn an_unconfigured_radio_proceeds() {
        let unconfigured = Modulation { bw_hz: 0, ..FAST };
        assert_eq!(
            turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, 0, unconfigured),
            None
        );
        // Out-of-range parameters have no symbol time; they must not shift.
        for bad in [
            Modulation { sf: 0, ..FAST },
            Modulation { sf: 13, ..FAST },
            Modulation {
                cr_denom: 4,
                ..FAST
            },
        ] {
            assert_eq!(turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, 0, bad), None);
        }
    }

    // -- starvation --------------------------------------------------------

    /// **Starvation control 1.** The radio transmits eventually even if the
    /// channel never goes quiet: at the bound, and past it, the answer is
    /// proceed — with the preamble still latched.
    #[test]
    fn starvation_control_bound_expired_proceeds() {
        for m in [FAST, SLOW] {
            let pre = false_preamble_bound_ms(m);
            assert_eq!(
                turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, pre - 1, m),
                Some(1)
            );
            assert_eq!(turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, pre, m), None);
            assert_eq!(
                turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, pre + 10_000, m),
                None
            );

            let full = frame_bound_ms(m);
            assert_eq!(turnaround_wait_ms(IRQ_HEADER_VALID, full - 1, m), Some(1));
            assert_eq!(turnaround_wait_ms(IRQ_HEADER_VALID, full, m), None);
        }
    }

    /// **Starvation control 2.** A false preamble — latched, never growing a
    /// header, never completing — costs one bounded wait and then the radio
    /// transmits. Driving the caller's loop exactly as the firmware does it:
    /// wait the returned time, add it to the cumulative elapsed, re-read the
    /// (unchanged) flags, ask again.
    #[test]
    fn starvation_control_false_preamble_defers_once_then_proceeds() {
        for m in [FAST, SLOW] {
            let flags = IRQ_PREAMBLE_DETECTED; // never grows a header
            let mut elapsed = 0u64;
            let mut waits = 0usize;
            while let Some(wait) = turnaround_wait_ms(flags, elapsed, m) {
                elapsed += wait;
                waits += 1;
                assert!(waits <= 4, "loop did not converge: elapsed={elapsed}");
            }
            assert_eq!(waits, 1, "a false preamble must be waited on exactly once");
            assert_eq!(elapsed, false_preamble_bound_ms(m));
        }
    }

    /// The same loop when the frame is real and slow: the header arrives
    /// during the preamble wait and raises the bound once, and the total is
    /// still capped by one frame. Repeated flag changes cannot ratchet the
    /// wait upward, because the bounds are constants of the modulation.
    #[test]
    fn a_real_frame_raises_the_bound_once_and_no_further() {
        let m = SLOW;
        let mut elapsed = 0u64;
        let first = turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, elapsed, m).expect("defer");
        elapsed += first;
        // Header decoded while we listened.
        let second = turnaround_wait_ms(IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID, elapsed, m)
            .expect("defer");
        elapsed += second;
        assert_eq!(elapsed, frame_bound_ms(m));
        // Nothing arrived after all: proceed, and the total wait is one frame.
        assert_eq!(
            turnaround_wait_ms(IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID, elapsed, m),
            None
        );
        assert!(elapsed <= frame_bound_ms(m));
    }

    // -- the bounds themselves --------------------------------------------

    /// The formula, and the two numbers it produces. A fixed millisecond
    /// constant would be wrong here: the whole point is that a frame at SF12
    /// occupies the channel 22x longer than one at SF7.
    #[test]
    fn bounds_scale_with_spreading_factor() {
        // SF7/BW125: t_sym = 1.024 ms.
        assert_eq!(FAST.preamble_time_ms(), 25); // ceil(24 * 1.024)
        assert_eq!(FAST.header_time_ms(), 21); // ceil(20 * 1.024)
        assert_eq!(false_preamble_bound_ms(FAST), 46);
        assert_eq!(frame_bound_ms(FAST), 416);

        // SF12/BW125: t_sym = 32.768 ms.
        assert_eq!(SLOW.preamble_time_ms(), 590); // ceil(18 * 32.768)
        assert_eq!(SLOW.header_time_ms(), 656); // ceil(20 * 32.768)
        assert_eq!(false_preamble_bound_ms(SLOW), 1246);
        assert_eq!(frame_bound_ms(SLOW), 9348);

        assert!(false_preamble_bound_ms(SLOW) > false_preamble_bound_ms(FAST));
        assert!(frame_bound_ms(SLOW) > frame_bound_ms(FAST));
    }

    #[test]
    fn frame_bound_is_one_maximum_size_frame() {
        for m in [FAST, SLOW] {
            assert_eq!(
                frame_bound_ms(m),
                airtime_ms_with_preamble(
                    (MAX_SINGLE_PAYLOAD + 1) as u32,
                    m.bw_hz,
                    m.sf,
                    m.cr_denom,
                    m.preamble_symbols
                )
            );
            // A frame bound below the false-preamble bound would mean a
            // decoded header buys less patience than a bare preamble.
            assert!(frame_bound_ms(m) > false_preamble_bound_ms(m));
        }
    }

    /// Every spreading factor the chip supports has a finite, monotonic
    /// bound — no shift overflow, no zero, no inversion.
    #[test]
    fn every_supported_modulation_has_a_finite_bound() {
        let mut prev = 0;
        for sf in 5..=12u8 {
            let m = Modulation {
                sf,
                preamble_symbols: leviculum_core::rnode::derive_preamble_symbols(sf, 5, 125_000),
                ..FAST
            };
            let bound = frame_bound_ms(m);
            assert!(bound > 0, "sf={sf}");
            assert!(
                turnaround_wait_ms(IRQ_PREAMBLE_DETECTED, 0, m).is_some(),
                "sf={sf}"
            );
            if sf >= 8 {
                // Below SF8 the derived preamble shrinks faster than the
                // symbol time grows, so only compare across the flat region.
                assert!(bound > prev, "sf={sf} bound={bound} prev={prev}");
            }
            prev = bound;
        }
    }

    // -- the bits have to be readable at all -------------------------------

    /// The #144 lesson, applied to this guard: a decision over IRQ bits the
    /// chip was configured not to latch is a decision that can never fire.
    /// Both bits this crate reads are in the RX latch mask.
    #[test]
    fn the_bits_this_reads_are_latched_during_rx() {
        let params = rx_irq_params();
        let latch = u16::from_be_bytes([params[0], params[1]]);
        let inputs = IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID;
        assert_eq!(
            latch & inputs,
            inputs,
            "rx latch mask {latch:#06x} drops {inputs:#06x}: the turnaround guard could never fire"
        );
        // Same two bits the RX-extend guard reads; they answer the same
        // question ("is a frame still arriving") at two different moments.
        assert_eq!(inputs, RX_EXTEND_INPUTS);
    }

    /// ...and they must not wake DIO1, or the deferral would end on the
    /// preamble instead of on the frame.
    #[test]
    fn the_bits_this_reads_do_not_wake_dio1() {
        let params = rx_irq_params();
        let dio1 = u16::from_be_bytes([params[2], params[3]]);
        assert_eq!(dio1 & (IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID), 0);
    }
}
