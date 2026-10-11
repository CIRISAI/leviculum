#![no_std]
//! The decisions the firmware's USB side makes, as pure functions
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
//!
//! # The commanded reset
//!
//! [`reset_plan`] orders what the firmware does between a reset frame and
//! `sys_reset` (leviculum#359): the persistent line, the ack with a deadline
//! ([`RESET_ACK_WITHIN_MS`], derived from periculum's reset waits), the drain,
//! the reset. The transport task in `usb.rs` runs the steps it is handed and
//! decides none of them.
//!
//! # The transport reader and the outgoing queue
//!
//! [`reader_step`] says what the transport port's reader does with one read:
//! deframe it, drop a frame a silent host tore ([`read_timeout_ms`]), or end
//! the session on a disconnect so the next one starts with an empty
//! deframer. [`offered`] is the serial interface's side of the queue in
//! front of the writer: when a packet is dropped. [`tx_counted`] is the
//! writer's side: what reached a host and is counted.
//!
//! # The debug port's drain
//!
//! [`drain_wake`] says what one wake of the debug writer does: nothing, open
//! the runtime-drain gate, or open it and write the log ring out. The port is
//! written only while DTR is up, and the gate opens on the first host or
//! after [`HEADLESS_DRAIN_OPEN_MS`] without one. [`drain_continues`] bounds a
//! drain against a host that holds DTR up and does not read.
//!
//! # Where a radio config goes
//!
//! [`config_route`] sends a host's radio config to the flash page on a boot
//! that never started LoRa and to the LoRa task's channel otherwise;
//! [`stored_answer`] is the page route's answer. [`full_slot`] says what a
//! config does when the channel's one slot is taken: a repeat of the queued
//! config waits for that apply, a different one gets one grace send.
//!
//! # The debug port's commands
//!
//! [`debug_commands`] says which of the three one-byte commands a packet
//! read on the debug port carries: `p` the post-mortem query, `s` the stack
//! watermark reset, `c` the heap census.

extern crate alloc;

use alloc::vec::Vec;
use leviculum_core::envelope::{encode_duty_hold, DutyHoldWire};
use leviculum_core::rnode::{
    airtime_ms_with_preamble, derive_preamble_symbols, RadioConfigWire, MAX_SINGLE_PAYLOAD,
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
/// arithmetic its comment carries. Python uses 100 ms but also sets
/// low_latency mode, so frames arrive as bulk USB packets; without it,
/// byte-by-byte delivery needs more time, and 500 ms gives a 34x margin for a
/// 167-byte frame at 115200. [`read_timeout_ms`] applies it.
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

// ---------------------------------------------------------------------------
// The duty-hold notice (leviculum#501)
// ---------------------------------------------------------------------------

/// Which duty-hold frames the transport port sends the host, and when.
///
/// The LoRa task raises and lowers `LORA_DUTY_HOLD` at the hold's edges and
/// wakes the USB task; the USB task asks [`DutyHoldNotice::edge`] whether
/// the state it reads now is one the host has not been told, and writes the
/// frame it gets back. A wake that finds no change sends nothing, so a
/// hold that opened and lifted between two wakes costs no frame and a
/// spurious wake costs none either. [`DutyHoldNotice::state`] is the
/// answer that rides behind a media report: the state as it stands,
/// whatever was sent before.
///
/// The firmware boots unheld, and so does this: the first frame a board
/// sends unsolicited is a `held`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DutyHoldNotice {
    told_held: bool,
}

impl DutyHoldNotice {
    /// A notice for a board that has told nobody anything: not held.
    pub const fn new() -> Self {
        Self { told_held: false }
    }

    /// The `TYPE_DUTY_HOLD` frame for an edge, or `None` when `held` is
    /// what the host was last told. The state counts as told once the frame
    /// is handed out, written or not: a host that was absent for the write
    /// learns it from the media query it sends on attach, not from a retry.
    pub fn edge(&mut self, held: bool, lt_ms: u32) -> Option<Vec<u8>> {
        if held == self.told_held {
            return None;
        }
        Some(self.state(held, lt_ms))
    }

