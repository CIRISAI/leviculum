#![no_std]
//! Two decisions the firmware's USB side makes, as pure functions
//! (leviculum#413, the "Still stranded" list of
//! `docs/src/concepts/firmware-host-test-seam.md`).
//!
//! # The 1200-baud touch
//!
//! [`is_bootloader_touch`] says whether one USB control-out request means
//! "reset into the UF2 bootloader". A false positive resets a board in the
//! middle of a session; a false negative costs a physical double-tap on every
//! flash. `BaudTouchHandler` in `leviculum-nrf/src/usb.rs` asks this function
//! and keeps only what cannot run on a host: the GPREGRET write and the
//! `sys_reset` after it.
//!
//! # The control-envelope answer windows
//!
//! Three durations the transport port's answers must stay inside, each
//! justified against a host-side timeout. They are constants, and the
//! firmware re-exports them under the names it always used. What depends on
//! the modulation is not the windows but the one lawful delay they must
//! cover, [`retune_deferral_bound_ms`]: the windows are fixed because the
//! host's answer windows are fixed, and a slow profile whose deferral bound
//! outgrows [`CONFIG_APPLY_WITHIN_MS`] is answered busy and acked on the
//! host's retry ([`apply_answer`]). Growing the window with the modulation
//! instead would push `CONFIG_DELIVER_WITHIN_MS + CONFIG_APPLY_WITHIN_MS`
//! past the 2 s lnsd waits per attempt, and lnsd would count a slow apply as
//! a lost answer rather than a busy one.

use leviculum_core::rnode::{
    airtime_ms_with_preamble, derive_preamble_symbols, MAX_SINGLE_PAYLOAD,
};

// ---------------------------------------------------------------------------
// The 1200-baud touch
// ---------------------------------------------------------------------------

/// `bmRequestType` bits 6..5, "Class" (USB 2.0 §9.3, Table 9-2). The same
/// numbering as `embassy_usb::control::RequestType`.
pub const REQUEST_TYPE_CLASS: u8 = 1;

/// `bmRequestType` bits 4..0, "Interface" (USB 2.0 §9.3, Table 9-2). The same
/// numbering as `embassy_usb::control::Recipient`.
pub const RECIPIENT_INTERFACE: u8 = 1;

/// CDC PSTN spec §6.3.10: SET_LINE_CODING is class request 0x20, addressed
/// to the CDC communication interface.
pub const REQ_SET_LINE_CODING: u8 = 0x20;

/// Length of the LINE_CODING structure SET_LINE_CODING carries: u32
/// dwDTERate (LE), u8 bCharFormat, u8 bParityType, u8 bDataBits.
pub const LINE_CODING_LEN: usize = 7;

/// The rate that means "reset into the bootloader". Strict equality only:
/// fuzzy matching would risk spurious resets when a terminal app rounds
/// 1215 to 1200.
pub const TOUCH_BAUD: u32 = 1200;

/// Split a raw `bmRequestType` into (direction, type, recipient), the three
/// fields embassy-usb hands the firmware already decoded. Direction is 1 for
/// device-to-host.
pub fn split_bm_request_type(bm_request_type: u8) -> (u8, u8, u8) {
    (
        bm_request_type >> 7,
        (bm_request_type >> 5) & 0b11,
        bm_request_type & 0b1_1111,
    )
}

/// Whether a control-out request is the 1200-baud touch.
///
/// Four terms gate before the rate is read: a class request, to an
/// interface, SET_LINE_CODING, with a payload long enough to hold a whole
/// LINE_CODING. Then the rate, little-endian, must be exactly
/// [`TOUCH_BAUD`]. Every other request falls through to the CDC-ACM class
/// untouched.
///
/// `request_type` and `recipient` are the numeric codes of USB 2.0 Table
/// 9-2 (the discriminants of embassy-usb's enums).
pub fn is_bootloader_touch(request_type: u8, recipient: u8, request: u8, data: &[u8]) -> bool {
    request_type == REQUEST_TYPE_CLASS
        && recipient == RECIPIENT_INTERFACE
        && request == REQ_SET_LINE_CODING
        && data.len() >= LINE_CODING_LEN
        && line_coding_rate(data) == Some(TOUCH_BAUD)
}

/// The dwDTERate of a LINE_CODING payload, or `None` when it is too short to
/// carry one.
pub fn line_coding_rate(data: &[u8]) -> Option<u32> {
    match data {
        [a, b, c, d, ..] => Some(u32::from_le_bytes([*a, *b, *c, *d])),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The control-envelope answer windows
// ---------------------------------------------------------------------------

/// A LoRa modulation, the three inputs the airtime depends on. The preamble
/// is not an input: the firmware programs the derived one
/// (`derive_preamble_symbols`), and so does this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modulation {
    /// Signal bandwidth in Hz.
    pub bandwidth_hz: u32,
    /// Spreading factor, 6 to 12.
    pub sf: u8,
    /// Coding rate denominator, 5 to 8 (4/5 to 4/8).
    pub cr: u8,
}

/// The project's default PHY: SF8, BW 125 kHz, CR 4/5 (the frequency,
/// 869.463 MHz, does not enter the airtime).
pub const DEFAULT_MODULATION: Modulation = Modulation {
    bandwidth_hz: 125_000,
    sf: 8,
    cr: 5,
};

/// lnflash's answer window for a radio-config frame
/// (`ACK_WITHIN`, `lnflash/src/radio.rs:57`).
pub const LNFLASH_ACK_WITHIN_MS: u64 = 3_500;

/// The tightest host answer window: lnsd's legacy config sender waits this
/// long per attempt (`CONFIG_ACK_TIMEOUT`,
/// `leviculum-std/src/interfaces/serial.rs:504`).
pub const LNSD_CONFIG_ACK_TIMEOUT_MS: u64 = 2_000;

/// How long a radio config may wait for the LoRa task to drain the previous
/// one before it is refused. A live consumer clears the depth-1 channel in
/// milliseconds; only a stuck one runs this clock out. Not a function of the
/// modulation: draining a channel is not airtime.
pub const CONFIG_DELIVER_WITHIN_MS: u64 = 500;

/// How long a delivered config may wait for the LoRa task to confirm the
/// apply. Covers [`retune_deferral_bound_ms`] at [`DEFAULT_MODULATION`], and
/// together with [`CONFIG_DELIVER_WITHIN_MS`] stays inside
/// [`LNSD_CONFIG_ACK_TIMEOUT_MS`].
pub const CONFIG_APPLY_WITHIN_MS: u64 = 1_200;

/// The longest a config frame's answer can take on the firmware side:
/// delivery grace plus apply wait.
pub const fn control_envelope_ms() -> u64 {
    CONFIG_DELIVER_WITHIN_MS + CONFIG_APPLY_WITHIN_MS
}

// The envelope fits the tightest host window, so even a refusal reaches lnsd
// inside its per-attempt wait. A build-time check, so a window widened past
// it fails the firmware build, not only this crate's tests.
const _: () = assert!(control_envelope_ms() <= LNSD_CONFIG_ACK_TIMEOUT_MS);

/// Incomplete-frame timeout on the transport CDC port. A property of the
/// serial line, not of the LoRa modulation: [`frame_timeout_margin`] is the
/// arithmetic its comment carries.
pub const FRAME_TIMEOUT_MS: u64 = 500;

/// The one lawful delay of a config apply: the retune defers to a frame that
/// is mid-air, for at most one maximum-size frame's airtime at the live
/// modulation and preamble. The same number `leviculum_core::sx126x::rx_extend_ms`
/// hands the radio driver.
pub fn retune_deferral_bound_ms(m: Modulation) -> u64 {
    let preamble = derive_preamble_symbols(m.sf, m.cr, m.bandwidth_hz);
    airtime_ms_with_preamble(
        (MAX_SINGLE_PAYLOAD + 1) as u32,
        m.bandwidth_hz,
        m.sf,
        m.cr,
        preamble,
    )
    .max(1)
}

/// What the answer to a config frame says when the apply takes its longest
/// lawful time at `m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyAnswer {
    /// The deferral fits the apply window: the frame is acked on its first
    /// attempt.
    Acked,
    /// The deferral outgrows the apply window: the first attempt is answered
    /// busy, and the host's retry finds the config applied.
    BusyThenAcked,
}

/// The answer a config frame gets at `m` when the retune defers for the full
/// [`retune_deferral_bound_ms`].
pub fn apply_answer(m: Modulation) -> ApplyAnswer {
    if retune_deferral_bound_ms(m) <= CONFIG_APPLY_WITHIN_MS {
        ApplyAnswer::Acked
    } else {
        ApplyAnswer::BusyThenAcked
    }
}

/// Microseconds `bytes` take on a serial line at `baud`, 8N1 (ten bits a
/// byte). Zero baud is answered with `u64::MAX`, not divided by.
pub fn serial_frame_us(bytes: u32, baud: u32) -> u64 {
    if baud == 0 {
        return u64::MAX;
    }
    (u64::from(bytes) * 10 * 1_000_000).div_ceil(u64::from(baud))
}

/// How many times [`FRAME_TIMEOUT_MS`] covers a `bytes`-long frame delivered
/// byte by byte at `baud`, rounded down.
pub fn frame_timeout_margin(bytes: u32, baud: u32) -> u64 {
    (FRAME_TIMEOUT_MS * 1_000) / serial_frame_us(bytes, baud).max(1)
}