    /// The `TYPE_DUTY_HOLD` frame for the state as it stands, for the
    /// media-query answer.
    pub fn state(&mut self, held: bool, lt_ms: u32) -> Vec<u8> {
        self.told_held = held;
        encode_duty_hold(&DutyHoldWire { held, lt_ms })
    }
}

// ---------------------------------------------------------------------------
// The commanded reset (leviculum#359)
// ---------------------------------------------------------------------------

/// periculum's per-attempt wait for the reset ack when it holds the port
/// itself: `reset_lnode` (`periculum/src/runner.rs`) reads for 2 s after each
/// of its three reset frames before it writes the next one.
pub const PERICULUM_RESET_ACK_WINDOW_MS: u64 = 2_000;

/// How long periculum gives a reset board to leave the bus
/// (`REENUM_GONE_WINDOW`, `periculum/src/runner.rs`). On the daemon route
/// (`reset_lnode_via_daemon`) it is the only evidence there is: lnsd writes
/// the frame and forgets it, and a board still on the bus when this runs out
/// is reported `no_reboot`.
pub const PERICULUM_REENUM_GONE_WINDOW_MS: u64 = 5_000;

/// How long the commanded reset waits for its ack to be taken off the wire
/// before it resets without it.
///
/// The ack is one short HDLC frame, a single 64-byte USB packet, and a host
/// that reads the port (lnsd always does, periculum does for the 2 s after
/// its frame) takes it within one USB frame. What the wait has to survive is
/// a host that holds DTR up and does not read: before the bound, the CDC
/// write pended for as long as that lasted and the board acked a reset it
/// then never performed. Half of [`PERICULUM_RESET_ACK_WINDOW_MS`], so that
/// with [`RESET_DRAIN_MS`] on top the board has left the bus before the
/// host's first attempt runs out and before it writes a second reset frame
/// into a board still busy with the first.
pub const RESET_ACK_WITHIN_MS: u64 = 1_000;

/// How long the USB device stays up after the ack, so the host can read the
/// ack before the device disappears (the 100 ms the firmware always waited).
pub const RESET_DRAIN_MS: u64 = 100;

// Ack wait plus drain is the longest a commanded reset stays on the bus, and
// it has to fit the tightest host window. A build-time check, so widening
// either fails the firmware build, not only this crate's tests.
const _: () = assert!(RESET_ACK_WITHIN_MS + RESET_DRAIN_MS < PERICULUM_RESET_ACK_WINDOW_MS);
const _: () = assert!(PERICULUM_RESET_ACK_WINDOW_MS < PERICULUM_REENUM_GONE_WINDOW_MS);

/// What the firmware knows when a reset frame arrives.
///
/// Two facts are inputs because they change the plan: whether a host is
/// there to read the ack, and how much of the ack window is already spent.
/// The other two are inputs because the plan answers them with no step,
/// and that answer is what the host tests pin (see [`reset_plan`]). Not an
/// input: whether a flash write is in flight. The firmware tracks no such
/// fact; a reset in the middle of one is what the stores' torn-write
/// handling is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetFacts {
    /// `sd_softdevice_is_enabled` answered non-zero.
    pub softdevice_enabled: bool,
    /// BLE connections holding a drain slot (`HVN_DRAIN.claimed()`).
    pub ble_links: u8,
    /// DTR is up on the transport port: a process holds it open.
    pub host_present: bool,
    /// Milliseconds since the reset frame was classified.
    pub since_command_ms: u64,
}

/// One step of the commanded reset, in the order [`reset_plan`] gives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetStep {
    /// The critical `[RESET] host-requested reboot` line, into the
    /// persistent tail. First, because it is the only record a census has
    /// that this boot ended by command: every later step can hang (the ack)
    /// or ends the boot (the reset), and a line written after either is a
    /// line that may never exist.
    Announce,
    /// Write the ack, and give up on it after `within_ms`. Before the drain,
    /// because the drain exists for the ack; bounded, because a host that
    /// holds DTR up without reading would otherwise keep a board that has
    /// agreed to reset running indefinitely.
    Ack {
        /// What is left of [`RESET_ACK_WITHIN_MS`].
        within_ms: u64,
    },
    /// Keep the USB device up for `ms`. After the ack and also after an ack
    /// that ran out: an abandoned write may still sit armed on the IN
    /// endpoint for a host that reads late. It is also the debug writer's
    /// window to put the `Announce` line on the debug port live.
    Drain {
        /// [`RESET_DRAIN_MS`].
        ms: u64,
    },
    /// `SCB::sys_reset()`. Last, and with nothing torn down before it:
    ///
    /// - SoftDevice enabled: `sd_nvic_SystemReset` is the CMSIS
    ///   `NVIC_SystemReset()` and nothing else (`nrf_nvic.h` of S140), the
    ///   same AIRCR write `sys_reset` makes, so routing through the
    ///   SoftDevice changes nothing. `sd_softdevice_disable` first would
    ///   need every BLE task stopped before it, an ordering this firmware
    ///   has nowhere else, for a hazard nobody has measured.
    /// - BLE links up: the reset drops them, and each peer sees the
    ///   supervision timeout it already handles for a board that walks out
    ///   of range. The reset census ran both arms (link up and quiet board)
    ///   78 times with no board lost.
    SystemReset,
}

/// The ordered steps of a commanded reset, at most four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetPlan {
    steps: [ResetStep; 4],
    len: usize,
}

impl ResetPlan {
    const fn new() -> Self {
        Self {
            steps: [ResetStep::SystemReset; 4],
            len: 0,
        }
    }

    fn push(&mut self, step: ResetStep) {
        if let Some(slot) = self.steps.get_mut(self.len) {
            *slot = step;
            self.len += 1;
        }
    }

    /// The steps in the order the firmware runs them.
    pub fn steps(&self) -> &[ResetStep] {
        self.steps.get(..self.len).unwrap_or(&[])
    }

    /// The longest the plan keeps the board on the bus: every bounded wait
    /// running out.
    pub fn worst_case_ms(&self) -> u64 {
        self.steps()
            .iter()
            .map(|step| match step {
                ResetStep::Ack { within_ms } => *within_ms,
                ResetStep::Drain { ms } => *ms,
                ResetStep::Announce | ResetStep::SystemReset => 0,
            })
            .sum()
    }
}

/// The commanded reset's steps for what the firmware knows now.
///
/// Always `Announce` first, `Drain` and `SystemReset` last. In between an
/// `Ack` when a host is there to read it and the ack window has time left;
/// with no host the ack has no reader (the DTR gate would refuse the write
/// anyway), and with the window spent it would only push the reset past the
/// host's wait. The SoftDevice and the BLE links do not change the plan:
/// [`ResetStep::SystemReset`] says why.
pub fn reset_plan(facts: &ResetFacts) -> ResetPlan {
    let mut plan = ResetPlan::new();
    plan.push(ResetStep::Announce);
    let left = RESET_ACK_WITHIN_MS.saturating_sub(facts.since_command_ms);
    if facts.host_present && left > 0 {
        plan.push(ResetStep::Ack { within_ms: left });
    }
    plan.push(ResetStep::Drain { ms: RESET_DRAIN_MS });
    plan.push(ResetStep::SystemReset);
    plan
}

// ---------------------------------------------------------------------------
// The transport reader (leviculum#359, leviculum#413)
// ---------------------------------------------------------------------------

/// How long the transport port's reader waits for the next USB packet when
/// no frame is open: an hour, which is "until the host writes". The loop
/// also wakes on every outgoing packet and every duty-hold edge, so this
/// bound is never what keeps a board responsive.
pub const IDLE_READ_TIMEOUT_MS: u64 = 3_600_000;

/// How long the reader waits for the next packet: [`FRAME_TIMEOUT_MS`] once
/// a frame has opened, so a frame torn by a host that stopped writing is
/// dropped before it can swallow the next one, else [`IDLE_READ_TIMEOUT_MS`].
pub fn read_timeout_ms(in_frame: bool) -> u64 {
    if in_frame {
        FRAME_TIMEOUT_MS
    } else {
        IDLE_READ_TIMEOUT_MS
    }
}

/// How one read on the transport port ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The read returned bytes.
    Bytes,
    /// The read returned an error: the host closed the port, the cable
    /// went, or the bus was reset.
    Disconnected,
    /// [`read_timeout_ms`] ran out with no packet.
    TimedOut,
}

/// What the reader does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderStep {
    /// Feed the bytes to the deframer.
    Deframe,
    /// Leave the read loop and wait for the next connection. The deframer
    /// is reset when it comes, so nothing of a frame the disconnect tore is
    /// carried into the next session.
    Rewait,
    /// Drop the partial frame and keep reading.
    DropPartial,
    /// Nothing was open: keep reading.
    Idle,
}

/// The reader's step for one read's outcome. A disconnect always ends the
/// session, mid-frame or not; a timeout drops only a frame that is open.
pub fn reader_step(outcome: ReadOutcome, in_frame: bool) -> ReaderStep {
    match outcome {
        ReadOutcome::Bytes => ReaderStep::Deframe,
        ReadOutcome::Disconnected => ReaderStep::Rewait,
        ReadOutcome::TimedOut if in_frame => ReaderStep::DropPartial,
        ReadOutcome::TimedOut => ReaderStep::Idle,
    }
}

// ---------------------------------------------------------------------------
// The serial interface's outgoing queue
// ---------------------------------------------------------------------------

/// Depth of the queue between the node and the transport port's writer. A
/// host that holds DTR up and does not read fills it in this many packets.
pub const OUTGOING_DEPTH: usize = 8;

/// What the serial interface reports for a packet offered to the outgoing
/// queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    /// Queued for the writer. Nothing is counted yet: the writer still drops
    /// a frame when the host closes the port before it is read, and counts
    /// what it wrote ([`tx_counted`]).
    Queued,
    /// The queue was full: the packet is dropped, nothing is counted, and the
    /// caller sees `BufferFull` with an `[IFACE_FULL]` line naming the depth.
    Full,
}

/// The interface's answer for a packet the queue did or did not take.
pub fn offered(accepted: bool) -> Offered {
    if accepted {
        Offered::Queued
    } else {
        Offered::Full
    }
}

/// Bytes the serial TX counter adds for a packet of `len` unframed bytes
/// whose framed write did or did not reach the host: the unframed packet
/// once every chunk was written, nothing for a frame the writer dropped.
///
/// The reference counts the unframed `datalen` after the serial write
/// returned (`RNodeInterface.py:724-725`); a write that fails raises before
/// it. The HDLC framing the writer adds is outside the count. Until
/// leviculum#413's census the board counted where the queue took the packet,
/// so a frame dropped because no host read the port was counted as sent.
pub fn tx_counted(written: bool, len: usize) -> usize {
    if written {
        len
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// The debug port's drain
// ---------------------------------------------------------------------------

/// How often the debug writer wakes when no log line signals it. It is also
/// how late a DTR change or the headless deadline can be seen.
pub const DEBUG_POLL_MS: u64 = 100;

/// How long one 64-byte write on the debug port may pend before the drain
/// gives up until the next wake.
pub const DEBUG_WRITE_WITHIN_MS: u64 = 50;

// A stalled write ends inside one poll period, so a host that holds DTR up
// without reading cannot keep the writer from its next wake. A build-time
// check, like the windows above.
const _: () = assert!(DEBUG_WRITE_WITHIN_MS < DEBUG_POLL_MS);

/// After this long without a host on the debug port the runtime-drain gate
/// opens anyway, so a board in the field without a debug host does not drop
/// its runtime lines for the whole boot.
pub const HEADLESS_DRAIN_OPEN_MS: u64 = 30_000;

/// What one wake of the debug writer does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainWake {
    /// Nothing: no host, and the boot is younger than
    /// [`HEADLESS_DRAIN_OPEN_MS`].
    Wait,
    /// Open the runtime-drain gate, write nothing: no host, but the boot has
    /// waited out [`HEADLESS_DRAIN_OPEN_MS`]. Runtime lines land in the ring
    /// from here and are written when a host attaches, as far as the ring
    /// holds them.
    OpenGate,
    /// Open the gate and write the ring out: a host holds DTR up. Opening is
    /// idempotent; the first wake that sees DTR is the one that opens it.
    Drain,
}

/// The debug writer's step for one wake. The port is never written while
/// DTR is down, because a CDC write with no reader pends until one comes.
pub fn drain_wake(dtr: bool, since_boot_ms: u64) -> DrainWake {
    if dtr {
        DrainWake::Drain
    } else if since_boot_ms >= HEADLESS_DRAIN_OPEN_MS {
        DrainWake::OpenGate
    } else {
        DrainWake::Wait
    }
}

/// How one bounded debug write ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugWrite {
    /// The host took the packet.
    Written,
    /// The USB stack refused it.
    Failed,
    /// [`DEBUG_WRITE_WITHIN_MS`] ran out: DTR is up but nobody reads.
    TimedOut,
}

/// Whether the drain writes the next chunk. Any write that did not land
/// ends this wake's drain; what is still in the ring waits for the next
/// wake, so a host that holds DTR up without reading costs the writer one
/// bounded wait a wake and no more.
pub fn drain_continues(write: DebugWrite) -> bool {
    write == DebugWrite::Written
}

// ---------------------------------------------------------------------------
// The debug port's commands
// ---------------------------------------------------------------------------

/// The one-byte commands one packet on the debug port asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebugCommands {
    /// `p`: print the panic and HardFault post-mortems.
    pub postmortem: bool,
    /// `s`: restart the stack watermark from here.
    pub stack_reset: bool,
    /// `c`: print a heap census on the main loop's next wake.
    pub heap_census: bool,
}

/// Which commands `packet` carries. Each runs at most once per packet,
/// wherever and however often its byte appears, and the firmware runs them
/// in the field order above. Every other byte is ignored, so terminal line
/// endings, a capital letter and stray input are harmless.
pub fn debug_commands(packet: &[u8]) -> DebugCommands {
    DebugCommands {
        postmortem: packet.contains(&b'p'),
        stack_reset: packet.contains(&b's'),
        heap_census: packet.contains(&b'c'),
    }
}

// ---------------------------------------------------------------------------
// Where a radio config goes
// ---------------------------------------------------------------------------

/// Where a radio config a host sent is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRoute {
    /// No LoRa task this boot: the channel has no consumer, a config fed to
    /// it would wedge its one slot for the rest of the boot, and no apply
    /// can be confirmed. Persist it and answer once the page write is
    /// confirmed, because "a reboot comes back on this config" is the whole
    /// of what this route may claim (#358, #363).
    Store,
    /// Hand it to the LoRa task and answer for the apply.
    Channel,
}

/// The route for a radio config on a boot that did or did not start LoRa.
pub fn config_route(lora_booted: bool) -> ConfigRoute {
    if lora_booted {
        ConfigRoute::Channel
    } else {
        ConfigRoute::Store
    }
}

/// The [`ConfigRoute::Store`] answer: stored for the next boot when the page
/// write is confirmed, refused as not persisted when it was lost.
pub fn stored_answer(
    persist: leviculum_core::envelope::Persist,
) -> leviculum_core::envelope::ConfigDelivery {
    use leviculum_core::envelope::{ConfigDelivery, Persist};
    match persist {
        Persist::Durable => ConfigDelivery::Stored,
        Persist::Lost => ConfigDelivery::StoreFailed,
    }
}

/// What a config does when the LoRa task's one-slot channel is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullSlot {
    /// The slot holds this very config: a host's retry of one the LoRa task
    /// has not consumed yet. Nothing is left to deliver, nothing is owed to
    /// flash; wait for its apply and answer for what is queued. Refusing it
    /// as undeliverable spent one of the host's three attempts on a config
    /// that was on its way, which is how one slow apply became
    /// `no_ack_after_3` (T114 DEC9947D, 2026-09-23).
    AlreadyQueued,
    /// A different config holds the slot: one send bounded by
    /// [`CONFIG_DELIVER_WITHIN_MS`] for a live LoRa task to drain it. A send
    /// that times out drops the value, so a refusal never also delivers;
    /// the refusal is [`leviculum_core::envelope::ConfigDelivery::Undeliverable`].
    GraceSend,
}

/// Decide [`FullSlot`] from the config the slot holds (`pending`) and the
/// one the host sent.
pub fn full_slot(pending: Option<RadioConfigWire>, sent: RadioConfigWire) -> FullSlot {
    if pending == Some(sent) {
        FullSlot::AlreadyQueued
    } else {
        FullSlot::GraceSend
    }
}
