//! RNode serial interface, detection, configuration, and data path
//!
//! Implements the full RNode lifecycle: detect → configure radio → validate →
//! go online → bidirectional data → reconnect on failure → graceful shutdown.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::event_log::Scalar;
use leviculum_channel_access::{jitter_slot_ms, ChannelAccess, JITTER_CW_SLOTS, JITTER_DIFS_SLOTS};
use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use leviculum_core::rnode;
use leviculum_core::transport::InterfaceId;
use rand_core::RngCore;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};

// ---------------------------------------------------------------------------
// Named constants for serial protocol timing and buffers
// ---------------------------------------------------------------------------

/// Serial baud rate for all RNode devices
const SERIAL_BAUD_RATE: u32 = 115_200;
/// Device settle time after opening serial port
const DEVICE_SETTLE: Duration = Duration::from_secs(2);
/// Wait for device to process configuration
const CONFIG_PROCESS_WAIT: Duration = Duration::from_millis(250);
/// Final settle before starting I/O
const FINAL_SETTLE: Duration = Duration::from_millis(300);
/// Detection phase read timeout
const DETECT_TIMEOUT: Duration = Duration::from_millis(200);
/// Configuration validation read timeout
const VALIDATE_TIMEOUT: Duration = Duration::from_millis(2000);
/// Reconnect retry interval
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
/// Serial read buffer size (detection + validation phases)
const SERIAL_READ_BUF: usize = 256;
/// Serial read buffer size (I/O phase, larger for sustained throughput)
const IO_READ_BUF: usize = 1024;
/// Frequency confirmation tolerance (Hz)
const FREQ_TOLERANCE_HZ: u32 = 100;
/// Device reset notification marker
const DEVICE_RESET_MARKER: u8 = 0xF8;

/// Result of an RNode detection probe
#[derive(Debug)]
struct RNodeDetectResult {
    detected: bool,
    firmware_version: Option<(u8, u8)>,
    platform: Option<u8>,
    mcu: Option<u8>,
}

/// Errors from RNode serial operations
#[derive(Debug, thiserror::Error)]
pub(crate) enum RNodeError {
    #[error("serial port error: {0}")]
    SerialPort(String),
    #[error("device not detected")]
    NotDetected,
    #[error("firmware {0}.{1} below minimum {2}.{3}")]
    FirmwareTooOld(u8, u8, u8, u8),
    #[error("radio config mismatch: {0}")]
    RadioMismatch(String),
}

impl From<tokio_serial::Error> for RNodeError {
    fn from(e: tokio_serial::Error) -> Self {
        RNodeError::SerialPort(e.to_string())
    }
}

impl From<std::io::Error> for RNodeError {
    fn from(e: std::io::Error) -> Self {
        RNodeError::SerialPort(e.to_string())
    }
}

/// Radio parameters for RNode configuration
struct RadioParams {
    frequency: u32,
    bandwidth: u32,
    tx_power: u8,
    /// Whether `tx_power` came from the absent-key default
    /// (`rnode::resolve_tx_power`) rather than an explicit `txpower` line.
    ///
    /// The default asks for the board maximum — capped by the lawful ERP limit
    /// for the frequency (`rnode::lawful_erp_dbm`) — without probing what this
    /// board's maximum is, which the RNode firmware answers by clamping to its own
    /// ceiling and echoing the clamped value
    /// (`RNode_Firmware/RNode_Firmware.ino:861-879`) — 17 dBm on an SX127x
    /// board, `PA_MAX_OUTPUT` on an SX1262 with an external PA. Confirmation is
    /// otherwise an exact match, so without this flag every such board would
    /// fail startup with `RadioMismatch` instead of running at its maximum.
    /// An explicitly configured power keeps the strict check: it is a value the
    /// operator chose, and a board that cannot deliver it must say so.
    tx_power_derived: bool,
    sf: u8,
    cr: u8,
    st_alock: Option<u16>,
    lt_alock: Option<u16>,
}

/// Configuration for spawning an RNode interface
pub(crate) struct RNodeInterfaceConfig {
    pub id: InterfaceId,
    pub name: String,
    pub port_path: String,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    /// See [`RadioParams::tx_power_derived`].
    pub tx_power_derived: bool,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    pub flow_control: bool,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    /// TEST-ONLY range emulation: drop deframed hops=0 ingress frames
    /// (see [`super::test_drop_direct_ingress_frame`]).
    pub test_drop_direct_ingress: bool,
    /// TEMPORARY (#347): the acquisition-jitter arm this interface runs,
    /// read from the environment by the builder that refuses a bad value.
    pub jitter_arm: JitterArm,
    /// The node's 16-byte identity hash, which with the interface name gives
    /// this interface its [`FrameClass`].
    pub identity_hash: [u8; 16],
}

impl RNodeInterfaceConfig {
    fn radio_params(&self) -> RadioParams {
        RadioParams {
            frequency: self.frequency,
            bandwidth: self.bandwidth,
            tx_power: self.tx_power,
            tx_power_derived: self.tx_power_derived,
            sf: self.sf,
            cr: self.cr,
            st_alock: self.st_alock,
            lt_alock: self.lt_alock,
        }
    }
}

/// A framed packet queued for serial transmission
struct QueuedFrame {
    data: Vec<u8>,
    payload_len: u64,
    high_priority: bool,
    /// Whether this is the one re-hand [`judge_airtime`] buys a frame the
    /// modem consumed without transmitting. Carried on the queue entry so the
    /// frame's second handover knows not to ask for a third.
    rehand: bool,
}

/// The ceiling of the randomised pre-TX wait this interface can impose, for
/// the `LinkProfile` a diagnostic reads it out of (`tx_jitter_max`).
///
/// Derived from the policy the TX loop actually runs — the widest value
/// [`ChannelAccess::acquisition_jitter_ms`] can return on this modulation,
/// DIFS plus the last slot of the contention window — so the figure a caller
/// sizes a delivery window with and the wait the loop imposes cannot drift
/// apart.
fn compute_jitter_max_ms(sf: u8, cr: u8, bandwidth_hz: u32) -> u64 {
    JITTER_ACQUISITION_SLOTS * jitter_slot_ms(bandwidth_hz, sf, cr)
}

/// The slots one acquisition of the channel can owe: DIFS plus the widest of
/// the equally likely contention draws.
///
/// The same count in every arm — the arms differ in what a slot is worth, not
/// in how many are drawn ([`arm_owed_jitter_ms`]).
const JITTER_ACQUISITION_SLOTS: u64 = JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 - 1;

/// What ONE acquisition of this carrier costs at worst, for the arm this
/// interface runs.
///
/// Beside [`compute_jitter_max_ms`], which is the per-frame term and stays
/// what it was: this is what the frame that TAKES the channel pays, and under
/// arm 3 the two are different quantities by a factor of the frame's airtime
/// over a contention slot. Trace 223 (2026-09-24) measured acquisitions of
/// 5.03 s and 5.53 s on a link whose per-frame ceiling is 360 ms, and a
/// diagnostic that priced the drain window on the 360 ms alone called two
/// arrivals inside its own arithmetic a loss.
///
/// The arm and its price both live here: the caller reads a number and learns
/// nothing about which policy produced it.
fn compute_acquisition_ceiling(
    arm: JitterArm,
    sf: u8,
    cr: u8,
    bandwidth_hz: u32,
) -> leviculum_core::transport::AcquisitionCeiling {
    let slot_bound = compute_jitter_max_ms(sf, cr, bandwidth_hz);
    // Arms 1 and 2 wait in slots of the modulation's own slot time, whatever
    // the frame about to go out is: one number covers every frame, so there is
    // no count of frames to report. (Arm 2 serves none of it on the host, but
    // the modem's own CSMA draws a window of the same shape before it keys, so
    // the ceiling a caller has to allow for is unchanged.) Arms 3 and 4
    // re-express the draw in whole frames, so their ceiling scales with the
    // frame it has to clear, and they differ only in how many frames it is.
    let frame_slots = arm.frame_ceiling();
    let full_frame_air = rnode::airtime_ms_with_preamble(
        rnode::HW_MTU as u32,
        bandwidth_hz,
        sf,
        cr,
        rnode::derive_preamble_symbols(sf, cr, bandwidth_hz),
    );
    leviculum_core::transport::AcquisitionCeiling {
        max_ms: slot_bound,
        frame_slots,
        full_frame_ms: match frame_slots {
            Some(slots) => slot_bound.max(slots * full_frame_air),
            None => slot_bound,
        },
    }
}

// ---------------------------------------------------------------------------
// The #347 jitter arms. TEMPORARY -- see scripts/env-knob-census.txt
// ---------------------------------------------------------------------------

/// The environment variable that selects which acquisition-jitter arm an
/// RNode host interface builds with.
pub(crate) const JITTER_ARM_ENV: &str = "LEVICULUM_JITTER_ARM";

/// The four arms by name, for the refusal message. A mistyped arm must not
/// be able to leave an operator guessing which one ran: a run that silently
/// fell back to arm 1 would be pooled into the wrong series, and a pooled
/// A/B is a void A/B.
pub(crate) const JITTER_ARM_CHOICES: &str = "1 (as it stands: the host's draw plus the modem's), \
     2 (modem CSMA only), 3 (the host's slot floored at the frame's airtime, over 2..15 frames), \
     4 (the same rule over 2..8 frames)";

/// Which acquisition-jitter policy this interface's TX loop runs: the four
/// arms of the #347 A/B, chosen once per interface build from
/// [`JITTER_ARM_ENV`].
///
/// TEMPORARY BY CONSTRUCTION. It exists so that one binary can run every arm
/// of the co-release series — patched trees on a bench is how a series gets
/// its arms mixed up, and a binary that states its arm on the bring-up line
/// cannot. It is removed together with the arms that lose: the winner becomes
/// the unconditional policy and this enum, the variable and the
/// [`arm_owed_jitter_ms`] match go with it. Deliberately NOT a config-file
/// key — a `.toml` key gets documented, depended on, and outlives the
/// question it was added to answer. The removal condition is pinned in
/// `scripts/env-knob-census.txt`, which `scripts/check-env-knobs.py` checks
/// on every `just fast` — in both directions, so the line has to go when the
/// knob does.
///
/// Arms 2 and 3 are the report of order 124 ("Two responders released by the
/// same frame — the four directions, costed", 2026-09-23, §"The three arms,
/// as binary patches on `a7e0f5c3`"): arm 2 draws and discharges the draw
/// without waiting it out, arm 3 re-expresses the same draw in units of the
/// frame it has to clear.
///
/// Arm 4 is Lew's decision of 2026-09-25 and is arm 3's rule over a shorter
/// span, not a knob on arm 3 — a knob would make "arm 3" name two policies in
/// the run documents and the register, and a series whose arm names are
/// ambiguous is a void series. It is a separate arm for exactly as long as
/// the A/B between the two spans runs.
///
/// A FIFTH arm is not a behaviour anybody has asked for. A new span is a new
/// [`Self::frame_ceiling`] and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum JitterArm {
    /// Arm 1 — the policy as it stands, and what a daemon without the
    /// variable runs: the host serves DIFS plus its own uniform draw, the
    /// modem's own CSMA draws a second window on top.
    #[default]
    AsIs,
    /// Arm 2 — modem CSMA only. The host still draws, so the RNG stream (and
    /// with it the CAD backoff ladder) is the same sequence in every arm, but
    /// the draw is discharged immediately and nothing is waited out.
    ModemOnly,
    /// Arm 3 — the draw re-expressed in units of the frame: the same number
    /// of slots, each slot floored at the airtime of the frame about to go,
    /// which scales DIFS with it exactly as 124's costing did, and both the
    /// count and its sub-frame position pinned to this interface's
    /// [`FrameClass`] — so that two ends of opposite class can never owe the
    /// same number of frames, and two ends of one class that draw the same
    /// count are still a quarter frame apart (trace 228 and 229). The
    /// guarantee is pairwise and class-conditional; [`FrameClass`] states
    /// what it does and does not cover.
    FrameSlot,
    /// Arm 4 — arm 3's rule with the count span 2..8 whole frames instead of
    /// 2..15, and everything else the same: the same draw, the same class
    /// parity, the same quarter-frame position, the same 126 ms floor at the
    /// rotation PHY.
    ///
    /// What the shorter span buys and what it costs, both measured on arm 3's
    /// own numbers: arm 3 ran 6/6 green on the rig with pinned identities
    /// (240/240 frames) and cost a 50 KB transfer 304-599 s against arm 1's
    /// 284-333 s, because one acquisition can owe fifteen frame airtimes —
    /// 7.5 s at the rotation PHY. Eight caps that at 4.0 s. The price is tie
    /// probability: seven counts split 4/3 between the classes instead of
    /// fourteen split 7/7, so a same-class pair meets more often
    /// ([`classed_frame_wait_ms`] does that arithmetic). Which trade is
    /// better is the measurement this arm exists for, and no default moves
    /// before it is on file.
    FrameSlotShort,
}

/// The top count arm 4 draws in, in whole frames.
///
/// Arm 3's ceiling is the draw window's own top ([`JITTER_ACQUISITION_SLOTS`],
/// 15) because arm 3 re-expresses that window one-for-one. Arm 4's is a
/// decision and not a derivation, so it is a number here rather than an
/// expression: eight frames, Lew 2026-09-25.
const ARM_FOUR_FRAME_CEILING: u64 = 8;

impl JitterArm {
    /// The digit the bring-up line carries and periculum reads back
    /// (`periculum/src/bench.rs::jitter_arm_of`).
    pub(crate) const fn digit(self) -> u8 {
        match self {
            Self::AsIs => 1,
            Self::ModemOnly => 2,
            Self::FrameSlot => 3,
            Self::FrameSlotShort => 4,
        }
    }

    /// The top whole-frame count this arm can owe for one acquisition, or
    /// `None` for the arms that wait in slots of the modulation rather than in
    /// frames.
    ///
    /// One function, because three things have to agree on it: the wait the TX
    /// loop actually imposes ([`classed_frame_wait_ms`]), the span the draw is
    /// folded onto, and the ceiling the interface publishes for a caller to
    /// size a delivery window with ([`compute_acquisition_ceiling`]). An arm
    /// whose published ceiling and served wait came from two constants is an
    /// arm whose drain window is priced on a wait nobody owes — which is the
    /// shape of the two false reds trace 223 produced.
    pub(crate) const fn frame_ceiling(self) -> Option<u64> {
        match self {
            // Slot-priced: one number covers every frame, and it is not a
            // count of frames at all.
            Self::AsIs | Self::ModemOnly => None,
            Self::FrameSlot => Some(JITTER_ACQUISITION_SLOTS),
            Self::FrameSlotShort => Some(ARM_FOUR_FRAME_CEILING),
        }
    }

    /// The arm one interface build runs.
    ///
    /// An unset variable — and an empty one, which is how a container
    /// harness that forwards its whole environment spells "not set" — is arm
    /// 1, the pre-#347 pacing. Any other value is refused by naming every
    /// arm, because the failure this knob exists to prevent is a run that
    /// thinks it measured an arm it did not run.
    pub(crate) fn from_env() -> Result<Self, String> {
        let Some(raw) = std::env::var_os(JITTER_ARM_ENV) else {
            return Ok(Self::AsIs);
        };
        let text = raw.to_string_lossy();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Self::AsIs);
        }
        Self::parse(trimmed).ok_or_else(|| {
            format!("{JITTER_ARM_ENV}={trimmed}: expected one of {JITTER_ARM_CHOICES}")
        })
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "1" => Some(Self::AsIs),
            "2" => Some(Self::ModemOnly),
            "3" => Some(Self::FrameSlot),
            "4" => Some(Self::FrameSlotShort),
            _ => None,
        }
    }
}

/// Where in the whole-frame counts of arm 3 or arm 4 one interface waits:
/// which half of the counts it draws in, and which quarter of a frame it takes
/// off the count it lands on.
///
/// Both arms pay their draw in whole frame airtimes, so two ends that draw the
/// same number of slots owe the same wait to the millisecond and key
/// together. Trace 228 (2026-09-24, `lora_ratchet_rotation_listened` arm 3,
/// window 24c run 1) caught exactly that: both daemons took their
/// post-drain acquisition on the same millisecond, both drew 1509 ms =
/// 3 x 503 ms, both frames died, and the identical hold cadence kept the
/// pair locked into the next exchange. Fourteen equally likely draws means
/// two ends sharing an acquisition anchor tie with p = 1/14.
///
/// Two fields of one hash answer that, and they answer two different halves
/// of it:
///
/// * The **residue class** pins each end to the even counts or the odd
///   ones. Two ends in DIFFERENT classes can then never owe the same number
///   of frames: their counts differ by at least one, which is one airtime of
///   the frame they are contending to send — 503 ms on the PHY 228 measured.
/// * The **sub-frame position** takes its quarter of that frame back off the
///   wait the count owes. Two ends of the SAME class that draw the same
///   count are then still a quarter frame apart — 126 ms at the rotation
///   PHY — which is what makes a same-count tie survivable rather than
///   fatal: the second end's CAD sees a preamble that has already started
///   and defers, where under the count alone both preambles began inside the
///   ~40 ms window in which the modem can see neither.
///
/// The position is subtracted rather than added so that the ceiling does not
/// move: the widest wait an arm can impose is still its span's top count at
/// position 0 — 15 frames under arm 3, 8 under arm 4 — which is what 224's
/// drain window and the selftest's acquisition term are priced on.
///
/// **What this guarantees, exactly.** A pair of ends of opposite class never
/// shares a wait, with probability 1 — the counts differ by at least a frame
/// less the three quarters a position can take off it, 125 ms at the
/// rotation PHY, still three times the blind window. That holds for both
/// spans, because it is a property of the parity split and not of the span's
/// width. A pair of the same class shares a wait only when it also draws the
/// same count AND carries the same position: 1/2 x 1/7 x 1/4 = 1/56 over
/// random identity pairs under arm 3, and 29/784 (about 1/27) under arm 4,
/// whose seven counts split 4/3 — against 1/14 for the count alone and 1/14
/// for no pinning at all ([`frame_counts_per_class`] carries that arithmetic).
/// Both fields are a property of one end's own identity, computed without
/// knowing who else is on the channel, so none of this is a global guarantee:
/// with three or more contenders on one anchor two of them share a class by
/// pigeonhole, and there it is a reduced probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct FrameClass {
    /// 0 for the even counts, 1 for the odd ones.
    parity: u64,
    /// Which quarter of a frame this end takes off its count, `0..4`.
    position: u64,
}

/// How many sub-frame positions one count is split into. Four, because the
/// separation a position buys is `frame / SUB_FRAME_POSITIONS` and it has to
/// stay well clear of the ~40 ms the modem is blind for after it keys: at
/// the rotation PHY's 503 ms frame a quarter is 126 ms, an eighth would be
/// 63 ms, and the next step down would be inside the window this exists to
/// clear.
const SUB_FRAME_POSITIONS: u64 = 4;

impl FrameClass {
    /// The class and position of this interface: the low bit of a stable
    /// hash of the node identity hash and the interface's own name, and the
    /// two bits above it.
    ///
    /// Both inputs, because either alone leaves a pair of contenders in one
    /// class by construction — two daemons run interfaces under the same
    /// generated name (`rnode_0` for both ends of every periculum LoRa
    /// cell), and two radios of ONE daemon share its identity hash.
    pub(crate) fn of(identity_hash: &[u8; 16], iface_name: &str) -> Self {
        let mut hash = fnv1a(FNV_OFFSET_BASIS, identity_hash);
        hash = fnv1a(hash, iface_name.as_bytes());
        Self {
            parity: hash & 1,
            position: (hash >> 1) % SUB_FRAME_POSITIONS,
        }
    }

    /// 0 for the even counts, 1 for the odd ones.
    pub(crate) const fn parity(self) -> u64 {
        self.parity
    }

    /// Which quarter of a frame this end takes off its count, `0..4`.
    pub(crate) const fn position(self) -> u64 {
        self.position
    }

    /// What this end takes off the wait its count owes, when a count is
    /// `unit_ms` wide.
    ///
    /// Rounded up, so that two positions are at least one whole quarter
    /// apart even when the frame does not divide by four — the rotation
    /// PHY's 503 ms frame gives 126 ms steps, and two adjacent positions
    /// 126 ms rather than the 125 ms a truncating quarter would leave
    /// between the first two.
    ///
    /// And capped one millisecond below the count, which is what keeps the
    /// CLASS guarantee intact where the rounding would eat it: two opposite
    /// classes are one count apart and nothing else, so an offset that could
    /// reach a whole count would let a position cancel the class. Three
    /// rounded-up quarters stay under the count for every frame worth
    /// sending and only reach it on a degenerate PHY — a frame no wider than
    /// the 6 ms slot floor — which is exactly where a guarantee is worth
    /// stating in arithmetic rather than in a comment.
    const fn sub_frame_offset_ms(self, unit_ms: u64) -> u64 {
        let quarter = unit_ms.div_ceil(SUB_FRAME_POSITIONS);
        let offset = self.position * quarter;
        if offset < unit_ms {
            offset
        } else {
            unit_ms.saturating_sub(1)
        }
    }
}

#[cfg(test)]
impl FrameClass {
    /// A class named outright, for the tests that need one property of a
    /// class rather than an identity that happens to carry it.
    const fn new(parity: u64, position: u64) -> Self {
        Self { parity, position }
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a, because the class has to be the same on every run of every build:
/// `std::hash::RandomState` is seeded per process, so a `DefaultHasher` class
/// would be a fresh coin flip after every restart and two ends could not
/// stay apart across a reconnect.
fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// How many counts of the span `JITTER_DIFS_SLOTS ..= ceiling_count` one
/// residue class holds.
///
/// The span's lowest count is DIFS, which is even, so class 0 takes the even
/// counts and class 1 the odd ones, and a span of an ODD number of counts
/// gives class 0 one more than class 1: arm 3's 2..=15 is fourteen counts split
/// 7/7, arm 4's 2..=8 is seven counts split 4/3.
///
/// That asymmetry is the whole price of a shorter span, so it is worth having
/// the arithmetic in one place. A draw is uniform over fourteen values, so a
/// class of `k` counts hands each count either `14 / k` or `14 / k + 1` of them
/// — the best a modulo of fourteen into `k` buckets can do, exactly even at
/// `k = 7` and off by one draw at `k = 4` and `k = 3`. Under arm 3 a same-class
/// pair on one acquisition anchor therefore ties on the count with p = 1/7;
/// under arm 4 with p = 50/196 in class 0 (draws 4,4,3,3) and 66/196 in class 1
/// (5,5,4), a little over the 1/4 and 1/3 a perfectly even split would give.
/// With the quarter-frame position on top (`1/4`, independent), a random
/// identity pair ties with p = 1/56 under arm 3 and 29/784 (about 1/27) under
/// arm 4.
fn frame_counts_per_class(ceiling_count: u64, parity: u64) -> u64 {
    let counts_in_span = ceiling_count.saturating_sub(JITTER_DIFS_SLOTS) + 1;
    // Never zero: this is a divisor, and a divisor must not depend on two
    // policy constants staying where they are. A span that held one count would
    // put every draw on it — a degenerate arm, which is a measurement somebody
    // has to explain, not a panic in the TX loop.
    counts_in_span.saturating_sub(parity).div_ceil(2).max(1)
}

/// The wait the whole-frame arms owe for a draw of `drawn_slots`, pinned to
/// `class`: a whole number of `unit_ms` counts up to `ceiling_count`, less the
/// class's sub-frame position.
///
/// The draw window is `JITTER_DIFS_SLOTS ..= JITTER_DIFS_SLOTS +
/// JITTER_CW_SLOTS - 1`, i.e. 2..=15 (14 values), and it is the same draw in
/// every arm. What differs is the span it is folded onto: arm 3 folds it onto
/// its own width (`ceiling_count` = 15, so class 1 reaches 15 and the window's
/// top is preserved one-for-one), arm 4 onto 2..=8 (`ceiling_count` = 8, which
/// class 0 reaches). Either way the span's top is reachable by the class that
/// carries it, so the ceiling a caller sizes its window with is a wait that can
/// actually happen, and every count stays at or above DIFS.
///
/// The fold is a modulo rather than a nudge to the neighbouring count, so the
/// draws spread as evenly over a class's counts as fourteen of them can — see
/// [`frame_counts_per_class`] for what that is per arm. A nudge would pile an
/// extra draw onto one count and make that count the likeliest place for two
/// same-class ends to meet, which is worse than the unpinned arm at the count
/// it picks.
///
/// The position then comes off the count's wait, which is where two ends of
/// the SAME class separate. Both bounds are stated here rather than left to
/// the arithmetic: never below `difs_ms`, which is what the medium owes
/// before any contention at all and what a degenerate PHY (a frame no wider
/// than a slot) would otherwise fall under, and never above the acquisition
/// ceiling the span's top defines.
fn classed_frame_wait_ms(
    drawn_slots: u64,
    class: FrameClass,
    unit_ms: u64,
    difs_ms: u64,
    ceiling_count: u64,
) -> u64 {
    let counts_per_class = frame_counts_per_class(ceiling_count, class.parity());
    let index = drawn_slots.saturating_sub(JITTER_DIFS_SLOTS) % counts_per_class;
    let count = JITTER_DIFS_SLOTS + class.parity() + 2 * index;
    (count * unit_ms)
        .saturating_sub(class.sub_frame_offset_ms(unit_ms))
        .max(difs_ms)
        .min(ceiling_count * unit_ms)
}

/// What one acquisition owes before the frame whose payload is
/// `payload_len` bytes may be handed to the modem, under `arm`.
///
/// Every arm draws. The draw is the interface's only consumer of the
/// channel-access RNG, so keeping it in all of them keeps the CAD backoff
/// ladder the same sequence in every arm — otherwise two arms would differ in
/// two things at once and the A/B would measure their sum (124 §3).
///
/// TEMPORARY, with [`JitterArm`]: when an arm wins, its branch becomes the
/// body of the enqueue path and this function goes away.
fn arm_owed_jitter_ms(
    arm: JitterArm,
    access: &mut ChannelAccess,
    payload_len: usize,
    bandwidth_hz: u32,
    sf: u8,
    cr: u8,
    frame_class: FrameClass,
) -> u64 {
    let drawn = access.acquisition_jitter_ms();
    match arm {
        JitterArm::AsIs => drawn,
        JitterArm::ModemOnly => {
            // Report the draw as served at once: the debt has to be
            // discharged by somebody, or the next acquisition inherits a
            // wait this arm decided not to serve.
            access.jitter_spent(drawn);
            0
        }
        JitterArm::FrameSlot | JitterArm::FrameSlotShort => {
            // The two whole-frame arms are one body and one span apart. Read
            // back rather than matched a second time, so an arm cannot serve a
            // span the interface does not publish.
            let Some(ceiling_count) = arm.frame_ceiling() else {
                return drawn;
            };
            let slot = access.jitter_slot();
            // `jitter_slot_ms` clamps to at least 6 ms, so this cannot be
            // zero; the guard is here because a division by a policy figure
            // must not depend on a clamp two crates away.
            if slot == 0 {
                return drawn;
            }
            let frame_air = rnode::airtime_ms_with_preamble(
                payload_len as u32,
                bandwidth_hz,
                sf,
                cr,
                rnode::derive_preamble_symbols(sf, cr, bandwidth_hz),
            );
            classed_frame_wait_ms(
                drawn / slot,
                frame_class,
                frame_air.max(slot),
                JITTER_DIFS_SLOTS * slot,
                ceiling_count,
            )
        }
    }
}

/// The channel-access policy one RNode transmit path runs: seeded from the
/// host's entropy, told the modulation whose symbol time sizes its slots.
///
/// Built per connection. A reconnect is a new acquisition — the radio was off
/// — so it starts owing a fresh wait, which is what a boot-fresh
/// [`ChannelAccess`] already does.
fn channel_access_for(bandwidth_hz: u32, sf: u8, cr: u8) -> ChannelAccess {
    let mut access = ChannelAccess::new(rand_core::OsRng.next_u32());
    access.set_phy(bandwidth_hz, sf, cr);
    access
}

/// What an announce costs on this interface's carrier, as the bits per second
/// the core's announce bandwidth cap takes its 2 % share of (Codeberg #404).
///
/// Python's `RNodeInterface` sets `self.bitrate` from the live radio settings
/// unconditionally (`RNodeInterface.py:695`), so a Python RNode neighbour on
/// the same channel holds back its transit announces whether or not anybody
/// wrote a `bitrate` line in a config file. Ours registered the config key or
/// nothing at all, which left the cap inert on the one medium where an
/// announce is most expensive.
///
/// The number is the effective rate out of the airtime arithmetic, not the
/// nominal symbol rate (see [`rnode::announce_cap_bitrate_bps`] for why the
/// two differ by a third at SF10 and why the deviation rule permits it). The
/// preamble is the RNode firmware's own derivation from the PHY
/// ([`rnode::derive_preamble_symbols`]) rather than the modem default, because
/// that is what the firmware programs and therefore what goes on the air —
/// this interface never sends a preamble length, it sends the PHY and lets the
/// firmware derive it.
fn announce_cap_bitrate(sf: u8, cr: u8, bandwidth_hz: u32) -> Option<u32> {
    let bps = rnode::announce_cap_bitrate_bps(
        bandwidth_hz,
        sf,
        cr,
        rnode::derive_preamble_symbols(sf, cr, bandwidth_hz),
    );
    // 0 is the value that REMOVES a cap entry, and it is what the arithmetic
    // returns for a PHY whose airtime is not computable. A radio that cannot
    // be described is not transmitting announces to cap, so say `None` and let
    // the config key (or nothing) decide.
    (bps > 0).then_some(bps)
}

/// Default channel buffer size for RNode interfaces.
/// Smaller than TCP because LoRa bitrates are orders of magnitude lower.
pub(crate) const RNODE_DEFAULT_BUFFER_SIZE: usize = 64;

/// Maximum queued TX packets when flow control is active and device is busy.
/// Python uses an unbounded queue. Bounded to 64 here because at LoRa bitrates,
/// a full unbounded queue contains minutes-old stale packets. Drop oldest,
/// counted and named by `RNODE_TX_QUEUE_DROP` (drops are loud).
const FLOW_CONTROL_QUEUE_LIMIT: usize = 64;

/// How long the CMD_READY flow-control gate may hold queued frames before the
/// io task reports it. One firmware channel-stat cadence: the
/// RNode emits CMD_STAT_CHTM every ~2 s, while a READY normally follows a TX
/// within one packet airtime — sub-second at every supported rate. A gate
/// still closed after a full stat cadence is no longer ordinary airtime wait;
/// it is the duty-lock shape (no TX completion ⇒ no READY,
/// `RNode_Firmware.ino:1624`).
const TX_GATED_EVENT_AFTER: Duration = Duration::from_secs(2);

/// Repeat cadence for `RNODE_TX_GATED` while the gate stays closed. Bounded
/// so an hour-long duty lock produces hundreds of log lines, not one per
/// event-loop iteration.
const TX_GATED_EVENT_REPEAT: Duration = Duration::from_secs(10);

/// The CMD_READY queue-state query. The firmware dispatches its
/// `command == CMD_READY` branch only on a byte *following* the command
/// byte (`serial_callback`, `RNode_Firmware.ino:765ff`: the first in-frame
/// byte just latches `command`), so the query must carry one payload byte;
/// its value is ignored. The response echoes CMD_READY with 0x01 (queue
/// not full) or 0x00 (queue full) — `RNode_Firmware.ino:1003-1008`,
/// `Utilities.h:1157,1164`. Those two indicate functions have no other
/// call sites: CMD_READY is strictly a query/response exchange, never a
/// spontaneous signal (the flowval leg-B deadlock, 2026-08-21, came from
/// waiting for one).
const READY_QUERY_FRAME: [u8; 4] = [kiss::FEND, rnode::CMD_READY, 0x00, kiss::FEND];

/// Ceiling for the CMD_READY re-query backoff while the gate is closed.
/// Under a duty lock the firmware queue stays full for minutes, and the
/// poll shares the serial line the modem also uses to deliver RX frames —
/// so the cadence must flatten out. 2 s (one CHTM stat cadence) bounds the
/// chatter to a 4-byte frame every 2 s while capping worst-case
/// gate-release latency at the same interval the firmware already uses
/// for its own periodic reporting.
const READY_POLL_MAX: Duration = Duration::from_secs(2);

/// First re-query delay after a TX or an unanswered/negative CMD_READY
/// query: one full-size packet airtime at the configured PHY. The queue
/// state can only change when a TX completes, and completing a queued
/// full-size frame takes at least this long — polling faster cannot
/// observe a transition, it only spends serial bandwidth. Subsequent
/// re-queries double the delay up to [`READY_POLL_MAX`].
fn ready_poll_initial(sf: u8, cr: u8, bandwidth_hz: u32) -> Duration {
    let bitrate = rnode::compute_bitrate(sf, cr, bandwidth_hz);
    if bitrate == 0 {
        return READY_POLL_MAX;
    }
    let airtime_ms = (rnode::HW_MTU as u64 * 8 * 1000) / bitrate as u64;
    Duration::from_millis(airtime_ms.max(1)).min(READY_POLL_MAX)
}

/// What the firmware on the other end of this connection has said about its
/// own channel access, as it said it.
///
/// Both figures are unsolicited and both are reported before the first frame
/// can be handed over, which is what lets the post-TX hold be priced from the
/// modem's own numbers rather than from ours:
///
/// * `CMD_STAT_PHYPRM` carries `csma_slot_time_ms` and `csma_difs_ms`.
///   `updateBitrate()` recomputes both from the modulation and then calls
///   `setPreamble()`, whose last statement is `kiss_indicate_phy_stats()`
///   (`Utilities.h:1226-1228,1243-1258`), so every radio configuration —
///   including the one this interface performs at bring-up — reports them.
/// * `CMD_STAT_CSMA` carries the contention band and its window. It is sent
///   only when the band CHANGES (`RNode_Firmware.ino:1614-1618`, inside the
///   `new_cw_band != cw_band` branch), so a run that stays in band 1
///   throughout never sends one and `cw_max` stays `None`.
///
/// A `None` field means the modem has not said, and the hold falls back to
/// the same reference derivation [`ChannelAccess`] draws its own waits from.
#[derive(Debug, Clone, Copy, Default)]
struct FirmwareCsma {
    /// `csma_slot_ms` — 12 symbol times, clamped (`Utilities.h:1247-1249`).
    slot_ms: Option<u64>,
    /// `difs_ms` = `CSMA_SIFS_MS + 2 * csma_slot_ms` (`Utilities.h:1250`).
    difs_ms: Option<u64>,
    /// `cw_max`, the EXCLUSIVE upper bound of `random(cw_min, cw_max)`
    /// (`RNode_Firmware.ino:1625`, Arduino `random`).
    cw_max: Option<u8>,
}

/// The wait one handed-over frame imposes before the next may follow it, and
/// the three terms it is made of — each term is an event field, so a census
/// can check the arithmetic rather than trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TxHold {
    held_ms: u64,
    airtime_ms: u64,
    difs_ms: u64,
    cw_ms: u64,
}

/// How long the host must hold the next frame after handing one to the modem,
/// so the modem's queue never holds more than one frame.
///
/// The RNode firmware runs its CSMA once per queue drain and then flushes
/// everything it holds with no carrier sense between frames (`tx_queue_handler`
/// → `flush_queue()`, `RNode_Firmware.ino:1623-1645`), so every frame that is
/// already in the modem's queue when the contest is won goes on the air deaf.
/// Two nodes that fill their queues in the same medium-free window therefore
/// destroy each other's bursts wholesale rather than one frame at a time —
/// `sent=10 recv=4` on `lora_ratchet_rotation`, 2026-09-23. The firmware sends
/// no TX-done, so the host cannot observe the frame leaving the air; it prices
/// it instead, from what it already knows:
///
/// ```text
/// hold = airtime(frame at the running PHY) + DIFS + longest contention draw
/// ```
///
/// The last two terms are the firmware's own, taken from its stat frames where
/// it has sent them ([`FirmwareCsma`]) and otherwise derived the way
/// [`ChannelAccess`] derives the wait it draws. The result is the instant the
/// modem can at the earliest be finished with the frame it holds: airtime for
/// the frame itself, and DIFS plus the widest window for the contest the
/// firmware runs before the NEXT one. A frame handed over then finds an empty
/// queue and gets its own CSMA contest.
///
/// This is not [`rnode::compute_spacing_ms`], which computes the same shape
/// from constants that assume a 24 ms slot and adds a fixed 100 ms margin.
/// The slot is a function of the modulation (6 ms above 30 kbps, up to 100 ms
/// at SF12), and the modem reports the value it is actually using.
///
/// Nothing here asks what the frame contains: a packet is a packet, and the
/// only input from the frame is its length on the air.
fn tx_hold(frame_len: u32, bandwidth_hz: u32, sf: u8, cr: u8, csma: &FirmwareCsma) -> TxHold {
    let airtime_ms = rnode::airtime_ms_with_preamble(
        frame_len,
        bandwidth_hz,
        sf,
        cr,
        rnode::derive_preamble_symbols(sf, cr, bandwidth_hz),
    );
    let slot_ms = csma
        .slot_ms
        .unwrap_or_else(|| jitter_slot_ms(bandwidth_hz, sf, cr));
    let difs_ms = csma.difs_ms.unwrap_or(JITTER_DIFS_SLOTS * slot_ms);
    // `random(cw_min, cw_max)` is upper-exclusive, so the longest draw is
    // `cw_max - 1` slots. Unreported, the band-1 window this interface's own
    // policy uses applies (`JITTER_CW_SLOTS` equally likely draws, the widest
    // being the last), which is the same figure `compute_jitter_max_ms`
    // reports as this interface's jitter ceiling.
    let cw_slots = csma
        .cw_max
        .map(|m| (m as u64).saturating_sub(1))
        .unwrap_or(JITTER_CW_SLOTS as u64 - 1);
    let cw_ms = cw_slots * slot_ms;
    TxHold {
        // The serial floor still binds underneath: a PHY whose airtime is not
        // computable (bandwidth 0 — `airtime_ms_with_preamble` returns 0)
        // must not turn into a hold of zero, which would hand the modem a
        // whole burst at serial speed.
        held_ms: (airtime_ms + difs_ms + cw_ms).max(rnode::MIN_SPACING_MS),
        airtime_ms,
        difs_ms,
        cw_ms,
    }
}

/// The hold a FULL-SIZE frame imposes at a given PHY, before the modem has
/// reported any CSMA figures of its own: the ceiling of [`tx_hold`].
///
/// Two callers, one derivation: the reconnect task logs it beside the
/// bitrate, and the handle reports it as this interface's per-frame
/// turnaround (`Interface::frame_turnaround_ms`). MTU-sized because a
/// turnaround a caller uses to size a timeout must bound every frame the
/// interface may be handed, not the one it happens to hold.
fn max_tx_hold(bandwidth_hz: u32, sf: u8, cr: u8) -> TxHold {
    tx_hold(
        rnode::HW_MTU as u32,
        bandwidth_hz,
        sf,
        cr,
        &FirmwareCsma::default(),
    )
}

// ---------------------------------------------------------------------------
// Airtime accounting: did the modem key the frame it was handed?
// ---------------------------------------------------------------------------

/// The window `airtime_short` is a ratio over. `update_airtime` takes two
/// 7500 ms bins — `AIRTIME_BINLEN_MS` is `STATUS_INTERVAL_MS * DCD_SAMPLES`
/// = 3 * 2500 (`RNode_Firmware/Config.h:176-183`) — as
/// `(airtime_bins[cb]+airtime_bins[pb])/(2*AIRTIME_BINLEN_MS)`
/// (`RNode_Firmware/RNode_Firmware.ino:698`).
const AIRTIME_WINDOW_MS: u64 = 15_000;

/// Full scale of the CHTM `airtime_*`/`channel_load_*` u16 fields: the
/// firmware multiplies the 0..1 ratio by 100*100 before it writes them
/// (`kiss_indicate_channel_stats`, `RNode_Firmware/Utilities.h:959-964`), so
/// one raw unit is 0.01 % — 1.5 ms inside [`AIRTIME_WINDOW_MS`], against the
/// several hundred milliseconds one frame costs at every PHY we run.
const CHTM_FULL_SCALE: u64 = 10_000;

/// The window the modem's own DCD busy fraction covers: a `DCD_SAMPLES` =
/// 2500 ring sampled every `STATUS_INTERVAL_MS` = 3 ms
/// (`RNode_Firmware/Config.h:176-178`), folded into `local_channel_util`
/// once a second (`RNode_Firmware/RNode_Firmware.ino:1451-1458`).
const DCD_WINDOW_MS: u64 = 7_500;

/// One CHTM period. `check_modem_status` folds the DCD ring into
/// `local_channel_util` and then calls `update_airtime`, whose last statement
/// emits the frame, every `UTIL_UPDATE_INTERVAL_MS` = 1000 ms
/// (`RNode_Firmware/Config.h:179`, `RNode_Firmware/RNode_Firmware.ino:1453`).
/// The 2500-sample ring wraps at a non-multiple of the interval, so the
/// spacing is never longer than this and occasionally shorter.
const CHTM_PERIOD: Duration = Duration::from_millis(1_000);

/// Milliseconds since the Unix epoch, for the two timestamps
/// `LORA_TX_UNACCOUNTED` carries. Wall clock rather than a process-local
/// monotonic base on purpose: the measurement that produced this observable
/// laid a sender's handovers against a listener's decodes and a second node's
/// receives, all in different processes on different hosts.
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What the frame's own airtime is worth in raw `airtime_short` units.
///
/// Rounded up, so any frame whose airtime is computable expects at least one
/// unit and an unmoved ledger is always a statement about a frame that should
/// have moved it. Zero means the PHY has no computable airtime (bandwidth 0 —
/// `airtime_ms_with_preamble` returns 0 there), and the accounting declines to
/// judge.
fn expected_airtime_raw(airtime_ms: u64) -> u16 {
    ((airtime_ms * CHTM_FULL_SCALE).div_ceil(AIRTIME_WINDOW_MS)).min(u16::MAX as u64) as u16
}

/// Configured airtime locks, in the raw units the CHTM fields use.
///
/// The firmware stores them as `st_airtime_limit = at/(100.0*100.0)` and
/// discards a limit at or above 1.0 (`RNode_Firmware/RNode_Firmware.ino:943-952`),
/// which is the same scaling `airtime_short` is reported in — so a limit and
/// a reading are directly comparable. 0 means no lock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AirtimeLock {
    st: u16,
    lt: u16,
}

impl AirtimeLock {
    fn from_config(st: Option<u16>, lt: Option<u16>) -> Self {
        let sane = |v: Option<u16>| match v {
            Some(v) if (v as u64) < CHTM_FULL_SCALE => v,
            _ => 0,
        };
        Self {
            st: sane(st),
            lt: sane(lt),
        }
    }
}

/// The numbers one "did this frame key?" decision is made from: the ledger as
/// it stood at the handover, the ledger in the CHTM under judgement, and what
/// the frame should have cost.
#[derive(Debug, Clone, Copy)]
struct AirtimeAccount {
    /// `airtime_short` (raw) as of the handover.
    baseline_short: u16,
    /// `airtime_short` (raw) in the CHTM being judged.
    observed_short: u16,
    /// `airtime_long` (raw) as of the handover.
    baseline_long: u16,
    /// `airtime_long` (raw) in the same CHTM.
    observed_long: u16,
    /// `channel_load_short` (raw) in the same CHTM — `total_channel_util`,
    /// which is `local_channel_util + airtime` clamped at 1.0.
    observed_load_short: u16,
    /// What the frame's own airtime is worth, per [`expected_airtime_raw`].
    expected_short: u16,
    /// The hold this frame owed ([`TxHold::held_ms`]): the span the medium
    /// would have had to be busy for, for the firmware's CSMA to still be
    /// legitimately holding the frame.
    hold_ms: u64,
    /// The locks this interface configured, if any.
    lock: AirtimeLock,
}

/// What the modem's ledger says about one handed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AirtimeVerdict {
    /// The ledger rose: the frame reached `add_airtime`, which is reachable
    /// only after `endPacket()` returned
    /// (`RNode_Firmware/RNode_Firmware.ino:744-751`).
    Keyed,
    /// The ledger did not move, the medium was free and no lock was armed:
    /// the modem consumed the frame without transmitting it.
    Unaccounted,
    /// The reading cannot decide. The reason is a stable token, logged rather
    /// than counted — every one of them is a case where a silent consume and
    /// an ordinary wait look alike from the host.
    Undecided(&'static str),
}

/// Judge one handed frame against the modem's own airtime ledger.
///
/// The firmware answers nothing on six of its ten paths from an accepted
/// `CMD_DATA` frame to no transmission (the path table of 2026-09-24 at
/// firmware tag 1.85; the two that can produce "written, never aired, not
/// queued afterwards" are the length guards that pop start and length BEFORE
/// testing them, `RNode_Firmware/RNode_Firmware.ino:586-589` in `flush_queue`
/// and `RNode_Firmware/RNode_Firmware.ino:626-628` in `pop_queue`). What it
/// does emit is one per-transmission receipt, and the host already logs it
/// without reading it: `kiss_indicate_channel_stats`
/// (`RNode_Firmware/RNode_Firmware.ino:712`) is the last statement of
/// `update_airtime`, which is the last statement of both queue drains,
/// reached only after `transmit()` ran
/// `endPacket()` and `add_airtime` folded the cost in. A rise in
/// `airtime_short` is therefore proof the frame keyed, and no rise is the
/// shape of a silent consume.
///
/// The decision is deliberately asymmetric: every ambiguity resolves AWAY
/// from the accusation, because a false `LORA_TX_UNACCOUNTED` would poison
/// the instrument it exists to be.
///
/// * **Any** rise is [`AirtimeVerdict::Keyed`], not just a rise of the
///   expected size. `airtime_bins` is written by `add_airtime` alone and the
///   host holds one frame in the modem at a time ([`tx_hold`]), so a rise in
///   this window can only be this frame — while a rise SMALLER than expected
///   is what a keyed frame looks like when a bin ages out in the same CHTM.
///   [`AirtimeAccount::expected_short`] is therefore reported, never
///   thresholded, and the couple of percent between our airtime derivation
///   and the firmware's own cost formula
///   (`RNode_Firmware/RNode_Firmware.ino:654-665`) cannot change a verdict.
/// * A NEGATIVE step is undecided, not accusing: `airtime_bins[nb] = 0`
///   drops a 7500 ms bin out of the two-bin window every 7500 ms
///   (`RNode_Firmware/RNode_Firmware.ino:698`), and a keyed frame plus an
///   aged-out one reads as a fall. Measured: `airtime_short` went 17.92 ->
///   16.40 across a keyed 3.28 % frame, 2026-09-24 on t-beam-1.
/// * An unmoved `airtime_short` with a RISEN `airtime_long` is undecided.
///   `longterm_airtime` sums all bins over an hour
///   (`RNode_Firmware/RNode_Firmware.ino:700-702`), so it does not fall when
///   the short window rotates: it is the independent receipt that separates
///   an exact cancellation from a silent consume. One raw unit of it is
///   360 ms, so it can only corroborate frames that cost at least that much.
/// * A medium that could have been busy for the whole hold is undecided. The
///   firmware's CSMA does not start its DIFS wait until the medium is free,
///   and restarts it whenever the medium goes busy again
///   (`RNode_Firmware/RNode_Firmware.ino:1630-1635`), so a busy medium is a
///   frame still legitimately queued. The host reads the busy fraction out of
///   the same CHTM — `channel_load_short` is `local_channel_util + airtime`
///   clamped at 1.0 (`RNode_Firmware/RNode_Firmware.ino:1459-1460`), so
///   subtracting `airtime_short` recovers the DCD fraction — and converts it
///   to milliseconds over [`DCD_WINDOW_MS`]. Busy for less than the hold
///   means the medium was demonstrably free part of it.
/// * An armed airtime lock is undecided. It defers rather than discards
///   (`RNode_Firmware/RNode_Firmware.ino:1624`, `tx_queue_handler` runs only
///   `if (!airtime_lock ...)`) and it signals nothing over KISS, so the host
///   can only infer it from its own configured limit against the reported
///   airtime.
fn judge_airtime(a: &AirtimeAccount) -> AirtimeVerdict {
    if a.expected_short == 0 {
        return AirtimeVerdict::Undecided("airtime_not_computable");
    }
    let step = a.observed_short as i32 - a.baseline_short as i32;
    if step > 0 {
        return AirtimeVerdict::Keyed;
    }
    if step < 0 {
        return AirtimeVerdict::Undecided("bin_rotation");
    }
    if a.observed_long > a.baseline_long {
        return AirtimeVerdict::Undecided("longterm_rose");
    }
    if a.lock.st != 0 && a.observed_short >= a.lock.st {
        return AirtimeVerdict::Undecided("st_airtime_lock");
    }
    if a.lock.lt != 0 && a.observed_long >= a.lock.lt {
        return AirtimeVerdict::Undecided("lt_airtime_lock");
    }
    // `total_channel_util` is clamped at 1.0, so at full scale the DCD
    // fraction underneath is unrecoverable and the medium was in any case as
    // busy as the modem can report.
    if (a.observed_load_short as u64) >= CHTM_FULL_SCALE {
        return AirtimeVerdict::Undecided("medium_busy");
    }
    let dcd_raw = a.observed_load_short.saturating_sub(a.observed_short) as u64;
    let busy_ms = dcd_raw * DCD_WINDOW_MS / CHTM_FULL_SCALE;
    if busy_ms >= a.hold_ms {
        return AirtimeVerdict::Undecided("medium_busy");
    }
    AirtimeVerdict::Unaccounted
}

/// A frame the modem has been handed and has not yet accounted for.
///
/// One at a time, because [`tx_hold`] keeps one frame at a time in the modem.
/// It carries its own copy of the serial frame: that copy is the whole of the
/// workaround, and it is why the re-hand costs nothing but the airtime of one
/// duplicate.
struct PendingHandover {
    /// The KISS frame as it was written, ready to be written again.
    data: Vec<u8>,
    payload_len: u64,
    high_priority: bool,
    /// Whether this frame IS a re-hand. A frame is re-handed once: a second
    /// silent consume of the same frame is a modem that is not going to send
    /// it, and repeating into that only buys latency for everything behind it.
    is_rehand: bool,
    /// When the frame was handed over, on the wall clock the event reports.
    /// The windows below are monotonic; this one is comparable across hosts.
    handed_unix_ms: u64,
    /// Earliest instant at which a missing rise means anything — the frame
    /// cannot have keyed before its hold elapsed.
    due: tokio::time::Instant,
    /// Last instant at which this CHTM is still about this frame:
    /// [`CHTM_PERIOD`] past `due`. Past it the two-bin window may have
    /// rotated and the reading is no longer the frame's.
    deadline: tokio::time::Instant,
    account: AirtimeAccount,
}

/// How many handed-but-unaccounted frames the interface tracks at once.
///
/// [`tx_hold`] keeps one frame in the modem at a time, so in the shape this
/// observable was built for the queue holds one entry — two while a frame
/// whose receipt CHTM has not arrived yet is followed by the next handover.
/// The cap exists so a modem that never sends CHTM at all (an AVR RNode
/// compiles `kiss_indicate_channel_stats` to nothing) cannot accumulate frame
/// copies forever.
const PENDING_HANDOVERS_MAX: usize = 8;

/// Settle the frames the modem has not yet accounted for against a
/// `CMD_STAT_CHTM` that has just arrived, oldest first.
///
/// Returns the one frame to re-hand, if this CHTM produced that verdict.
///
/// Oldest-first and one accusation per CHTM. Each pending frame carries the
/// ledger as it stood at its own handover, so a rise credits the OLDEST
/// unresolved frame and then the next — which is the honest granularity: the
/// ledger is an aggregate over the whole modem, and nothing in it says which
/// frame a millisecond belonged to. In a pipeline that therefore under-reports
/// (a rise belonging to a later frame absolves an earlier one), and in the
/// shape this was measured in — one frame in the modem at a time — the two
/// coincide.
///
/// A verdict of [`AirtimeVerdict::Undecided`] LEAVES the frame pending: the
/// reasons are properties of one reading, and the next CHTM inside the
/// frame's deadline may decide it. Past the deadline the frame is dropped
/// unjudged, because the two-bin window may have rotated by then and the
/// reading is no longer about this frame.
fn settle_handovers(
    name: &str,
    counters: &InterfaceCounters,
    pendings: &mut VecDeque<PendingHandover>,
    cs: &rnode::ChannelStats,
    now: tokio::time::Instant,
    chtm_unix_ms: u64,
) -> Option<QueuedFrame> {
    while let Some(mut front) = pendings.pop_front() {
        if now > front.deadline {
            tracing::debug!(
                target: "leviculum_std::interfaces::rnode::tx_trace",
                "LORA_TX_ACCOUNT iface={name} len={} verdict=undecided reason=chtm_late",
                front.payload_len
            );
            continue;
        }
        front.account.observed_short = cs.airtime_short;
        front.account.observed_long = cs.airtime_long;
        front.account.observed_load_short = cs.channel_load_short;
        match judge_airtime(&front.account) {
            AirtimeVerdict::Keyed => {
                tracing::debug!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX_ACCOUNT iface={name} len={} verdict=keyed",
                    front.payload_len
                );
                continue;
            }
            AirtimeVerdict::Undecided(reason) => {
                tracing::debug!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX_ACCOUNT iface={name} len={} verdict=undecided reason={reason}",
                    front.payload_len
                );
                pendings.push_front(front);
                return None;
            }
            // The ledger has not moved, but the frame's own hold has not
            // elapsed either: the modem may not have keyed it YET. Nothing to
            // say until a CHTM arrives past the hold.
            AirtimeVerdict::Unaccounted if now < front.due => {
                pendings.push_front(front);
                return None;
            }
            AirtimeVerdict::Unaccounted => {
                counters
                    .tx_unaccounted
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Warn, not debug: a frame the modem consumed without
                // transmitting is packet loss with no other witness on this
                // host. Same target as `LORA_TX` and `LORA_CHTM` so a capture
                // that reads the handovers reads this beside them, and the
                // two timestamps are the pair that pins it: the handover the
                // accusation is about, and the reading that failed to
                // account for it.
                tracing::warn!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX_UNACCOUNTED iface={name} len={} handover_t={} chtm_t={} \
                     expected_delta={:.2}",
                    front.payload_len,
                    front.handed_unix_ms,
                    chtm_unix_ms,
                    front.account.expected_short as f64 / 100.0
                );
                if front.is_rehand {
                    // Second strike for the same frame. One re-hand is a
                    // dropped frame recovered; a modem that swallows the
                    // re-hand too is not going to send this frame, and the
                    // frame is now lost for good — counted the way the
                    // `error_txfailed` path counts the frames it abandons.
                    counters
                        .tx_queue_drops
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters
                        .tx_dropped_bytes
                        .fetch_add(front.payload_len, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(
                        event = "RNODE_TX_QUEUE_DROP",
                        iface = %Scalar(name),
                        len = front.payload_len,
                        depth = pendings.len(),
                        reason = "unaccounted_twice",
                    );
                    return None;
                }
                tracing::warn!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX_REHAND iface={name} len={} handover_t={}",
                    front.payload_len,
                    front.handed_unix_ms
                );
                return Some(QueuedFrame {
                    data: front.data,
                    payload_len: front.payload_len,
                    high_priority: front.high_priority,
                    rehand: true,
                });
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Configuration (includes detection)
// ---------------------------------------------------------------------------

/// Open + configure a serial RNode in one call. Retained for the hardware
/// smoke test; the production path uses [`open_serial_port`] +
/// [`configure_stream`] separately via the reconnect loop.
#[cfg(test)]
async fn configure_rnode(
    port_path: &str,
    radio: &RadioParams,
) -> Result<(tokio_serial::SerialStream, RNodeDetectResult), RNodeError> {
    let mut port = open_serial_port(port_path).await?;
    let detect_result = configure_stream(&mut port, radio, port_path).await?;
    Ok((port, detect_result))
}

/// Open the serial port (115200/8N1, no flow control) and wait for the device
/// to settle. Serial-specific: opening the port toggles DTR, which reboots many
/// RNode devices, hence the settle delay before any protocol I/O.
async fn open_serial_port(port_path: &str) -> Result<tokio_serial::SerialStream, RNodeError> {
    let builder = tokio_serial::new(port_path, SERIAL_BAUD_RATE)
        .data_bits(tokio_serial::DataBits::Eight)
        .stop_bits(tokio_serial::StopBits::One)
        .parity(tokio_serial::Parity::None)
        .flow_control(tokio_serial::FlowControl::None);

    let port = tokio_serial::SerialStream::open(&builder)?;

    // Wait for device to settle (reboot-on-open)
    tokio::time::sleep(DEVICE_SETTLE).await;

    Ok(port)
}

/// Detect the RNode, validate firmware, configure the radio, and confirm —
/// over an already-open, settled byte channel.
///
/// Carrier-agnostic: works on any `AsyncRead + AsyncWrite` stream, whether a
/// `tokio_serial::SerialStream` or a host-supplied channel (USB-CDC, BLE GATT,
/// mock pipe). The far end speaks RNode KISS regardless of substrate.
///
/// Sequence follows Python `RNodeInterface.configure_device()` from the detect
/// step onward (the port-open + device-settle step is the caller's, since it is
/// substrate-specific):
/// 1. Detect + validate firmware >= 1.52
/// 2. Send config commands: radio OFF, frequency, bandwidth, txpower, sf, cr,
///    [st_alock], [lt_alock], radio ON
/// 3. Sleep 250ms, read confirmation frames
/// 4. Validate: frequency within 100 Hz, others exact match
/// 5. Sleep 300ms
///
/// The leading radio OFF in step 2 is the one frame Python does not send.
/// `initRadio` (`reference/Reticulum/RNS/Interfaces/RNodeInterface.py:470-478`)
/// writes the five parameters, the two airtime locks and then `RADIO_STATE_ON`,
/// and never an OFF; `reset_radio_state` above it (`:414-422`) only clears
/// Python's own read-back variables and puts nothing on the wire. Ours is a
/// deliberate deviation, and `send_radio_config` carries the firmware reasoning
/// for it. Anyone diffing this block against Python should expect that extra
/// frame rather than remove it as drift.
async fn configure_stream<S>(
    port: &mut S,
    radio: &RadioParams,
    name: &str,
) -> Result<RNodeDetectResult, RNodeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // --- Detection phase ---
    let detect_result = detect_on_port(port).await?;

    // Validate firmware version
    if let Some((major, minor)) = detect_result.firmware_version {
        if !rnode::validate_firmware(major, minor) {
            return Err(RNodeError::FirmwareTooOld(
                major,
                minor,
                rnode::REQUIRED_FW_MAJ,
                rnode::REQUIRED_FW_MIN,
            ));
        }
    } else {
        return Err(RNodeError::NotDetected);
    }

    // --- Configuration phase ---
    rnode::validate_config(
        radio.frequency,
        radio.bandwidth,
        radio.tx_power,
        radio.sf,
        radio.cr,
    )
    .map_err(|e| RNodeError::RadioMismatch(e.to_string()))?;
    send_radio_config(port, radio).await?;

    // Wait for device to process configuration
    tokio::time::sleep(CONFIG_PROCESS_WAIT).await;

    // Read and validate confirmation frames
    validate_radio_config(port, radio, name).await?;

    // Final settle
    tokio::time::sleep(FINAL_SETTLE).await;

    Ok(detect_result)
}

/// Read KISS frames from the serial port until the deadline, calling `handler`
/// for each successfully deframed frame.
async fn read_frames_until_deadline<S>(
    port: &mut S,
    timeout: Duration,
    mut handler: impl FnMut(u8, &[u8]),
) -> Result<(), RNodeError>
where
    S: AsyncRead + Unpin,
{
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; SERIAL_READ_BUF];
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, port.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                tracing::info!("rnode read {} bytes: {:02x?}", n, &buf[..n.min(32)]);
                for frame in deframer.process(&buf[..n]) {
                    if let KissDeframeResult::Frame { command, payload } = frame {
                        tracing::debug!(
                            "rnode KISS frame: cmd=0x{:02x} len={}",
                            command,
                            payload.len()
                        );
                        handler(command, &payload);
                    }
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::debug!("rnode read timeout ({:?} remaining)", remaining);
                break;
            }
        }
    }
    Ok(())
}

/// Send detect query and parse response frames from an open port.
async fn detect_on_port<S>(port: &mut S) -> Result<RNodeDetectResult, RNodeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let query = rnode::build_detect_query();
    port.write_all(&query).await?;

    let mut result = RNodeDetectResult {
        detected: false,
        firmware_version: None,
        platform: None,
        mcu: None,
    };

    read_frames_until_deadline(port, DETECT_TIMEOUT, |command, payload| match command {
        rnode::CMD_DETECT if payload.first() == Some(&rnode::DETECT_RESP) => {
            result.detected = true;
        }
        rnode::CMD_FW_VERSION => {
            result.firmware_version = rnode::decode_firmware_version(payload);
        }
        rnode::CMD_PLATFORM => {
            result.platform = payload.first().copied();
        }
        rnode::CMD_MCU => {
            result.mcu = payload.first().copied();
        }
        _ => {}
    })
    .await?;

    if !result.detected {
        return Err(RNodeError::NotDetected);
    }

    Ok(result)
}

/// Send radio configuration commands to the RNode.
async fn send_radio_config<S>(port: &mut S, radio: &RadioParams) -> Result<(), RNodeError>
where
    S: AsyncWrite + Unpin,
{
    let mut config_bytes = Vec::with_capacity(64);
    // Idle the radio before touching modulation parameters. The firmware's
    // setters are no-ops while the modem is offline (`Utilities.h`
    // `setBandwidth`/`setFrequency`/`setSpreadingFactor`/... all guard on
    // `radio_online`); they only store `lora_*`, and `startRadio()` applies
    // the whole stored set in one go. Reconfiguring a *running* modem instead
    // pokes registers underneath an active receive, which most boards shrug
    // off and at least one does not — it comes up mute while every readback
    // still reports a healthy modem.
    config_bytes.extend_from_slice(&rnode::build_set_radio_state(rnode::RADIO_STATE_OFF));
    config_bytes.extend_from_slice(&rnode::build_set_frequency(radio.frequency));
    config_bytes.extend_from_slice(&rnode::build_set_bandwidth(radio.bandwidth));
    config_bytes.extend_from_slice(&rnode::build_set_txpower(radio.tx_power));
    config_bytes.extend_from_slice(&rnode::build_set_sf(radio.sf));
    config_bytes.extend_from_slice(&rnode::build_set_cr(radio.cr));
    if let Some(st) = radio.st_alock {
        config_bytes.extend_from_slice(&rnode::build_set_st_alock(st));
    }
    if let Some(lt) = radio.lt_alock {
        config_bytes.extend_from_slice(&rnode::build_set_lt_alock(lt));
    }
    config_bytes.extend_from_slice(&rnode::build_set_radio_state(rnode::RADIO_STATE_ON));

    tracing::info!("rnode: sending {} config bytes", config_bytes.len());
    port.write_all(&config_bytes).await?;
    port.flush().await?;
    tracing::info!("rnode: config sent and flushed");
    Ok(())
}

/// Read confirmation frames and validate they match the requested config.
async fn validate_radio_config<S>(
    port: &mut S,
    radio: &RadioParams,
    name: &str,
) -> Result<(), RNodeError>
where
    S: AsyncRead + Unpin,
{
    let mut confirmed_freq: Option<u32> = None;
    let mut confirmed_bw: Option<u32> = None;
    let mut confirmed_txp: Option<u8> = None;
    let mut confirmed_sf: Option<u8> = None;
    let mut confirmed_cr: Option<u8> = None;
    let mut confirmed_radio_state: Option<u8> = None;

    read_frames_until_deadline(port, VALIDATE_TIMEOUT, |command, payload| match command {
        rnode::CMD_FREQUENCY if payload.len() >= 4 => {
            confirmed_freq = Some(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]));
        }
        rnode::CMD_BANDWIDTH if payload.len() >= 4 => {
            confirmed_bw = Some(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]));
        }
        rnode::CMD_TXPOWER if !payload.is_empty() => {
            confirmed_txp = Some(payload[0]);
        }
        rnode::CMD_SF if !payload.is_empty() => {
            confirmed_sf = Some(payload[0]);
        }
        rnode::CMD_CR if !payload.is_empty() => {
            confirmed_cr = Some(payload[0]);
        }
        rnode::CMD_RADIO_STATE if !payload.is_empty() => {
            confirmed_radio_state = Some(payload[0]);
        }
        _ => {}
    })
    .await?;

    // Log warnings for missing confirmations to aid debugging
    if confirmed_freq.is_none() {
        tracing::debug!("{}: no frequency confirmation received", name);
    }
    if confirmed_bw.is_none() {
        tracing::debug!("{}: no bandwidth confirmation received", name);
    }
    if confirmed_txp.is_none() {
        tracing::debug!("{}: no tx_power confirmation received", name);
    }
    if confirmed_sf.is_none() {
        tracing::debug!("{}: no spreading factor confirmation received", name);
    }
    if confirmed_cr.is_none() {
        tracing::debug!("{}: no coding rate confirmation received", name);
    }
    if confirmed_radio_state.is_none() {
        tracing::debug!("{}: no radio_state confirmation received", name);
    }

    if let Some(cf) = confirmed_freq {
        if cf.abs_diff(radio.frequency) > FREQ_TOLERANCE_HZ {
            return Err(RNodeError::RadioMismatch(format!(
                "frequency: requested {} Hz, got {} Hz",
                radio.frequency, cf
            )));
        }
    }
    if let Some(cb) = confirmed_bw {
        if cb != radio.bandwidth {
            return Err(RNodeError::RadioMismatch(format!(
                "bandwidth: requested {} Hz, got {} Hz",
                radio.bandwidth, cb
            )));
        }
    }
    if let Some(ct) = confirmed_txp {
        // A derived board-maximum request is allowed to come back clamped
        // DOWN: that is the board reporting its own ceiling, which is what the
        // absent-key default asked for in the first place. Coming back HIGHER
        // than requested is a mismatch either way — nothing may transmit above
        // what was asked for.
        if ct < radio.tx_power && radio.tx_power_derived {
            tracing::info!(
                "{}: board maximum is {} dBm, below the {} dBm default request; using {} dBm",
                name,
                ct,
                radio.tx_power,
                ct
            );
        } else if ct != radio.tx_power {
            return Err(RNodeError::RadioMismatch(format!(
                "tx_power: requested {} dBm, got {} dBm",
                radio.tx_power, ct
            )));
        }
    }
    if let Some(cs) = confirmed_sf {
        if cs != radio.sf {
            return Err(RNodeError::RadioMismatch(format!(
                "sf: requested {}, got {}",
                radio.sf, cs
            )));
        }
    }
    if let Some(cc) = confirmed_cr {
        if cc != radio.cr {
            return Err(RNodeError::RadioMismatch(format!(
                "cr: requested {}, got {}",
                radio.cr, cc
            )));
        }
    }
    if confirmed_radio_state == Some(rnode::RADIO_STATE_OFF) {
        return Err(RNodeError::RadioMismatch(
            "radio did not turn on".to_string(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Radio stats (Codeberg #25)
// ---------------------------------------------------------------------------

/// Parse an RNode `CMD_STAT_*` frame and fold the values into the interface's
/// shared radio stats (Codeberg #25).
///
/// Decoding uses the shared `leviculum_core::rnode` decoders; the scaling and
/// clamping applied here match Python `RNodeInterface.process_incoming`
/// (RNodeInterface.py:878-1066) so the stored values carry the same units
/// Python exposes through `get_interface_stats`:
///
/// - RSSI: dBm (`raw - 157`), stored as `last_rssi`.
/// - SNR: dB (`signed raw * 0.25`), stored as `last_snr`.
/// - CHTM: airtime/channel-load are `raw_u16 / 100.0` percent; `noise_floor`
///   is dBm (only present on single-interface 11-byte frames).
/// - BAT: `(state, percent)`.
/// - TEMP: Celsius (`raw - 120`), clamped to `[-30, 90]`, else `None`.
///
/// A `CMD_STAT_CHTM` frame additionally emits the `LORA_CHTM` trace event
/// (see the emission site below); `name` is the interface name it carries.
///
/// Returns `true` if `command` was a recognised stat frame. Shared by the I/O
/// task and unit tests so the parse/state path is exercised without a serial
/// port.
fn apply_radio_stat(name: &str, counters: &InterfaceCounters, command: u8, payload: &[u8]) -> bool {
    match command {
        rnode::CMD_STAT_RSSI => {
            if let Some(rssi) = rnode::decode_rssi(payload) {
                counters.update_radio(|r| r.last_rssi = Some(rssi));
            }
        }
        rnode::CMD_STAT_SNR => {
            if let Some(raw) = rnode::decode_snr(payload) {
                counters.update_radio(|r| r.last_snr = Some(raw as f64 * 0.25));
            }
        }
        rnode::CMD_STAT_CHTM => {
            if let Some(cs) = rnode::decode_channel_stats(payload) {
                let airtime_short = cs.airtime_short as f64 / 100.0;
                let airtime_long = cs.airtime_long as f64 / 100.0;
                let channel_load_short = cs.channel_load_short as f64 / 100.0;
                let channel_load_long = cs.channel_load_long as f64 / 100.0;
                counters.update_radio(|r| {
                    r.airtime_short = airtime_short;
                    r.airtime_long = airtime_long;
                    r.channel_load_short = channel_load_short;
                    r.channel_load_long = channel_load_long;
                    if let Some(nf) = cs.noise_floor {
                        r.noise_floor = Some(nf);
                    }
                });
                // The modem's own account of whether it keyed. `LORA_TX`
                // dates the write to the serial port; between that write and
                // the air sit the firmware's send queue and its CSMA, so a
                // frame handed over and never keyed reads there as a
                // handover. This event closes that gap from the modem's side,
                // and it is the arrival plus the airtime delta that carries
                // the answer, not the payload alone:
                //
                // * `kiss_indicate_channel_stats()` is the last statement of
                //   `update_airtime()` (RNode_Firmware.ino:712), and
                //   `update_airtime()` is the last statement of both
                //   `flush_queue()` (:606) and `pop_queue()` (:644) — after
                //   `transmit()` ran `LoRa->endPacket()` and `add_airtime()`
                //   (:751) folded the just-keyed packet's airtime cost in. So
                //   every keyed burst is followed by a CHTM frame carrying
                //   its cost, on top of the ~1 s idle cadence.
                // * A rise in `airtime_short` is the proof. `airtime_bins` is
                //   written by `add_airtime()` alone, and `airtime` is their
                //   two-bin ratio (:698) over 2*`AIRTIME_BINLEN_MS` = 15000 ms
                //   (Config.h:183, 3*2500), scaled by 100*100 into the u16 on
                //   the wire. One raw unit is therefore 1.5 ms of airtime,
                //   far below any frame at any supported PHY.
                // * Arrival alone is NOT the proof: `transmit()` with
                //   `radio_online == false` answers `CMD_ERROR TXFAILED` and
                //   keys nothing, yet its caller still emits a CHTM — with
                //   `airtime_short` unmoved. A failed `endPacket()` (:744)
                //   instead answers MODEM_TIMEOUT + TXFAILED and hard-resets,
                //   so no CHTM follows at all.
                //
                // Two limits an analysis has to respect. This dates a BURST,
                // not a frame: at every bitrate below
                // LORA_GUARD_THRESHOLD_BPS = 14 kbps (Config.h:89) —
                // which is every LoRa PHY we run — `should_flush` is true
                // (:1644) and `flush_queue()` drains the whole queue before
                // the single CHTM, whose airtime covers all of it. And the
                // frame is ESP32/nRF52 only; an AVR RNode compiles
                // `kiss_indicate_channel_stats()` to nothing and this event
                // never appears for it.
                //
                // THE OTHER HALF OF THE EVENT reads the RECEIVE side, and it
                // is a different kind of number: the two `airtime_*` fields
                // are the modem's own account of what it keyed, the two
                // `channel_load_*` fields are a measurement of the air.
                // `check_modem_status()` samples `LoRa->dcd()` every
                // STATUS_INTERVAL_MS = 3 ms into a DCD_SAMPLES = 2500 ring
                // (Config.h:176,178) and recomputes `local_channel_util`
                // from it once a second (UTIL_UPDATE_INTERVAL, :180;
                // RNode_Firmware.ino:1451-1458), so it is the busy fraction
                // of the last 7500 ms. `kiss_indicate_channel_stats()`
                // scales it by 100*100 into the u16 (Utilities.h:963), so
                // one raw unit is 0.01 % and the SMALLEST possible busy
                // reading — a single DCD sample, 1/2500 — is 0.04 %, four
                // units, which the two-decimal format below keeps.
                //
                // That resolution is what lets a zero here be read as a
                // negative rather than a rounding artifact, which is the
                // receive-side half of the evidence in
                // `tests/mvr/sender_modem_counts_a_frame_the_far_modem_
                // never_hears.rs`: a frame the far modem never heard, while
                // the sending modem's `airtime_short` rose by that frame's
                // own cost. That file owns the arithmetic and the two reds
                // it was argued from; what belongs here is only the unit.
                //
                // Two limits, the mirror of the ones above.
                // `total_channel_util = local_channel_util + airtime`
                // (:1459) and is clamped at 1.0 (:1460), so on a modem that
                // is itself keying this field is NOT a clean reading of the
                // air — subtract `airtime_short` first, and on a loaded
                // channel the clamp has already discarded the remainder.
                // And `dcd()` is preamble/header detection, not an energy
                // threshold: interference that never resolves into a LoRa
                // preamble moves `noise_floor` and `interference_detected`
                // (:1401,1415) and leaves this at zero.
                //
                // Same target and level as `LORA_TX` so a run that captures
                // the handovers captures the keying beside them.
                tracing::debug!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_CHTM iface={name} airtime_short={airtime_short:.2} \
                     airtime_long={airtime_long:.2} \
                     channel_load_short={channel_load_short:.2} \
                     channel_load_long={channel_load_long:.2}"
                );
            }
        }
        rnode::CMD_STAT_BAT => {
            if let Some((state, percent)) = rnode::decode_battery(payload) {
                counters.update_radio(|r| {
                    r.battery_state = state;
                    r.battery_percent = percent;
                });
            }
        }
        rnode::CMD_STAT_TEMP => {
            if let Some(temp) = rnode::decode_temperature(payload) {
                let clamped = (-30..=90).contains(&temp).then_some(temp);
                counters.update_radio(|r| r.cpu_temp = clamped);
            }
        }
        _ => return false,
    }
    true
}

/// The ` rssi=<dBm> snr=<dB>` suffix a data frame's RX log lines carry
/// (Codeberg #364: the air sniffer could not say whether a lost frame was
/// weaker).
///
/// The firmware indicates the frame's RSSI and SNR to the host BEFORE the
/// data frame itself, on every MCU variant (RNode_Firmware.ino:454-458 and
/// :495-499 AVR, :1668-1670 ESP32, :1689-1691 nRF52 — always
/// `kiss_indicate_stat_rssi(); kiss_indicate_stat_snr();
/// kiss_write_packet();`), so at `CMD_DATA` time the most recently stored
/// stat values are this frame's own signal report. That last-seen pairing is
/// the reference's: Python keeps `r_stat_rssi`/`r_stat_snr` from the
/// preceding stat frames (RNodeInterface.py:877-880, the SNR a signed byte
/// scaled by 0.25) and reads them for the frame that follows. Key names
/// match the firmware's `[LORA] RX` line. Empty until the first stat frame —
/// a stream that never carried stats (a mock, an older firmware) logs the
/// bare line rather than an invented value.
fn signal_suffix(counters: &InterfaceCounters) -> String {
    use std::fmt::Write as _;
    let mut suffix = String::new();
    if let Some(radio) = counters.radio_stats() {
        if let Some(rssi) = radio.last_rssi {
            let _ = write!(suffix, " rssi={rssi}");
        }
        if let Some(snr) = radio.last_snr {
            let _ = write!(suffix, " snr={snr}");
        }
    }
    suffix
}

// ---------------------------------------------------------------------------
// I/O task
// ---------------------------------------------------------------------------

/// Count and name the frames a return path leaves behind (Codeberg #316).
///
/// The io task's send queue is task-local: whatever it still holds when the
/// task returns is gone, while frames still sitting in the mpsc channel are
/// inherited by the reconnected task. Losing the held ones is a legitimate
/// deviation from Python's keep-across-reconnect `packet_queue`; losing them
/// *silently* is a black hole — under a long duty lock the queue is full
/// precisely when a port bounce is most likely.
///
/// One summary event per return path, never one per frame: a reconnect must
/// not be able to produce 64 log lines in one moment. `depth` is therefore
/// always 0 — nothing survives this call.
fn abandon_send_queue(
    name: &str,
    counters: &InterfaceCounters,
    send_queue: &mut VecDeque<QueuedFrame>,
    reason: &'static str,
) {
    let abandoned = send_queue.len();
    if abandoned == 0 {
        return;
    }
    let abandoned_bytes: u64 = send_queue.iter().map(|f| f.payload_len).sum();
    send_queue.clear();
    counters
        .tx_queue_drops
        .fetch_add(abandoned as u64, std::sync::atomic::Ordering::Relaxed);
    counters
        .tx_dropped_bytes
        .fetch_add(abandoned_bytes, std::sync::atomic::Ordering::Relaxed);
    tracing::warn!(
        event = "RNODE_TX_QUEUE_DROP",
        iface = %Scalar(name),
        frames = abandoned,
        depth = 0,
        reason = reason,
    );
}

/// [`abandon_send_queue`] for the multi-vport loop, whose shared queue is
/// just as task-local.
///
/// The frames are vport-tagged, so each abandoned frame is counted on its
/// owning vport's counters, while the single summary event names the
/// physical interface: the port that dropped is a property of the shared
/// line, not of any one vport.
fn abandon_multi_send_queue(
    name: &str,
    vports: &[VportRuntime],
    send_queue: &mut VecDeque<(usize, Vec<u8>)>,
    reason: &'static str,
) {
    let abandoned = send_queue.len();
    if abandoned == 0 {
        return;
    }
    for (subint, frame) in send_queue.drain(..) {
        vports[subint]
            .counters
            .tx_queue_drops
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vports[subint]
            .counters
            .tx_dropped_bytes
            .fetch_add(frame.len() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    tracing::warn!(
        event = "RNODE_TX_QUEUE_DROP",
        iface = %Scalar(name),
        frames = abandoned,
        depth = 0,
        reason = reason,
    );
}

/// Deregister one vport whose logical interface is gone.
///
/// The granularity is the point (#283): only this vport's queued frames are
/// abandoned and only this vport stops being routed to. The shared serial port
/// and every other vport on it keep running, so a single torn-down logical
/// interface no longer bounces the physical radio.
fn deregister_vport(
    name: &str,
    vports: &[VportRuntime],
    send_queue: &mut VecDeque<(usize, Vec<u8>)>,
    dead: &mut [bool],
    subint: usize,
) {
    if dead[subint] {
        return;
    }
    dead[subint] = true;

    let mut abandoned = 0usize;
    send_queue.retain(|(queued, frame)| {
        if *queued != subint {
            return true;
        }
        abandoned += 1;
        vports[subint]
            .counters
            .tx_queue_drops
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vports[subint]
            .counters
            .tx_dropped_bytes
            .fetch_add(frame.len() as u64, std::sync::atomic::Ordering::Relaxed);
        false
    });

    tracing::warn!(
        event = "RNODE_VPORT_DEREGISTERED",
        iface = %Scalar(name),
        vport_iface = %Scalar(&vports[subint].name),
        vport = vports[subint].vport,
        frames = abandoned,
        reason = "incoming_closed",
    );
}

/// Bidirectional I/O loop for a configured RNode.
///
/// Returns the `outgoing_rx` on disconnect so the reconnect wrapper can
/// reuse the same channel (matching TCP interface pattern).
///
/// Send-side channel access: packets are not sent immediately. A frame that
/// acquires an idle channel first serves the randomised wait
/// [`ChannelAccess`] draws for it (DIFS plus a contention window, the
/// reference firmware's band-1 draw); every further frame of the same burst
/// owes no draw of its own, but follows only once the frame before it has
/// left the air ([`tx_hold`]), so the modem's queue never holds more than one
/// frame and the firmware's CSMA runs for every frame instead of once per
/// burst. What a frame CONTAINS never enters into either wait — see the
/// enqueue branch. RNode firmware CSMA handles radio-level collision
/// avoidance on top.
#[allow(clippy::too_many_arguments)]
async fn rnode_io_task<S>(
    name: String,
    mut port: S,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    flow_control: bool,
    mut access: ChannelAccess,
    bandwidth_hz: u32,
    sf: u8,
    cr: u8,
    drop_direct_ingress: bool,
    jitter_arm: JitterArm,
    frame_class: FrameClass,
    alock: AirtimeLock,
) -> mpsc::Receiver<OutgoingPacket>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    super::log_direct_ingress_filter_armed(drop_direct_ingress, &name);
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; IO_READ_BUF];
    // The gate starts open: before the first TX there is no queue state
    // worth asking about, and the firmware never volunteers CMD_READY
    // (RNode_Firmware.ino:1003-1008 answers only a host query), so a
    // closed initial gate could never open.
    let mut interface_ready = true;
    let mut send_queue: VecDeque<QueuedFrame> = VecDeque::new();
    let mut send_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut timer_ready = false;
    // What `send_timer` is currently spending, when it is spending an
    // acquisition wait: the policy is told what was served once it elapses.
    // Zero while the timer is the post-TX hold, which is not a wait the
    // acquisition owes.
    let mut jitter_armed_ms: u64 = 0;
    // What the modem has said about its own channel access, filled in from
    // the stat frames it sends unsolicited. Read once per handover, to price
    // the hold the next frame serves (see [`tx_hold`]).
    let mut fw_csma = FirmwareCsma::default();

    // Gate 2 reopen state: CMD_READY is a query protocol, not a courtesy.
    // After every TX (flow_control on) the gate closes and we ask the
    // firmware whether its queue has room; a 0x01 response reopens it. A
    // 0x00 response — or a response that never comes, e.g. lost on a noisy
    // line — leads to a re-query when `ready_query_timer` fires: the timer
    // is armed at query time, so a lost response degrades into the same
    // bounded re-poll instead of a stuck gate. The delay starts at one
    // packet airtime and doubles up to READY_POLL_MAX (see the constants
    // above for the justification).
    let ready_poll_start = ready_poll_initial(sf, cr, bandwidth_hz);
    let mut ready_poll = ready_poll_start;
    let mut ready_query_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;

    // The modem's airtime ledger as of the last CHTM, and the frames it has
    // been handed and has not accounted for. `None` until the first CHTM: with
    // no baseline there is nothing to take a step against, which is also what
    // keeps a modem that never sends CHTM (an AVR RNode, or any modem before
    // its first stat frame) out of the accounting entirely.
    let mut ledger: Option<(u16, u16)> = None;
    let mut pendings: VecDeque<PendingHandover> = VecDeque::new();

    // Duty-lock visibility: when the CMD_READY gate (Gate 2 below)
    // holds queued frames, say so. The firmware's duty lock produces exactly
    // this shape — the queue stays full, every poll answers 0x00, the gate
    // stays closed — and it used
    // to be invisible from the host. `gate_blocked_since` starts when frames
    // are held and the gate is closed; `gate_event_timer` fires
    // RNODE_TX_GATED after TX_GATED_EVENT_AFTER, then every
    // TX_GATED_EVENT_REPEAT; `gate_announced` pairs the eventual
    // RNODE_TX_RELEASED with it.
    let mut gate_blocked_since: Option<tokio::time::Instant> = None;
    let mut gate_event_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut gate_announced = false;

    // Periodic heartbeat: send CMD_DETECT every 5 minutes to keep the
    // serial link alive and verify the RNode firmware is responsive.
    // This does NOT transmit over LoRa, it's a serial-only ping.
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);
    let mut heartbeat_timer = Box::pin(tokio::time::sleep(HEARTBEAT_INTERVAL));
    let mut heartbeat_pending = false;

    loop {
        // The one frame a CHTM this iteration asked to re-hand. Set in the
        // read branch, acted on below the select together with the send
        // gates — a frame pushed back onto the queue has to arm the
        // acquisition wait, and that state lives out here.
        let mut rehand: Option<QueuedFrame> = None;

        tokio::select! {
            // Branch 1: Read from serial port
            result = port.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::warn!("{}: serial port EOF", name);
                        abandon_send_queue(&name, &counters, &mut send_queue, "serial_eof");
                        return outgoing_rx;
                    }
                    Ok(n) => {
                        let frames = deframer.process(&buf[..n]);
                        for frame in frames {
                            if let KissDeframeResult::Frame { command, payload } = frame {
                                match command {
                                    rnode::CMD_DATA => {
                                        let signal = signal_suffix(&counters);
                                        tracing::debug!(
                                            "{}: RX {} bytes from radio{}",
                                            name,
                                            payload.len(),
                                            signal
                                        );
                                        // TEST-ONLY range emulation: an
                                        // out-of-range frame was never heard,
                                        // so it is dropped before any counter
                                        // or trace event that the delivery
                                        // analysis reads.
                                        if super::test_drop_direct_ingress_frame(
                                            drop_direct_ingress, &name, &payload, &counters,
                                        ) {
                                            continue;
                                        }
                                        // Bug #25 capture-compare: structured
                                        // event at the RNode → host serial
                                        // boundary. Mirrors `LORA_TX` on the
                                        // send side; together they let the
                                        // analysis align TX on one node with
                                        // RX on the other.
                                        tracing::debug!(
                                            target: "leviculum_std::interfaces::rnode::rx_trace",
                                            "LORA_RX iface={name} len={}{signal}",
                                            payload.len()
                                        );
                                        counters.rx_bytes.fetch_add(
                                            payload.len() as u64,
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        let pkt = IncomingPacket { data: payload.to_vec() };
                                        if incoming_tx.send(pkt).await.is_err() {
                                            // Event loop shut down
                                            send_goodbye(&mut port, &name).await;
                                            abandon_send_queue(
                                                &name, &counters, &mut send_queue,
                                                "incoming_closed",
                                            );
                                            return outgoing_rx;
                                        }
                                    }
                                    rnode::CMD_READY => {
                                        // Response to our post-TX/re-poll query — the
                                        // firmware never sends CMD_READY unsolicited.
                                        // 0x01: queue not full, the gate reopens.
                                        // 0x00: still full; the timer armed with the
                                        // query re-asks on its bounded backoff.
                                        if flow_control && payload.first() == Some(&0x01) {
                                            tracing::debug!(
                                                "{}: CMD_READY answer: queue not full",
                                                name
                                            );
                                            interface_ready = true;
                                            ready_query_timer = None;
                                            ready_poll = ready_poll_start;
                                        } else if flow_control {
                                            tracing::debug!(
                                                "{}: CMD_READY answer: queue full",
                                                name
                                            );
                                        }
                                    }
                                    rnode::CMD_DETECT => {
                                        if payload.first() == Some(&rnode::DETECT_RESP)
                                            && heartbeat_pending
                                        {
                                            tracing::debug!("{}: heartbeat OK", name);
                                            heartbeat_pending = false;
                                        }
                                    }
                                    rnode::CMD_RESET => {
                                        if payload.first() == Some(&DEVICE_RESET_MARKER) {
                                            tracing::warn!("{}: device reset (0xF8)", name);
                                            abandon_send_queue(
                                                &name, &counters, &mut send_queue,
                                                "device_reset",
                                            );
                                            return outgoing_rx;
                                        }
                                    }
                                    rnode::CMD_ERROR => {
                                        let Some(code) = payload.first().copied() else {
                                            tracing::warn!("{}: CMD_ERROR with empty payload", name);
                                            continue;
                                        };
                                        match code {
                                            rnode::ERROR_INITRADIO => {
                                                tracing::error!("{}: radio init failed", name);
                                                abandon_send_queue(
                                                    &name, &counters, &mut send_queue,
                                                    "error_initradio",
                                                );
                                                return outgoing_rx;
                                            }
                                            rnode::ERROR_TXFAILED => {
                                                tracing::error!("{}: TX failed", name);
                                                abandon_send_queue(
                                                    &name, &counters, &mut send_queue,
                                                    "error_txfailed",
                                                );
                                                return outgoing_rx;
                                            }
                                            rnode::ERROR_EEPROM_LOCKED => {
                                                tracing::error!("{}: EEPROM locked", name);
                                            }
                                            rnode::ERROR_QUEUE_FULL => {
                                                tracing::warn!("{}: device TX queue full", name);
                                            }
                                            rnode::ERROR_MEMORY_LOW => {
                                                tracing::warn!("{}: device memory low", name);
                                            }
                                            rnode::ERROR_MODEM_TIMEOUT => {
                                                tracing::error!("{}: modem timeout", name);
                                                abandon_send_queue(
                                                    &name, &counters, &mut send_queue,
                                                    "error_modem_timeout",
                                                );
                                                return outgoing_rx;
                                            }
                                            _ => {
                                                tracing::warn!(
                                                    "{}: unknown device error 0x{:02X}",
                                                    name, code
                                                );
                                            }
                                        }
                                    }
                                    // Bug #25 investigation telemetry: explicit parsers
                                    // for the two CSMA-related stat frames the firmware
                                    // emits unsolicited. Structured events under the
                                    // `leviculum_std::interfaces::rnode::csma_probe`
                                    // tracing target let the debugger correlate
                                    // firmware CSMA state with on-air TX behaviour.
                                    // They are no longer measurement-only: the two
                                    // figures the modem reports about its own contest
                                    // are what the post-TX hold is priced from
                                    // (`FirmwareCsma`, `tx_hold`).
                                    rnode::CMD_STAT_CSMA if payload.len() >= 3 => {
                                        let cw_band = payload[0];
                                        let cw_min = payload[1];
                                        let cw_max = payload[2];
                                        fw_csma.cw_max = Some(cw_max);
                                        tracing::debug!(
                                            target: "leviculum_std::interfaces::rnode::csma_probe",
                                            "CSMA_STAT iface={name} cw_band={cw_band} \
                                             cw_min={cw_min} cw_max={cw_max}"
                                        );
                                    }
                                    rnode::CMD_STAT_PHYPRM => {
                                        tracing::debug!(
                                            target: "leviculum_std::interfaces::rnode::csma_probe",
                                            "CSMA_PHY_RAW iface={name} payload_len={} bytes={:?}",
                                            payload.len(), payload
                                        );
                                        if payload.len() >= 12 {
                                            let symbol_time_ms =
                                                u16::from_be_bytes([payload[0], payload[1]]) as f32
                                                    / 1000.0;
                                            let symbol_rate =
                                                u16::from_be_bytes([payload[2], payload[3]]);
                                            let preamble_symbols =
                                                u16::from_be_bytes([payload[4], payload[5]]);
                                            let preamble_time_ms =
                                                u16::from_be_bytes([payload[6], payload[7]]);
                                            let csma_slot_time_ms =
                                                u16::from_be_bytes([payload[8], payload[9]]);
                                            let csma_difs_ms =
                                                u16::from_be_bytes([payload[10], payload[11]]);
                                            // A modem that reports a zero slot or
                                            // DIFS has not configured its radio yet;
                                            // taking those would price the hold at
                                            // the airtime alone.
                                            if csma_slot_time_ms > 0 {
                                                fw_csma.slot_ms = Some(csma_slot_time_ms as u64);
                                            }
                                            if csma_difs_ms > 0 {
                                                fw_csma.difs_ms = Some(csma_difs_ms as u64);
                                            }
                                            tracing::debug!(
                                                target: "leviculum_std::interfaces::rnode::csma_probe",
                                                "CSMA_PHY iface={name} symbol_time_ms={symbol_time_ms:.3} \
                                                 symbol_rate={symbol_rate} preamble_symbols={preamble_symbols} \
                                                 preamble_time_ms={preamble_time_ms} \
                                                 csma_slot_time_ms={csma_slot_time_ms} \
                                                 csma_difs_ms={csma_difs_ms}"
                                            );
                                        }
                                    }
                                    // Radio statistics (Codeberg #25): parse and
                                    // store on the shared counters so `interface_stats`
                                    // surfaces the RNode radio rows (airtime, channel
                                    // load, noise floor, temperature, battery) to
                                    // rnstatus/lnstatus. Field names/units mirror
                                    // Python RNodeInterface's `r_*` attributes.
                                    // CHTM first, and on its own: besides the
                                    // radio rows it is the modem's only
                                    // per-transmission receipt, and the
                                    // frames handed over since the last one
                                    // are settled against it
                                    // (`settle_handovers`).
                                    rnode::CMD_STAT_CHTM => {
                                        apply_radio_stat(&name, &counters, command, &payload);
                                        if let Some(cs) =
                                            rnode::decode_channel_stats(&payload)
                                        {
                                            let now = tokio::time::Instant::now();
                                            rehand = settle_handovers(
                                                &name,
                                                &counters,
                                                &mut pendings,
                                                &cs,
                                                now,
                                                unix_ms(),
                                            );
                                            ledger =
                                                Some((cs.airtime_short, cs.airtime_long));
                                        }
                                    }
                                    cmd @ (rnode::CMD_STAT_RSSI
                                    | rnode::CMD_STAT_SNR
                                    | rnode::CMD_STAT_BAT
                                    | rnode::CMD_STAT_TEMP) => {
                                        apply_radio_stat(&name, &counters, cmd, &payload);
                                    }
                                    _ => {
                                        tracing::trace!(
                                            "{}: unhandled cmd 0x{:02X}",
                                            name, command
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("{}: serial read error: {}", name, e);
                        abandon_send_queue(&name, &counters, &mut send_queue, "serial_read_error");
                        return outgoing_rx;
                    }
                }
            }

            // Branch 2: Outgoing packet from driver → enqueue with jitter
            recv = outgoing_rx.recv() => {
                match recv {
                    Some(pkt) => {
                        let frame = rnode::build_data_frame(&pkt.data);
                        let high_priority = pkt.high_priority;
                        if send_queue.len() >= FLOW_CONTROL_QUEUE_LIMIT {
                            if let Some(dropped) = send_queue.pop_front() {
                                // A dropped frame must be loud —
                                // counted and catalogued, never only a log
                                // line. A silent drop is how lora_window_ab's
                                // third transfer vanished for six minutes.
                                counters
                                    .tx_queue_drops
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                counters.tx_dropped_bytes.fetch_add(
                                    dropped.payload_len,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                tracing::warn!(
                                    event = "RNODE_TX_QUEUE_DROP",
                                    iface = %Scalar(&name),
                                    len = dropped.payload_len,
                                    depth = send_queue.len(),
                                    reason = "queue_full",
                                );
                            }
                        }
                        let queued = QueuedFrame {
                            data: frame,
                            payload_len: pkt.data.len() as u64,
                            high_priority,
                            rehand: false,
                        };
                        if high_priority {
                            // Insert before the first non-high-priority packet
                            let pos = send_queue
                                .iter()
                                .position(|f| !f.high_priority)
                                .unwrap_or(send_queue.len());
                            send_queue.insert(pos, queued);
                            tracing::debug!(
                                "{}: send queue: {} packets (priority insert at {})",
                                name, send_queue.len(), pos
                            );
                        } else {
                            send_queue.push_back(queued);
                        }
                        // Channel access asks the medium, never the packet.
                        // `high_priority` decides WHERE in the queue a frame
                        // sits — queue discipline, kept above — and nothing
                        // about whether the interface may key the radio
                        // early. The bypass that used to stand here let every
                        // proof, link request and data packet skip the wait
                        // outright, leaving announces as the only jittered
                        // traffic; a packet is a packet, and two senders that
                        // the same event released collide whatever their
                        // frames mean (#347).
                        //
                        // A pending `send_timer` means a wait is already
                        // running and this frame rides it out. Otherwise the
                        // policy decides: an acquisition of a channel we have
                        // handed back owes DIFS plus a randomised contention
                        // window, and a continuation of the burst we are
                        // already transmitting owes nothing, because the
                        // frame before it served the wait.
                        if send_timer.is_none() {
                            // TEMPORARY (#347): which arm of the #347
                            // series this build runs. Arm 1 is
                            // `access.acquisition_jitter_ms()` and nothing
                            // else.
                            let owed = arm_owed_jitter_ms(
                                jitter_arm,
                                &mut access,
                                pkt.data.len(),
                                bandwidth_hz,
                                sf,
                                cr,
                                frame_class,
                            );
                            if owed == 0 {
                                timer_ready = true;
                                tracing::debug!(
                                    "{}: send queue: {} packets (burst continuation)",
                                    name, send_queue.len()
                                );
                            } else {
                                timer_ready = false;
                                jitter_armed_ms = owed;
                                tracing::debug!(
                                    "{}: send queue: {} packets, acquisition jitter {}ms \
                                     (slot {}ms)",
                                    name, send_queue.len(), owed, access.jitter_slot()
                                );
                                send_timer = Some(Box::pin(
                                    tokio::time::sleep(Duration::from_millis(owed))
                                ));
                            }
                        }
                    }
                    None => {
                        // Event loop shut down
                        send_goodbye(&mut port, &name).await;
                        abandon_send_queue(&name, &counters, &mut send_queue, "outgoing_closed");
                        return outgoing_rx;
                    }
                }
            }

            // Branch 3: Send timer fires
            _ = async {
                if let Some(ref mut timer) = send_timer {
                    timer.await;
                }
            }, if send_timer.is_some() => {
                send_timer = None;
                // A wait armed here is always served in full: nothing cancels
                // the sleep, and an inbound frame does not cut it short the
                // way the firmware's listening `rx_window` does. Report what
                // was served, so the debt is discharged and the rest of this
                // acquisition's burst is not asked to wait a second time.
                if jitter_armed_ms > 0 {
                    access.jitter_spent(jitter_armed_ms);
                    jitter_armed_ms = 0;
                }
                timer_ready = true;
            }

            // Branch 3b: flow-control gate held past the reporting threshold
            // Warn level: an engaged duty lock is an operational
            // condition the operator must be able to see.
            _ = async {
                if let Some(ref mut timer) = gate_event_timer {
                    timer.await;
                }
            }, if gate_event_timer.is_some() => {
                let held_ms = gate_blocked_since
                    .map(|s| s.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                tracing::warn!(
                    event = "RNODE_TX_GATED",
                    iface = %Scalar(&name),
                    held_ms = held_ms,
                    depth = send_queue.len(),
                );
                gate_announced = true;
                gate_event_timer = Some(Box::pin(tokio::time::sleep(TX_GATED_EVENT_REPEAT)));
            }

            // Branch 3c: no queue-not-full answer within the backoff window
            // — ask again, with the next window doubled up to READY_POLL_MAX.
            _ = async {
                if let Some(ref mut timer) = ready_query_timer {
                    timer.await;
                }
            }, if ready_query_timer.is_some() => {
                if let Err(e) = port.write_all(&READY_QUERY_FRAME).await {
                    tracing::warn!("{}: ready query write error: {}", name, e);
                    abandon_send_queue(
                        &name, &counters, &mut send_queue, "ready_query_write_error",
                    );
                    return outgoing_rx;
                }
                if let Err(e) = port.flush().await {
                    tracing::warn!("{}: ready query flush error: {}", name, e);
                    abandon_send_queue(
                        &name, &counters, &mut send_queue, "ready_query_flush_error",
                    );
                    return outgoing_rx;
                }
                ready_query_timer = Some(Box::pin(tokio::time::sleep(ready_poll)));
                ready_poll = (ready_poll * 2).min(READY_POLL_MAX);
            }

            // Branch 4: Periodic heartbeat. CMD_DETECT ping to verify firmware
            _ = &mut heartbeat_timer => {
                let detect_frame = [kiss::FEND, rnode::CMD_DETECT, rnode::DETECT_REQ, kiss::FEND];
                if let Err(e) = port.write_all(&detect_frame).await {
                    tracing::warn!("{}: heartbeat write error: {}", name, e);
                    abandon_send_queue(
                        &name, &counters, &mut send_queue, "heartbeat_write_error",
                    );
                    return outgoing_rx;
                }
                heartbeat_pending = true;
                tracing::debug!("{}: heartbeat sent", name);
                heartbeat_timer = Box::pin(tokio::time::sleep(HEARTBEAT_INTERVAL));
            }
        }

        // A frame the modem consumed without transmitting goes back to the
        // FRONT of the queue: it is older than everything behind it. It owes
        // the same channel access as any other acquisition — this re-hands a
        // frame, it does not let one skip the wait — so the arming below is
        // the enqueue branch's, for a queue that was idle when the verdict
        // came in.
        if let Some(frame) = rehand.take() {
            let payload_len = frame.payload_len as usize;
            send_queue.push_front(frame);
            if send_timer.is_none() && !timer_ready {
                let owed = arm_owed_jitter_ms(
                    jitter_arm,
                    &mut access,
                    payload_len,
                    bandwidth_hz,
                    sf,
                    cr,
                    frame_class,
                );
                if owed == 0 {
                    timer_ready = true;
                } else {
                    jitter_armed_ms = owed;
                    send_timer = Some(Box::pin(tokio::time::sleep(Duration::from_millis(owed))));
                }
            }
        }

        // Track the flow-control gate's hold state after every
        // iteration, before the send attempt below (a reopening gate must
        // emit its RNODE_TX_RELEASED with the frames still held, not after
        // the send block has already dispatched the first of them).
        // "Holding" means frames are queued and only the READY gate keeps
        // them there. Every TX re-enters this state on the next iteration
        // (the gate closes at TX and stays closed until a query answers
        // 0x01); events fire only if it persists past
        // TX_GATED_EVENT_AFTER, so ordinary airtime waits stay silent. The
        // queue cannot drain while the gate is closed, so leaving the hold
        // state means the gate reopened — if the hold was announced,
        // RNODE_TX_RELEASED closes the pair opened by RNODE_TX_GATED.
        let gate_holding = flow_control && !interface_ready && !send_queue.is_empty();
        match (gate_holding, gate_blocked_since) {
            (true, None) => {
                gate_blocked_since = Some(tokio::time::Instant::now());
                gate_event_timer = Some(Box::pin(tokio::time::sleep(TX_GATED_EVENT_AFTER)));
            }
            (false, Some(since)) => {
                if gate_announced {
                    tracing::warn!(
                        event = "RNODE_TX_RELEASED",
                        iface = %Scalar(&name),
                        held_ms = since.elapsed().as_millis() as u64,
                        depth = send_queue.len(),
                    );
                }
                gate_blocked_since = None;
                gate_event_timer = None;
                gate_announced = false;
            }
            _ => {}
        }

        // After any branch: try to send if all gates are open
        //   Gate 1: timer_ready (jitter/spacing delay elapsed)
        //   Gate 2: interface_ready || !flow_control
        if timer_ready && (interface_ready || !flow_control) {
            if let Some(queued) = send_queue.pop_front() {
                if let Err(e) = port.write_all(&queued.data).await {
                    tracing::warn!("{}: write error: {}", name, e);
                    // The frame in hand never made it out either — put it
                    // back so the count names every frame that is lost.
                    send_queue.push_front(queued);
                    abandon_send_queue(&name, &counters, &mut send_queue, "serial_write_error");
                    return outgoing_rx;
                }
                // tcdrain: block until firmware has received all bytes.
                // Without this, write_all() returns as soon as bytes enter
                // the OS serial buffer, multiple frames accumulate in the
                // firmware queue and flush_queue() sends them all in one
                // burst without CSMA between them.
                if let Err(e) = port.flush().await {
                    tracing::warn!("{}: flush error: {}", name, e);
                    // Bytes may have reached the OS buffer but not the
                    // firmware; count the frame as lost rather than as sent.
                    send_queue.push_front(queued);
                    abandon_send_queue(&name, &counters, &mut send_queue, "serial_flush_error");
                    return outgoing_rx;
                }
                counters
                    .tx_bytes
                    .fetch_add(queued.payload_len, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!("{}: TX {} bytes to serial", name, queued.payload_len);
                // Bug #25 capture-compare: structured event at the host →
                // RNode serial boundary. Measurement-only; DEBUG-level under
                // the dedicated target so it can be filtered independently
                // of the rest of the rnode logs.
                tracing::debug!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX iface={name} len={}",
                    queued.payload_len
                );

                timer_ready = false;
                if flow_control {
                    // Ask, don't wait: close the gate and query the queue
                    // state. The firmware answers 0x01/0x00 to this query;
                    // it never volunteers a READY.
                    interface_ready = false;
                    if let Err(e) = port.write_all(&READY_QUERY_FRAME).await {
                        tracing::warn!("{}: ready query write error: {}", name, e);
                        abandon_send_queue(
                            &name,
                            &counters,
                            &mut send_queue,
                            "ready_query_write_error",
                        );
                        return outgoing_rx;
                    }
                    if let Err(e) = port.flush().await {
                        tracing::warn!("{}: ready query flush error: {}", name, e);
                        abandon_send_queue(
                            &name,
                            &counters,
                            &mut send_queue,
                            "ready_query_flush_error",
                        );
                        return outgoing_rx;
                    }
                    ready_poll = ready_poll_start;
                    ready_query_timer = Some(Box::pin(tokio::time::sleep(ready_poll)));
                    ready_poll = (ready_poll * 2).min(READY_POLL_MAX);
                }

                // Hold the next frame until this one has left the air. The
                // flush() above means the firmware has the frame; from here
                // the host is blind — the firmware sends no TX-done — so the
                // wait is priced from the PHY and the modem's own CSMA
                // figures rather than observed (see `tx_hold` for why a
                // modem that holds two frames sends the second one deaf).
                // What the frame CONTAINS does not enter into it; only how
                // long it occupies the air.
                let hold = tx_hold(queued.payload_len as u32, bandwidth_hz, sf, cr, &fw_csma);
                tracing::debug!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX_HOLD iface={name} held_ms={} airtime_ms={} difs_ms={} cw_ms={}",
                    hold.held_ms, hold.airtime_ms, hold.difs_ms, hold.cw_ms
                );
                send_timer = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                    hold.held_ms,
                ))));

                // What the modem owes for the frame it now holds. The ledger
                // it reports in its next `CMD_STAT_CHTM` is the only receipt
                // it gives, so the expectation is recorded here and settled
                // there (`settle_handovers`). No baseline yet means no
                // accounting for this frame: a step needs something to step
                // from.
                if let Some((baseline_short, baseline_long)) = ledger {
                    // The firmware prepends its own header byte before it
                    // charges the packet (`transmit`,
                    // `RNode_Firmware/RNode_Firmware.ino:720-724`), and
                    // `add_airtime` is called with that count, so the cost it
                    // books is for one byte more than the payload.
                    let charged_ms = rnode::airtime_ms_with_preamble(
                        queued.payload_len as u32 + 1,
                        bandwidth_hz,
                        sf,
                        cr,
                        rnode::derive_preamble_symbols(sf, cr, bandwidth_hz),
                    );
                    let due = tokio::time::Instant::now() + Duration::from_millis(hold.held_ms);
                    if pendings.len() >= PENDING_HANDOVERS_MAX {
                        // A modem that answers with no CHTM at all cannot be
                        // accounted against; it must not cost memory either.
                        if let Some(dropped) = pendings.pop_front() {
                            tracing::debug!(
                                target: "leviculum_std::interfaces::rnode::tx_trace",
                                "LORA_TX_ACCOUNT iface={name} len={} verdict=undecided                                  reason=no_chtm",
                                dropped.payload_len
                            );
                        }
                    }
                    pendings.push_back(PendingHandover {
                        data: queued.data,
                        payload_len: queued.payload_len,
                        high_priority: queued.high_priority,
                        is_rehand: queued.rehand,
                        handed_unix_ms: unix_ms(),
                        due,
                        deadline: due + CHTM_PERIOD,
                        account: AirtimeAccount {
                            baseline_short,
                            baseline_long,
                            // Filled in from the CHTM under judgement; a
                            // reading identical to the baseline is what the
                            // accusation is made of, so that is the honest
                            // initial value.
                            observed_short: baseline_short,
                            observed_long: baseline_long,
                            observed_load_short: 0,
                            expected_short: expected_airtime_raw(charged_ms),
                            hold_ms: hold.held_ms,
                            lock: alock,
                        },
                    });
                }
            } else {
                // Nothing left to send: the burst is over and the channel
                // goes back. Whatever arrives next is a new acquisition and
                // owes a fresh wait — the same rule the firmware applies
                // after its post-TX listening window.
                access.channel_released();
                timer_ready = false;
            }
        }
    }
}

/// Best-effort send radio-off + leave commands on shutdown
async fn send_goodbye<S>(port: &mut S, name: &str)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let mut goodbye = rnode::build_set_radio_state(rnode::RADIO_STATE_OFF);
    goodbye.extend_from_slice(&rnode::build_leave());
    if let Err(e) = port.write_all(&goodbye).await {
        tracing::debug!("{name}: goodbye write failed (expected on disconnect): {e}");
    }
}

// ---------------------------------------------------------------------------
// Reconnect wrapper
// ---------------------------------------------------------------------------

/// Runtime parameters for the reconnect loop, independent of how the byte
/// channel is obtained (serial path vs. host-supplied channel factory).
struct RNodeReconnectCtx {
    id: InterfaceId,
    name: String,
    radio: RadioParams,
    flow_control: bool,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    jitter_max_ms: u64,
    /// TEST-ONLY range emulation: drop deframed hops=0 ingress frames
    /// (see [`super::test_drop_direct_ingress_frame`]).
    test_drop_direct_ingress: bool,
    /// TEMPORARY (#347): which acquisition-jitter arm the io task runs.
    jitter_arm: JitterArm,
    /// Which residue class of arm 3's whole-frame counts this interface
    /// draws in ([`FrameClass`]). Resolved once per interface, not per
    /// connection: a reconnect must not re-roll it, or two ends that were
    /// apart could land together after one of them loses its port.
    frame_class: FrameClass,
}

/// Reconnect loop: open channel → configure → I/O → on disconnect → wait → retry.
///
/// Carrier-agnostic: `connect` yields a fresh, opened (but unconfigured) byte
/// channel each attempt — a serial port for the path-based interface, or a
/// host-supplied duplex channel for [`spawn_rnode_channel_interface`]. The loop
/// runs detect/configure/validate on it via [`configure_stream`], so the
/// lifecycle is identical regardless of substrate.
async fn rnode_reconnect_task<S, C, Fut>(
    ctx: RNodeReconnectCtx,
    connect: C,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    ready: Arc<super::ReadySignal>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<S, RNodeError>> + Send,
{
    let radio = &ctx.radio;
    let bitrate_bps = rnode::compute_bitrate(radio.sf, radio.cr, radio.bandwidth);
    // What a full-size frame costs the next one behind it, at this PHY and
    // before the modem has reported its own CSMA figures — the ceiling of the
    // post-TX hold, stated where the bitrate is stated.
    let max_hold = max_tx_hold(radio.bandwidth, radio.sf, radio.cr);
    tracing::debug!(
        "{}: bitrate={} bps, tx_hold(mtu)={}ms (airtime {}ms + DIFS {}ms + cw {}ms), \
         jitter_max={}ms (DIFS + contention window), jitter_arm={}",
        ctx.name,
        bitrate_bps,
        max_hold.held_ms,
        max_hold.airtime_ms,
        max_hold.difs_ms,
        max_hold.cw_ms,
        ctx.jitter_max_ms,
        ctx.jitter_arm.digit(),
    );
    let mut has_connected_before = false;

    loop {
        // Between here and a configured radio the carrier is down (L-0020).
        counters.set_online(false);
        // Open the channel, then configure it. Combined so either step's error
        // routes through the same reconnect-retry path.
        let opened = async {
            let mut port = connect().await?;
            let detect = configure_stream(&mut port, radio, &ctx.name).await?;
            Ok::<_, RNodeError>((port, detect))
        }
        .await;

        match opened {
            Ok((port, detect)) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;
                counters.set_online(true);
                // Readiness contract (L-0024): detect answered, firmware
                // validated, radio configured — only now may a caller that
                // waited on `ready` transmit. One-way, like the TCP client's
                // Option α signal: reconnects keep the signal set.
                ready.signal_ready();

                if let Some((major, minor)) = detect.firmware_version {
                    tracing::info!(
                        "{}: configured (FW {}.{}, freq={} Hz, bw={} Hz, sf={}, cr={}, txp={} dBm)",
                        ctx.name,
                        major,
                        minor,
                        radio.frequency,
                        radio.bandwidth,
                        radio.sf,
                        radio.cr,
                        radio.tx_power
                    );
                }

                // Notify driver about reconnection so it can re-announce
                if is_reconnect {
                    if let Some(ref notify) = ctx.reconnect_notify {
                        if let Err(e) = notify.try_send(ctx.id) {
                            tracing::warn!("{}: reconnect notify failed: {}", ctx.name, e);
                        }
                    }
                }

                outgoing_rx = rnode_io_task(
                    ctx.name.clone(),
                    port,
                    incoming_tx.clone(),
                    outgoing_rx,
                    Arc::clone(&counters),
                    ctx.flow_control,
                    channel_access_for(radio.bandwidth, radio.sf, radio.cr),
                    radio.bandwidth,
                    radio.sf,
                    radio.cr,
                    ctx.test_drop_direct_ingress,
                    /* jitter_arm = */ ctx.jitter_arm,
                    /* frame_class = */ ctx.frame_class,
                    /* alock = */ AirtimeLock::from_config(radio.st_alock, radio.lt_alock),
                )
                .await;

                counters.set_online(false);
                tracing::warn!("{}: disconnected", ctx.name);
            }
            Err(e) => {
                tracing::warn!("{}: configuration failed: {}", ctx.name, e);
            }
        }

        // Check if event loop shut down
        if incoming_tx.is_closed() {
            tracing::debug!("{}: event loop shut down, stopping reconnect", ctx.name);
            return;
        }

        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Custom byte-channel factory (phone-attached radios)
// ---------------------------------------------------------------------------

/// The two boxed halves of a duplex byte channel, as yielded by an
/// [`RNodeChannelFactory`]. Separate read/write halves rather than a single
/// `AsyncRead + AsyncWrite` object because a trait object can name only one
/// non-marker trait; the interface re-joins them with [`tokio::io::join`].
pub type RNodeChannelHalves = (
    Box<dyn AsyncRead + Send + Unpin>,
    Box<dyn AsyncWrite + Send + Unpin>,
);

/// The future returned by [`RNodeChannelFactory::open`]: resolves to a fresh
/// pair of channel halves, or a boxed error.
pub type RNodeChannelOpenFuture = Pin<
    Box<
        dyn std::future::Future<
                Output = Result<RNodeChannelHalves, Box<dyn std::error::Error + Send + Sync>>,
            > + Send,
    >,
>;

/// A factory the reconnect loop calls to obtain a fresh duplex byte channel to
/// the RNode firmware.
///
/// Lets a host application supply the radio I/O over any substrate — USB-CDC,
/// BLE GATT (notify characteristic for read + write characteristic for write),
/// BT-Classic SPP, or an in-process mock pipe — on platforms where leviculum
/// never sees `/dev/ttyACM*` and cannot `open()` a serial path (Android, iOS).
/// The far end still speaks RNode KISS; leviculum still drives
/// detection/configuration/lifecycle. `open` is called once per (re)connection
/// attempt and should return a freshly-established channel each time.
pub trait RNodeChannelFactory: Send + Sync + 'static {
    /// Open a fresh duplex byte channel to the radio.
    fn open(&self) -> RNodeChannelOpenFuture;
}

/// Configuration for spawning an RNode interface over a host-supplied byte
/// channel (see [`RNodeChannelFactory`]). Mirrors [`RNodeInterfaceConfig`] but
/// replaces `port_path` with a `channel_factory`.
pub(crate) struct RNodeChannelInterfaceConfig {
    pub id: InterfaceId,
    pub name: String,
    pub channel_factory: Arc<dyn RNodeChannelFactory>,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    pub flow_control: bool,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    /// TEMPORARY (#347): see [`RNodeInterfaceConfig::jitter_arm`].
    pub jitter_arm: JitterArm,
    /// See [`RNodeInterfaceConfig::identity_hash`].
    pub identity_hash: [u8; 16],
}

impl RNodeChannelInterfaceConfig {
    fn radio_params(&self) -> RadioParams {
        RadioParams {
            frequency: self.frequency,
            bandwidth: self.bandwidth,
            tx_power: self.tx_power,
            // The channel API spells `tx_power` as a plain `u8`: the caller
            // always states one, so it is never the absent-key default and the
            // strict confirmation check applies.
            tx_power_derived: false,
            sf: self.sf,
            cr: self.cr,
            st_alock: self.st_alock,
            lt_alock: self.lt_alock,
        }
    }
}

/// Radio + transport parameters for a channel-backed RNode interface.
///
/// Used two ways:
/// - construction-time, built from
///   [`ReticulumNodeBuilder::add_rnode_channel_interface`](crate::driver::ReticulumNodeBuilder::add_rnode_channel_interface);
/// - runtime, passed to
///   [`ReticulumNode::spawn_rnode_channel_interface`](crate::driver::ReticulumNode::spawn_rnode_channel_interface)
///   to hot-plug a radio after the node is running.
///
/// The node assigns the `InterfaceId` and name; this config carries only the
/// caller-relevant fields. `frequency`/`bandwidth` are Hz, `tx_power` is dBm.
pub struct RNodeChannelConfig {
    pub factory: Arc<dyn RNodeChannelFactory>,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    pub flow_control: bool,
    pub buffer_size: usize,
}

/// Lifecycle handle for a runtime-attached channel-backed RNode interface,
/// returned by
/// [`ReticulumNode::spawn_rnode_channel_interface`](crate::driver::ReticulumNode::spawn_rnode_channel_interface).
///
/// **Hold it to keep the radio attached; drop it to detach.** Dropping (or
/// calling [`detach`](Self::detach)) signals the interface task to stop, which
/// closes its channel and makes the node's event loop tear the interface down
/// and remove it from routing — cleanly, without rebuilding the node.
///
/// (The interface's I/O channels live inside the node's event loop, which is
/// why this is a small control handle rather than the internal
/// `InterfaceHandle`: the latter's channels must be owned by the loop for the
/// interface to route at all.)
pub struct RNodeChannelHandle {
    id: InterfaceId,
    // Dropping this Sender resolves the task's shutdown receiver, which exits
    // the reconnect loop -> closes the incoming channel -> event loop detaches.
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl RNodeChannelHandle {
    pub(crate) fn new(id: InterfaceId, shutdown: tokio::sync::oneshot::Sender<()>) -> Self {
        Self {
            id,
            _shutdown: shutdown,
        }
    }

    /// The id the node assigned to this interface.
    pub fn id(&self) -> InterfaceId {
        self.id
    }

    /// Detach the interface now. Equivalent to dropping the handle; provided as
    /// an explicit, self-documenting call for host bindings.
    pub fn detach(self) {}
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Wire up channels + counters, spawn the reconnect task with the given
/// connector, and return an `InterfaceHandle`. Shared by both the serial
/// (`spawn_rnode_interface`) and channel (`spawn_rnode_channel_interface`)
/// entry points.
fn spawn_rnode_with_connector<S, C, Fut>(
    ctx: RNodeReconnectCtx,
    buffer_size: usize,
    connect: C,
    shutdown: Option<tokio::sync::oneshot::Receiver<()>>,
) -> InterfaceHandle
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<S, RNodeError>> + Send,
{
    let (incoming_tx, incoming_rx) = mpsc::channel(buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(buffer_size);
    let counters = Arc::new(InterfaceCounters::new());
    // Codeberg #25: RNode reports radio stats via CMD_STAT_*; mark the counters
    // radio-capable so interface_stats always emits the radio keys (with their
    // defaults) even before the first frame arrives.
    counters.enable_radio_stats();
    // Offline until the reconnect loop has detected and configured the radio
    // (L-0020); covers the window between registration and first configure.
    counters.set_online(false);
    let ready = super::ReadySignal::new();

    let id = ctx.id;
    let name = ctx.name.clone();
    let task_counters = Arc::clone(&counters);
    let task_ready = Arc::clone(&ready);
    let bitrate = rnode::compute_bitrate(ctx.radio.sf, ctx.radio.cr, ctx.radio.bandwidth);
    // Copied out beside `bitrate` and for the same reason: `ctx` moves into
    // the reconnect task below, and the handle must report the PHY the task is
    // about to program, not a second guess at it.
    let announce_cap_bps = announce_cap_bitrate(ctx.radio.sf, ctx.radio.cr, ctx.radio.bandwidth);
    // Copied out before `ctx` moves into the task: the handle reports the
    // same pre-TX jitter ceiling the TX loop actually draws against, rather
    // than recomputing it and risking the two drifting apart.
    let tx_jitter_max_ms = ctx.jitter_max_ms;
    // Copied out beside it, and for the same reason: what the frame that
    // TAKES the channel pays under the arm this build runs. A separate number
    // because arm 3 prices its slots in whole frames, so the two are only
    // equal on the arms that do not (#347, trace 223).
    let acquisition = compute_acquisition_ceiling(
        ctx.jitter_arm,
        ctx.radio.sf,
        ctx.radio.cr,
        ctx.radio.bandwidth,
    );
    // Copied out before `ctx` moves, for the same reason: what a full-size
    // frame costs the frame behind it at the PHY this task is about to
    // program. The receiver of a resource over this link floors its part
    // timeout with it (Codeberg #36/#374).
    let frame_turnaround_ms = max_tx_hold(ctx.radio.bandwidth, ctx.radio.sf, ctx.radio.cr).held_ms;

    tokio::spawn(async move {
        let run = rnode_reconnect_task(
            ctx,
            connect,
            incoming_tx,
            outgoing_rx,
            task_counters,
            task_ready,
        );
        match shutdown {
            // Runtime-attached interface: stop promptly when the caller drops
            // its RNodeChannelHandle (the Sender drops, this resolves). The
            // select drops `run`, cancelling the in-flight configure/IO and the
            // reconnect sleeps; the task then ends and its incoming channel
            // closes, which the event loop turns into a detach.
            Some(sd) => {
                tokio::select! {
                    _ = sd => {}
                    _ = run => {}
                }
            }
            // Construction-time interface: lives until the event loop drops the
            // handle (closing the incoming channel ends the reconnect loop).
            None => run.await,
        }
    });

    InterfaceHandle {
        info: InterfaceInfo {
            transit: true,
            id,
            name,
            hw_mtu: Some(rnode::HW_MTU as u32),
            is_local_client: false,
            bitrate: Some(bitrate),
            announce_cap_bitrate: announce_cap_bps,
            tx_jitter_max_ms: Some(tx_jitter_max_ms),
            acquisition: Some(acquisition),
            frame_turnaround_ms: Some(frame_turnaround_ms),
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Rnode,
            ingress_control: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        // RNode readiness is async: the reconnect task signals once the
        // channel is open, the firmware probe answered, and the radio is
        // configured (L-0024).
        ready,
    }
}

// One more parameter than clippy's default, for the same reason
// `rnode_io_task` above carries the allow: this is a struct-filling helper for
// the two spawn paths, and folding its fields into an intermediate struct would
// buy a type whose only job is to be unpacked one line later.
#[allow(clippy::too_many_arguments)]
fn reconnect_ctx_from_radio(
    id: InterfaceId,
    name: String,
    radio: RadioParams,
    flow_control: bool,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    test_drop_direct_ingress: bool,
    jitter_arm: JitterArm,
    frame_class: FrameClass,
) -> RNodeReconnectCtx {
    let jitter_max_ms = compute_jitter_max_ms(radio.sf, radio.cr, radio.bandwidth);
    RNodeReconnectCtx {
        id,
        name,
        radio,
        flow_control,
        reconnect_notify,
        jitter_max_ms,
        test_drop_direct_ingress,
        jitter_arm,
        frame_class,
    }
}

/// Spawn a complete RNode interface over a serial port, with reconnection.
///
/// Creates channels + counters, spawns the reconnect task, and returns an
/// `InterfaceHandle` for the event loop. Each (re)connection opens the serial
/// port fresh via [`open_serial_port`].
pub(crate) fn spawn_rnode_interface(config: RNodeInterfaceConfig) -> InterfaceHandle {
    let frame_class = FrameClass::of(&config.identity_hash, &config.name);
    let ctx = reconnect_ctx_from_radio(
        config.id,
        config.name.clone(),
        config.radio_params(),
        config.flow_control,
        config.reconnect_notify,
        config.test_drop_direct_ingress,
        config.jitter_arm,
        frame_class,
    );
    let buffer_size = config.buffer_size;
    let port_path = config.port_path;
    spawn_rnode_with_connector(
        ctx,
        buffer_size,
        move || {
            let path = port_path.clone();
            async move { open_serial_port(&path).await }
        },
        None,
    )
}

/// Spawn a complete RNode interface over a host-supplied byte channel, with
/// reconnection. The lifecycle (detect → configure → online → I/O →
/// reconnect-on-drop) is identical to [`spawn_rnode_interface`]; only the
/// transport differs. Each (re)connection calls
/// [`RNodeChannelFactory::open`] for a fresh duplex channel.
pub(crate) fn spawn_rnode_channel_interface(
    config: RNodeChannelInterfaceConfig,
    shutdown: Option<tokio::sync::oneshot::Receiver<()>>,
) -> InterfaceHandle {
    let frame_class = FrameClass::of(&config.identity_hash, &config.name);
    let ctx = reconnect_ctx_from_radio(
        config.id,
        config.name.clone(),
        config.radio_params(),
        config.flow_control,
        config.reconnect_notify,
        // Range emulation is a rig affordance; the phone-attached channel
        // path never needs it.
        false,
        config.jitter_arm,
        frame_class,
    );
    let buffer_size = config.buffer_size;
    let factory = config.channel_factory;
    spawn_rnode_with_connector(
        ctx,
        buffer_size,
        move || {
            let factory = Arc::clone(&factory);
            async move {
                let (read_half, write_half) = factory
                    .open()
                    .await
                    .map_err(|e| RNodeError::SerialPort(e.to_string()))?;
                // Re-join the two boxed halves into one AsyncRead + AsyncWrite
                // stream for the carrier-agnostic configure/IO path.
                Ok(tokio::io::join(read_half, write_half))
            }
        },
        shutdown,
    )
}

// ---------------------------------------------------------------------------
// Multi-interface (multi-vport RNode)
// ---------------------------------------------------------------------------
//
// An `RNodeMultiInterface` drives a single RNode that carries several LoRa
// transceivers, each exposed as a virtual port (vport). One serial link is
// shared; every per-vport command is prefixed with a CMD_SEL_INT frame
// (see `leviculum_core::rnode::build_vport_command`). Each vport is registered
// with the transport as its own logical interface, so announces and paths work
// per band exactly as if the radios were separate devices.
//
// Layering (matches the single-RNode interface): the carrier-medium specifics
// (serial framing, vport multiplexing, per-vport radio config push) live here;
// the transport sees N ordinary interfaces and stays vport-agnostic.

/// Radio + routing parameters for one vport subinterface.
pub(crate) struct RNodeSubinterfaceParams {
    /// Transport interface id assigned to this vport.
    pub id: InterfaceId,
    /// Display name (`<multi name>[<sub name>]`).
    pub name: String,
    /// Virtual port index on the device.
    pub vport: u8,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    /// See [`RadioParams::tx_power_derived`].
    pub tx_power_derived: bool,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    /// Whether this subinterface may transmit (Python `interface.OUT`).
    pub outgoing: bool,
}

impl RNodeSubinterfaceParams {
    fn radio_params(&self) -> RadioParams {
        RadioParams {
            frequency: self.frequency,
            bandwidth: self.bandwidth,
            tx_power: self.tx_power,
            tx_power_derived: self.tx_power_derived,
            sf: self.sf,
            cr: self.cr,
            st_alock: self.st_alock,
            lt_alock: self.lt_alock,
        }
    }
}

/// Configuration for spawning a multi-vport RNode interface over a serial port.
pub(crate) struct RNodeMultiInterfaceConfig {
    /// Name of the parent multi interface.
    pub name: String,
    /// Serial port shared by all vports.
    pub port_path: String,
    /// One entry per enabled subinterface (vport).
    pub subinterfaces: Vec<RNodeSubinterfaceParams>,
    pub flow_control: bool,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
}

/// Per-vport runtime state the shared hub task holds: the radio params to push,
/// the routing tag, and the channel to deliver received packets to this vport's
/// logical interface.
struct VportRuntime {
    id: InterfaceId,
    name: String,
    vport: u8,
    radio: RadioParams,
    outgoing: bool,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    counters: Arc<InterfaceCounters>,
}

/// A TX packet after merging all vports' outgoing channels, tagged with the
/// index into the hub's `VportRuntime` list it came from.
struct TaggedOutgoing {
    subint: usize,
    packet: OutgoingPacket,
}

/// Detect a multi-vport RNode and read its reported per-vport chip types.
///
/// Sends [`build_detect_query_multi`](rnode::build_detect_query_multi) (detect +
/// firmware + platform + MCU + interfaces) and collects the responses. The
/// returned `Vec<u8>` holds one chip-type byte per vport in vport order (empty
/// if the firmware did not report — older single-radio firmware, or a mock).
async fn detect_multi_on_port<S>(port: &mut S) -> Result<(RNodeDetectResult, Vec<u8>), RNodeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let query = rnode::build_detect_query_multi();
    port.write_all(&query).await?;

    let mut result = RNodeDetectResult {
        detected: false,
        firmware_version: None,
        platform: None,
        mcu: None,
    };
    let mut chip_types: Vec<u8> = Vec::new();

    read_frames_until_deadline(port, DETECT_TIMEOUT, |command, payload| match command {
        rnode::CMD_DETECT if payload.first() == Some(&rnode::DETECT_RESP) => {
            result.detected = true;
        }
        rnode::CMD_FW_VERSION => {
            result.firmware_version = rnode::decode_firmware_version(payload);
        }
        rnode::CMD_PLATFORM => {
            result.platform = payload.first().copied();
        }
        rnode::CMD_MCU => {
            result.mcu = payload.first().copied();
        }
        rnode::CMD_INTERFACES => {
            // One frame can carry several 2-byte records; a device may also
            // emit multiple frames. Append in arrival (vport) order.
            chip_types.extend(rnode::decode_interfaces(payload));
        }
        _ => {}
    })
    .await?;

    if !result.detected {
        return Err(RNodeError::NotDetected);
    }

    Ok((result, chip_types))
}

/// Push per-vport radio configuration: for each vport, a CMD_SEL_INT frame
/// followed by frequency/bandwidth/txpower/sf/cr/[alock]/radio-on, mirroring
/// Python `RNodeSubInterface.initRadio` driving the parent's `set*` methods.
async fn send_multi_radio_config<S>(port: &mut S, vports: &[VportRuntime]) -> Result<(), RNodeError>
where
    S: AsyncWrite + Unpin,
{
    let mut bytes = Vec::with_capacity(64 * vports.len());
    for v in vports {
        let r = &v.radio;
        bytes.extend_from_slice(&rnode::build_vport_command(
            v.vport,
            &rnode::build_set_frequency(r.frequency),
        ));
        bytes.extend_from_slice(&rnode::build_vport_command(
            v.vport,
            &rnode::build_set_bandwidth(r.bandwidth),
        ));
        bytes.extend_from_slice(&rnode::build_vport_command(
            v.vport,
            &rnode::build_set_txpower(r.tx_power),
        ));
        bytes.extend_from_slice(&rnode::build_vport_command(
            v.vport,
            &rnode::build_set_sf(r.sf),
        ));
        bytes.extend_from_slice(&rnode::build_vport_command(
            v.vport,
            &rnode::build_set_cr(r.cr),
        ));
        if let Some(st) = r.st_alock {
            bytes.extend_from_slice(&rnode::build_vport_command(
                v.vport,
                &rnode::build_set_st_alock(st),
            ));
        }
        if let Some(lt) = r.lt_alock {
            bytes.extend_from_slice(&rnode::build_vport_command(
                v.vport,
                &rnode::build_set_lt_alock(lt),
            ));
        }
        bytes.extend_from_slice(&rnode::build_vport_command(
            v.vport,
            &rnode::build_set_radio_state(rnode::RADIO_STATE_ON),
        ));
    }
    port.write_all(&bytes).await?;
    port.flush().await?;
    Ok(())
}

/// Detect + validate firmware + validate vports + push per-vport config over an
/// already-open, settled channel. The single-RNode analogue is
/// [`configure_stream`]; this adds the vport dimension.
async fn configure_multi_stream<S>(
    port: &mut S,
    vports: &[VportRuntime],
    name: &str,
) -> Result<RNodeDetectResult, RNodeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (detect, chip_types) = detect_multi_on_port(port).await?;

    match detect.firmware_version {
        Some((maj, min)) if rnode::validate_firmware(maj, min) => {}
        Some((maj, min)) => {
            return Err(RNodeError::FirmwareTooOld(
                maj,
                min,
                rnode::REQUIRED_FW_MAJ,
                rnode::REQUIRED_FW_MIN,
            ));
        }
        None => return Err(RNodeError::NotDetected),
    }

    // Validate each vport index against the device's report. When the device
    // reported types (`!chip_types.is_empty()`), a configured vport must exist;
    // this is Python's hard check. When it reported none (mock or firmware that
    // does not answer CMD_INTERFACES), proceed best-effort -- the SEL_INT frames
    // are still correct, and refusing to start would strand a usable radio.
    for v in vports {
        rnode::validate_config(
            v.radio.frequency,
            v.radio.bandwidth,
            v.radio.tx_power,
            v.radio.sf,
            v.radio.cr,
        )
        .map_err(|e| RNodeError::RadioMismatch(format!("{}: {}", v.name, e)))?;
        if !chip_types.is_empty() && (v.vport as usize) >= chip_types.len() {
            return Err(RNodeError::RadioMismatch(format!(
                "vport {} for {} does not exist on device ({} vports reported)",
                v.vport,
                v.name,
                chip_types.len()
            )));
        }
    }

    send_multi_radio_config(port, vports).await?;
    tokio::time::sleep(CONFIG_PROCESS_WAIT).await;

    // Drain confirmation frames for the settle window. Per-vport strict
    // validation is deferred (HW gap): unlike the single interface we do not
    // fail startup on a missing echo, because a partial multi-band device
    // should still bring up the vports it can. Config correctness is covered by
    // the byte-level KAT and the mock exchange.
    let _ = read_frames_until_deadline(port, CONFIG_PROCESS_WAIT, |_, _| {}).await;

    tracing::info!(
        "{}: multi-vport configured ({} vports, device reported {} chip types)",
        name,
        vports.len(),
        chip_types.len()
    );
    Ok(detect)
}

/// Shared serial I/O loop for a multi-vport RNode.
///
/// RX: tracks the selected vport from CMD_SEL_INT frames and routes each
/// following CMD_DATA frame to that vport's logical interface. TX: pulls
/// vport-tagged packets from `merged_rx`, prefixes each with its vport's
/// CMD_SEL_INT, and paces writes with the serial-level spacing floor.
///
/// Returns when the channel fails so the reconnect wrapper can retry.
async fn rnode_multi_io_task<S>(
    name: &str,
    port: &mut S,
    vports: &[VportRuntime],
    vport_to_subint: &HashMap<u8, usize>,
    merged_rx: &mut mpsc::Receiver<TaggedOutgoing>,
    flow_control: bool,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; IO_READ_BUF];
    let mut selected_vport: u8 = 0;
    // Vports whose logical interface has been torn down. A dead vport is
    // deregistered from routing, not a reason to drop the shared radio: the
    // other vports on the same serial port are still carrying traffic (#283).
    // Seeded from the channel state so a vport that died during a disconnect
    // is not rediscovered one lost frame at a time.
    let mut dead: Vec<bool> = vports.iter().map(|v| v.incoming_tx.is_closed()).collect();
    if dead.iter().all(|&d| d) {
        tracing::debug!("{}: no live vport left, io task not starting", name);
        return;
    }
    let mut interface_ready = true;
    let mut send_queue: VecDeque<(usize, Vec<u8>)> = VecDeque::new();
    let mut send_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut timer_ready = true;

    // CMD_READY reopen state, same query protocol as the single-radio io
    // task (the firmware only ever answers a host query). The vports share
    // one firmware queue, so one gate and one poll cadence: seeded from the
    // slowest vport's packet airtime — the conservative bound on how fast
    // the shared queue can drain.
    let ready_poll_start = vports
        .iter()
        .map(|v| ready_poll_initial(v.radio.sf, v.radio.cr, v.radio.bandwidth))
        .max()
        .unwrap_or(READY_POLL_MAX);
    let mut ready_poll = ready_poll_start;
    let mut ready_query_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;

    loop {
        tokio::select! {
            result = port.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::warn!("{}: serial port EOF", name);
                        abandon_multi_send_queue(name, vports, &mut send_queue, "serial_eof");
                        return;
                    }
                    Ok(n) => {
                        for frame in deframer.process(&buf[..n]) {
                            let KissDeframeResult::Frame { command, payload } = frame else { continue; };
                            match command {
                                rnode::CMD_SEL_INT => {
                                    if let Some(vp) = rnode::decode_select_interface(&payload) {
                                        selected_vport = vp;
                                    }
                                }
                                rnode::CMD_DATA => {
                                    match vport_to_subint.get(&selected_vport) {
                                        Some(&idx) if !dead[idx] => {
                                            let v = &vports[idx];
                                            v.counters.rx_bytes.fetch_add(
                                                payload.len() as u64,
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
                                            tracing::debug!(
                                                "{}: RX {} bytes on vport {} -> {}",
                                                name, payload.len(), selected_vport, v.name
                                            );
                                            if v.incoming_tx
                                                .send(IncomingPacket { data: payload.to_vec() })
                                                .await
                                                .is_err()
                                            {
                                                // This vport's event loop is gone. Deregister
                                                // it and keep serving the others: returning
                                                // here tore down the shared radio, and the
                                                // reconnect loop then bounced it every five
                                                // seconds forever, because the next frame for
                                                // the same dead vport ended it again (#283).
                                                deregister_vport(
                                                    name, vports, &mut send_queue, &mut dead, idx,
                                                );
                                                if dead.iter().all(|&d| d) {
                                                    tracing::debug!(
                                                        "{}: last vport shut down, ending io task",
                                                        name
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                        // A vport whose interface is gone: its frames are
                                        // dropped where they arrive, without touching the
                                        // radio the live vports share.
                                        Some(&idx) => {
                                            tracing::trace!(
                                                "{}: RX for deregistered vport {} -> {} (dropped)",
                                                name, selected_vport, vports[idx].name
                                            );
                                        }
                                        None => {
                                            tracing::warn!(
                                                "{}: RX data for unknown vport {} (dropped)",
                                                name, selected_vport
                                            );
                                        }
                                    }
                                }
                                rnode::CMD_READY if flow_control => {
                                    // Answer to our queue-state query: 0x01 reopens
                                    // the gate, 0x00 leaves the armed timer polling.
                                    if payload.first() == Some(&0x01) {
                                        interface_ready = true;
                                        ready_query_timer = None;
                                        ready_poll = ready_poll_start;
                                    }
                                }
                                rnode::CMD_ERROR => {
                                    match payload.first().copied() {
                                        Some(rnode::ERROR_INITRADIO) => {
                                            tracing::error!("{}: radio init failed", name);
                                            abandon_multi_send_queue(
                                                name, vports, &mut send_queue, "error_initradio",
                                            );
                                            return;
                                        }
                                        Some(rnode::ERROR_TXFAILED) => {
                                            tracing::error!("{}: TX failed", name);
                                            abandon_multi_send_queue(
                                                name, vports, &mut send_queue, "error_txfailed",
                                            );
                                            return;
                                        }
                                        Some(code) => {
                                            tracing::warn!("{}: device error 0x{:02X}", name, code);
                                        }
                                        None => {}
                                    }
                                }
                                rnode::CMD_RESET
                                    if payload.first() == Some(&DEVICE_RESET_MARKER) =>
                                {
                                    tracing::warn!("{}: device reset (0xF8)", name);
                                    abandon_multi_send_queue(
                                        name, vports, &mut send_queue, "device_reset",
                                    );
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("{}: serial read error: {}", name, e);
                        abandon_multi_send_queue(
                            name, vports, &mut send_queue, "serial_read_error",
                        );
                        return;
                    }
                }
            }

            recv = merged_rx.recv() => {
                match recv {
                    Some(tagged) => {
                        // A subinterface with outgoing = false must never transmit
                        // (Python `interface.OUT = False`). Drop silently.
                        if !vports[tagged.subint].outgoing {
                            tracing::debug!(
                                "{}: dropping TX on non-outgoing vport {}",
                                name, vports[tagged.subint].vport
                            );
                            continue;
                        }
                        // A deregistered vport keeps its hands off the shared
                        // radio in both directions (#283).
                        if dead[tagged.subint] {
                            tracing::debug!(
                                "{}: dropping TX on deregistered vport {}",
                                name, vports[tagged.subint].vport
                            );
                            continue;
                        }
                        if send_queue.len() >= FLOW_CONTROL_QUEUE_LIMIT {
                            if let Some((shed_subint, shed)) = send_queue.pop_front() {
                                // A dropped frame must be loud, same as the
                                // single-radio twin. The count lands on the
                                // vport that owned the shed frame; the event
                                // names the physical interface — the port
                                // that dropped is a property of the shared
                                // line, not of any one vport.
                                vports[shed_subint]
                                    .counters
                                    .tx_queue_drops
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                vports[shed_subint].counters.tx_dropped_bytes.fetch_add(
                                    shed.len() as u64,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                tracing::warn!(
                                    event = "RNODE_TX_QUEUE_DROP",
                                    iface = %Scalar(name),
                                    len = shed.len(),
                                    depth = send_queue.len(),
                                    reason = "queue_full",
                                );
                            }
                        }
                        send_queue.push_back((tagged.subint, tagged.packet.data));
                    }
                    None => {
                        // All vport senders dropped: interface tearing down.
                        abandon_multi_send_queue(
                            name, vports, &mut send_queue, "outgoing_closed",
                        );
                        return;
                    }
                }
            }

            _ = async {
                if let Some(ref mut timer) = send_timer {
                    timer.await;
                }
            }, if send_timer.is_some() => {
                send_timer = None;
                timer_ready = true;
            }

            // No queue-not-full answer within the backoff window — re-query.
            _ = async {
                if let Some(ref mut timer) = ready_query_timer {
                    timer.await;
                }
            }, if ready_query_timer.is_some() => {
                if let Err(e) = port.write_all(&READY_QUERY_FRAME).await {
                    tracing::warn!("{}: ready query write error: {}", name, e);
                    abandon_multi_send_queue(
                        name, vports, &mut send_queue, "ready_query_write_error",
                    );
                    return;
                }
                if let Err(e) = port.flush().await {
                    tracing::warn!("{}: ready query flush error: {}", name, e);
                    abandon_multi_send_queue(
                        name, vports, &mut send_queue, "ready_query_flush_error",
                    );
                    return;
                }
                ready_query_timer = Some(Box::pin(tokio::time::sleep(ready_poll)));
                ready_poll = (ready_poll * 2).min(READY_POLL_MAX);
            }
        }

        // Send if the spacing gate and the flow-control gate are both open.
        if timer_ready && (interface_ready || !flow_control) {
            if let Some((subint, data)) = send_queue.pop_front() {
                let v = &vports[subint];
                let frame = rnode::build_vport_data_frame(v.vport, &data);
                if let Err(e) = port.write_all(&frame).await {
                    tracing::warn!("{}: write error: {}", name, e);
                    // The frame in hand is lost with the rest; count it.
                    send_queue.push_front((subint, data));
                    abandon_multi_send_queue(name, vports, &mut send_queue, "serial_write_error");
                    return;
                }
                if let Err(e) = port.flush().await {
                    tracing::warn!("{}: flush error: {}", name, e);
                    send_queue.push_front((subint, data));
                    abandon_multi_send_queue(name, vports, &mut send_queue, "serial_flush_error");
                    return;
                }
                v.counters
                    .tx_bytes
                    .fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(
                    "{}: TX {} bytes on vport {} ({})",
                    name,
                    data.len(),
                    v.vport,
                    v.name
                );
                timer_ready = false;
                if flow_control {
                    // Same query protocol as the single-radio task: close
                    // the gate, ask the shared queue for room.
                    interface_ready = false;
                    if let Err(e) = port.write_all(&READY_QUERY_FRAME).await {
                        tracing::warn!("{}: ready query write error: {}", name, e);
                        abandon_multi_send_queue(
                            name,
                            vports,
                            &mut send_queue,
                            "ready_query_write_error",
                        );
                        return;
                    }
                    if let Err(e) = port.flush().await {
                        tracing::warn!("{}: ready query flush error: {}", name, e);
                        abandon_multi_send_queue(
                            name,
                            vports,
                            &mut send_queue,
                            "ready_query_flush_error",
                        );
                        return;
                    }
                    ready_poll = ready_poll_start;
                    ready_query_timer = Some(Box::pin(tokio::time::sleep(ready_poll)));
                    ready_poll = (ready_poll * 2).min(READY_POLL_MAX);
                }
                // One firmware queue serves every vport, and it is flushed
                // whole once its CSMA contest is won, so the single-radio
                // rule applies unchanged here: hold the next frame — whatever
                // vport it belongs to — until this one has left the air. The
                // hold is priced at the PHY of the vport that just
                // transmitted, which is the radio the air-time was spent on.
                // This task parses no stat frames, so the CSMA terms are the
                // reference derivation rather than the modem's own report.
                let hold = tx_hold(
                    data.len() as u32,
                    v.radio.bandwidth,
                    v.radio.sf,
                    v.radio.cr,
                    &FirmwareCsma::default(),
                );
                tracing::debug!(
                    target: "leviculum_std::interfaces::rnode::tx_trace",
                    "LORA_TX_HOLD iface={} held_ms={} airtime_ms={} difs_ms={} cw_ms={}",
                    v.name, hold.held_ms, hold.airtime_ms, hold.difs_ms, hold.cw_ms
                );
                send_timer = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                    hold.held_ms,
                ))));
            }
            // Nothing queued: leave `timer_ready` set so the next packet ships
            // immediately without waiting on a fresh spacing timer.
        }
    }
}

/// Reconnect loop for a multi-vport RNode: open channel -> configure all vports
/// -> shared I/O -> on disconnect notify each vport, wait, retry.
async fn rnode_multi_reconnect_task<S, C, Fut>(
    name: String,
    connect: C,
    vports: Vec<VportRuntime>,
    mut merged_rx: mpsc::Receiver<TaggedOutgoing>,
    flow_control: bool,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<S, RNodeError>>,
{
    let vport_to_subint: HashMap<u8, usize> = vports
        .iter()
        .enumerate()
        .map(|(i, v)| (v.vport, i))
        .collect();
    let mut has_connected_before = false;

    loop {
        let opened = async {
            let mut port = connect().await?;
            configure_multi_stream(&mut port, &vports, &name).await?;
            Ok::<_, RNodeError>(port)
        }
        .await;

        match opened {
            Ok(mut port) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;
                if is_reconnect {
                    if let Some(ref notify) = reconnect_notify {
                        for v in &vports {
                            if let Err(e) = notify.try_send(v.id) {
                                tracing::warn!("{}: reconnect notify failed: {}", name, e);
                            }
                        }
                    }
                }

                rnode_multi_io_task(
                    &name,
                    &mut port,
                    &vports,
                    &vport_to_subint,
                    &mut merged_rx,
                    flow_control,
                )
                .await;

                tracing::warn!("{}: disconnected", name);
            }
            Err(e) => {
                tracing::warn!("{}: configuration failed: {}", name, e);
            }
        }

        // Stop if every vport's logical interface has been torn down.
        if vports.iter().all(|v| v.incoming_tx.is_closed()) {
            tracing::debug!("{}: all vports shut down, stopping reconnect", name);
            return;
        }

        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

/// Build the per-vport `InterfaceHandle`s and spawn the shared hub task.
///
/// Returns one `InterfaceHandle` per subinterface, each registered with the
/// transport as an independent logical interface. All share the single serial
/// port via the hub task; a per-vport relay forwards that vport's outgoing
/// channel into the hub's merged TX channel, tagged so the hub knows which
/// vport (and CMD_SEL_INT) to emit.
pub(crate) fn spawn_rnode_multi_interface(
    config: RNodeMultiInterfaceConfig,
) -> Vec<InterfaceHandle> {
    let RNodeMultiInterfaceConfig {
        name,
        port_path,
        subinterfaces,
        flow_control,
        buffer_size,
        reconnect_notify,
    } = config;

    let (merged_tx, merged_rx) = mpsc::channel::<TaggedOutgoing>(buffer_size.max(1) * 2);

    let mut handles: Vec<InterfaceHandle> = Vec::with_capacity(subinterfaces.len());
    let mut runtimes: Vec<VportRuntime> = Vec::with_capacity(subinterfaces.len());

    for (subint_idx, sub) in subinterfaces.iter().enumerate() {
        let (incoming_tx, incoming_rx) = mpsc::channel::<IncomingPacket>(buffer_size);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<OutgoingPacket>(buffer_size);
        let counters = Arc::new(InterfaceCounters::new());
        let bitrate = rnode::compute_bitrate(sub.sf, sub.cr, sub.bandwidth);

        handles.push(InterfaceHandle {
            info: InterfaceInfo {
                transit: true,
                id: sub.id,
                name: sub.name.clone(),
                hw_mtu: Some(rnode::HW_MTU as u32),
                is_local_client: false,
                bitrate: Some(bitrate),
                // Each vport is an independent logical interface on its own
                // carrier, so each takes its own share; capping only the
                // section-index one would leave the rest uncapped.
                announce_cap_bitrate: announce_cap_bitrate(sub.sf, sub.cr, sub.bandwidth),
                tx_jitter_max_ms: Some(compute_jitter_max_ms(sub.sf, sub.cr, sub.bandwidth)),
                // A vport's transmit path runs no #347 arm of its own, so the
                // acquisition it can owe is the unmodified slot-priced one.
                acquisition: Some(compute_acquisition_ceiling(
                    JitterArm::AsIs,
                    sub.sf,
                    sub.cr,
                    sub.bandwidth,
                )),
                frame_turnaround_ms: Some(max_tx_hold(sub.bandwidth, sub.sf, sub.cr).held_ms),
                ifac: None,
                mode: leviculum_core::traits::InterfaceMode::default(),
                kind: leviculum_core::traits::InterfaceKind::Rnode,
                ingress_control: None,
            },
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            counters: Arc::clone(&counters),
            credit: None,
            ready: super::ReadySignal::ready_immediate(),
        });

        // Relay this vport's outgoing packets into the shared merged channel,
        // tagged with its index. Lives until the handle's outgoing sender drops.
        let relay_tx = merged_tx.clone();
        let subint = subint_idx;
        tokio::spawn(async move {
            while let Some(pkt) = outgoing_rx.recv().await {
                if relay_tx
                    .send(TaggedOutgoing {
                        subint,
                        packet: pkt,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        runtimes.push(VportRuntime {
            id: sub.id,
            name: sub.name.clone(),
            vport: sub.vport,
            radio: sub.radio_params(),
            outgoing: sub.outgoing,
            incoming_tx,
            counters,
        });
    }
    // Drop the hub's own clone so the merged channel closes once every relay
    // (i.e. every vport handle) is gone.
    drop(merged_tx);

    tokio::spawn(async move {
        rnode_multi_reconnect_task(
            name,
            move || {
                let path = port_path.clone();
                async move { open_serial_port(&path).await }
            },
            runtimes,
            merged_rx,
            flow_control,
            reconnect_notify,
        )
        .await;
    });

    handles
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// In-process RNode firmware stub over one half of a `tokio::io::duplex`
    /// pair. Answers the detect probe (so firmware validation passes) and echoes
    /// each radio-config command back as its confirmation. When it receives the
    /// first outbound `CMD_DATA` from the interface (proving the I/O phase is
    /// live) it records the payload and injects one inbound data frame in reply
    /// — injecting earlier would have it swallowed by the config-validation
    /// read window, which ignores data frames.
    async fn rnode_firmware_stub(
        mut peer: tokio::io::DuplexStream,
        inbound: Vec<u8>,
        got_outbound: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut buf = [0u8; 512];
        let mut injected = false;
        // kiss::frame clears its output each call, so accumulate via a scratch.
        let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
            let mut one = Vec::new();
            kiss::frame(cmd, payload, &mut one);
            reply.extend_from_slice(&one);
        };
        loop {
            let n = match peer.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut reply: Vec<u8> = Vec::new();
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    match command {
                        rnode::CMD_DETECT => {
                            // Answer the probe: detected + firmware >= required.
                            push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                            push(
                                &mut reply,
                                rnode::CMD_FW_VERSION,
                                &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                            );
                            push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                            push(&mut reply, rnode::CMD_MCU, &[0x00]);
                        }
                        rnode::CMD_FREQUENCY
                        | rnode::CMD_BANDWIDTH
                        | rnode::CMD_TXPOWER
                        | rnode::CMD_SF
                        | rnode::CMD_CR
                        | rnode::CMD_RADIO_STATE => {
                            // Echo the requested value back as confirmation.
                            push(&mut reply, command, &payload);
                        }
                        rnode::CMD_DATA => {
                            let _ = got_outbound.try_send(payload.to_vec());
                            if !injected {
                                injected = true;
                                push(&mut reply, rnode::CMD_DATA, &inbound);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
                return;
            }
        }
    }

    // #19: a host-supplied byte channel drives the full RNode lifecycle —
    // detect → configure → online → outbound frame → inbound frame — with no
    // serial port, via spawn_rnode_channel_interface + RNodeChannelFactory.
    #[tokio::test]
    async fn test_rnode_channel_interface_lifecycle() {
        // The factory hands leviculum one (split) half of an in-memory duplex;
        // the firmware stub owns the other half.
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (read_half, write_half) = tokio::io::split(port);
        let halves: std::sync::Mutex<Option<RNodeChannelHalves>> = std::sync::Mutex::new(Some((
            Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
            Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
        )));

        struct MockFactory(std::sync::Mutex<Option<RNodeChannelHalves>>);
        impl RNodeChannelFactory for MockFactory {
            fn open(&self) -> RNodeChannelOpenFuture {
                let taken = self.0.lock().unwrap().take();
                Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
            }
        }

        let (got_tx, mut got_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let stub = tokio::spawn(async move {
            rnode_firmware_stub(peer, b"inbound-over-channel".to_vec(), got_tx).await;
        });

        let mut handle = spawn_rnode_channel_interface(
            RNodeChannelInterfaceConfig {
                id: InterfaceId(0),
                name: "rnode_channel_test".to_string(),
                channel_factory: Arc::new(MockFactory(halves)),
                frequency: 868_000_000,
                bandwidth: 125_000,
                tx_power: 17,
                sf: 7,
                cr: 5,
                st_alock: None,
                lt_alock: None,
                flow_control: false,
                buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
                reconnect_notify: None,
                jitter_arm: JitterArm::AsIs,
                identity_hash: [0u8; 16],
            },
            None,
        );

        // Bitrate is computed at spawn from the radio params.
        assert!(handle.info.bitrate.is_some());

        // Queue an outbound packet. Once detect+configure complete and the I/O
        // phase begins, it must traverse the channel to the firmware stub.
        handle
            .outgoing
            .send(OutgoingPacket {
                peer: None,
                data: b"outbound-over-channel".to_vec(),
                high_priority: false,
            })
            .await
            .expect("send to interface");

        // The stub records the outbound payload (proves detect → configure →
        // online completed over the channel) and injects an inbound frame in
        // reply, which must surface on the interface's incoming channel.
        let outbound = tokio::time::timeout(Duration::from_secs(8), got_rx.recv())
            .await
            .expect("outbound CMD_DATA must reach the stub within 8s (lifecycle reached online)")
            .expect("got_outbound channel open");
        assert_eq!(outbound, b"outbound-over-channel");

        let incoming = tokio::time::timeout(Duration::from_secs(8), handle.incoming.recv())
            .await
            .expect("inbound packet must arrive within 8s")
            .expect("incoming channel open");
        assert_eq!(incoming.data, b"inbound-over-channel");

        stub.abort();
    }

    // L-0024: `ready` must reflect the RNode lifecycle (channel open →
    // firmware probe → radio config), not handle construction. A caller
    // that sends after a construction-time `ready` sends into a port that
    // may not even answer the detect probe yet.
    #[tokio::test]
    async fn test_rnode_channel_ready_waits_for_configuration() {
        struct MockFactory(std::sync::Mutex<Option<RNodeChannelHalves>>);
        impl RNodeChannelFactory for MockFactory {
            fn open(&self) -> RNodeChannelOpenFuture {
                let taken = self.0.lock().unwrap().take();
                Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
            }
        }
        let config =
            |factory: Arc<dyn RNodeChannelFactory>, name: &str| RNodeChannelInterfaceConfig {
                id: InterfaceId(0),
                name: name.to_string(),
                channel_factory: factory,
                frequency: 868_000_000,
                bandwidth: 125_000,
                tx_power: 17,
                sf: 7,
                cr: 5,
                st_alock: None,
                lt_alock: None,
                flow_control: false,
                buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
                reconnect_notify: None,
                jitter_arm: JitterArm::AsIs,
                identity_hash: [0u8; 16],
            };
        let halves_of = |port: tokio::io::DuplexStream| {
            let (read_half, write_half) = tokio::io::split(port);
            std::sync::Mutex::new(Some((
                Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
                Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
            )))
        };

        // (a) A channel whose far end never answers the detect probe: the
        // interface must NOT report ready.
        let (port, silent_peer) = tokio::io::duplex(64 * 1024);
        let handle = spawn_rnode_channel_interface(
            config(Arc::new(MockFactory(halves_of(port))), "rnode_ready_silent"),
            None,
        );
        assert!(
            handle.ready.wait(Duration::from_millis(300)).await.is_err(),
            "L-0024: ready fired although the firmware probe never answered"
        );
        drop(silent_peer);
        drop(handle);

        // (b) Positive control: with a firmware stub that answers detect and
        // confirms the radio config, ready fires.
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (got_tx, _got_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let stub = tokio::spawn(async move {
            rnode_firmware_stub(peer, Vec::new(), got_tx).await;
        });
        let handle = spawn_rnode_channel_interface(
            config(Arc::new(MockFactory(halves_of(port))), "rnode_ready_probed"),
            None,
        );
        handle
            .ready
            .wait(Duration::from_secs(8))
            .await
            .expect("ready must fire once detect+configure completed");
        stub.abort();
    }

    /// Minimal firmware stub that answers detect + echoes config confirmations,
    /// signals once the radio is switched on (`configured`), and signals again
    /// when the channel closes (`closed`) — i.e. when the interface task is torn
    /// down. Used by the runtime attach/detach test.
    async fn rnode_firmware_stub_signals(
        mut peer: tokio::io::DuplexStream,
        configured: tokio::sync::mpsc::Sender<()>,
        closed: tokio::sync::mpsc::Sender<()>,
    ) {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut buf = [0u8; 512];
        let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
            let mut one = Vec::new();
            kiss::frame(cmd, payload, &mut one);
            reply.extend_from_slice(&one);
        };
        loop {
            let n = match peer.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = closed.try_send(());
                    return;
                }
                Ok(n) => n,
            };
            let mut reply: Vec<u8> = Vec::new();
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    match command {
                        rnode::CMD_DETECT => {
                            push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                            push(
                                &mut reply,
                                rnode::CMD_FW_VERSION,
                                &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                            );
                            push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                            push(&mut reply, rnode::CMD_MCU, &[0x00]);
                        }
                        rnode::CMD_FREQUENCY
                        | rnode::CMD_BANDWIDTH
                        | rnode::CMD_TXPOWER
                        | rnode::CMD_SF
                        | rnode::CMD_CR => push(&mut reply, command, &payload),
                        rnode::CMD_RADIO_STATE => {
                            push(&mut reply, command, &payload);
                            let _ = configured.try_send(());
                        }
                        _ => {}
                    }
                }
            }
            if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
                let _ = closed.try_send(());
                return;
            }
        }
    }

    // #19 follow-up: attach a channel-backed RNode interface to a RUNNING node
    // at runtime (hot-plug), then detach it by dropping the handle.
    #[tokio::test]
    async fn test_runtime_attach_detach_rnode_channel() {
        use crate::driver::ReticulumNodeBuilder;

        let td = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync");
        node.start().await.expect("start");

        // Wire a mock radio over an in-memory duplex.
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (read_half, write_half) = tokio::io::split(port);
        let halves: std::sync::Mutex<Option<RNodeChannelHalves>> = std::sync::Mutex::new(Some((
            Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
            Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
        )));
        struct MockFactory(std::sync::Mutex<Option<RNodeChannelHalves>>);
        impl RNodeChannelFactory for MockFactory {
            fn open(&self) -> RNodeChannelOpenFuture {
                let taken = self.0.lock().unwrap().take();
                Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
            }
        }

        let (cfg_tx, mut cfg_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (closed_tx, mut closed_rx) = tokio::sync::mpsc::channel::<()>(1);
        let stub = tokio::spawn(rnode_firmware_stub_signals(peer, cfg_tx, closed_tx));

        // Hot-plug: attach the radio to the already-running node.
        let handle = node
            .spawn_rnode_channel_interface(RNodeChannelConfig {
                factory: Arc::new(MockFactory(halves)),
                frequency: 868_000_000,
                bandwidth: 125_000,
                tx_power: 17,
                sf: 7,
                cr: 5,
                st_alock: None,
                lt_alock: None,
                flow_control: false,
                buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
            })
            .expect("attach must succeed on a running node");

        // The interface ran its detect → configure lifecycle over the channel.
        tokio::time::timeout(Duration::from_secs(8), cfg_rx.recv())
            .await
            .expect("interface must reach configured within 8s (runtime attach worked)")
            .expect("cfg channel open");

        // Detach by dropping the handle: the task stops and the channel closes.
        handle.detach();
        tokio::time::timeout(Duration::from_secs(8), closed_rx.recv())
            .await
            .expect("channel must close within 8s of dropping the handle (detach worked)")
            .expect("closed channel open");

        stub.abort();
        node.stop().await.expect("stop");
    }

    // Codeberg #136 follow-up: `InterfaceStatusSnapshot.interface_id` exists so
    // a caller holding a runtime handle can pair it with a snapshot without
    // matching on the name. That only holds if the id in the snapshot is the
    // same id the handle reports, which is what this asserts — together with
    // the `kind` from #140, since both fields are populated from the same
    // registry entry and a hot-plugged interface is the case that exercises
    // the dynamic-registration path rather than the config path.
    #[tokio::test]
    async fn test_interface_stats_id_pairs_with_runtime_handle() {
        use crate::driver::ReticulumNodeBuilder;

        let td = tempfile::tempdir().expect("tempdir");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync");
        node.start().await.expect("start");

        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (read_half, write_half) = tokio::io::split(port);
        let halves: std::sync::Mutex<Option<RNodeChannelHalves>> = std::sync::Mutex::new(Some((
            Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
            Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
        )));
        struct MockFactory(std::sync::Mutex<Option<RNodeChannelHalves>>);
        impl RNodeChannelFactory for MockFactory {
            fn open(&self) -> RNodeChannelOpenFuture {
                let taken = self.0.lock().unwrap().take();
                Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
            }
        }

        let (cfg_tx, mut cfg_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (closed_tx, _closed_rx) = tokio::sync::mpsc::channel::<()>(1);
        let stub = tokio::spawn(rnode_firmware_stub_signals(peer, cfg_tx, closed_tx));

        let handle = node
            .spawn_rnode_channel_interface(RNodeChannelConfig {
                factory: Arc::new(MockFactory(halves)),
                frequency: 868_000_000,
                bandwidth: 125_000,
                tx_power: 17,
                sf: 7,
                cr: 5,
                st_alock: None,
                lt_alock: None,
                flow_control: false,
                buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
            })
            .expect("attach must succeed on a running node");
        tokio::time::timeout(Duration::from_secs(8), cfg_rx.recv())
            .await
            .expect("interface must reach configured within 8s")
            .expect("cfg channel open");

        // The snapshot list must contain exactly the handle's id, and that
        // entry must be the radio (not some other interface that happened to
        // land on the same index).
        let mut entry = None;
        for _ in 0..80 {
            entry = node
                .interface_stats()
                .into_iter()
                .find(|e| e.interface_id == handle.id());
            if entry.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let entry = entry.unwrap_or_else(|| {
            panic!(
                "no snapshot carried the handle's id {:?}; snapshots = {:?}",
                handle.id(),
                node.interface_stats()
                    .iter()
                    .map(|e| (e.interface_id, e.name.clone()))
                    .collect::<Vec<_>>()
            )
        });
        assert_eq!(
            entry.kind,
            leviculum_core::traits::InterfaceKind::Rnode,
            "the entry paired by id must be the hot-plugged radio"
        );
        assert!(
            entry.name.starts_with("rnode_channel_"),
            "paired entry name should be the radio's, got {:?}",
            entry.name
        );

        handle.detach();
        stub.abort();
        node.stop().await.expect("stop");
    }

    #[tokio::test]
    #[ignore] // Requires RNode hardware at /dev/ttyACM0
    async fn test_configure_real_rnode() {
        let radio = RadioParams {
            frequency: 868_000_000,
            bandwidth: 125_000,
            tx_power: 17,
            tx_power_derived: false,
            sf: 7,
            cr: 5,
            st_alock: None,
            lt_alock: None,
        };
        let result = configure_rnode("/dev/ttyACM0", &radio).await;

        match result {
            Ok((mut port, detect)) => {
                println!("RNode configured successfully!");
                if let Some((major, minor)) = detect.firmware_version {
                    println!("  Firmware: {major}.{minor}");
                }
                if let Some(platform) = detect.platform {
                    let name = match platform {
                        rnode::PLATFORM_ESP32 => "ESP32",
                        rnode::PLATFORM_NRF52 => "nRF52",
                        rnode::PLATFORM_AVR => "AVR",
                        _ => "Unknown",
                    };
                    println!("  Platform: {name} (0x{platform:02X})");
                }
                if let Some(mcu) = detect.mcu {
                    println!("  MCU: 0x{mcu:02X}");
                }
                // Turn radio off and send leave
                send_goodbye(&mut port, "test_rnode").await;
                println!("  Radio off, leave sent");
            }
            Err(e) => {
                panic!("Configuration failed: {e}");
            }
        }
    }

    #[tokio::test]
    #[ignore] // Requires RNode hardware at /dev/ttyACM0
    async fn test_rnode_interface_lifecycle() {
        let config = RNodeInterfaceConfig {
            id: InterfaceId(0),
            name: "test_rnode".to_string(),
            port_path: "/dev/ttyACM0".to_string(),
            frequency: 868_000_000,
            bandwidth: 125_000,
            tx_power: 17,
            tx_power_derived: false,
            sf: 7,
            cr: 5,
            st_alock: None,
            lt_alock: None,
            flow_control: true,
            buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
            reconnect_notify: None,
            test_drop_direct_ingress: false,
            jitter_arm: JitterArm::AsIs,
            identity_hash: [0u8; 16],
        };

        let mut handle = spawn_rnode_interface(config);

        // Wait for the interface to come online (~5s for detect + configure)
        println!("Waiting for RNode to come online...");
        tokio::time::sleep(Duration::from_secs(6)).await;

        assert!(
            handle.info.bitrate.is_some(),
            "bitrate should be computed at spawn"
        );
        println!("Bitrate: {} bps", handle.info.bitrate.unwrap());

        // Send a test packet via outgoing channel
        let test_data = b"Hello from Rust RNode test";
        handle
            .outgoing
            .send(OutgoingPacket {
                peer: None,
                data: test_data.to_vec(),
                high_priority: false,
            })
            .await
            .expect("send should succeed");
        println!("Sent test packet ({} bytes)", test_data.len());

        // Brief wait, then drop the handle to trigger shutdown
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Drop outgoing sender to signal shutdown
        drop(handle.outgoing);

        // Read remaining incoming (drain)
        while let Ok(pkt) = handle.incoming.try_recv() {
            println!("Received: {} bytes", pkt.data.len());
        }

        println!("Interface lifecycle test complete");
    }

    /// The seed every io-task test's channel access is built from. Fixed, so
    /// the acquisition wait is an exact number a test can predict by building
    /// a second [`ChannelAccess`] from it, instead of a random tail that
    /// turns a timing assertion into a coin flip.
    const TEST_ACCESS_SEED: u32 = 0x5EED_0347;

    /// The channel-access policy the wall-clock io-task tests hand the loop.
    ///
    /// The modulation is the fastest the reference's slot arithmetic allows
    /// (bitrate above `JITTER_FAST_THRESHOLD_BPS`, so the 6 ms fast floor
    /// binds): an acquisition then costs the test between 12 and 90 ms of
    /// real time instead of the bench PHY's 48..360 ms. These tests are about
    /// framing, gating and counters, not about the size of the wait — the
    /// test that IS about the size of the wait runs on paused time at the
    /// bench PHY.
    fn test_channel_access() -> ChannelAccess {
        let mut access = ChannelAccess::new(TEST_ACCESS_SEED);
        access.set_phy(500_000, 5, 5);
        access
    }

    /// The hold is the frame's own airtime plus the contest the firmware
    /// runs before the next frame, and every term comes from the running PHY
    /// — none of it is a constant chosen for one modulation.
    ///
    /// The corpus PHYs are checked at one frame length so the numbers are
    /// comparable. What separates them is the airtime, which spans a factor
    /// of 16 across the corpus (167 ms at SF7/250 to 2673 ms at SF10/125 for
    /// an announce-sized frame), and at SF10 the slot as well: 12 symbols are
    /// 98 ms there against the 24 ms floor every SF7 cell clamps up to. A
    /// hold built on the fixed 24 ms slot `rnode::compute_spacing_ms`
    /// assumes would under-price SF10's contest by a second.
    #[test]
    fn the_hold_is_the_frames_airtime_plus_the_firmwares_contest() {
        // One announce-sized frame, the population the corpus cells put on
        // the air most (`rnode::ANNOUNCE_CAP_REFERENCE_BYTES`).
        let len = rnode::ANNOUNCE_CAP_REFERENCE_BYTES as u32;
        let none = FirmwareCsma::default();

        for (label, bw, sf, cr) in [
            ("lora_ratchet_rotation SF7/62.5", 62_500u32, 7u8, 5u8),
            ("bench_single_pair_medium SF7/125", 125_000, 7, 5),
            ("bench_single_pair_fast SF7/250", 250_000, 7, 5),
            ("bench_single_pair_slow SF10/125", 125_000, 10, 8),
        ] {
            let hold = tx_hold(len, bw, sf, cr, &none);
            let slot = jitter_slot_ms(bw, sf, cr);
            println!(
                "TX_HOLD phy={label} len={len} held_ms={} airtime_ms={} difs_ms={} \
                 cw_ms={} slot_ms={slot}",
                hold.held_ms, hold.airtime_ms, hold.difs_ms, hold.cw_ms
            );
            assert_eq!(
                hold.held_ms,
                hold.airtime_ms + hold.difs_ms + hold.cw_ms,
                "{label}: the hold is the sum of its three terms"
            );
            assert_eq!(
                hold.difs_ms,
                JITTER_DIFS_SLOTS * slot,
                "{label}: DIFS is two slots of THIS modulation"
            );
            assert_eq!(
                hold.cw_ms,
                (JITTER_CW_SLOTS as u64 - 1) * slot,
                "{label}: the contention term is the widest draw of THIS \
                 modulation"
            );
            assert!(
                hold.airtime_ms > rnode::airtime_ms_with_preamble(len, bw, sf, cr, 8),
                "{label}: the airtime must charge the preamble the firmware \
                 derives, not the modem default of 8 symbols"
            );
        }
    }

    /// The figure this interface reports as its per-frame turnaround
    /// (`Interface::frame_turnaround_ms`): the hold a FULL-SIZE frame imposes
    /// at the running PHY.
    ///
    /// It has to bound the hold of every frame LENGTH the interface may be
    /// handed, because the receiver of a resource sizes a timeout with it and
    /// a figure below the real cost is the whole of Codeberg #36/#374. At the
    /// SF7/BW62.5 carrier `lora_window_ab_pythonlike` ran on (2026-09-23)
    /// that means clearing the 1866 ms a 491 B part cost there.
    ///
    /// It does NOT bound every contention BAND: the figure is derived at
    /// spawn, before the modem has sent a `CMD_STAT_CSMA`, so it prices the
    /// band-1 window this interface's own policy draws from (cw 312 ms at
    /// this PHY). The same part in the firmware's band 3 costs 2586 ms. The
    /// handle reports a fixed number and a reported band widens the real
    /// hold underneath it; carrying the live band out to the handle is
    /// follow-on work, not a hole this test papers over.
    #[test]
    fn the_reported_turnaround_bounds_every_frames_hold() {
        for (label, bw, sf, cr) in [
            ("sf7/bw62.5 (the run 184 traced)", 62_500u32, 7u8, 5u8),
            ("sf8/bw125 (the project default PHY)", 125_000, 8, 5),
            ("sf12/bw125 (the slowest)", 125_000, 12, 5),
        ] {
            let reported = max_tx_hold(bw, sf, cr).held_ms;
            for len in [1u32, 100, 491, rnode::HW_MTU as u32] {
                let hold = tx_hold(len, bw, sf, cr, &FirmwareCsma::default()).held_ms;
                assert!(
                    hold <= reported,
                    "{label}: a {len} B frame holds for {hold} ms, above the \
                     {reported} ms reported"
                );
            }
        }
        let part_of_the_run = tx_hold(491, 62_500, 7, 5, &FirmwareCsma::default());
        assert_eq!(
            (
                part_of_the_run.airtime_ms,
                part_of_the_run.difs_ms,
                part_of_the_run.cw_ms
            ),
            (1_506, 48, 312),
            "the terms the night run of 2026-09-23 was priced with"
        );
        assert!(
            max_tx_hold(62_500, 7, 5).held_ms >= part_of_the_run.held_ms,
            "the reported turnaround must cover the 1866 ms one 491 B part \
             cost on that run, got {}",
            max_tx_hold(62_500, 7, 5).held_ms
        );
    }

    /// Where the modem has reported its own CSMA figures, they are what the
    /// hold is priced from — ours are the fallback, not the authority.
    #[test]
    fn a_reported_contention_window_overrides_the_derived_one() {
        let len = 100;
        let (bw, sf, cr) = (62_500u32, 7u8, 5u8);
        let derived = tx_hold(len, bw, sf, cr, &FirmwareCsma::default());

        // What a band-3 firmware reports: `cw_min = 30, cw_max = 44`
        // (`RNode_Firmware.ino:1616-1617`), and a slot of its own.
        let reported = FirmwareCsma {
            slot_ms: Some(30),
            difs_ms: Some(60),
            cw_max: Some(45),
        };
        let hold = tx_hold(len, bw, sf, cr, &reported);
        assert_eq!(hold.difs_ms, 60, "the reported DIFS is used as reported");
        assert_eq!(
            hold.cw_ms,
            44 * 30,
            "`random(cw_min, cw_max)` is upper-exclusive, so the longest draw \
             is cw_max - 1 slots of the reported slot time"
        );
        assert_eq!(hold.airtime_ms, derived.airtime_ms, "the PHY is unchanged");
        assert!(
            hold.held_ms > derived.held_ms,
            "a wider reported window must widen the hold: {} vs {}",
            hold.held_ms,
            derived.held_ms
        );
    }

    /// The frame's cost in the units the modem reports its ledger in, and the
    /// floor that keeps an accusation from being vacuous.
    ///
    /// One raw unit is 1.5 ms of airtime (10000 units across the firmware's
    /// 15 s two-bin window), so an announce-sized frame at the bench PHY is a
    /// three-digit hole and nothing about the reading is marginal. Rounded UP
    /// so that every frame whose airtime is computable expects at least one
    /// unit: an expectation of zero would make "the ledger did not move" a
    /// statement about nothing, and [`judge_airtime`] declines to judge it.
    #[test]
    fn a_frames_cost_in_ledger_units_is_never_a_vacuous_zero() {
        // The bench PHY, for the frame length the rig measurement carried,
        // plus the header byte the firmware charges.
        let airtime_ms = rnode::airtime_ms_with_preamble(
            148,
            62_500,
            7,
            5,
            rnode::derive_preamble_symbols(7, 5, 62_500),
        );
        let raw = expected_airtime_raw(airtime_ms);
        assert_eq!(
            raw as u64,
            (airtime_ms * CHTM_FULL_SCALE).div_ceil(AIRTIME_WINDOW_MS),
            "the expectation is the frame's airtime in CHTM units and nothing \
             else"
        );
        assert!(
            raw > 100,
            "a 148-byte frame at SF7/62.5 kHz costs {airtime_ms} ms, which is \
             hundreds of raw units, not a marginal signal: got {raw}"
        );
        assert_eq!(
            expected_airtime_raw(1),
            1,
            "a frame too short to fill one unit still expects one"
        );
        assert_eq!(
            expected_airtime_raw(0),
            0,
            "an uncomputable airtime expects nothing, and is not judged"
        );
    }

    /// The account the judge is given for a frame that cost 3.36 % of the
    /// modem's short window on an idle medium: the shape both rig
    /// measurements produced, before any single field is varied.
    fn idle_account() -> AirtimeAccount {
        AirtimeAccount {
            baseline_short: 1_000,
            observed_short: 1_000,
            baseline_long: 50,
            observed_long: 50,
            observed_load_short: 1_000,
            expected_short: 336,
            hold_ms: 900,
            lock: AirtimeLock::default(),
        }
    }

    /// An unmoved ledger is an accusation ONLY when nothing else can explain
    /// it. Every ambiguity has to resolve away from the accusation, because a
    /// false `LORA_TX_UNACCOUNTED` would poison the instrument it exists to
    /// be — this is the table of what must NOT produce one.
    #[test]
    fn every_ambiguity_resolves_away_from_the_accusation() {
        assert_eq!(
            judge_airtime(&idle_account()),
            AirtimeVerdict::Unaccounted,
            "the measured shape: the ledger did not move, the medium was idle, \
             no lock was armed"
        );

        // Any rise, not a rise of the expected size. `airtime_bins` is written
        // by `add_airtime` alone, and one frame at a time is in the modem, so
        // a rise in this window can only be this frame — while a rise SMALLER
        // than expected is what a keyed frame looks like when a bin ages out
        // in the same reading.
        let mut one_unit = idle_account();
        one_unit.observed_short += 1;
        assert_eq!(
            judge_airtime(&one_unit),
            AirtimeVerdict::Keyed,
            "one raw unit of rise is proof the frame reached `add_airtime`"
        );

        // Measured on t-beam-1, 2026-09-24: airtime_short went 17.92 -> 16.40
        // across a frame that DID key, because a 7500 ms bin aged out of the
        // two-bin window in the same reading.
        let mut fell = idle_account();
        fell.observed_short -= 480;
        assert_eq!(
            judge_airtime(&fell),
            AirtimeVerdict::Undecided("bin_rotation"),
            "a falling ledger is a rotation, never an accusation"
        );

        // The independent receipt: `longterm_airtime` sums all bins over an
        // hour, so it does not fall when the short window rotates. An exact
        // cancellation in the short window is the one residual false-positive
        // shape, and this is what separates it.
        let mut long_rose = idle_account();
        long_rose.observed_long += 1;
        assert_eq!(
            judge_airtime(&long_rose),
            AirtimeVerdict::Undecided("longterm_rose"),
            "the hour-long ledger rising says the frame keyed even when the \
             15 s one has not moved"
        );

        // A medium that could have been busy for the whole hold: the firmware
        // does not start its DIFS wait until the medium is free, so the frame
        // may simply still be queued. `channel_load_short` is the DCD busy
        // fraction plus the modem's own airtime, over 7500 ms.
        let mut busy = idle_account();
        busy.observed_load_short = 1_000 + (900 * CHTM_FULL_SCALE / DCD_WINDOW_MS) as u16;
        assert_eq!(
            judge_airtime(&busy),
            AirtimeVerdict::Undecided("medium_busy"),
            "busy for at least the hold is a frame still legitimately waiting"
        );
        let mut nearly_busy = idle_account();
        nearly_busy.observed_load_short = 1_000 + (880 * CHTM_FULL_SCALE / DCD_WINDOW_MS) as u16;
        assert_eq!(
            judge_airtime(&nearly_busy),
            AirtimeVerdict::Unaccounted,
            "busy for LESS than the hold means the medium was demonstrably \
             free part of it, and the frame should have gone"
        );
        let mut clamped = idle_account();
        clamped.observed_load_short = CHTM_FULL_SCALE as u16;
        assert_eq!(
            judge_airtime(&clamped),
            AirtimeVerdict::Undecided("medium_busy"),
            "`total_channel_util` is clamped at 1.0, so at full scale the DCD \
             fraction underneath is unrecoverable"
        );

        // An armed airtime lock defers rather than discards and signals
        // nothing over KISS; the host can only infer it from the limit it
        // configured itself.
        let mut st_locked = idle_account();
        st_locked.lock = AirtimeLock::from_config(Some(1_000), None);
        assert_eq!(
            judge_airtime(&st_locked),
            AirtimeVerdict::Undecided("st_airtime_lock"),
            "at or above the short-term limit the firmware holds the queue"
        );
        let mut lt_locked = idle_account();
        lt_locked.lock = AirtimeLock::from_config(None, Some(50));
        assert_eq!(
            judge_airtime(&lt_locked),
            AirtimeVerdict::Undecided("lt_airtime_lock"),
            "same for the long-term limit"
        );
        let mut under_lock = idle_account();
        under_lock.lock = AirtimeLock::from_config(Some(1_001), Some(51));
        assert_eq!(
            judge_airtime(&under_lock),
            AirtimeVerdict::Unaccounted,
            "a limit the reported airtime has not reached explains nothing"
        );

        // A limit at or above full scale is discarded by the firmware itself,
        // so it must not silence the host either.
        assert_eq!(
            AirtimeLock::from_config(Some(CHTM_FULL_SCALE as u16), Some(0)),
            AirtimeLock { st: 0, lt: 0 },
            "the firmware zeroes a limit of 1.0 or more; so do we"
        );

        let mut uncomputable = idle_account();
        uncomputable.expected_short = 0;
        assert_eq!(
            judge_airtime(&uncomputable),
            AirtimeVerdict::Undecided("airtime_not_computable"),
            "with no expectation there is nothing to be missing"
        );
    }

    /// One pending frame, built as the handover site builds it.
    fn pending(payload_len: u64, is_rehand: bool, due_in: Duration) -> PendingHandover {
        let due = tokio::time::Instant::now() + due_in;
        PendingHandover {
            data: rnode::build_data_frame(&vec![0xAA; payload_len as usize]),
            payload_len,
            high_priority: false,
            is_rehand,
            handed_unix_ms: unix_ms(),
            due,
            deadline: due + CHTM_PERIOD,
            account: AirtimeAccount {
                expected_short: 336,
                hold_ms: 900,
                ..idle_account()
            },
        }
    }

    /// A CHTM that reports the idle account's own ledger back, unmoved.
    fn frozen_chtm() -> rnode::ChannelStats {
        rnode::ChannelStats {
            airtime_short: 1_000,
            airtime_long: 50,
            channel_load_short: 1_000,
            channel_load_long: 50,
            current_rssi: None,
            noise_floor: None,
            interference: None,
        }
    }

    /// The settlement rules the io task depends on: nothing is said before the
    /// frame's own hold has elapsed, the first verdict past it buys one
    /// re-hand, and the re-hand's own verdict buys a counted drop instead of a
    /// second retry.
    #[tokio::test]
    async fn a_silent_consume_buys_one_re_hand_and_then_a_counted_drop() {
        let counters = InterfaceCounters::new();
        let mut pendings: VecDeque<PendingHandover> = VecDeque::new();

        // Before the hold elapses, an unmoved ledger says nothing: the modem
        // may simply not have keyed the frame YET.
        pendings.push_back(pending(168, false, Duration::from_secs(30)));
        let early = settle_handovers(
            "t",
            &counters,
            &mut pendings,
            &frozen_chtm(),
            tokio::time::Instant::now(),
            unix_ms(),
        );
        assert!(
            early.is_none(),
            "no verdict before the frame's hold elapses"
        );
        assert_eq!(
            pendings.len(),
            1,
            "and the frame stays pending, to be judged by a later reading"
        );
        assert_eq!(
            counters
                .tx_unaccounted
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        // Past the hold, the same reading is the accusation — and it returns
        // the frame to re-hand, marked so its own handover cannot ask for a
        // third.
        pendings.clear();
        pendings.push_back(pending(168, false, Duration::from_millis(0)));
        let first = settle_handovers(
            "t",
            &counters,
            &mut pendings,
            &frozen_chtm(),
            tokio::time::Instant::now() + Duration::from_millis(1),
            unix_ms(),
        );
        let frame = first.expect("the frame must come back to be re-handed");
        assert!(frame.rehand, "the retry must be marked as one");
        assert_eq!(frame.payload_len, 168);
        assert_eq!(
            counters
                .tx_unaccounted
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a frame that is being retried is not yet a lost frame"
        );

        // The retry's own verdict: counted, named, and not retried again.
        pendings.clear();
        pendings.push_back(pending(168, true, Duration::from_millis(0)));
        let second = settle_handovers(
            "t",
            &counters,
            &mut pendings,
            &frozen_chtm(),
            tokio::time::Instant::now() + Duration::from_millis(1),
            unix_ms(),
        );
        assert!(second.is_none(), "one re-hand, not two");
        assert_eq!(
            counters
                .tx_unaccounted
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        assert_eq!(
            counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the frame is lost now, and counted the way the error_txfailed \
             path counts the frames it abandons"
        );
        assert_eq!(
            counters
                .tx_dropped_bytes
                .load(std::sync::atomic::Ordering::Relaxed),
            168
        );

        // A reading that arrives past the deadline is about a window that may
        // have rotated: the frame is dropped unjudged rather than accused.
        pendings.clear();
        pendings.push_back(pending(168, false, Duration::from_millis(0)));
        let late = settle_handovers(
            "t",
            &counters,
            &mut pendings,
            &frozen_chtm(),
            tokio::time::Instant::now() + CHTM_PERIOD + Duration::from_millis(50),
            unix_ms(),
        );
        assert!(late.is_none(), "a late reading accuses nobody");
        assert!(pendings.is_empty(), "and does not keep the frame either");
        assert_eq!(
            counters
                .tx_unaccounted
                .load(std::sync::atomic::Ordering::Relaxed),
            2,
            "the counter must not move on a reading that decided nothing"
        );
    }

    /// A PHY whose airtime cannot be computed must still leave the serial
    /// floor standing: a hold of zero would hand the modem a whole burst at
    /// 115200 baud, which is the defect this hold exists to prevent.
    #[test]
    fn an_uncomputable_phy_still_holds_the_serial_floor() {
        let hold = tx_hold(100, 0, 7, 5, &FirmwareCsma::default());
        assert_eq!(hold.airtime_ms, 0, "airtime is not computable at bw 0");
        assert!(
            hold.held_ms >= rnode::MIN_SPACING_MS,
            "the serial floor must bind, got {}ms",
            hold.held_ms
        );
    }

    /// A directed packet waits for the channel like any other, and a burst
    /// behind it waits for nothing (Codeberg #347).
    ///
    /// The interface used to ask what a packet was: a `high_priority` frame
    /// at the front of an idle queue — every proof, link request and data
    /// packet — skipped the randomised pre-TX wait outright, and only
    /// announces were ever jittered. That is type-awareness in
    /// collision-avoidance logic, which the medium's own policy has no room
    /// for: two senders released by the same event key up together whatever
    /// their frames contain.
    ///
    /// What the wait must cost is asserted exactly, not bounded, because
    /// both halves of the claim are numbers:
    ///
    /// * acquisition — the first directed frame leaves at exactly the draw
    ///   [`ChannelAccess`] makes for this seed at the bench PHY;
    /// * burst continuation — the two frames queued behind it owe no draw of
    ///   their own, and leave one [`tx_hold`] apart, the wait that keeps the
    ///   modem's queue at one frame;
    /// * release — a frame handed over after the queue drained is a new
    ///   acquisition and owes a fresh draw again.
    ///
    /// Paused time, so the draw costs no wall clock and the assertions are
    /// equalities rather than windows.
    #[tokio::test(start_paused = true)]
    async fn a_directed_packet_is_jittered_on_acquisition_and_free_in_a_burst() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        // The oracle: the same policy, from the same seed, driven through the
        // same calls the io task makes. Its draws are the io task's draws.
        let mut oracle = ChannelAccess::new(TEST_ACCESS_SEED);
        oracle.set_phy(125_000, 7, 5);
        let first_wait = oracle.acquisition_jitter_ms();
        assert!(
            first_wait >= JITTER_DIFS_SLOTS * oracle.jitter_slot(),
            "the bench PHY must owe at least DIFS, got {first_wait}ms"
        );

        let mut task_access = ChannelAccess::new(TEST_ACCESS_SEED);
        task_access.set_phy(125_000, 7, 5);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_acq".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                counters,
                /* flow_control = */ false,
                task_access,
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        // Three directed packets at once: a link request and the data behind
        // it, the shape the bypass was written for.
        let start = tokio::time::Instant::now();
        for payload in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            outgoing_tx
                .send(OutgoingPacket {
                    peer: None,
                    data: payload.to_vec(),
                    high_priority: true,
                })
                .await
                .expect("send to io task");
        }

        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut at = Vec::new();
        let mut payloads = Vec::new();
        let mut buf = [0u8; 256];
        while payloads.len() < 3 {
            let n = tokio::time::timeout(Duration::from_secs(30), peer.read(&mut buf))
                .await
                .expect("the io task must drain the queue")
                .expect("read from duplex");
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    if command == rnode::CMD_DATA {
                        at.push(start.elapsed().as_millis() as u64);
                        payloads.push(payload);
                    }
                }
            }
        }
        assert_eq!(
            payloads,
            vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()],
            "order must be untouched by the change"
        );

        assert_eq!(
            at[0], first_wait,
            "a directed frame acquiring an idle channel must serve the wait \
             the policy drew ({first_wait}ms), not skip it because of what it \
             carries"
        );
        // No draw of their own — but each one waits out the frame before it,
        // at that frame's own length. Derived from the same PHY the io task
        // runs, never typed.
        // "one" and "two" are the frames whose airtime the second and third
        // handover wait out; both are 3 bytes, so one figure covers both.
        let hold = tx_hold(3, 125_000, 7, 5, &FirmwareCsma::default()).held_ms;
        assert_eq!(
            at[1] - at[0],
            hold,
            "the second frame of the burst owes no draw, but must not reach \
             the modem before the first has left the air"
        );
        assert_eq!(
            at[2] - at[1],
            hold,
            "the third frame of the burst waits out the second the same way"
        );

        // The queue has drained: the hold after the last frame finds
        // nothing to send and hands the channel back. What comes after is a
        // new acquisition, and the policy draws for it again — the oracle
        // walks the same three calls the io task made.
        oracle.jitter_spent(first_wait);
        oracle.channel_released();
        let second_wait = oracle.acquisition_jitter_ms();

        tokio::time::sleep(Duration::from_secs(1)).await;
        let released = tokio::time::Instant::now();
        outgoing_tx
            .send(OutgoingPacket {
                peer: None,
                data: b"four".to_vec(),
                high_priority: true,
            })
            .await
            .expect("send to io task");
        let mut fourth = None;
        while fourth.is_none() {
            let n = tokio::time::timeout(Duration::from_secs(30), peer.read(&mut buf))
                .await
                .expect("the io task must send the fourth frame")
                .expect("read from duplex");
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    if command == rnode::CMD_DATA {
                        fourth = Some((released.elapsed().as_millis() as u64, payload));
                    }
                }
            }
        }
        let (fourth_at, fourth_payload) = fourth.expect("fourth frame");
        assert_eq!(fourth_payload, b"four".to_vec());
        assert_eq!(
            fourth_at, second_wait,
            "a frame arriving after the channel was released owes a fresh draw"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }

    /// A frame that jumps a wait already running serves that wait, and the
    /// frame it jumped follows it onto the air at serial spacing (Codeberg
    /// #347).
    ///
    /// This is the shape `lora_lncp_link_retry` was RED in on 2026-09-17
    /// 02:50 (bench log `hw-vollauf4-0cfc8255.log`). alpha's announce
    /// rebroadcast had armed a pre-TX wait; the third link request — the one
    /// the cell's two proxy drops leave to carry the handshake — arrived
    /// while that wait was still running, was priority-inserted at the head
    /// of the queue, and left 511 ms later. The announce it had jumped
    /// followed it to the modem 51 ms behind, so alpha keyed its radio a
    /// second time in the window its peer's link proof was due in, and
    /// neither frame survived: of 21 frames on the air in that cell, those
    /// two were the only losses.
    ///
    /// Two properties are pinned, both of them load-bearing:
    ///
    /// * the jumper does NOT get a bypass. It rides out the wait that was
    ///   already armed. Restoring a bypass here — "a link request should not
    ///   wait for an announce's jitter" — is the type-awareness #347
    ///   removed, and it puts the jumper on the air phase-locked to whatever
    ///   the peer is about to send.
    /// * the jumped frame follows only once the jumper has left the air
    ///   ([`tx_hold`]). Until 2026-09-23 it followed at the serial floor,
    ///   which let one acquisition hand the modem a burst — that is why a
    ///   burst was deaf to the answer to its own first frame, and it is the
    ///   defect this hold closes.
    ///
    /// Paused time and the fixed seed, so both figures are equalities.
    #[tokio::test(start_paused = true)]
    async fn a_frame_that_jumps_a_pending_wait_does_not_skip_it() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let mut oracle = ChannelAccess::new(TEST_ACCESS_SEED);
        oracle.set_phy(125_000, 7, 5);
        let wait = oracle.acquisition_jitter_ms();
        assert!(
            wait >= JITTER_DIFS_SLOTS * oracle.jitter_slot(),
            "the bench PHY must owe at least DIFS, got {wait}ms"
        );

        let mut task_access = ChannelAccess::new(TEST_ACCESS_SEED);
        task_access.set_phy(125_000, 7, 5);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_jump".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                counters,
                /* flow_control = */ false,
                task_access,
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        // The announce acquires the idle channel and arms the wait.
        let start = tokio::time::Instant::now();
        outgoing_tx
            .send(OutgoingPacket {
                peer: None,
                data: b"announce".to_vec(),
                high_priority: false,
            })
            .await
            .expect("send to io task");

        // The link request arrives partway through that wait — the run's
        // ordering, where 1.4 s of a pending wait had elapsed when the
        // handshake's frame came down from the core.
        tokio::time::sleep(Duration::from_millis(wait / 2)).await;
        outgoing_tx
            .send(OutgoingPacket {
                peer: None,
                data: b"linkrequest".to_vec(),
                high_priority: true,
            })
            .await
            .expect("send to io task");

        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut at = Vec::new();
        let mut payloads = Vec::new();
        let mut buf = [0u8; 256];
        while payloads.len() < 2 {
            let n = tokio::time::timeout(Duration::from_secs(30), peer.read(&mut buf))
                .await
                .expect("the io task must drain the queue")
                .expect("read from duplex");
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    if command == rnode::CMD_DATA {
                        at.push(start.elapsed().as_millis() as u64);
                        payloads.push(payload);
                    }
                }
            }
        }

        assert_eq!(
            payloads,
            vec![b"linkrequest".to_vec(), b"announce".to_vec()],
            "priority ordering still decides WHERE in the queue a frame sits"
        );
        assert_eq!(
            at[0], wait,
            "the frame that jumped the queue must serve the wait that was \
             already running ({wait}ms), not key the radio on arrival because \
             of what it carries"
        );
        let hold = tx_hold(
            b"linkrequest".len() as u32,
            125_000,
            7,
            5,
            &FirmwareCsma::default(),
        )
        .held_ms;
        assert_eq!(
            at[1] - at[0],
            hold,
            "the jumped frame follows inside the same acquisition, owing no \
             draw of its own — but not before the frame that jumped it has \
             left the air"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }

    /// Two peers released by the same event must not key their radios
    /// together (Codeberg #347).
    ///
    /// This is the shape `lora_ratchet_basic` was RED in on 2026-09-17
    /// 14:16 (bench log `hw-vollauf4-660b41b4.log`). The selftest's ratchet
    /// phase hands one packet to each end of the pair and sleeps 200 ms, ten
    /// times over, so both ends' frames reach their modems inside the same
    /// millisecond. The interface then still held a bypass — a
    /// `high_priority` frame at the head of an idle queue keyed the radio on
    /// arrival — so neither end drew anything, and the two stayed locked for
    /// all ten rounds:
    ///
    /// ```text
    /// alpha 14:16:53.107046  beta 14:16:53.107050
    /// alpha 14:16:53.308862  beta 14:16:53.308861
    /// ...   ten pairs, median gap 0.12 ms, 20 frames handed to the radios
    /// ```
    ///
    /// Of those 20 frames, 0 were received. The same cell nine hours
    /// earlier had its frames parked behind an announce's pending wait,
    /// left as two bursts 1.7 s apart at a median gap of 59.6 ms, and
    /// delivered 20 of 20. Across the three ratchet cells of that run the
    /// separation is the whole distribution: phase-locked 27 of 60
    /// delivered, de-phased 59 of 60.
    ///
    /// What is pinned here is the property the bypass destroyed, and it is
    /// a property of the PAIR, which no single-interface test can see:
    ///
    /// * each end serves its own policy's draw, so the two frames reach the
    ///   radios at least one jitter slot apart;
    /// * the draws are per-interface entropy, not a constant. A constant
    ///   seed leaves every assertion above passing — both ends still "wait"
    ///   — while putting every node in the mesh on the same draw, which is
    ///   the 2026-09-17 air again by another route.
    ///
    /// ONE SLOT IS NOT A CLEARANCE, and the first bullet must not be read as
    /// one. It says the two ends do not key TOGETHER; whether the second
    /// frame is clear of the first is a question about the PHY, which this
    /// test does not hold. At the corpus's own bench PHY (BW250/SF7/CR5) a
    /// slot is 24 ms and a 115-byte frame holds the channel 118 ms, so a
    /// one-slot separation leaves the pair overlapping for four fifths of
    /// its airtime — and 106 of the 196 equally likely draw pairs overlap at
    /// all. That is measured air, not arithmetic: on 2026-09-22
    /// `bench_dual_pair_fast_rnode_only` lost both its reds to one such
    /// pair, two proofs keyed 4.2 ms apart and neither received
    /// (bench log `hw-vollauf4-b74027aa.log`, VERDICT line 722). The figures and the count are
    /// pinned in
    /// `tests/mvr/two_responders_overlap_inside_one_airtime.rs`.
    ///
    /// The cell's own PHY and paused time, so the figures are equalities.
    #[tokio::test(start_paused = true)]
    async fn two_peers_released_by_the_same_event_do_not_key_together() {
        // The ratchet cells' radio block: 12 symbol times at SF7/BW62.5k is
        // a 24 ms slot, DIFS is two of them, the draw adds 0..=13 more.
        const BW: u32 = 62_500;
        const SF: u8 = 7;
        const CR: u8 = 5;
        let slot = jitter_slot_ms(BW, SF, CR);

        // Two interfaces are two radios, and two radios are two seeds.
        const SEED_A: u32 = 0x5EED_0347;
        const SEED_B: u32 = 0x5EED_0408;
        let mut oracle_a = ChannelAccess::new(SEED_A);
        oracle_a.set_phy(BW, SF, CR);
        let mut oracle_b = ChannelAccess::new(SEED_B);
        oracle_b.set_phy(BW, SF, CR);
        let wait_a = oracle_a.acquisition_jitter_ms();
        let wait_b = oracle_b.acquisition_jitter_ms();
        assert!(
            wait_a >= JITTER_DIFS_SLOTS * slot && wait_b >= JITTER_DIFS_SLOTS * slot,
            "both ends must owe at least DIFS on this PHY, got {wait_a}ms and {wait_b}ms"
        );

        let (port_a, mut peer_a) = tokio::io::duplex(8192);
        let (port_b, mut peer_b) = tokio::io::duplex(8192);
        let (incoming_tx_a, _incoming_rx_a) = mpsc::channel::<IncomingPacket>(16);
        let (incoming_tx_b, _incoming_rx_b) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx_a, outgoing_rx_a) = mpsc::channel::<OutgoingPacket>(16);
        let (outgoing_tx_b, outgoing_rx_b) = mpsc::channel::<OutgoingPacket>(16);

        let mut access_a = ChannelAccess::new(SEED_A);
        access_a.set_phy(BW, SF, CR);
        let counters_a = Arc::new(InterfaceCounters::new());
        let task_a = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_peer_a".to_string(),
                port_a,
                incoming_tx_a,
                outgoing_rx_a,
                counters_a,
                /* flow_control = */ false,
                access_a,
                BW,
                SF,
                CR,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        let mut access_b = ChannelAccess::new(SEED_B);
        access_b.set_phy(BW, SF, CR);
        let counters_b = Arc::new(InterfaceCounters::new());
        let task_b = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_peer_b".to_string(),
                port_b,
                incoming_tx_b,
                outgoing_rx_b,
                counters_b,
                /* flow_control = */ false,
                access_b,
                BW,
                SF,
                CR,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        /// When the peer end of a modem's serial line sees its first data
        /// frame, in ms since `start`.
        async fn keyed_at<S>(peer: &mut S, start: tokio::time::Instant) -> u64
        where
            S: tokio::io::AsyncRead + Unpin,
        {
            let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
            let mut buf = [0u8; 256];
            loop {
                let n = tokio::time::timeout(Duration::from_secs(30), peer.read(&mut buf))
                    .await
                    .expect("the io task must hand its frame to the modem")
                    .expect("read from duplex");
                for f in deframer.process(&buf[..n]) {
                    if let KissDeframeResult::Frame { command, .. } = f {
                        if command == rnode::CMD_DATA {
                            return start.elapsed().as_millis() as u64;
                        }
                    }
                }
            }
        }

        // The released-by-the-same-event moment: one directed packet down
        // each end, nothing between them. On the air this was the selftest's
        // `send_single_msg(ep_a)` / `send_single_msg(ep_b)` pair.
        let start = tokio::time::Instant::now();
        let directed = || OutgoingPacket {
            peer: None,
            data: b"ratchet".to_vec(),
            high_priority: true,
        };
        outgoing_tx_a
            .send(directed())
            .await
            .expect("send to peer a");
        outgoing_tx_b
            .send(directed())
            .await
            .expect("send to peer b");

        let (at_a, at_b) = tokio::join!(keyed_at(&mut peer_a, start), keyed_at(&mut peer_b, start));

        assert_eq!(
            at_a, wait_a,
            "peer a keyed at {at_a}ms, not the {wait_a}ms its own policy drew"
        );
        assert_eq!(
            at_b, wait_b,
            "peer b keyed at {at_b}ms, not the {wait_b}ms its own policy drew"
        );
        assert!(
            at_a.abs_diff(at_b) >= slot,
            "two peers released by the same event reached their radios \
             {}ms apart, inside one {slot}ms slot — that is the phase lock \
             the draw exists to break, and on 2026-09-17 it cost every \
             frame of the exchange",
            at_a.abs_diff(at_b)
        );

        // The separation above must be a property of production, not of two
        // seeds chosen to differ: `channel_access_for` draws each
        // interface's seed from the host's entropy. Thirty-two instances
        // landing on one draw has probability 14^-31 — below anything this
        // suite can observe — so a failure here means the seed stopped
        // being entropy, not that the dice were unkind.
        let draws: std::collections::BTreeSet<u64> = (0..32)
            .map(|_| channel_access_for(BW, SF, CR).acquisition_jitter_ms())
            .collect();
        assert!(
            draws.len() > 1,
            "every interface on this host drew the same wait, {:?}ms: the \
             per-interface seed is no longer entropy, so every node keys \
             together again",
            draws
        );

        drop(outgoing_tx_a);
        drop(outgoing_tx_b);
        let _ = tokio::time::timeout(Duration::from_secs(5), task_a).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), task_b).await;
    }

    // -- the three #347 jitter arms, on the co-release cell's PHY ---------
    //
    // TEMPORARY, with [`JitterArm`]: when an arm wins these three go with
    // the two that lose, and what survives is the winner's arithmetic under
    // the name the policy then has.

    /// The PHY of `lora_co_release_announce_relay`, which is the cell the
    /// A/B is taken on and the PHY 124 priced all three arms at. Stated as
    /// the radio block states it; every figure below is DERIVED from these
    /// three numbers by the same functions the interface calls, so a change
    /// to the slot derivation or to the preamble moves the test and the
    /// interface together instead of leaving a typed millisecond behind.
    const CO_RELEASE_BW: u32 = 250_000;
    const CO_RELEASE_SF: u8 = 7;
    const CO_RELEASE_CR: u8 = 5;

    /// The frame the two far ends answered with on 2026-09-22 — a 115-byte
    /// proof, the frame whose airtime the pair had to clear and did not
    /// (`tests/mvr/two_responders_overlap_inside_one_airtime.rs`).
    const CO_RELEASE_PAYLOAD: usize = 115;

    /// What one such frame holds the channel for, from the interface's own
    /// airtime function and its own preamble derivation.
    fn co_release_frame_air_ms() -> u64 {
        rnode::airtime_ms_with_preamble(
            CO_RELEASE_PAYLOAD as u32,
            CO_RELEASE_BW,
            CO_RELEASE_SF,
            CO_RELEASE_CR,
            rnode::derive_preamble_symbols(CO_RELEASE_SF, CO_RELEASE_CR, CO_RELEASE_BW),
        )
    }

    /// A policy at the co-release PHY, seeded so the draw sequence is the
    /// test's and not the host's entropy.
    fn co_release_access(seed: u32) -> ChannelAccess {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(CO_RELEASE_BW, CO_RELEASE_SF, CO_RELEASE_CR);
        access
    }

    /// Every wait `arm` can impose at this PHY on an interface of
    /// `class`, as a multiple of `unit`, collected over enough acquisitions
    /// that the whole draw window is reachable. Each iteration releases the
    /// channel first, so each is a fresh acquisition and not the remainder
    /// of one.
    fn arm_window(arm: JitterArm, unit: u64, class: FrameClass) -> std::collections::BTreeSet<u64> {
        const ACQUISITIONS: usize = 2000;
        let mut access = co_release_access(0x5EED_0347);
        let mut seen = std::collections::BTreeSet::new();
        // Under the whole-frame arms the class's sub-frame position is a
        // fixed offset inside the count, so the count a wait came from is the
        // wait with that offset put back. Putting it back here rather than
        // widening the window keeps this helper about the COUNT window, which
        // is the thing 124 priced and the thing the position must not move.
        let offset = match arm {
            JitterArm::FrameSlot | JitterArm::FrameSlotShort => class.sub_frame_offset_ms(unit),
            JitterArm::AsIs | JitterArm::ModemOnly => 0,
        };
        for _ in 0..ACQUISITIONS {
            access.channel_released();
            let owed = arm_owed_jitter_ms(
                arm,
                &mut access,
                CO_RELEASE_PAYLOAD,
                CO_RELEASE_BW,
                CO_RELEASE_SF,
                CO_RELEASE_CR,
                class,
            );
            let counted = owed + offset;
            assert_eq!(
                counted % unit,
                0,
                "a wait of {owed}ms is not a whole number of {unit}ms units, \
                 offset by the {offset}ms this class takes off its count"
            );
            seen.insert(counted / unit);
        }
        seen
    }

    /// Every class an interface can carry: the two residue classes times the
    /// four sub-frame positions. What a property has to hold over, whenever
    /// it is a property of the arm rather than of one class.
    fn every_class() -> impl Iterator<Item = FrameClass> {
        (0..2).flat_map(|parity| (0..SUB_FRAME_POSITIONS).map(move |p| FrameClass::new(parity, p)))
    }

    /// The draw window every arm shares: DIFS plus a uniform draw over
    /// `JITTER_CW_SLOTS`, so 2..=15 units of whatever the arm's unit is
    /// (reference Config.h:102/108-111, and 124's table is priced on it).
    ///
    /// The DRAW is this in all four arms. What arm 4 narrows is the span the
    /// draw is folded onto, not the draw ([`frame_span`]).
    fn expected_window() -> std::collections::BTreeSet<u64> {
        (JITTER_DIFS_SLOTS..JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64).collect()
    }

    /// The whole-frame arms with the top count each one draws in: the axis
    /// every class property below is parametrised over.
    ///
    /// Arm 4 is arm 3's rule over a shorter span, so every guarantee the class
    /// buys has to hold for both. A test that pinned arm 3 alone would let arm
    /// 4 reach the rig with its fold, its DIFS floor or its ceiling broken,
    /// and an A/B against a broken arm measures nothing.
    fn frame_arms() -> impl Iterator<Item = (JitterArm, u64)> {
        [JitterArm::FrameSlot, JitterArm::FrameSlotShort]
            .into_iter()
            .filter_map(|arm| arm.frame_ceiling().map(|ceiling| (arm, ceiling)))
    }

    /// The counts one whole-frame arm's span holds: `DIFS..=ceiling`, so
    /// 2..=15 under arm 3 (the draw window itself) and 2..=8 under arm 4.
    fn frame_span(ceiling: u64) -> std::collections::BTreeSet<u64> {
        (JITTER_DIFS_SLOTS..=ceiling).collect()
    }

    /// ARM 1 — the policy as it stands, unchanged by the selector.
    ///
    /// The arm the selector defaults to must be `acquisition_jitter_ms` and
    /// nothing else: if the default arm drifted, every run that did not name
    /// an arm — which is every run in the corpus and every document already
    /// on disk, read back as arm 1 — would be a measurement of something
    /// else under the old name.
    #[test]
    fn arm_one_owes_exactly_what_the_policy_draws() {
        let slot = jitter_slot_ms(CO_RELEASE_BW, CO_RELEASE_SF, CO_RELEASE_CR);

        // The shape of this cell, and the reason the three arms exist: one
        // slot is a fraction of the frame it is meant to separate, so two
        // answers a slot apart still overlap.
        assert!(
            co_release_frame_air_ms() > 4 * slot,
            "the co-release PHY is supposed to be the one where a frame ({}ms) \\
             holds the channel for several slots ({slot}ms)",
            co_release_frame_air_ms()
        );

        // Seed by seed, the arm and a bare policy are the same value.
        for i in 0..64u32 {
            let seed = 0x5EED_0347u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
            let mut armed = co_release_access(seed);
            let mut bare = co_release_access(seed);
            assert_eq!(
                arm_owed_jitter_ms(
                    JitterArm::AsIs,
                    &mut armed,
                    CO_RELEASE_PAYLOAD,
                    CO_RELEASE_BW,
                    CO_RELEASE_SF,
                    CO_RELEASE_CR,
                    FrameClass::default(),
                ),
                bare.acquisition_jitter_ms(),
                "arm 1 must be the unmodified policy (seed {seed:#x})"
            );
        }

        // And neither field of the class arm 3 pins its wait to is consulted
        // here: arms 1 and 2 draw in slots, and a slot-priced wait that moved
        // with the interface's identity would be a second variable in the
        // A/B.
        for class in every_class() {
            assert_eq!(
                arm_window(JitterArm::AsIs, slot, class),
                expected_window(),
                "arm 1's wait is DIFS plus 0..=13 slots of {slot}ms, whatever \
                 the class ({class:?})"
            );
        }
    }

    /// ARM 2 — modem CSMA only: the host draws and discharges, and waits for
    /// nothing.
    ///
    /// Two things are pinned, and the second is what makes the A/B a
    /// one-variable comparison: the wait is zero, AND the draw still
    /// happened, so the channel-access RNG stands at the same place it would
    /// under arm 1 and the CAD backoff ladder behind it is the same sequence
    /// in both arms.
    #[test]
    fn arm_two_draws_and_discharges_without_waiting_it_out() {
        for i in 0..64u32 {
            let seed = 0x5EED_0347u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
            let mut modem_only = co_release_access(seed);
            let owed = arm_owed_jitter_ms(
                JitterArm::ModemOnly,
                &mut modem_only,
                CO_RELEASE_PAYLOAD,
                CO_RELEASE_BW,
                CO_RELEASE_SF,
                CO_RELEASE_CR,
                FrameClass::default(),
            );
            assert_eq!(owed, 0, "arm 2 imposes no host wait (seed {seed:#x})");
            assert_eq!(
                modem_only.acquisition_jitter_ms(),
                0,
                "the draw must be discharged, or the next frame of this \\
                 acquisition inherits a wait arm 2 decided not to serve"
            );

            // Same seed, arm 1, one acquisition: after a release both arms
            // must draw the same next value, which they can only do from the
            // same place in the stream.
            let mut as_is = co_release_access(seed);
            let _ = arm_owed_jitter_ms(
                JitterArm::AsIs,
                &mut as_is,
                CO_RELEASE_PAYLOAD,
                CO_RELEASE_BW,
                CO_RELEASE_SF,
                CO_RELEASE_CR,
                FrameClass::default(),
            );
            modem_only.channel_released();
            as_is.channel_released();
            assert_eq!(
                modem_only.acquisition_jitter_ms(),
                as_is.acquisition_jitter_ms(),
                "arm 2 must consume the same randomness as arm 1 (seed {seed:#x}), \\
                 or the two arms differ in two things at once"
            );
        }
    }

    /// ARMS 3 AND 4 — the same draw, in units of the frame instead of the
    /// slot, folded onto a span of 2..15 whole frames (arm 3) or 2..8 (arm 4).
    ///
    /// `slots * max(frame_air, slot)`: the number of units is the policy's own
    /// draw folded onto the arm's span, the unit is the airtime of the frame
    /// about to go, and DIFS scales with it because it is counted in the same
    /// units (124's costing, and the 236..1770 ms it predicts at this PHY
    /// under arm 3).
    ///
    /// Both arms in one cell, because they are one body with one number
    /// different: an assertion that named arm 3 alone would go green on an arm
    /// 4 that had quietly kept arm 3's span, which is the one way this change
    /// can fail and still look finished.
    #[test]
    fn the_whole_frame_arms_floor_the_slot_at_the_frames_own_airtime() {
        let slot = jitter_slot_ms(CO_RELEASE_BW, CO_RELEASE_SF, CO_RELEASE_CR);
        let frame = co_release_frame_air_ms();
        assert!(
            frame > slot,
            "at this PHY the frame ({frame}ms) is what floors the slot ({slot}ms)"
        );

        for (arm, ceiling) in frame_arms() {
            let digit = arm.digit();
            for i in 0..64u32 {
                let seed = 0x5EED_0347u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
                for class in every_class() {
                    let mut framed = co_release_access(seed);
                    let mut bare = co_release_access(seed);
                    let drawn = bare.acquisition_jitter_ms();
                    assert_eq!(
                        arm_owed_jitter_ms(
                            arm,
                            &mut framed,
                            CO_RELEASE_PAYLOAD,
                            CO_RELEASE_BW,
                            CO_RELEASE_SF,
                            CO_RELEASE_CR,
                            class,
                        ),
                        classed_frame_wait_ms(
                            drawn / slot,
                            class,
                            frame,
                            JITTER_DIFS_SLOTS * slot,
                            ceiling
                        ),
                        "arm {digit} is the drawn number of slots folded onto this \
                         interface's class inside 2..={ceiling}, each count one frame \
                         wide, less the class's sub-frame position (seed {seed:#x}, \
                         {class:?})"
                    );
                }
            }

            // The window is the same span in every position of one class --
            // the arm's counts, one frame wide -- split into the even counts
            // and the odd ones, and together they are the whole span. The
            // position moves a wait inside its count, never between counts, so
            // it does not appear here at all: every position of one parity
            // walks the same counts.
            for class in every_class() {
                assert_eq!(
                    arm_window(arm, frame, class),
                    arm_window(arm, frame, FrameClass::new(class.parity(), 0)),
                    "arm {digit}: the position must not move a wait out of its \
                     count ({class:?})"
                );
            }
            let even = arm_window(arm, frame, FrameClass::new(0, 0));
            let odd = arm_window(arm, frame, FrameClass::new(1, 0));
            assert_eq!(
                even,
                frame_span(ceiling)
                    .into_iter()
                    .filter(|c| c % 2 == 0)
                    .collect(),
                "arm {digit}: class 0's wait is the even counts of {frame}ms frames"
            );
            assert_eq!(
                odd,
                frame_span(ceiling)
                    .into_iter()
                    .filter(|c| c % 2 == 1)
                    .collect(),
                "arm {digit}: class 1's wait is the odd counts of {frame}ms frames"
            );
            assert!(
                even.is_disjoint(&odd),
                "arm {digit}: two ends of opposite class must not share a single count"
            );
            let reached: std::collections::BTreeSet<u64> = even.union(&odd).copied().collect();
            assert_eq!(
                reached,
                frame_span(ceiling),
                "arm {digit}: the two classes together must be the whole 2..={ceiling} span"
            );
            assert_eq!(
                reached.iter().next_back().copied(),
                Some(ceiling),
                "arm {digit}: some class has to reach the span's top, or the \
                 acquisition ceiling every window is priced on is a wait \
                 nobody owes"
            );
        }

        // What separates the two arms, in one line each: arm 3's span IS the
        // draw window, one count per draw, and arm 4's is the seven counts
        // 2..=8 inside it. That is the only difference between them, and a
        // patch that made it two differences would have to move this.
        assert_eq!(
            frame_span(JITTER_ACQUISITION_SLOTS),
            expected_window(),
            "arm 3 re-expresses the draw window one-for-one"
        );
        assert!(frame_span(ARM_FOUR_FRAME_CEILING).is_subset(&expected_window()));
        assert_eq!(
            frame_span(ARM_FOUR_FRAME_CEILING).len(),
            7,
            "arm 4's span is the seven counts 2..=8 (Lew 2026-09-25)"
        );

        // The `max` in 124's formula is a guard and never the operative
        // term: a frame carries a preamble, so at every PHY the interface
        // can program even an EMPTY one is longer than a slot. Stated here
        // rather than left implicit — a preamble derivation that stopped
        // being true of this would turn the whole-frame arms back into arm 1
        // for the short frames, silently.
        for (bw, sf, cr) in [
            (CO_RELEASE_BW, CO_RELEASE_SF, CO_RELEASE_CR),
            (125_000, 8, 5),
            (500_000, 5, 5),
            (62_500, 12, 8),
        ] {
            let empty = rnode::airtime_ms_with_preamble(
                0,
                bw,
                sf,
                cr,
                rnode::derive_preamble_symbols(sf, cr, bw),
            );
            let slot_here = jitter_slot_ms(bw, sf, cr);
            assert!(
                empty >= slot_here,
                "bw={bw} sf={sf} cr={cr}: an empty frame is {empty}ms against a \
                 {slot_here}ms slot, so the whole-frame floor has become the \
                 operative term and the arm is no longer the one 124 priced"
            );
        }
    }

    /// TRACE 228 — two ends that draw the same slot count must not owe the
    /// same number of whole frames, under either whole-frame arm.
    ///
    /// The minimal reproduction of the loss 228 measured: under arm 3, both
    /// daemons of `lora_ratchet_rotation_listened` took their post-drain
    /// acquisition on the same millisecond of window 24c run 1, both drew
    /// 1509 ms = 3 x 503 ms, both keyed inside the other's CAD-blind window,
    /// and both frames died. Same anchor plus the same count is the whole
    /// mechanism, so the test hands two interfaces the same RNG seed — the
    /// identical draw, every time, rather than once in fourteen — and asks
    /// what each owes.
    ///
    /// The guarantee is the parity split and not the span's width, so arm 4
    /// has to keep it with seven counts split 4/3 exactly as arm 3 keeps it
    /// with fourteen split 7/7: the two classes' counts still differ by an odd
    /// number of frames, and the positions can still take back at most three
    /// quarters of one.
    ///
    /// Red before the class existed: arm 3 was `(drawn / slot) * frame_air`,
    /// a function of the draw alone, so two ends on one seed owed the same
    /// millisecond for all 64 seeds.
    #[test]
    fn the_whole_frame_arms_keep_two_ends_of_opposite_class_off_one_frame_count() {
        let frame = rotation_frame_air_ms();
        // What a frame is worth once the widest sub-frame position has taken
        // its share: 503 - 3 x 126 = 125 ms at this PHY. That is the floor
        // under every cross-class pair, and 229's decision is that anything
        // this far above the ~40 ms CAD-blind window is survivable.
        let floor = frame - (SUB_FRAME_POSITIONS - 1) * frame.div_ceil(SUB_FRAME_POSITIONS);

        // Two identities, in the two classes. Real hashes from two daemons
        // are not guaranteed to differ in class (see `FrameClass`), so the
        // pair is chosen for the property under test and the test says so
        // rather than drawing two at random and hoping.
        let (alpha, beta) = opposite_class_identities();
        let class_a = FrameClass::of(&alpha, "rnode_0");
        let class_b = FrameClass::of(&beta, "rnode_0");
        assert_ne!(
            class_a.parity(),
            class_b.parity(),
            "the fixture's two identities must straddle the two classes"
        );

        for (arm, _) in frame_arms() {
            let digit = arm.digit();
            for i in 0..64u32 {
                let seed = 0x5EED_0228u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
                let mut access_a = rotation_access(seed);
                let mut access_b = rotation_access(seed);
                let owed_a = rotation_owed(arm, &mut access_a, class_a);
                let owed_b = rotation_owed(arm, &mut access_b, class_b);
                assert_ne!(
                    owed_a, owed_b,
                    "arm {digit}, seed {seed:#x}: two ends on one acquisition \
                     anchor owed the same {owed_a}ms, which is 228's collision"
                );
                assert!(
                    owed_a.abs_diff(owed_b) >= floor,
                    "arm {digit}, seed {seed:#x}: the two ends are {}ms apart, \
                     less than the {floor}ms a frame less three sub-frame \
                     positions leaves",
                    owed_a.abs_diff(owed_b)
                );
            }

            // And not only for the two identities the fixture found: the counts
            // of two opposite classes differ by an odd number of frames and the
            // positions can take at most three quarters of one frame back off
            // them, so every pairing of an even class with an odd one clears the
            // same floor. Exhaustive over the sixteen pairs, because it is the
            // position that made this a subtraction and a subtraction is where
            // an off-by-one would hide.
            for a in every_class().filter(|c| c.parity() == 0) {
                for b in every_class().filter(|c| c.parity() == 1) {
                    for i in 0..16u32 {
                        let seed = 0x5EED_0229u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
                        let mut access_a = rotation_access(seed);
                        let mut access_b = rotation_access(seed);
                        let owed_a = rotation_owed(arm, &mut access_a, a);
                        let owed_b = rotation_owed(arm, &mut access_b, b);
                        assert!(
                            owed_a.abs_diff(owed_b) >= floor,
                            "arm {digit}: {a:?} and {b:?} on seed {seed:#x} are \
                             {}ms apart, under the {floor}ms floor",
                            owed_a.abs_diff(owed_b)
                        );
                    }
                }
            }
        }

        // The stagger is what the fix is for: what is left of a frame
        // airtime once the positions have had their share is still far
        // outside the window the modem is blind for after it keys, which the
        // slot-priced arms are not.
        let slot = jitter_slot_ms(ROTATION_BW, ROTATION_SF, ROTATION_CR);
        assert!(
            floor > 4 * slot,
            "at the rotation PHY the cross-class floor ({floor}ms) is several \
             slots ({slot}ms); below that the stagger would not clear the \
             blind window"
        );

        // The class is a property of the interface, not of the moment: a
        // reconnect, a restart or a second look must hand out the same one,
        // or two ends that were apart could land together after a port
        // flaps. `FrameClass::of` is pure, so this pins the hash rather than
        // any state.
        assert_eq!(FrameClass::of(&alpha, "rnode_0"), class_a);
        assert_eq!(FrameClass::of(&beta, "rnode_0"), class_b);
    }

    /// TRACE 229 — two ends of ONE class that draw the same count are still
    /// a quarter frame apart, because their sub-frame positions differ. Under
    /// either whole-frame arm: the position is a fraction of the frame, so it
    /// does not know how wide the count span is.
    ///
    /// This is what the position is for. The count alone left half of all
    /// pairs — the same-class half — contending over a handful of counts, and
    /// one acquisition in seven of those (one in four under arm 4's wider
    /// class) put both ends on the same millisecond again, which is 228's
    /// collision with a smaller p. A position taken off the count separates
    /// them by a quarter of the frame they are contending to send: 126 ms at
    /// the rotation PHY, where the modem is blind for the tens of milliseconds
    /// after it keys, so the second end sees a preamble that has already
    /// started and defers.
    ///
    /// Red before the position existed: arm 3 was the count alone, so two
    /// ends of one class on one seed owed the same millisecond for every
    /// seed and every pair of positions.
    #[test]
    fn two_ends_of_one_class_are_a_quarter_frame_apart_when_their_positions_differ() {
        let frame = rotation_frame_air_ms();
        let quarter = frame.div_ceil(SUB_FRAME_POSITIONS);
        assert_eq!(
            (frame, quarter),
            (503, 126),
            "the rotation PHY's frame and its quarter, which 229's decision \
             is priced on"
        );

        for (arm, _) in frame_arms() {
            let digit = arm.digit();
            for parity in 0..2 {
                for pos_a in 0..SUB_FRAME_POSITIONS {
                    for pos_b in (pos_a + 1)..SUB_FRAME_POSITIONS {
                        let class_a = FrameClass::new(parity, pos_a);
                        let class_b = FrameClass::new(parity, pos_b);
                        for i in 0..32u32 {
                            let seed = 0x5EED_0229u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
                            // One seed on both ends is the 228 anchor: the same
                            // draw and therefore the same count, every time
                            // rather than one acquisition in seven.
                            let mut access_a = rotation_access(seed);
                            let mut access_b = rotation_access(seed);
                            let owed_a = rotation_owed(arm, &mut access_a, class_a);
                            let owed_b = rotation_owed(arm, &mut access_b, class_b);
                            assert!(
                                owed_a.abs_diff(owed_b) >= quarter,
                                "arm {digit}, seed {seed:#x}: positions {pos_a} and \
                                 {pos_b} of class {parity} owed {owed_a}ms and \
                                 {owed_b}ms, under the {quarter}ms a quarter frame buys"
                            );
                        }
                    }
                }
            }
        }
    }

    /// What the class does NOT promise, stated as a test so nobody reads the
    /// pairwise guarantee as a global one. True of both whole-frame arms, and
    /// of arm 4 more often than of arm 3.
    ///
    /// Each end derives its class and its position from its own identity
    /// without knowing who else is on the channel, so two ends carry the
    /// same pair of fields one time in eight. They then have to draw the
    /// same count on top of that, which is one acquisition in seven under arm
    /// 3 — 1/56 over random identity pairs, against 1/14 for the count alone
    /// — and one in four or three under arm 4's narrower span, 29/784 over
    /// random pairs. Both are a smaller number and not a zero, and a pair that
    /// lands on all three is back in 228's collision.
    #[test]
    fn two_ends_of_one_class_and_one_position_can_still_meet_on_one_count() {
        for (arm, _) in frame_arms() {
            let digit = arm.digit();
            let mut collisions = 0usize;
            for i in 0..70u32 {
                let seed = 0x5EED_0229u32.wrapping_add(i.wrapping_mul(0x9E37_79B9));
                let mut access_a = rotation_access(seed);
                let mut access_b = rotation_access(seed);
                let class = FrameClass::new(0, 2);
                let owed_a = rotation_owed(arm, &mut access_a, class);
                let owed_b = rotation_owed(arm, &mut access_b, class);
                if owed_a == owed_b {
                    collisions += 1;
                }
            }
            assert_eq!(
                collisions, 70,
                "arm {digit}: two ends of ONE class and ONE position on one \
                 anchor and one draw still owe the same wait; the class \
                 separates classes and positions, not identities"
            );
        }
    }

    /// The fold itself: the counts of one class, each carrying as near an
    /// equal share of the fourteen draws as the class's width allows.
    ///
    /// Uniformity is the property that keeps the same-class case as good as it
    /// can be. Nudging an odd draw to the neighbouring even count would be the
    /// obvious fold and would pile three of the fourteen draws onto one count,
    /// making that count the likeliest place for two same-class ends to meet —
    /// 30/196 instead of 28/196 for a pair, which is worse than the 1/14 the
    /// unpinned arm had.
    ///
    /// Arm 3's fourteen draws divide evenly over its seven counts per class,
    /// two each. Arm 4's cannot: its seven counts split 4/3 and fourteen is a
    /// multiple of neither, so the best any fold can do is `14 / k` or
    /// `14 / k + 1` draws per count — 4,4,3,3 in class 0 and 5,5,4 in class 1.
    /// A modulo gives exactly that, and that is what is asserted here: not that
    /// every count is equally likely under every arm, which arm 4 cannot have,
    /// but that no count carries more than one draw above the floor, which is
    /// the best available and what the tie probabilities on
    /// [`frame_counts_per_class`] are computed from.
    #[test]
    fn the_class_fold_spreads_the_draws_as_evenly_as_the_span_allows() {
        let window: Vec<u64> =
            (JITTER_DIFS_SLOTS..JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64).collect();
        // One unit wide enough that the position never reaches the DIFS
        // floor, so the fold is read here and nothing else: the wait is the
        // count's, less a fixed offset this test puts back.
        let unit = rotation_frame_air_ms();
        let difs = JITTER_DIFS_SLOTS * jitter_slot_ms(ROTATION_BW, ROTATION_SF, ROTATION_CR);
        for (arm, ceiling) in frame_arms() {
            let digit = arm.digit();
            for class in every_class() {
                let mut hits: std::collections::BTreeMap<u64, usize> =
                    std::collections::BTreeMap::new();
                for drawn in &window {
                    let wait = classed_frame_wait_ms(*drawn, class, unit, difs, ceiling);
                    let count = (wait + class.sub_frame_offset_ms(unit)) / unit;
                    assert_eq!(
                        (wait + class.sub_frame_offset_ms(unit)) % unit,
                        0,
                        "arm {digit}: a wait of {wait}ms is not a count of {unit}ms \
                         less this class's position ({class:?})"
                    );
                    assert_eq!(
                        count % 2,
                        class.parity(),
                        "arm {digit}: count {count} is not in class {}",
                        class.parity()
                    );
                    assert!(
                        frame_span(ceiling).contains(&count),
                        "arm {digit}: count {count} left the 2..={ceiling} span, so \
                         the arm is no longer the one being measured"
                    );
                    *hits.entry(count).or_default() += 1;
                }
                let counts_per_class = frame_counts_per_class(ceiling, class.parity());
                assert_eq!(
                    hits.len() as u64,
                    counts_per_class,
                    "arm {digit}, class {}: must reach {counts_per_class} counts, \
                     reached {hits:?}",
                    class.parity()
                );
                assert_eq!(
                    hits.values().sum::<usize>(),
                    window.len(),
                    "arm {digit}, class {}: every draw has to land on a count, \
                     {hits:?}",
                    class.parity()
                );
                let floor = window.len() as u64 / counts_per_class;
                assert!(
                    hits.values()
                        .all(|n| *n as u64 == floor || *n as u64 == floor + 1),
                    "arm {digit}, class {}: the fold is lopsided by more than the \
                     {} draws that do not divide, {hits:?}",
                    class.parity(),
                    window.len() as u64 % counts_per_class
                );
            }
        }

        // The two spans' exact shares, spelled out, because the tie
        // probabilities the arms are chosen between are computed from these
        // numbers and nowhere else: a change to the fold has to move this line
        // and say what the new probability is.
        let shares = |ceiling: u64, parity: u64| -> Vec<usize> {
            let class = FrameClass::new(parity, 0);
            let mut hits: std::collections::BTreeMap<u64, usize> =
                std::collections::BTreeMap::new();
            for drawn in &window {
                *hits
                    .entry(classed_frame_wait_ms(*drawn, class, unit, difs, ceiling))
                    .or_default() += 1;
            }
            hits.into_values().collect()
        };
        assert_eq!(
            shares(JITTER_ACQUISITION_SLOTS, 0),
            vec![2; 7],
            "arm 3, class 0"
        );
        assert_eq!(
            shares(JITTER_ACQUISITION_SLOTS, 1),
            vec![2; 7],
            "arm 3, class 1"
        );
        assert_eq!(
            shares(ARM_FOUR_FRAME_CEILING, 0),
            vec![4, 4, 3, 3],
            "arm 4's class 0 holds four of the seven counts: 50/196 that a \
             same-class pair ties on the count"
        );
        assert_eq!(
            shares(ARM_FOUR_FRAME_CEILING, 1),
            vec![5, 5, 4],
            "arm 4's class 1 holds three: 66/196 that a same-class pair ties \
             on the count"
        );
    }

    /// The class of an interface is its identity AND its name.
    ///
    /// Either alone leaves a pair in one class by construction: both ends of
    /// every periculum LoRa cell build their interface as `rnode_0`
    /// (`driver::interface_build::rnode`), and two radios of one daemon
    /// share its identity hash.
    #[test]
    fn the_class_reads_both_the_identity_and_the_interface_name() {
        let (alpha, beta) = opposite_class_identities();
        assert_ne!(
            FrameClass::of(&alpha, "rnode_0"),
            FrameClass::of(&beta, "rnode_0"),
            "two identities under one interface name must be separable"
        );
        let names: std::collections::BTreeSet<FrameClass> = ["rnode_0", "rnode_1"]
            .into_iter()
            .map(|n| FrameClass::of(&alpha, n))
            .collect();
        assert_eq!(
            names.len(),
            2,
            "one node's two radios must be separable: {names:?}"
        );
    }

    /// Both fields come off one hash, and one hash has to spread identities
    /// over all eight of them.
    ///
    /// The 1/56 the position buys is 1/2 x 1/7 x 1/4, and the first and last
    /// factors are this: a hash whose position bits were skewed — or worse,
    /// correlated with the parity bit — would hand most of the mesh one
    /// pair of fields and the arithmetic would be a fiction. 256 identities
    /// that differ in one byte, which is the shape a lab rig actually has.
    #[test]
    fn the_hash_spreads_identities_over_every_class_and_position() {
        let mut hits: std::collections::BTreeMap<FrameClass, usize> =
            std::collections::BTreeMap::new();
        for n in 0..=255u8 {
            let mut id = [0u8; 16];
            id[0] = n;
            *hits.entry(FrameClass::of(&id, "rnode_0")).or_default() += 1;
        }
        assert_eq!(
            hits.len(),
            2 * SUB_FRAME_POSITIONS as usize,
            "every class and position must be reachable: {hits:?}"
        );
        assert!(
            hits.values()
                .all(|n| *n == 256 / (2 * SUB_FRAME_POSITIONS as usize)),
            "the hash is skewed across classes or positions: {hits:?}"
        );
    }

    /// The two bounds the position is not allowed to break, under either
    /// whole-frame arm and on a PHY where it would: a frame no wider than a
    /// contention slot.
    ///
    /// The whole-frame unit is `max(frame_air, slot)`, so a degenerate PHY
    /// collapses it onto the slot and the arm onto arm 1 — and there the
    /// lowest count IS DIFS, so a position taken off it would put a frame on
    /// the air before the medium's own inter-frame space had passed. The floor
    /// is what stops that; the ceiling is what keeps 224's drain window and the
    /// selftest's acquisition term priced on a wait that can actually happen.
    /// Arm 4's narrower span moves the ceiling and not the floor, so both
    /// bounds are read per arm.
    #[test]
    fn the_position_never_breaks_the_difs_floor_or_the_ceiling() {
        // The smallest slot `jitter_slot_ms` can return, and a unit equal to
        // it: the case the `max` in the whole-frame formula leaves.
        let unit = 6;
        let difs = JITTER_DIFS_SLOTS * unit;
        for (arm, count_ceiling) in frame_arms() {
            let digit = arm.digit();
            let ceiling = count_ceiling * unit;
            let mut floored = 0usize;
            for class in every_class() {
                for drawn in JITTER_DIFS_SLOTS..JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 {
                    let wait = classed_frame_wait_ms(drawn, class, unit, difs, count_ceiling);
                    assert!(
                        wait >= difs,
                        "arm {digit}: {class:?} owed {wait}ms on a draw of {drawn}, \
                         under the {difs}ms DIFS the medium owes before any contention"
                    );
                    assert!(
                        wait <= ceiling,
                        "arm {digit}: {class:?} owed {wait}ms on a draw of {drawn}, \
                         over the {ceiling}ms ceiling every window is priced on"
                    );
                    if wait == difs {
                        floored += 1;
                    }
                }
            }
            assert!(
                floored > 0,
                "arm {digit}: on this PHY the floor has to be the operative term \
                 for the lowest count, or the test is not exercising it"
            );

            // And the class still separates here, which is the property the
            // position is not allowed to buy its own separation with: two
            // opposite classes are one count apart and nothing else, so an
            // offset that reached a whole count would cancel the class on
            // exactly the PHY where a count is narrowest.
            for a in every_class().filter(|c| c.parity() == 0) {
                for b in every_class().filter(|c| c.parity() == 1) {
                    for drawn in JITTER_DIFS_SLOTS..JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 {
                        assert_ne!(
                            classed_frame_wait_ms(drawn, a, unit, difs, count_ceiling),
                            classed_frame_wait_ms(drawn, b, unit, difs, count_ceiling),
                            "arm {digit}: {a:?} and {b:?} owe the same wait on a \
                             draw of {drawn}"
                        );
                    }
                }
            }
        }
    }

    /// The PHY of `lora_ratchet_rotation_listened`, the cell trace 223
    /// (2026-09-24) measured the three arms on: SF7 at 62.5 kHz, CR 4:5 —
    /// 2734 bps — carrying the 147-byte frames that cell sends.
    const ROTATION_BW: u32 = 62_500;
    const ROTATION_SF: u8 = 7;
    const ROTATION_CR: u8 = 5;
    const ROTATION_PAYLOAD: usize = 147;

    /// What one `lora_ratchet_rotation_listened` frame holds the channel
    /// for, from the interface's own airtime function — 503 ms, the unit
    /// trace 228 caught both ends paying three of.
    fn rotation_frame_air_ms() -> u64 {
        rnode::airtime_ms_with_preamble(
            ROTATION_PAYLOAD as u32,
            ROTATION_BW,
            ROTATION_SF,
            ROTATION_CR,
            rnode::derive_preamble_symbols(ROTATION_SF, ROTATION_CR, ROTATION_BW),
        )
    }

    /// A policy at the rotation PHY, seeded by the test. Two of these on one
    /// seed are the 228 anchor: the same draw on both ends.
    fn rotation_access(seed: u32) -> ChannelAccess {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(ROTATION_BW, ROTATION_SF, ROTATION_CR);
        access
    }

    /// What one acquisition of `access` owes under `arm` at the rotation
    /// PHY, for an interface of `class`.
    fn rotation_owed(arm: JitterArm, access: &mut ChannelAccess, class: FrameClass) -> u64 {
        arm_owed_jitter_ms(
            arm,
            access,
            ROTATION_PAYLOAD,
            ROTATION_BW,
            ROTATION_SF,
            ROTATION_CR,
            class,
        )
    }

    /// Two 16-byte identity hashes that land in the two different classes,
    /// searched for rather than typed so the fixture survives a change to
    /// the hash.
    fn opposite_class_identities() -> ([u8; 16], [u8; 16]) {
        let of = |n: u8| {
            let mut id = [0u8; 16];
            id[0] = n;
            id
        };
        let alpha = (0u8..=255)
            .map(of)
            .find(|id| FrameClass::of(id, "rnode_0") == FrameClass::new(0, 0))
            .expect("some identity is in class 0");
        let beta = (0u8..=255)
            .map(of)
            .find(|id| FrameClass::of(id, "rnode_0") == FrameClass::new(1, 0))
            .expect("some identity is in class 1");
        (alpha, beta)
    }

    /// What the interface reports one ACQUISITION of the channel can cost,
    /// as opposed to what it reports one frame costs the frame behind it.
    ///
    /// Trace 223: arm 3 lost nothing on the air in nine runs — 40/40 data
    /// frames delivered, zero mutual key-ups — and was still called red
    /// twice, because two frames arrived 0.46 s and 0.68 s after a drain
    /// window priced on `tx_jitter_max` alone. That window buys 360 ms for a
    /// handover the same trace measured at 5.03 s and 5.53 s, because under
    /// arm 3 a slot is a whole frame wide. A caller that can only read the
    /// per-frame ceiling cannot price the difference, so the interface has to
    /// state it.
    ///
    /// Arm 4 is the same statement with a smaller number, and that number is
    /// the arm's whole purpose: eight frames instead of fifteen, which is what
    /// the selftest's acquisition term and 224's drain window shrink by.
    #[test]
    fn the_acquisition_ceiling_is_the_widest_wait_the_arm_can_impose() {
        let slot = jitter_slot_ms(ROTATION_BW, ROTATION_SF, ROTATION_CR);
        let frame = rnode::airtime_ms_with_preamble(
            ROTATION_PAYLOAD as u32,
            ROTATION_BW,
            ROTATION_SF,
            ROTATION_CR,
            rnode::derive_preamble_symbols(ROTATION_SF, ROTATION_CR, ROTATION_BW),
        );
        assert!(
            frame > slot,
            "at the rotation PHY the frame ({frame}ms) is what floors the \
             whole-frame arms' slot ({slot}ms); below that the arms would not \
             differ here"
        );
        // The largest frame the interface can be handed, which is what the
        // resource timeout floors on.
        let mtu_air = rnode::airtime_ms_with_preamble(
            rnode::HW_MTU as u32,
            ROTATION_BW,
            ROTATION_SF,
            ROTATION_CR,
            rnode::derive_preamble_symbols(ROTATION_SF, ROTATION_CR, ROTATION_BW),
        );

        // Arms 1 and 2 wait in slots of the modulation, so one number covers
        // every frame — the 360 ms shape 223 found in the profile.
        for arm in [JitterArm::AsIs, JitterArm::ModemOnly] {
            let ceiling = compute_acquisition_ceiling(arm, ROTATION_SF, ROTATION_CR, ROTATION_BW);
            assert_eq!(
                ceiling.frame_slots,
                None,
                "arm {} is slot-priced",
                arm.digit()
            );
            assert_eq!(
                (ceiling.for_frame_air_ms(frame), ceiling.max_ms),
                (
                    JITTER_ACQUISITION_SLOTS * slot,
                    JITTER_ACQUISITION_SLOTS * slot
                ),
                "arm {}'s acquisition is the same wait whatever the frame",
                arm.digit()
            );
            assert_eq!(ceiling.full_frame_ms, JITTER_ACQUISITION_SLOTS * slot);
        }

        // The whole-frame arms are the same number of counts, each one frame
        // wide — and it is a ceiling of what the TX loop actually owes, not a
        // figure derived beside it: every wait the arm can impose on this frame
        // is covered, and the widest one reaches it.
        //
        // Pinning the count to a class (trace 228) must not move it: the
        // ceiling is a bound on what ANY interface can owe, so no class may
        // exceed it, and the class that carries the top of the span must still
        // reach it -- otherwise the selftest's acquisition term and 224's
        // drain window would be priced on a wait nobody can owe.
        for (arm, count_ceiling) in frame_arms() {
            let digit = arm.digit();
            let priced = compute_acquisition_ceiling(arm, ROTATION_SF, ROTATION_CR, ROTATION_BW);
            assert_eq!(
                priced.frame_slots,
                Some(count_ceiling),
                "arm {digit} prices its acquisition in whole frames"
            );
            assert_eq!(
                priced.for_frame_air_ms(frame),
                count_ceiling * frame,
                "arm {digit} owes {count_ceiling} frames of {frame}ms, not \
                 {count_ceiling} slots of {slot}ms"
            );
            assert_eq!(
                priced.full_frame_ms,
                count_ceiling * mtu_air,
                "arm {digit}'s full-frame figure is the same arithmetic at the MTU"
            );
            assert!(
                priced.for_frame_air_ms(frame)
                    > 10 * compute_jitter_max_ms(ROTATION_SF, ROTATION_CR, ROTATION_BW),
                "arm {digit}: the whole point is that the two ceilings are \
                 different quantities"
            );

            let mut widest: std::collections::BTreeMap<FrameClass, u64> =
                std::collections::BTreeMap::new();
            for class in every_class() {
                let mut access = ChannelAccess::new(0x5EED_0223);
                access.set_phy(ROTATION_BW, ROTATION_SF, ROTATION_CR);
                for _ in 0..2000 {
                    access.channel_released();
                    let owed = rotation_owed(arm, &mut access, class);
                    assert!(
                        owed <= priced.for_frame_air_ms(frame),
                        "arm {digit}: {class:?} owed {owed}ms against a ceiling of {}ms",
                        priced.for_frame_air_ms(frame)
                    );
                    let entry = widest.entry(class).or_default();
                    *entry = (*entry).max(owed);
                }
            }
            // Each class tops out exactly where its own count and position put
            // it, one count and/or one position below the span's top for every
            // class but the one that carries it -- what a span split in two and
            // then in four costs.
            for class in every_class() {
                let counts_per_class = frame_counts_per_class(count_ceiling, class.parity());
                let top_count = JITTER_DIFS_SLOTS + class.parity() + 2 * (counts_per_class - 1);
                assert_eq!(
                    widest[&class],
                    top_count * frame - class.sub_frame_offset_ms(frame),
                    "arm {digit}: {class:?} does not top out where its count and \
                     position put it"
                );
            }
            // Which class that is follows from the span: arm 3's top count 15
            // is odd, arm 4's 8 is even.
            let top_class = FrameClass::new((count_ceiling - JITTER_DIFS_SLOTS) % 2, 0);
            assert_eq!(
                widest[&top_class],
                priced.for_frame_air_ms(frame),
                "arm {digit}: a ceiling the loop can never reach would over-price \
                 every window"
            );
        }

        // And the figure the A/B is about, stated in milliseconds: one
        // acquisition of this carrier costs a 147-byte frame 7.5 s under arm 3
        // and 4.0 s under arm 4. That ratio is what the selftest charges per
        // sender (`drain_budget`'s third term, via
        // `LinkProfile::acquisition_ceiling_ms`), so arm 4's priced window
        // shrinks by exactly 8/15 and not by a number chosen beside it.
        let arm3 = compute_acquisition_ceiling(
            JitterArm::FrameSlot,
            ROTATION_SF,
            ROTATION_CR,
            ROTATION_BW,
        );
        let arm4 = compute_acquisition_ceiling(
            JitterArm::FrameSlotShort,
            ROTATION_SF,
            ROTATION_CR,
            ROTATION_BW,
        );
        assert_eq!(
            arm4.frame_slots,
            Some(8),
            "arm 4's span tops out at eight whole frames (Lew 2026-09-25)"
        );
        assert_eq!(
            (arm3.for_frame_air_ms(frame), arm4.for_frame_air_ms(frame)),
            (7545, 4024),
            "the two arms' priced acquisitions at the rotation PHY, in ms"
        );
        assert_eq!(
            arm4.for_frame_air_ms(frame) * JITTER_ACQUISITION_SLOTS,
            arm3.for_frame_air_ms(frame) * ARM_FOUR_FRAME_CEILING,
            "arm 4's priced acquisition is arm 3's times 8/15"
        );
        assert_eq!(
            arm4.full_frame_ms * JITTER_ACQUISITION_SLOTS,
            arm3.full_frame_ms * ARM_FOUR_FRAME_CEILING,
            "and so is the full-frame figure the resource timeout floors on"
        );
    }

    /// The selector itself: unset is arm 1, the four digits are the four
    /// arms, and anything else is refused by name.
    ///
    /// The refusal is the point. A daemon that fell back to arm 1 on a
    /// typo would run an arm nobody chose and log it as chosen, and the run
    /// would be pooled into the wrong series — which is exactly the mixing
    /// the one-binary selector exists to prevent.
    #[test]
    fn the_selector_takes_four_values_and_refuses_the_rest() {
        assert_eq!(JitterArm::parse("1"), Some(JitterArm::AsIs));
        assert_eq!(JitterArm::parse("2"), Some(JitterArm::ModemOnly));
        assert_eq!(JitterArm::parse("3"), Some(JitterArm::FrameSlot));
        assert_eq!(JitterArm::parse("4"), Some(JitterArm::FrameSlotShort));
        assert_eq!(JitterArm::default(), JitterArm::AsIs);
        assert_eq!(JitterArm::AsIs.digit(), 1);
        assert_eq!(JitterArm::ModemOnly.digit(), 2);
        assert_eq!(JitterArm::FrameSlot.digit(), 3);
        assert_eq!(JitterArm::FrameSlotShort.digit(), 4);

        for refused in ["0", "5", "", " ", "arm2", "2.0", "-1", "true"] {
            assert_eq!(
                JitterArm::parse(refused),
                None,
                "{refused:?} is not one of the four arms"
            );
        }

        // Every arm's digit is its own, and the digit is what periculum reads
        // off the bring-up line into the run document: two arms sharing one
        // would pool two series under one name, which is the failure the
        // selector exists to prevent, one layer down.
        let digits: std::collections::BTreeSet<u8> = [
            JitterArm::AsIs,
            JitterArm::ModemOnly,
            JitterArm::FrameSlot,
            JitterArm::FrameSlotShort,
        ]
        .into_iter()
        .map(JitterArm::digit)
        .collect();
        assert_eq!(digits.len(), 4, "two arms share a digit: {digits:?}");

        // And the refusal names every arm an operator may have meant,
        // including the newest one: a message that listed three arms after a
        // fourth existed would send a typo hunting for a missing feature.
        for digit in ["1", "2", "3", "4"] {
            assert!(
                JITTER_ARM_CHOICES.contains(digit),
                "the refusal must name arm {digit}: {JITTER_ARM_CHOICES}"
            );
        }
    }

    /// Reproduce the flow-control startup deadlock without hardware.
    ///
    /// With `flow_control = true`, the io task historically initialised
    /// `interface_ready = false` and only flipped it on receipt of a
    /// `CMD_READY` (0x0F) frame. The RNode firmware sends `CMD_READY`
    /// **after** each TX as a "I can accept the next frame" signal — never
    /// spontaneously after init. That produced a chicken-and-egg stall: no
    /// TX ⇒ no `CMD_READY` ⇒ no TX, ever. Observed on miauhaus 2026-04-29
    /// as 0 bytes TX in 24 minutes uptime while the send queue spammed
    /// "send queue full, dropping oldest".
    ///
    /// Test fails (timeout) before the fix, passes after.
    #[tokio::test]
    async fn test_flow_control_initial_ready_no_stall() {
        // tokio::io::duplex gives us a pair of in-memory streams: `port` is
        // what the io task drives; `peer` simulates the RNode-side serial
        // endpoint that we read TX bytes from. The peer never sends
        // CMD_READY — the test's whole point is that the first TX must go
        // through *without* one.
        let (port, mut peer) = tokio::io::duplex(8192);

        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ true,
                test_channel_access(),
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        // Submit one outgoing packet. With flow_control = true and the
        // fixed initial-ready bug, this must reach `peer` within a short
        // timeout. With the bug, the io task stalls waiting for
        // CMD_READY and `peer.read` times out.
        let payload = b"hello";
        outgoing_tx
            .send(OutgoingPacket {
                peer: None,
                data: payload.to_vec(),
                high_priority: false,
            })
            .await
            .expect("send to io task");

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut buf))
            .await
            .expect("io task must send first frame within 1s (would deadlock if interface_ready stayed false)")
            .expect("read from duplex");

        assert!(n >= 3, "expected at least a 3-byte KISS frame, got {n}");
        // KISS data frame: FEND CMD_DATA payload FEND
        assert_eq!(buf[0], kiss::FEND, "first byte should be KISS FEND");
        assert_eq!(buf[1], rnode::CMD_DATA, "second byte should be CMD_DATA");

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            tx_bytes,
            payload.len() as u64,
            "tx_bytes counter must reflect the dispatched payload"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    /// Read KISS frames from `peer` until `timeout` elapses or no more bytes
    /// arrive. Returns every successfully deframed `(command, payload)` pair
    /// in arrival order. Used by the multi-frame send tests below.
    async fn drain_kiss_frames<S: tokio::io::AsyncReadExt + Unpin>(
        peer: &mut S,
        timeout: Duration,
    ) -> Vec<(u8, Vec<u8>)> {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut frames: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, peer.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    for f in deframer.process(&buf[..n]) {
                        if let KissDeframeResult::Frame { command, payload } = f {
                            frames.push((command, payload.to_vec()));
                        }
                    }
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        frames
    }

    /// The TEST-ONLY `test_drop_direct_ingress` knob at the interface level
    /// (deframe → filter, no daemon): a deframed CMD_DATA frame whose wire
    /// hops byte (`raw[1]`) is 0 must be dropped before the transport
    /// channel, a relayed copy (hops >= 1) must pass, and with the knob off
    /// (the default) the hops-0 frame passes too.
    #[tokio::test]
    async fn test_drop_direct_ingress_filters_hops0_at_the_rnode_boundary() {
        async fn rx_through_io_task(
            drop_direct: bool,
            frames: &[Vec<u8>],
        ) -> (Vec<Vec<u8>>, Arc<InterfaceCounters>) {
            let (port, mut peer) = tokio::io::duplex(8192);
            let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingPacket>(16);
            let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
            let counters = Arc::new(InterfaceCounters::new());
            let task_counters = Arc::clone(&counters);
            let task = tokio::spawn(async move {
                rnode_io_task(
                    "test_rnode_deaf".to_string(),
                    port,
                    incoming_tx,
                    outgoing_rx,
                    task_counters,
                    /* flow_control = */ false,
                    test_channel_access(),
                    125_000,
                    7,
                    5,
                    drop_direct,
                    /* jitter_arm = */ JitterArm::AsIs,
                    /* frame_class = */ FrameClass::default(),
                    /* alock = */ AirtimeLock::default(),
                )
                .await;
            });
            for f in frames {
                let mut out = Vec::new();
                kiss::frame(rnode::CMD_DATA, f, &mut out);
                peer.write_all(&out).await.expect("write frame");
            }
            let mut got = Vec::new();
            while let Ok(Some(pkt)) =
                tokio::time::timeout(Duration::from_millis(300), incoming_rx.recv()).await
            {
                got.push(pkt.data);
            }
            drop(outgoing_tx);
            drop(peer);
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            (got, counters)
        }

        // flags(1) hops(1) tail — the filter reads only raw[1]. A frame
        // transmitted by its originator carries wire hops 0; a copy relayed
        // by one transport hop carries 1.
        let direct = vec![0x00, 0x00, 0xAA, 0xBB];
        let relayed = vec![0x00, 0x01, 0xAA, 0xBB];

        // Knob on: the direct frame vanishes before the transport channel
        // and is counted; the relayed copy arrives.
        let (got, counters) = rx_through_io_task(true, &[direct.clone(), relayed.clone()]).await;
        assert_eq!(got, vec![relayed.clone()], "only the relayed copy passes");
        assert_eq!(
            counters
                .test_direct_ingress_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            counters.rx_bytes.load(std::sync::atomic::Ordering::Relaxed),
            relayed.len() as u64,
            "a dropped frame was never heard, so it must not count as RX"
        );

        // Default off: both frames arrive, nothing is counted as dropped.
        let (got, counters) = rx_through_io_task(false, &[direct.clone(), relayed.clone()]).await;
        assert_eq!(got, vec![direct, relayed]);
        assert_eq!(
            counters
                .test_direct_ingress_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    /// The filter announces itself (Codeberg #223): when
    /// `test_drop_direct_ingress` is on, the io task emits exactly the
    /// armed line the periculum cells assert on — from the same task that
    /// holds the flag the drop filter reads, so the line proves the
    /// production drop path is live and not merely that a config key
    /// parsed. Off (the default), the line must be absent.
    #[tokio::test]
    async fn test_drop_direct_ingress_announces_arming_at_the_rnode_boundary() {
        async fn logs_from_io_task(drop_direct: bool) -> String {
            let (buf, _guard) = capture_logs();
            let (port, peer) = tokio::io::duplex(8192);
            let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
            let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
            let counters = Arc::new(InterfaceCounters::new());
            let task = tokio::spawn(async move {
                rnode_io_task(
                    "test_rnode_armed".to_string(),
                    port,
                    incoming_tx,
                    outgoing_rx,
                    counters,
                    /* flow_control = */ false,
                    test_channel_access(),
                    125_000,
                    7,
                    5,
                    drop_direct,
                    /* jitter_arm = */ JitterArm::AsIs,
                    /* frame_class = */ FrameClass::default(),
                    /* alock = */ AirtimeLock::default(),
                )
                .await;
            });
            // Let the io task start and emit (or not emit) the armed line,
            // then shut it down by closing its peer and channels.
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(outgoing_tx);
            drop(peer);
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            let captured = buf.lock().unwrap();
            String::from_utf8_lossy(&captured).into_owned()
        }

        let logs = logs_from_io_task(true).await;
        assert!(
            logs.contains("DIRECT_INGRESS_FILTER armed iface=test_rnode_armed"),
            "armed line missing with the knob on; logs:\n{logs}"
        );

        let logs = logs_from_io_task(false).await;
        assert!(
            !logs.contains("DIRECT_INGRESS_FILTER"),
            "armed line must be absent with the knob off; logs:\n{logs}"
        );
    }

    /// A data frame's RX lines carry the RSSI/SNR the firmware indicated
    /// immediately before it (Codeberg #364). The mocked KISS stream
    /// reproduces the firmware's order on every MCU variant — CMD_STAT_RSSI,
    /// CMD_STAT_SNR, then the CMD_DATA frame (RNode_Firmware.ino:1689-1691)
    /// — and the pairing is the reference's last-seen one
    /// (RNodeInterface.py:877-880): the stats stored when CMD_DATA arrives
    /// are that frame's own report. A frame no stat frame preceded logs the
    /// bare line, not an invented value.
    #[tokio::test]
    async fn test_rx_lines_carry_the_preceding_stat_frames_rssi_and_snr() {
        let (buf, _guard) = capture_logs();
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_signal".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                counters,
                /* flow_control = */ false,
                test_channel_access(),
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        // The mocked KISS stream, in the firmware's order: a data frame no
        // stat frame preceded (a mock, an older firmware), then a real
        // reception's RSSI stat, SNR stat, data frame. Raw 100 is 100-157 =
        // -57 dBm; raw 21 is 21*0.25 = 5.25 dB, proving the scaled value
        // (not the raw byte) is logged. `kiss::frame` clears its output, so
        // each frame is built and written on its own.
        let mut wire = Vec::new();
        for (command, payload) in [
            (rnode::CMD_DATA, &[0x00, 0x00, 0x01][..]),
            (rnode::CMD_STAT_RSSI, &[100][..]),
            (rnode::CMD_STAT_SNR, &[21][..]),
            (rnode::CMD_DATA, &[0x00, 0x00, 0x02, 0x03][..]),
        ] {
            kiss::frame(command, payload, &mut wire);
            peer.write_all(&wire)
                .await
                .expect("write mocked KISS stream");
        }

        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(2), incoming_rx.recv())
                .await
                .expect("data frame within 2s")
                .expect("incoming channel open");
        }
        drop(outgoing_tx);
        drop(peer);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;

        let captured = buf.lock().unwrap();
        let logs = String::from_utf8_lossy(&captured);
        let rx_events: Vec<&str> = logs.lines().filter(|l| l.contains("LORA_RX")).collect();
        assert_eq!(rx_events.len(), 2, "two LORA_RX events; logs:\n{logs}");
        assert!(
            rx_events[0].contains("len=3")
                && !rx_events[0].contains("rssi=")
                && !rx_events[0].contains("snr="),
            "the un-preceded frame carries no signal keys: {}",
            rx_events[0]
        );
        assert!(
            rx_events[1].contains("len=4 rssi=-57 snr=5.25"),
            "the preceded frame carries the paired stats: {}",
            rx_events[1]
        );
        assert!(
            logs.contains("RX 4 bytes from radio rssi=-57 snr=5.25"),
            "the human RX line carries the same pair; logs:\n{logs}"
        );
    }

    /// Steady-state throughput sanity for the default `flow_control = false`
    /// path: pushed packets must all reach `peer` without anyone feeding
    /// CMD_READY back. This is the configuration that lnsd (and Python-RNS)
    /// actually defaults to and that miauhaus runs after the 2026-04-30
    /// flow_control flip. Guards against a regression where someone
    /// reintroduces a hidden CMD_READY-gate on the non-flow-control path.
    #[tokio::test]
    async fn test_no_flow_control_multi_frame_throughput() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ false,
                test_channel_access(),
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        let payloads: [&[u8]; 3] = [b"alpha", b"bravo", b"charlie"];
        for p in payloads.iter() {
            outgoing_tx
                .send(OutgoingPacket {
                    peer: None,
                    data: p.to_vec(),
                    high_priority: false,
                })
                .await
                .expect("send to io task");
        }

        // Two holds at this PHY (~0.4 s each for a 5-byte frame at
        // SF7/125 kHz), the acquisition draw (≤ 90 ms on the fast policy
        // `test_channel_access` runs) and serial latency. Wall clock, so the
        // window is generous; what is asserted is that all three arrive, not
        // when.
        let frames = drain_kiss_frames(&mut peer, Duration::from_secs(5)).await;
        let data_frames: Vec<&Vec<u8>> = frames
            .iter()
            .filter(|(c, _)| *c == rnode::CMD_DATA)
            .map(|(_, p)| p)
            .collect();

        assert_eq!(
            data_frames.len(),
            3,
            "all three frames must reach the peer; got {} CMD_DATA frames out of {} total",
            data_frames.len(),
            frames.len()
        );
        for (i, p) in payloads.iter().enumerate() {
            assert_eq!(
                data_frames[i].as_slice(),
                *p,
                "frame {} payload mismatch",
                i
            );
        }

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let total_payload: usize = payloads.iter().map(|p| p.len()).sum();
        assert_eq!(
            tx_bytes, total_payload as u64,
            "tx_bytes counter must sum payloads"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    // The former `test_flow_control_with_cmd_ready_multi_frame_throughput`
    // encoded the refuted protocol model — an unsolicited CMD_READY after
    // every TX. The firmware never sends one (`RNode_Firmware.ino:1003-1008`
    // answers only a host query; `kiss_indicate_ready`, `Utilities.h:1157`,
    // has no other call site), so the round-trip it guarded cannot occur on
    // hardware. The corrected query/response round-trip is covered by the
    // scripted-stub suite below.

    // -----------------------------------------------------------------------
    // Multi-vport (RNodeMultiInterface) tests
    // -----------------------------------------------------------------------

    /// In-process multi-vport RNode firmware stub over one half of a duplex.
    ///
    /// Answers the multi detect probe (detected + firmware + platform + MCU +
    /// CMD_INTERFACES chip-type report), tracks the selected vport from
    /// CMD_SEL_INT, records the frequency configured for each vport, and for
    /// every CMD_DATA it receives records `(vport, payload)` and echoes the same
    /// payload back tagged with that vport. This lets a test prove both that TX
    /// frames carry the right vport and that RX frames route to the right
    /// logical interface.
    async fn rnode_multi_firmware_stub(
        peer: tokio::io::DuplexStream,
        chip_types: Vec<u8>,
        freq_report: tokio::sync::mpsc::Sender<(u8, u32)>,
        data_report: tokio::sync::mpsc::Sender<(u8, Vec<u8>)>,
    ) {
        rnode_multi_firmware_stub_scripted(peer, chip_types, freq_report, data_report, None).await
    }

    /// The firmware-queue model the multi-vport stub answers CMD_READY
    /// queries from. Every vport feeds the one queue the modem owns, so the
    /// model is one budget and one depth, not one per vport. `None` gives
    /// the plain stub back: a modem that ignores the query entirely.
    struct MultiStubFlow {
        /// Frames the modem can still put on the air before its queue jams.
        air_budget: usize,
        /// The duty lock lifting: `send(n)` hands the modem n more slots.
        resume_rx: mpsc::Receiver<usize>,
        /// CMD_READY queries answered — proves the host asked rather than
        /// waited for a READY the firmware never volunteers.
        ready_queries: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// [`rnode_multi_firmware_stub`] plus the scripted CMD_READY query
    /// protocol, the multi-vport twin of [`rnode_firmware_stub_scripted`].
    /// A CMD_DATA frame beyond `air_budget` sits in the shared queue, and
    /// while anything sits there a query is answered 0x00; `resume_rx`
    /// refreshes the budget and drains it, announcing nothing — exactly
    /// like the firmware, which reports queue state only when asked
    /// (`RNode_Firmware.ino:1003-1008`).
    async fn rnode_multi_firmware_stub_scripted(
        mut peer: tokio::io::DuplexStream,
        chip_types: Vec<u8>,
        freq_report: tokio::sync::mpsc::Sender<(u8, u32)>,
        data_report: tokio::sync::mpsc::Sender<(u8, Vec<u8>)>,
        mut flow: Option<MultiStubFlow>,
    ) {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut buf = [0u8; 1024];
        let mut selected: u8 = 0;
        // Frames held in the shared firmware queue. One-deep model, as in
        // the single-radio stub: any held frame means the queue is full.
        let mut queued: usize = 0;
        let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
            let mut one = Vec::new();
            kiss::frame(cmd, payload, &mut one);
            reply.extend_from_slice(&one);
        };
        loop {
            let n = tokio::select! {
                read = peer.read(&mut buf) => match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                },
                resumed = async {
                    match flow {
                        Some(ref mut f) => f.resume_rx.recv().await,
                        None => std::future::pending::<Option<usize>>().await,
                    }
                } => {
                    let Some(slots) = resumed else { return };
                    let f = flow.as_mut().expect("resume arm is armed only with a flow model");
                    f.air_budget += slots;
                    while queued > 0 && f.air_budget > 0 {
                        queued -= 1;
                        f.air_budget -= 1;
                    }
                    continue;
                }
            };
            let mut reply: Vec<u8> = Vec::new();
            for f in deframer.process(&buf[..n]) {
                let KissDeframeResult::Frame { command, payload } = f else {
                    continue;
                };
                match command {
                    rnode::CMD_DETECT => {
                        push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                        push(
                            &mut reply,
                            rnode::CMD_FW_VERSION,
                            &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                        );
                        push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_NRF52]);
                        push(&mut reply, rnode::CMD_MCU, &[0x00]);
                    }
                    rnode::CMD_INTERFACES => {
                        // Report one 2-byte record per vport: [0x00, chip_type].
                        let mut rep = Vec::with_capacity(chip_types.len() * 2);
                        for &t in &chip_types {
                            rep.push(0x00);
                            rep.push(t);
                        }
                        push(&mut reply, rnode::CMD_INTERFACES, &rep);
                    }
                    rnode::CMD_SEL_INT => {
                        if let Some(v) = rnode::decode_select_interface(&payload) {
                            selected = v;
                        }
                    }
                    rnode::CMD_FREQUENCY if payload.len() >= 4 => {
                        let hz =
                            u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                        let _ = freq_report.try_send((selected, hz));
                    }
                    rnode::CMD_DATA => {
                        let _ = data_report.try_send((selected, payload.to_vec()));
                        if let Some(fl) = flow.as_mut() {
                            // Airtime or queue, whichever the budget allows.
                            // No READY here: the firmware does not announce
                            // TX completion.
                            if fl.air_budget > 0 {
                                fl.air_budget -= 1;
                            } else {
                                queued += 1;
                            }
                        }
                        // Echo the payload back tagged with the same vport, using
                        // the same SEL_INT + CMD_DATA framing the host uses.
                        reply.extend_from_slice(&rnode::build_vport_data_frame(selected, &payload));
                    }
                    rnode::CMD_READY => {
                        // Answer the host's queue-state query, if this stub
                        // was scripted to answer at all.
                        if let Some(fl) = flow.as_mut() {
                            fl.ready_queries
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            push(
                                &mut reply,
                                rnode::CMD_READY,
                                &[if queued > 0 { 0x00 } else { 0x01 }],
                            );
                        }
                    }
                    // Drain the rest of the per-vport config commands silently.
                    _ => {}
                }
            }
            if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
                return;
            }
        }
    }

    /// Drive the full multi-vport exchange against the mock firmware: configure
    /// two vports over one shared serial link, then prove that (a) each vport's
    /// radio config is pushed under the correct CMD_SEL_INT, (b) a packet sent on
    /// a vport's logical interface reaches the firmware tagged with that vport,
    /// and (c) a frame the firmware emits tagged with a vport routes back to that
    /// vport's logical interface and no other.
    #[tokio::test]
    async fn test_multi_vport_config_and_routing() {
        let (port, peer) = tokio::io::duplex(64 * 1024);

        let (freq_tx, mut freq_rx) = tokio::sync::mpsc::channel::<(u8, u32)>(8);
        let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(8);
        // Two vports: vport 0 = SX127X (sub-GHz), vport 1 = SX128X (2.4 GHz).
        let stub = tokio::spawn(rnode_multi_firmware_stub(
            peer,
            vec![rnode::CHIP_SX127X, rnode::CHIP_SX128X],
            freq_tx,
            data_tx,
        ));

        // Build the two vport runtimes with their own incoming channels.
        let (in0_tx, mut in0_rx) = mpsc::channel::<IncomingPacket>(16);
        let (in1_tx, mut in1_rx) = mpsc::channel::<IncomingPacket>(16);
        let vports = vec![
            VportRuntime {
                id: InterfaceId(10),
                name: "multi[low]".to_string(),
                vport: 0,
                radio: RadioParams {
                    frequency: 865_600_000,
                    bandwidth: 125_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    sf: 7,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in0_tx,
                counters: Arc::new(InterfaceCounters::new()),
            },
            VportRuntime {
                id: InterfaceId(11),
                name: "multi[high]".to_string(),
                vport: 1,
                radio: RadioParams {
                    frequency: 2_400_000_000,
                    bandwidth: 500_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    // SF8, not the SF5 this fixture used to carry: SF5 and SF6
                    // are refused by `validate_config` since #350, and the
                    // hub validates every vport's block before it configures
                    // the radio. Still one apart from the other vport's SF7,
                    // so the two blocks stay distinguishable.
                    sf: 8,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in1_tx,
                counters: Arc::new(InterfaceCounters::new()),
            },
        ];

        // A connector that yields the (single) duplex port on first call.
        let port_holder = std::sync::Mutex::new(Some(port));
        let connect = move || {
            let taken = port_holder.lock().unwrap().take();
            async move { taken.ok_or(RNodeError::NotDetected) }
        };

        let (merged_tx, merged_rx) = mpsc::channel::<TaggedOutgoing>(16);
        let hub = tokio::spawn(async move {
            rnode_multi_reconnect_task(
                "multi".to_string(),
                connect,
                vports,
                merged_rx,
                /* flow_control = */ false,
                None,
            )
            .await;
        });

        // (a) Both vports' frequencies were pushed under their own SEL_INT.
        let mut freqs = std::collections::HashMap::new();
        for _ in 0..2 {
            let (vport, hz) = tokio::time::timeout(Duration::from_secs(5), freq_rx.recv())
                .await
                .expect("frequency config must be pushed within 5s")
                .expect("freq channel open");
            freqs.insert(vport, hz);
        }
        assert_eq!(freqs.get(&0), Some(&865_600_000));
        assert_eq!(freqs.get(&1), Some(&2_400_000_000));

        // (b) A packet on vport 0's logical interface reaches the firmware
        // tagged vport 0, and (c) echoes back onto vport 0's incoming channel.
        merged_tx
            .send(TaggedOutgoing {
                subint: 0,
                packet: OutgoingPacket {
                    peer: None,
                    data: b"ping0".to_vec(),
                    high_priority: false,
                },
            })
            .await
            .expect("send to hub");

        let (rx_vport0, rx_data0) = tokio::time::timeout(Duration::from_secs(5), data_rx.recv())
            .await
            .expect("firmware must receive vport-0 frame within 5s")
            .expect("data channel open");
        assert_eq!(rx_vport0, 0, "TX frame must be tagged vport 0");
        assert_eq!(rx_data0, b"ping0");

        let echoed0 = tokio::time::timeout(Duration::from_secs(5), in0_rx.recv())
            .await
            .expect("echo must route to vport-0 interface within 5s")
            .expect("in0 channel open");
        assert_eq!(echoed0.data, b"ping0");

        // vport 1 must NOT have received vport 0's echo.
        assert!(
            in1_rx.try_recv().is_err(),
            "vport-0 echo must not leak into vport-1's interface"
        );

        // (b/c) repeat for vport 1 to prove the routing is per-vport, not fixed.
        merged_tx
            .send(TaggedOutgoing {
                subint: 1,
                packet: OutgoingPacket {
                    peer: None,
                    data: b"ping1".to_vec(),
                    high_priority: false,
                },
            })
            .await
            .expect("send to hub");

        let (rx_vport1, rx_data1) = tokio::time::timeout(Duration::from_secs(5), data_rx.recv())
            .await
            .expect("firmware must receive vport-1 frame within 5s")
            .expect("data channel open");
        assert_eq!(rx_vport1, 1, "TX frame must be tagged vport 1");
        assert_eq!(rx_data1, b"ping1");

        let echoed1 = tokio::time::timeout(Duration::from_secs(5), in1_rx.recv())
            .await
            .expect("echo must route to vport-1 interface within 5s")
            .expect("in1 channel open");
        assert_eq!(echoed1.data, b"ping1");
        assert!(
            in0_rx.try_recv().is_err(),
            "vport-1 echo must not leak into vport-0's interface"
        );

        hub.abort();
        stub.abort();
    }

    /// A subinterface with `outgoing = false` must never transmit: a packet
    /// queued on it is dropped at the hub, never reaching the firmware.
    #[tokio::test]
    async fn test_multi_vport_non_outgoing_drops_tx() {
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (freq_tx, mut freq_rx) = tokio::sync::mpsc::channel::<(u8, u32)>(8);
        let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(8);
        let stub = tokio::spawn(rnode_multi_firmware_stub(
            peer,
            vec![rnode::CHIP_SX127X],
            freq_tx,
            data_tx,
        ));

        let (in0_tx, _in0_rx) = mpsc::channel::<IncomingPacket>(16);
        let vports = vec![VportRuntime {
            id: InterfaceId(20),
            name: "multi[rxonly]".to_string(),
            vport: 0,
            radio: RadioParams {
                frequency: 868_000_000,
                bandwidth: 125_000,
                tx_power: 0,
                tx_power_derived: false,
                sf: 7,
                cr: 5,
                st_alock: None,
                lt_alock: None,
            },
            outgoing: false,
            incoming_tx: in0_tx,
            counters: Arc::new(InterfaceCounters::new()),
        }];

        let port_holder = std::sync::Mutex::new(Some(port));
        let connect = move || {
            let taken = port_holder.lock().unwrap().take();
            async move { taken.ok_or(RNodeError::NotDetected) }
        };
        let (merged_tx, merged_rx) = mpsc::channel::<TaggedOutgoing>(16);
        let hub = tokio::spawn(async move {
            rnode_multi_reconnect_task(
                "multi".to_string(),
                connect,
                vports,
                merged_rx,
                false,
                None,
            )
            .await;
        });

        // Wait until configured (frequency pushed) so the io loop is running.
        tokio::time::timeout(Duration::from_secs(5), freq_rx.recv())
            .await
            .expect("configure within 5s")
            .expect("freq channel");

        merged_tx
            .send(TaggedOutgoing {
                subint: 0,
                packet: OutgoingPacket {
                    peer: None,
                    data: b"nope".to_vec(),
                    high_priority: false,
                },
            })
            .await
            .expect("send to hub");

        // The non-outgoing vport must drop it: no CMD_DATA ever reaches the stub.
        let got = tokio::time::timeout(Duration::from_millis(500), data_rx.recv()).await;
        assert!(
            got.is_err(),
            "outgoing=false subinterface must not transmit, but firmware saw data"
        );

        hub.abort();
        stub.abort();
    }

    /// One vport's logical interface going away must not take the shared
    /// physical radio with it (Codeberg #283).
    ///
    /// Before the fix, the send failure on the dead vport's incoming channel
    /// returned from the whole io task. The reconnect loop then reopened the
    /// port (its stop condition needs *every* vport closed), the next frame for
    /// the same dead vport ended it again, and the surviving vports lost the
    /// radio every five seconds forever.
    ///
    /// The connector hands out the duplex port exactly once, so any reconnect
    /// leaves the surviving vport with a dead radio and the round trip below
    /// times out — the bounce cannot pass this test quietly.
    #[tokio::test]
    async fn test_multi_vport_one_dead_vport_does_not_bounce_the_radio() {
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (freq_tx, _freq_rx) = tokio::sync::mpsc::channel::<(u8, u32)>(8);
        let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(64);
        let stub = tokio::spawn(rnode_multi_firmware_stub(
            peer,
            vec![rnode::CHIP_SX127X, rnode::CHIP_SX128X],
            freq_tx,
            data_tx,
        ));

        let (in0_tx, mut in0_rx) = mpsc::channel::<IncomingPacket>(16);
        let (in1_tx, mut in1_rx) = mpsc::channel::<IncomingPacket>(16);
        let vports = vec![
            VportRuntime {
                id: InterfaceId(40),
                name: "multi[low]".to_string(),
                vport: 0,
                radio: RadioParams {
                    frequency: 865_600_000,
                    bandwidth: 125_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    sf: 7,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in0_tx,
                counters: Arc::new(InterfaceCounters::new()),
            },
            VportRuntime {
                id: InterfaceId(41),
                name: "multi[high]".to_string(),
                vport: 1,
                radio: RadioParams {
                    frequency: 2_400_000_000,
                    bandwidth: 500_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    // SF8, not the SF5 this fixture used to carry: SF5 and SF6
                    // are refused by `validate_config` since #350, and the
                    // hub validates every vport's block before it configures
                    // the radio. Still one apart from the other vport's SF7,
                    // so the two blocks stay distinguishable.
                    sf: 8,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in1_tx,
                counters: Arc::new(InterfaceCounters::new()),
            },
        ];

        let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opens_in_task = opens.clone();
        let port_holder = std::sync::Mutex::new(Some(port));
        let connect = move || {
            opens_in_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let taken = port_holder.lock().unwrap().take();
            async move { taken.ok_or(RNodeError::NotDetected) }
        };

        let (merged_tx, merged_rx) = mpsc::channel::<TaggedOutgoing>(16);
        let hub = tokio::spawn(async move {
            rnode_multi_reconnect_task(
                "multi".to_string(),
                connect,
                vports,
                merged_rx,
                false,
                None,
            )
            .await;
        });

        // A full round trip on vport 0 proves the io loop is running before
        // its receiver is dropped — the deregistration must come from the send
        // failure, not from the state the loop was seeded with.
        let ping = |subint: usize, data: &'static [u8]| {
            let merged_tx = merged_tx.clone();
            async move {
                merged_tx
                    .send(TaggedOutgoing {
                        subint,
                        packet: OutgoingPacket {
                            peer: None,
                            data: data.to_vec(),
                            high_priority: false,
                        },
                    })
                    .await
                    .expect("send to hub");
            }
        };
        ping(0, b"warmup").await;
        let echoed = tokio::time::timeout(Duration::from_secs(5), in0_rx.recv())
            .await
            .expect("vport-0 echo within 5s")
            .expect("in0 open");
        assert_eq!(echoed.data, b"warmup");

        // vport 0's logical interface is torn down.
        drop(in0_rx);

        // A frame for the now-dead vport: the send fails and the vport is
        // deregistered. Before the fix, this returned from the io task.
        ping(0, b"to-the-dead").await;
        let (vp, _) = tokio::time::timeout(Duration::from_secs(5), data_rx.recv())
            .await
            .expect("firmware sees the vport-0 frame within 5s")
            .expect("data channel open");
        assert_eq!(vp, 0);

        // The surviving vport still has the radio. This is the assertion the
        // bug fails: after a bounce the connector has no port left to hand out.
        ping(1, b"still-here").await;
        let alive = tokio::time::timeout(Duration::from_secs(5), in1_rx.recv())
            .await
            .expect("vport-1 must keep the radio after vport-0 died")
            .expect("in1 open");
        assert_eq!(alive.data, b"still-here");

        // A second frame for the dead vport must be dropped where it arrives,
        // not rediscovered as a fresh teardown.
        ping(0, b"still-dead").await;
        ping(1, b"and-still-here").await;
        let alive = tokio::time::timeout(Duration::from_secs(5), in1_rx.recv())
            .await
            .expect("vport-1 must survive a second frame for the dead vport")
            .expect("in1 open");
        assert_eq!(alive.data, b"and-still-here");

        assert_eq!(
            opens.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the shared radio must be opened once; a reconnect is the #283 bounce"
        );

        // When the LAST vport goes too, the hub stops on its own rather than
        // reconnecting forever: the all-closed exit stays reachable.
        drop(in1_rx);
        ping(1, b"final").await;
        tokio::time::timeout(Duration::from_secs(5), hub)
            .await
            .expect("hub must stop once every vport is gone")
            .expect("hub task must not panic");

        stub.abort();
    }

    /// The multi-vport twin of behaviour 3 (Codeberg #319): the shared
    /// hub's host-side queue overflow must be exactly as loud as the
    /// single-radio one — every shed frame counted, and counted on the
    /// vport that OWNED the frame (the queue is vport-tagged), while the
    /// `RNODE_TX_QUEUE_DROP` event names the physical interface, because
    /// the port that dropped is a property of the shared line. The gate
    /// closes after the first TX and stays closed because the stub never
    /// answers a CMD_READY query; the queue is then filled past the cap
    /// with vport-1 frames and overflowed by vport-0 pushes, so the shed
    /// oldest frames all belong to vport 1 — pinning the attribution.
    #[tokio::test(start_paused = true)]
    async fn test_multi_vport_host_queue_overflow_is_loud() {
        let (logs, guard) = capture_logs();
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (freq_tx, mut freq_rx) = tokio::sync::mpsc::channel::<(u8, u32)>(8);
        let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(256);
        let stub = tokio::spawn(rnode_multi_firmware_stub(
            peer,
            vec![rnode::CHIP_SX127X, rnode::CHIP_SX128X],
            freq_tx,
            data_tx,
        ));

        // Keep both incoming receivers alive: the stub echoes CMD_DATA, and
        // a dropped receiver would end the io task via `incoming_closed`.
        let (in0_tx, _in0_rx) = mpsc::channel::<IncomingPacket>(256);
        let (in1_tx, _in1_rx) = mpsc::channel::<IncomingPacket>(256);
        let counters0 = Arc::new(InterfaceCounters::new());
        let counters1 = Arc::new(InterfaceCounters::new());
        let vports = vec![
            VportRuntime {
                id: InterfaceId(30),
                name: "multi[low]".to_string(),
                vport: 0,
                radio: RadioParams {
                    frequency: 865_600_000,
                    bandwidth: 125_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    sf: 7,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in0_tx,
                counters: Arc::clone(&counters0),
            },
            VportRuntime {
                id: InterfaceId(31),
                name: "multi[high]".to_string(),
                vport: 1,
                radio: RadioParams {
                    frequency: 2_400_000_000,
                    bandwidth: 500_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    // SF8, not the SF5 this fixture used to carry: SF5 and SF6
                    // are refused by `validate_config` since #350, and the
                    // hub validates every vport's block before it configures
                    // the radio. Still one apart from the other vport's SF7,
                    // so the two blocks stay distinguishable.
                    sf: 8,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in1_tx,
                counters: Arc::clone(&counters1),
            },
        ];

        let port_holder = std::sync::Mutex::new(Some(port));
        let connect = move || {
            let taken = port_holder.lock().unwrap().take();
            async move { taken.ok_or(RNodeError::NotDetected) }
        };
        let (merged_tx, merged_rx) = mpsc::channel::<TaggedOutgoing>(16);
        let hub = tokio::spawn(async move {
            rnode_multi_reconnect_task(
                "multi".to_string(),
                connect,
                vports,
                merged_rx,
                /* flow_control = */ true,
                None,
            )
            .await;
        });

        // Wait until both vports are configured so the io loop is running.
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), freq_rx.recv())
                .await
                .expect("frequency config must be pushed within 5s")
                .expect("freq channel open");
        }

        // The bootstrap frame ships (the gate starts open); its post-TX
        // CMD_READY query goes unanswered, so the gate stays closed for good.
        merged_tx
            .send(TaggedOutgoing {
                subint: 0,
                packet: OutgoingPacket {
                    peer: None,
                    data: b"bootstrap".to_vec(),
                    high_priority: false,
                },
            })
            .await
            .expect("send to hub");
        let (_, boot) = tokio::time::timeout(Duration::from_secs(5), data_rx.recv())
            .await
            .expect("bootstrap frame ships ungated")
            .expect("data channel open");
        assert_eq!(boot, b"bootstrap");

        // Fill the held queue to the cap with vport-1 frames, then overflow
        // it with vport-0 pushes: each sheds the oldest held frame, all of
        // which are vport 1's.
        const EXCESS: usize = 5;
        for i in 0..FLOW_CONTROL_QUEUE_LIMIT {
            merged_tx
                .send(TaggedOutgoing {
                    subint: 1,
                    packet: OutgoingPacket {
                        peer: None,
                        data: format!("v1-{i:03}").into_bytes(),
                        high_priority: false,
                    },
                })
                .await
                .expect("send to hub");
        }
        for i in 0..EXCESS {
            merged_tx
                .send(TaggedOutgoing {
                    subint: 0,
                    packet: OutgoingPacket {
                        peer: None,
                        data: format!("v0-{i:03}").into_bytes(),
                        high_priority: false,
                    },
                })
                .await
                .expect("send to hub");
        }
        // Let the hub drain the merged channel into its send queue.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            counters1
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            EXCESS as u64,
            "every shed frame must be counted on the vport that owned it"
        );
        assert_eq!(
            counters0
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the vport whose push caused the shed lost nothing of its own"
        );

        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_eq!(
            count_event(&logs, "RNODE_TX_QUEUE_DROP"),
            EXCESS,
            "each shed frame must be named by a structured event\n--- logs ---\n{logs}"
        );
        let line = logs
            .lines()
            .find(|l| l.contains("event=\"RNODE_TX_QUEUE_DROP\""))
            .expect("checked non-zero above");
        for key in ["iface=multi", "len=", "depth=", "reason=\"queue_full\""] {
            assert!(
                line.contains(key),
                "drop event must carry {key}; line: {line}"
            );
        }

        hub.abort();
        stub.abort();
    }

    /// Everything a multi-vport flow-control test needs (Codeberg #317):
    /// two vports on one shared serial line, the hub under test, and a
    /// scripted stub that models the single firmware queue both vports
    /// feed. The vports carry deliberately different PHYs — vport 0 at
    /// SF7/125 kHz is the slower packet, vport 1 at SF8/500 kHz the faster
    /// — so a poll cadence seeded from the wrong vport is visible. Time is
    /// `start_paused`, so the airtime-seeded cadence costs no wall clock.
    struct MultiFlowHarness {
        merged_tx: mpsc::Sender<TaggedOutgoing>,
        /// `(vport, payload)` for every CMD_DATA the stub received.
        data_rx: tokio::sync::mpsc::Receiver<(u8, Vec<u8>)>,
        /// Lifts the duty lock: `send(n)` gives the modem n more air slots.
        resume_tx: mpsc::Sender<usize>,
        /// CMD_READY queries the stub has answered.
        ready_queries: Arc<std::sync::atomic::AtomicUsize>,
        hub: tokio::task::JoinHandle<()>,
        stub: tokio::task::JoinHandle<()>,
        /// Held so the hub's `incoming_tx.send` of the stub's echo never
        /// fails mid-test and tears a vport down.
        _incoming_rx: Vec<mpsc::Receiver<IncomingPacket>>,
    }

    impl Drop for MultiFlowHarness {
        fn drop(&mut self) {
            self.hub.abort();
            self.stub.abort();
        }
    }

    /// The two vports the flow-control harness runs, in `subint` order:
    /// index 0 is vport 0 (sub-GHz, SF7/125 kHz), index 1 is vport 1
    /// (2.4 GHz, SF8/500 kHz).
    fn multi_flow_vports(
        in0_tx: mpsc::Sender<IncomingPacket>,
        in1_tx: mpsc::Sender<IncomingPacket>,
    ) -> Vec<VportRuntime> {
        vec![
            VportRuntime {
                id: InterfaceId(40),
                name: "multi[low]".to_string(),
                vport: 0,
                radio: RadioParams {
                    frequency: 865_600_000,
                    bandwidth: 125_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    sf: 7,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in0_tx,
                counters: Arc::new(InterfaceCounters::new()),
            },
            VportRuntime {
                id: InterfaceId(41),
                name: "multi[high]".to_string(),
                vport: 1,
                radio: RadioParams {
                    frequency: 2_400_000_000,
                    bandwidth: 500_000,
                    tx_power: 0,
                    tx_power_derived: false,
                    sf: 8,
                    cr: 5,
                    st_alock: None,
                    lt_alock: None,
                },
                outgoing: true,
                incoming_tx: in1_tx,
                counters: Arc::new(InterfaceCounters::new()),
            },
        ]
    }

    /// Bring the harness up and return once both vports are configured, so
    /// the shared io loop is running and every later frame is a TX under
    /// test rather than a config echo.
    async fn spawn_multi_flow_harness(flow_control: bool, air_budget: usize) -> MultiFlowHarness {
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (freq_tx, mut freq_rx) = tokio::sync::mpsc::channel::<(u8, u32)>(8);
        let (data_tx, data_rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(256);
        let (resume_tx, resume_rx) = mpsc::channel::<usize>(16);
        let ready_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let stub = tokio::spawn(rnode_multi_firmware_stub_scripted(
            peer,
            vec![rnode::CHIP_SX127X, rnode::CHIP_SX128X],
            freq_tx,
            data_tx,
            Some(MultiStubFlow {
                air_budget,
                resume_rx,
                ready_queries: Arc::clone(&ready_queries),
            }),
        ));

        let (in0_tx, in0_rx) = mpsc::channel::<IncomingPacket>(256);
        let (in1_tx, in1_rx) = mpsc::channel::<IncomingPacket>(256);
        let vports = multi_flow_vports(in0_tx, in1_tx);

        let port_holder = std::sync::Mutex::new(Some(port));
        let connect = move || {
            let taken = port_holder.lock().unwrap().take();
            async move { taken.ok_or(RNodeError::NotDetected) }
        };
        let (merged_tx, merged_rx) = mpsc::channel::<TaggedOutgoing>(64);
        let hub = tokio::spawn(async move {
            rnode_multi_reconnect_task(
                "multi".to_string(),
                connect,
                vports,
                merged_rx,
                flow_control,
                None,
            )
            .await;
        });

        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), freq_rx.recv())
                .await
                .expect("frequency config must be pushed within 5s")
                .expect("freq channel open");
        }

        MultiFlowHarness {
            merged_tx,
            data_rx,
            resume_tx,
            ready_queries,
            hub,
            stub,
            _incoming_rx: vec![in0_rx, in1_rx],
        }
    }

    impl MultiFlowHarness {
        async fn push(&self, subint: usize, payload: &[u8]) {
            self.merged_tx
                .send(TaggedOutgoing {
                    subint,
                    packet: OutgoingPacket {
                        peer: None,
                        data: payload.to_vec(),
                        high_priority: false,
                    },
                })
                .await
                .expect("hub alive");
        }

        async fn expect_frame(&mut self, vport: u8, want: &[u8], ctx: &str) {
            let (got_vport, got) =
                tokio::time::timeout(Duration::from_secs(5), self.data_rx.recv())
                    .await
                    .unwrap_or_else(|_| {
                        panic!("{ctx}: expected {want:?} on vport {vport}, stub saw nothing")
                    })
                    .expect("stub data channel open");
            assert_eq!((got_vport, got.as_slice()), (vport, want), "{ctx}");
        }

        /// Assert the stub sees no CMD_DATA for `window`.
        async fn expect_silence(&mut self, window: Duration, ctx: &str) {
            if let Ok(Some(got)) = tokio::time::timeout(window, self.data_rx.recv()).await {
                panic!("{ctx}: stub must see no CMD_DATA, saw {got:?}");
            }
        }

        fn queries(&self) -> usize {
            self.ready_queries
                .load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Advance virtual time until the stub has answered `n` queries.
        /// Bounded well below the fast vport's airtime, so a host that never
        /// asks fails the assertion instead of hanging — and so the wait
        /// itself cannot burn a re-query window.
        async fn wait_for_queries(&self, n: usize) {
            for _ in 0..50 {
                if self.queries() >= n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            panic!(
                "stub answered {} CMD_READY queries, expected at least {n}",
                self.queries()
            );
        }
    }

    /// The multi-vport twin of the reopen contract: the firmware answers
    /// CMD_READY only when asked, so a hub that waits for a spontaneous
    /// READY starves here. Budget 0 jams the shared queue on the first
    /// frame; `resume` drains it silently, and only a query can learn that
    /// there is room again. The second frame is pushed on the OTHER vport,
    /// which pins the second half of the contract: one gate serves the
    /// whole device, so vport 1 is released by the answer to the query
    /// vport 0's TX provoked.
    #[tokio::test(start_paused = true)]
    async fn test_multi_vport_flow_control_reopen_is_learned_by_query() {
        let mut h = spawn_multi_flow_harness(true, 0).await;
        h.push(0, b"first").await;
        h.expect_frame(0, b"first", "cold-start frame ships ungated")
            .await;

        h.resume_tx.send(1).await.expect("stub alive");
        h.push(1, b"second").await;
        h.expect_frame(
            1,
            b"second",
            "the shared gate must reopen via a CMD_READY query — the firmware never volunteers READY",
        )
        .await;
        assert!(
            h.queries() > 0,
            "the reopen must have been learned through a CMD_READY query"
        );
    }

    /// Lock semantics on the shared line: the stub airs three frames, the
    /// fourth ships and jams the one firmware queue, and from then on the
    /// 0x00 answers must hold everything host-side — whichever vport owns
    /// the frame. The pushes alternate vports so a per-vport gate (which
    /// the firmware has no room for: one queue, one modem) would leak the
    /// held frames through the other vport. After the lock lifts, the held
    /// frames drain in submission order with their vport tags intact.
    #[tokio::test(start_paused = true)]
    async fn test_multi_vport_flow_control_duty_lock_holds_frames_host_side() {
        let mut h = spawn_multi_flow_harness(true, 3).await;
        let script: [(usize, &[u8]); 6] = [
            (0, b"m1"),
            (1, b"m2"),
            (0, b"m3"),
            (1, b"m4"),
            (0, b"m5"),
            (1, b"m6"),
        ];
        for (subint, payload) in script {
            h.push(subint, payload).await;
        }
        for (subint, payload) in &script[..4] {
            h.expect_frame(*subint as u8, payload, "pre-lock frames flow")
                .await;
        }
        h.expect_silence(Duration::from_secs(5), "m5/m6 held under the lock")
            .await;

        h.resume_tx.send(1000).await.expect("stub alive");
        h.expect_frame(0, b"m5", "held frames drain in order after release")
            .await;
        h.expect_frame(1, b"m6", "held frames drain in order after release")
            .await;
    }

    /// One firmware queue, one poll cadence — and it must be priced at the
    /// SLOWEST vport's packet airtime, the conservative bound on how fast
    /// the shared queue can drain. Vport 1 (SF8/500 kHz) is the faster
    /// radio here; a cadence seeded from it would re-query roughly twice as
    /// often as the shared line can justify, on a line the modem also needs
    /// for RX delivery.
    #[tokio::test(start_paused = true)]
    async fn test_multi_vport_ready_poll_is_seeded_by_the_slowest_vport() {
        let slow = ready_poll_initial(7, 5, 125_000);
        let fast = ready_poll_initial(8, 5, 500_000);
        assert!(
            fast < slow,
            "fixture: vport 1 must be the faster radio ({fast:?} vs {slow:?})"
        );

        let mut h = spawn_multi_flow_harness(true, 0).await;
        h.push(0, b"seed").await;
        h.expect_frame(0, b"seed", "cold-start frame ships ungated")
            .await;
        // Budget 0: the post-TX query is answered 0x00, the gate stays shut
        // and the re-query timer is armed with the seed under test.
        h.wait_for_queries(1).await;

        tokio::time::sleep(fast + (slow - fast) / 2).await;
        assert_eq!(
            h.queries(),
            1,
            "a cadence seeded from the fast vport would have re-queried by now"
        );

        tokio::time::sleep(slow).await;
        assert_eq!(
            h.queries(),
            2,
            "past the slowest vport's airtime the hub must re-query"
        );
    }

    /// Off means off on the shared line too: with `flow_control = false`
    /// every frame ships in order even though the modelled firmware queue
    /// is jammed from the first frame, and the hub puts no CMD_READY query
    /// on the serial line at all. The default does not change.
    #[tokio::test(start_paused = true)]
    async fn test_multi_vport_flow_control_off_never_queries() {
        let mut h = spawn_multi_flow_harness(false, 0).await;
        let script: [(usize, &[u8]); 4] = [(0, b"n1"), (1, b"n2"), (0, b"n3"), (1, b"n4")];
        for (subint, payload) in script {
            h.push(subint, payload).await;
        }
        for (subint, payload) in &script {
            h.expect_frame(
                *subint as u8,
                payload,
                "flow_control=false ships every frame without READY",
            )
            .await;
        }
        // Give a wrongly-armed gate timer ample time to fire.
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(
            h.queries(),
            0,
            "flow_control off must put no CMD_READY query on the serial line"
        );
    }

    /// A modem that answers nothing keeps the gate closed: with
    /// `flow_control = true` against a peer that never responds to the
    /// CMD_READY queries (dead serial, wedged firmware), the io task ships
    /// exactly one frame and then holds the rest host-side. That is the
    /// safe contract — a silent modem must not be flooded on hope; the
    /// re-query poll keeps asking at a bounded cadence and the reconnect
    /// path owns recovery from a truly dead device.
    #[tokio::test]
    async fn test_flow_control_without_cmd_ready_stalls_after_first_frame() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ true,
                test_channel_access(),
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        let payloads: [&[u8]; 3] = [b"alpha", b"bravo", b"charlie"];
        for p in payloads.iter() {
            outgoing_tx
                .send(OutgoingPacket {
                    peer: None,
                    data: p.to_vec(),
                    high_priority: false,
                })
                .await
                .expect("send to io task");
        }

        // Generous read window. The first frame must arrive; any later
        // frame would mean the gate opened without a queue-not-full answer.
        // The CMD_READY queries the io task sends land in `frames` too and
        // are filtered out below — only CMD_DATA counts.
        let frames = drain_kiss_frames(&mut peer, Duration::from_millis(500)).await;
        let data_frames: Vec<&Vec<u8>> = frames
            .iter()
            .filter(|(c, _)| *c == rnode::CMD_DATA)
            .map(|(_, p)| p)
            .collect();

        assert_eq!(
            data_frames.len(),
            1,
            "exactly one frame must reach the wire; the others must remain queued \
             waiting for CMD_READY (got {} CMD_DATA frames)",
            data_frames.len()
        );
        assert_eq!(
            data_frames[0].as_slice(),
            payloads[0],
            "the single TX must be the first queued payload"
        );

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            tx_bytes,
            payloads[0].len() as u64,
            "tx_bytes must reflect exactly the first payload"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    // -----------------------------------------------------------------------
    // CMD_READY flow control under a firmware duty lock
    // -----------------------------------------------------------------------

    /// Scripted firmware stub at the io-task boundary for the duty-lock
    /// flow-control tests.
    ///
    /// Reads KISS frames from its half of the duplex and records every
    /// `CMD_DATA` payload on `data_tx`. TX model: a one-deep firmware queue
    /// behind a scripted airtime budget — a frame received while
    /// `air_budget > 0` airs at once (budget decrements), a frame received
    /// at budget zero stays in the queue, which is the duty-lock shape
    /// (serial still drains into the queue, but no TX completes,
    /// `RNode_Firmware.ino:1624`). A budget increment on `resume_rx` models
    /// the lock releasing: queued frames air against the refreshed budget —
    /// silently.
    ///
    /// The stub speaks `CMD_READY` **only as the response to a host
    /// CMD_READY query**, exactly like the firmware
    /// (`RNode_Firmware.ino:1003-1008`): 0x01 while its queue has room,
    /// 0x00 while it is full. `kiss_indicate_ready`/`_not_ready`
    /// (`Utilities.h:1157,1164`) have no other call sites, so a spontaneous
    /// READY — after a TX, on queue drain, ever — would be dishonest. The
    /// previous stub's READY-follows-each-TX script was exactly the wrong
    /// protocol model that let six green tests hide the flowval leg-B
    /// deadlock (2026-08-21): an implementation that waits for an
    /// unsolicited READY starves against this stub, which is the point.
    ///
    /// `end_rx` scripts the disconnect: the stub either drops its half of
    /// the duplex (the io task reads `Ok(0)`, a port that went away) or
    /// announces a firmware reset — the two return paths the held-frame
    /// tests drive (Codeberg #316).
    async fn rnode_firmware_stub_scripted(
        mut peer: tokio::io::DuplexStream,
        mut air_budget: usize,
        mut resume_rx: mpsc::Receiver<usize>,
        mut end_rx: mpsc::Receiver<StubEnd>,
        data_tx: mpsc::Sender<Vec<u8>>,
        ready_queries: Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut buf = [0u8; 1024];
        // Frames sitting in the firmware queue while the lock holds TX.
        // One-deep model: any held frame means the queue is full.
        let mut queued: usize = 0;
        loop {
            tokio::select! {
                read = peer.read(&mut buf) => {
                    let n = match read {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    for f in deframer.process(&buf[..n]) {
                        if let KissDeframeResult::Frame { command, payload } = f {
                            match command {
                                rnode::CMD_DATA => {
                                    if data_tx.send(payload.to_vec()).await.is_err() {
                                        return;
                                    }
                                    if air_budget > 0 {
                                        air_budget -= 1;
                                    } else {
                                        queued += 1;
                                    }
                                    // No READY here: the firmware does not
                                    // announce TX completion.
                                }
                                rnode::CMD_READY => {
                                    ready_queries.fetch_add(
                                        1,
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                    let mut resp = Vec::new();
                                    kiss::frame(
                                        rnode::CMD_READY,
                                        &[if queued > 0 { 0x00 } else { 0x01 }],
                                        &mut resp,
                                    );
                                    if peer.write_all(&resp).await.is_err() {
                                        return;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                resumed = resume_rx.recv() => {
                    match resumed {
                        Some(n) => {
                            // Lock released: the queue drains against the
                            // refreshed budget. Nothing is announced — only
                            // the next query learns about the room.
                            air_budget += n;
                            while queued > 0 && air_budget > 0 {
                                queued -= 1;
                                air_budget -= 1;
                            }
                        }
                        None => return,
                    }
                }
                end = end_rx.recv() => {
                    match end {
                        // Returning drops `peer`, so the io task's next read
                        // yields `Ok(0)` — the port went away mid-gate.
                        Some(StubEnd::Eof) | None => return,
                        Some(StubEnd::DeviceReset) => {
                            let mut resp = Vec::new();
                            kiss::frame(rnode::CMD_RESET, &[DEVICE_RESET_MARKER], &mut resp);
                            if peer.write_all(&resp).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Everything a duty-lock test needs: `rnode_io_task` under test, wired
    /// to [`rnode_firmware_stub_scripted`] over an in-memory duplex. All
    /// timing is driven by `start_paused` tokio time, so the 2 s gated
    /// threshold and multi-second hold windows cost no wall clock.
    /// How a test ends the scripted stub's session.
    #[derive(Clone, Copy, Debug)]
    enum StubEnd {
        /// The port goes away: the stub drops the duplex, the io task reads
        /// `Ok(0)`.
        Eof,
        /// The firmware announces a reset (`CMD_RESET` 0xF8) on an otherwise
        /// healthy port.
        DeviceReset,
    }

    struct DutyLockHarness {
        outgoing_tx: mpsc::Sender<OutgoingPacket>,
        counters: Arc<InterfaceCounters>,
        resume_tx: mpsc::Sender<usize>,
        /// Scripts the disconnect (see [`StubEnd`]).
        end_tx: mpsc::Sender<StubEnd>,
        data_rx: mpsc::Receiver<Vec<u8>>,
        /// CMD_READY queries the stub has answered — proves the host asks.
        ready_queries: Arc<std::sync::atomic::AtomicUsize>,
        /// Held so the io task's `incoming_tx.send` never fails mid-test.
        _incoming_rx: mpsc::Receiver<IncomingPacket>,
    }

    fn spawn_duty_lock_harness(flow_control: bool, air_budget: usize) -> DutyLockHarness {
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (incoming_tx, incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let (resume_tx, resume_rx) = mpsc::channel::<usize>(16);
        let (end_tx, end_rx) = mpsc::channel::<StubEnd>(4);
        let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(256);
        let counters = Arc::new(InterfaceCounters::new());
        let ready_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        tokio::spawn(rnode_firmware_stub_scripted(
            peer,
            air_budget,
            resume_rx,
            end_rx,
            data_tx,
            Arc::clone(&ready_queries),
        ));
        let task_counters = Arc::clone(&counters);
        tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_duty".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                flow_control,
                test_channel_access(),
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        DutyLockHarness {
            outgoing_tx,
            counters,
            resume_tx,
            end_tx,
            data_rx,
            ready_queries,
            _incoming_rx: incoming_rx,
        }
    }

    impl DutyLockHarness {
        async fn push(&self, payload: &[u8]) {
            self.outgoing_tx
                .send(OutgoingPacket {
                    peer: None,
                    data: payload.to_vec(),
                    high_priority: false,
                })
                .await
                .expect("io task alive");
        }

        async fn expect_frame(&mut self, want: &[u8], ctx: &str) {
            let got = tokio::time::timeout(Duration::from_secs(2), self.data_rx.recv())
                .await
                .unwrap_or_else(|_| panic!("{ctx}: expected frame {want:?}, stub saw nothing"))
                .expect("stub data channel open");
            assert_eq!(got, want, "{ctx}");
        }

        /// Push `count` frames while the gate is closed, then let the io task
        /// pull them out of the channel into its task-local send queue —
        /// which is exactly the queue a disconnect abandons.
        async fn hold_frames(&self, count: usize) {
            for i in 0..count {
                self.push(format!("h{i:03}").as_bytes()).await;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        /// Script the stub's disconnect and give the io task time to see it.
        async fn end_session(&self, end: StubEnd) {
            self.end_tx.send(end).await.expect("stub alive");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        /// Assert the stub sees no `CMD_DATA` for `window`. The serial-only
        /// `CMD_DETECT` heartbeat is not TX data and is exempt by design.
        async fn expect_silence(&mut self, window: Duration, ctx: &str) {
            if let Ok(Some(got)) = tokio::time::timeout(window, self.data_rx.recv()).await {
                panic!("{ctx}: stub must see no CMD_DATA, saw {got:?}");
            }
        }
    }

    fn count_event(logs: &str, event: &str) -> usize {
        let needle = format!("event=\"{event}\"");
        logs.lines().filter(|l| l.contains(&needle)).count()
    }

    /// Behaviour 1, lock semantics: the stub airs the first three frames
    /// (each post-TX query answers 0x01), then the duty lock holds TX
    /// (no completion, `RNode_Firmware.ino:1624`). The fourth frame still
    /// ships — it is what fills the firmware queue — and its query answers
    /// 0x00, so with `flow_control = true` the host must hold frames five
    /// and six on its side; they must never appear at the stub.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_duty_lock_holds_frames_host_side() {
        let mut h = spawn_duty_lock_harness(true, 3);
        let payloads: [&[u8]; 6] = [b"f1", b"f2", b"f3", b"f4", b"f5", b"f6"];
        for p in payloads {
            h.push(p).await;
        }
        for p in &payloads[..4] {
            h.expect_frame(p, "frames up to the one that fills the firmware queue flow")
                .await;
        }
        h.expect_silence(
            Duration::from_secs(5),
            "once the queue is full every poll answers 0x00, \
             so the remaining frames are held host-side",
        )
        .await;
    }

    /// Behaviour 2, bootstrap: the firmware never speaks CMD_READY
    /// unsolicited, so from a cold start with `flow_control = true` the
    /// first frame must go out without asking anything (the gate starts
    /// open; there is no queue state worth asking about before the first
    /// TX). Here the duty lock already holds TX, so that frame jams the
    /// one-deep firmware queue, the post-TX query answers 0x00, and the
    /// second frame waits host-side until a poll learns the queue drained.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_bootstrap_first_frame_needs_no_ready() {
        let mut h = spawn_duty_lock_harness(true, 0);
        h.push(b"first").await;
        h.push(b"second").await;
        h.expect_frame(
            b"first",
            "cold start: the first frame must ship without any READY exchange",
        )
        .await;
        h.expect_silence(
            Duration::from_secs(3),
            "the second frame must wait while every poll answers 0x00",
        )
        .await;
        h.resume_tx.send(1).await.expect("stub alive");
        h.expect_frame(b"second", "the next poll after the drain re-opens the gate")
            .await;
    }

    /// Behaviour 3, host-queue overflow is loud: with the gate
    /// closed, frames past `FLOW_CONTROL_QUEUE_LIMIT` are dropped oldest-
    /// first. Required: every drop is counted on the interface counters and
    /// named by a structured `RNODE_TX_QUEUE_DROP` event — a silent drop is
    /// exactly the black-hole failure this batch exists to prevent.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_host_queue_overflow_is_loud() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(true, 0);
        h.push(b"bootstrap").await;
        h.expect_frame(b"bootstrap", "bootstrap frame ships ungated")
            .await;

        // The gate is now closed for good (every poll answers 0x00).
        // Overfill the host-side queue past its cap.
        const EXCESS: usize = 5;
        for i in 0..(FLOW_CONTROL_QUEUE_LIMIT + EXCESS) {
            h.push(format!("q{i:03}").as_bytes()).await;
        }
        // Let the io task drain the channel into its send queue.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            h.counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            EXCESS as u64,
            "every dropped frame must be counted on the interface counters"
        );
        h.expect_silence(
            Duration::from_millis(500),
            "nothing may leak to the stub while gated",
        )
        .await;

        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_eq!(
            count_event(&logs, "RNODE_TX_QUEUE_DROP"),
            EXCESS,
            "each drop must be named by a structured event\n--- logs ---\n{logs}"
        );
        let line = logs
            .lines()
            .find(|l| l.contains("event=\"RNODE_TX_QUEUE_DROP\""))
            .expect("checked non-zero above");
        for key in ["iface=test_rnode_duty", "len=", "depth="] {
            assert!(
                line.contains(key),
                "drop event must carry {key}; line: {line}"
            );
        }
    }

    /// Behaviour 4, gated-too-long visibility: while the gate
    /// holds queued frames beyond `TX_GATED_EVENT_AFTER` (one CHTM cadence,
    /// 2 s — a READY normally follows a TX within one packet airtime, and a
    /// gate still closed after a full firmware stat cadence is the duty-lock
    /// shape, not airtime wait), the io task emits `RNODE_TX_GATED` with the
    /// held duration and queue depth, repeated every `TX_GATED_EVENT_REPEAT`
    /// (10 s) — bounded, not per loop iteration. Held 25 s ⇒ exactly three
    /// events (t = 2, 12, 22 s), deterministic under paused time.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_gated_too_long_emits_bounded_events() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(true, 0);
        h.push(b"bootstrap").await;
        h.expect_frame(b"bootstrap", "bootstrap frame ships ungated")
            .await;
        h.push(b"held").await;

        tokio::time::sleep(Duration::from_secs(25)).await;

        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_eq!(
            count_event(&logs, "RNODE_TX_GATED"),
            3,
            "held 25 s ⇒ events at 2/12/22 s, repeated at a bounded rate\n--- logs ---\n{logs}"
        );
        let line = logs
            .lines()
            .find(|l| l.contains("event=\"RNODE_TX_GATED\""))
            .expect("checked non-zero above");
        for key in ["iface=test_rnode_duty", "held_ms=", "depth=1"] {
            assert!(
                line.contains(key),
                "gated event must carry {key}; line: {line}"
            );
        }
    }

    /// Behaviour 5, release: the duty lock lifts, the queue drains, the
    /// next poll answers 0x01, the held frames drain in order, and one
    /// `RNODE_TX_RELEASED` closes the pair opened by `RNODE_TX_GATED`. The
    /// post-release drain re-closes the gate only for sub-threshold
    /// moments, so no further gated/release events may fire.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_release_drains_in_order_and_pairs_events() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(true, 2);
        let payloads: [&[u8]; 5] = [b"r1", b"r2", b"r3", b"r4", b"r5"];
        for p in payloads {
            h.push(p).await;
        }
        // Budget 2: r1/r2 air, r3 ships into the locked firmware and jams
        // its queue; r4/r5 are held host-side by the 0x00 poll answers.
        for p in &payloads[..3] {
            h.expect_frame(p, "pre-lock frames flow").await;
        }
        // 5 s > the 2 s threshold: the gated event must have fired before
        // the release below closes the pair.
        h.expect_silence(Duration::from_secs(5), "r4/r5 held under the lock")
            .await;

        h.resume_tx.send(1000).await.expect("stub alive");
        h.expect_frame(b"r4", "held frames drain in order after release")
            .await;
        h.expect_frame(b"r5", "held frames drain in order after release")
            .await;

        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_eq!(
            count_event(&logs, "RNODE_TX_GATED"),
            1,
            "one gated event during the 5 s hold\n--- logs ---\n{logs}"
        );
        assert_eq!(
            count_event(&logs, "RNODE_TX_RELEASED"),
            1,
            "exactly one release event closes the pair\n--- logs ---\n{logs}"
        );
        let line = logs
            .lines()
            .find(|l| l.contains("event=\"RNODE_TX_RELEASED\""))
            .expect("checked non-zero above");
        for key in ["iface=test_rnode_duty", "held_ms=", "depth=2"] {
            assert!(
                line.contains(key),
                "release event must carry {key}; line: {line}"
            );
        }
    }

    /// Behaviour 6, off means off: with `flow_control = false`
    /// (the default) every frame ships without any READY exchange, in
    /// order, and none of the duty-lock machinery speaks — no gated,
    /// release, or queue-drop events, and no CMD_READY queries on the
    /// serial line either. The default does not change in this batch.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_off_never_gates_and_stays_silent() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(false, 0);
        let payloads: [&[u8]; 4] = [b"n1", b"n2", b"n3", b"n4"];
        for p in payloads {
            h.push(p).await;
        }
        for p in &payloads {
            h.expect_frame(p, "flow_control=false ships every frame without READY")
                .await;
        }
        // Give a wrongly-armed gate timer ample time to fire before reading
        // the captured logs.
        tokio::time::sleep(Duration::from_secs(30)).await;

        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        for event in ["RNODE_TX_GATED", "RNODE_TX_RELEASED", "RNODE_TX_QUEUE_DROP"] {
            assert_eq!(
                count_event(&logs, event),
                0,
                "{event} must be absent with flow_control off\n--- logs ---\n{logs}"
            );
        }
        assert_eq!(
            h.ready_queries.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "flow_control off must put no CMD_READY queries on the serial line"
        );
    }

    /// Find the single `RNODE_TX_QUEUE_DROP` line in `logs` and assert it
    /// carries exactly the abandon shape for `reason`, with `frames` frames.
    fn assert_single_abandon_event(logs: &str, reason: &str, frames: usize) {
        assert_eq!(
            count_event(logs, "RNODE_TX_QUEUE_DROP"),
            1,
            "one summary event per return path, never one per frame\
             \n--- logs ---\n{logs}"
        );
        let line = logs
            .lines()
            .find(|l| l.contains("event=\"RNODE_TX_QUEUE_DROP\""))
            .expect("checked non-zero above");
        for key in [
            "iface=test_rnode_duty".to_string(),
            format!("frames={frames}"),
            format!("reason=\"{reason}\""),
        ] {
            assert!(
                line.contains(&key),
                "abandon event must carry {key}; line: {line}"
            );
        }
    }

    /// Behaviour 7, a port drop must not swallow held frames
    /// (Codeberg #316): the gate is closed, frames are held host-side, and
    /// then the port goes away. `rnode_io_task`'s send queue is task-local,
    /// so those frames are gone — the reconnected task only inherits what is
    /// still in the mpsc channel. That loss is legitimate; keeping it silent
    /// is not. Every abandoned frame must land on `tx_queue_drops` and the
    /// return path must name itself once.
    #[tokio::test(start_paused = true)]
    async fn test_held_frames_are_counted_when_the_port_drops() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(true, 0);
        h.push(b"bootstrap").await;
        h.expect_frame(b"bootstrap", "bootstrap frame ships ungated")
            .await;

        // The bootstrap frame jammed the stub's one-deep queue, so every
        // poll now answers 0x00 and these stay held on our side.
        const HELD: usize = 3;
        h.hold_frames(HELD).await;
        assert_eq!(
            h.counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "nothing is dropped while the frames are merely held"
        );

        h.end_session(StubEnd::Eof).await;

        assert_eq!(
            h.counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            HELD as u64,
            "every frame the dying io task abandons must be counted"
        );
        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_single_abandon_event(&logs, "serial_eof", HELD);
    }

    /// Behaviour 7b, the same shape through a second return path: the
    /// firmware announces a reset (`CMD_RESET` 0xF8) on a port that is
    /// otherwise fine. The io task returns just as abruptly, so the held
    /// frames are just as gone — and must be just as loud, with the reason
    /// naming this path rather than the EOF one.
    #[tokio::test(start_paused = true)]
    async fn test_held_frames_are_counted_on_device_reset() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(true, 0);
        h.push(b"bootstrap").await;
        h.expect_frame(b"bootstrap", "bootstrap frame ships ungated")
            .await;

        const HELD: usize = 4;
        h.hold_frames(HELD).await;
        h.end_session(StubEnd::DeviceReset).await;

        assert_eq!(
            h.counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            HELD as u64,
            "a device reset abandons the queue exactly like an EOF does"
        );
        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_single_abandon_event(&logs, "device_reset", HELD);
    }

    /// Behaviour 7c, silence stays honest: a disconnect with an empty send
    /// queue abandons nothing, so it must say nothing. An unconditional
    /// event at every return path would make `RNODE_TX_QUEUE_DROP` fire on
    /// every ordinary reconnect and train the operator to ignore it.
    #[tokio::test(start_paused = true)]
    async fn test_empty_queue_at_disconnect_stays_silent() {
        let (logs, guard) = capture_logs();
        let mut h = spawn_duty_lock_harness(true, 1);
        h.push(b"only").await;
        h.expect_frame(b"only", "the single frame airs, leaving the queue empty")
            .await;

        h.end_session(StubEnd::Eof).await;

        assert_eq!(
            h.counters
                .tx_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "an empty queue at disconnect drops nothing"
        );
        drop(guard);
        let logs = String::from_utf8(logs.lock().unwrap().clone()).expect("utf8 logs");
        assert_eq!(
            count_event(&logs, "RNODE_TX_QUEUE_DROP"),
            0,
            "no frames abandoned means no event\n--- logs ---\n{logs}"
        );
    }

    /// The regression net for the flowval leg-B deadlock (2026-08-21):
    /// the firmware answers CMD_READY only when queried
    /// (`RNode_Firmware.ino:1003-1008`) — never spontaneously, not after a
    /// TX, not on queue drain (`kiss_indicate_ready`/`_not_ready`,
    /// `Utilities.h:1157,1164`, have no other call sites). An
    /// implementation that waits for an unsolicited READY starves here:
    /// the stub's queue drains after `resume`, but only a query can learn
    /// that, so `expect_frame(second)` times out against a
    /// wait-for-spontaneous-READY host. The query counter additionally
    /// pins that the reopen was learned by asking.
    #[tokio::test(start_paused = true)]
    async fn test_flow_control_reopen_is_learned_by_query_never_spontaneous() {
        let mut h = spawn_duty_lock_harness(true, 0);
        h.push(b"first").await;
        h.expect_frame(b"first", "cold-start frame ships ungated")
            .await;
        // The frame sits in the firmware queue under the lock; the host
        // gate is closed. Lift the lock: the queue drains, and the
        // firmware says nothing on its own.
        h.resume_tx.send(1).await.expect("stub alive");
        h.push(b"second").await;
        h.expect_frame(
            b"second",
            "the gate must reopen via a CMD_READY query — the firmware never volunteers READY",
        )
        .await;
        assert!(
            h.ready_queries.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "the reopen must have been learned through a CMD_READY query"
        );
    }

    // -----------------------------------------------------------------------
    // Radio stats parse/state path (Codeberg #25)
    // -----------------------------------------------------------------------

    /// A fresh RNode interface is marked radio-capable so `radio_stats()`
    /// returns the default record (0.0 airtime/channel-load, None noise/temp,
    /// Unknown battery) even before any `CMD_STAT_*` frame arrives — mirroring
    /// Python's `hasattr(interface, "r_airtime_short")` always being true for an
    /// RNodeInterface. A non-radio interface stays `None`.
    #[test]
    fn radio_stats_default_present_after_enable() {
        let c = InterfaceCounters::new();
        assert!(
            c.radio_stats().is_none(),
            "non-radio interface has no stats"
        );

        c.enable_radio_stats();
        let r = c.radio_stats().expect("radio stats present after enable");
        assert_eq!(r.airtime_short, 0.0);
        assert_eq!(r.airtime_long, 0.0);
        assert_eq!(r.channel_load_short, 0.0);
        assert_eq!(r.channel_load_long, 0.0);
        assert_eq!(r.noise_floor, None);
        assert_eq!(r.cpu_temp, None);
        assert_eq!(r.battery_state, rnode::BatteryState::Unknown);
        assert_eq!(r.last_rssi, None);
        assert_eq!(r.last_snr, None);
    }

    /// CMD_STAT_RSSI payload is a single raw byte; stored value is dBm
    /// (`raw - 157`), matching Python `r_stat_rssi = byte - RSSI_OFFSET`.
    #[test]
    fn apply_radio_stat_rssi_dbm() {
        let c = InterfaceCounters::new();
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_RSSI,
            &[100]
        ));
        assert_eq!(c.radio_stats().unwrap().last_rssi, Some(-57));
    }

    /// CMD_STAT_SNR payload is one signed byte scaled by 0.25 dB, matching
    /// Python `r_stat_snr = signed_byte * 0.25`.
    #[test]
    fn apply_radio_stat_snr_scaled() {
        let c = InterfaceCounters::new();
        // 0x28 = 40 -> 10.0 dB
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_SNR,
            &[0x28]
        ));
        assert_eq!(c.radio_stats().unwrap().last_snr, Some(10.0));
        // 0xF0 = -16 (signed) -> -4.0 dB
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_SNR,
            &[0xF0]
        ));
        assert_eq!(c.radio_stats().unwrap().last_snr, Some(-4.0));
    }

    /// CMD_STAT_CHTM (single-interface, 11 bytes): four big-endian u16 airtime/
    /// channel-load fields scaled to percent (`raw / 100.0`), plus RSSI, noise
    /// floor (`raw - 157` dBm), and interference. Matches Python's
    /// `ats/100.0 ... nfl - RSSI_OFFSET`.
    #[test]
    fn apply_radio_stat_chtm_scale_and_noise_floor() {
        let c = InterfaceCounters::new();
        let payload = [
            0x01, 0x2C, // airtime_short = 300 -> 3.0%
            0x03, 0xE8, // airtime_long = 1000 -> 10.0%
            0x00, 0xC8, // channel_load_short = 200 -> 2.0%
            0x02, 0x58, // channel_load_long = 600 -> 6.0%
            0xC8, // current_rssi raw 200
            100,  // noise_floor raw 100 -> -57 dBm
            0xFF, // interference none
        ];
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_CHTM,
            &payload
        ));
        let r = c.radio_stats().unwrap();
        assert_eq!(r.airtime_short, 3.0);
        assert_eq!(r.airtime_long, 10.0);
        assert_eq!(r.channel_load_short, 2.0);
        assert_eq!(r.channel_load_long, 6.0);
        assert_eq!(r.noise_floor, Some(-57));
    }

    /// `channel_load_short=0.00` has to be a NEGATIVE, not a rounding
    /// artifact, because that is what a lost-frame analysis reads it as: a
    /// receiving modem that saw no preamble while its peer charged itself
    /// for the frame (leviculum#24, and
    /// `bench_single_pair_slow_ca_rnode_only` on 2026-09-23).
    ///
    /// The firmware's smallest possible busy reading is ONE DCD sample of
    /// the 2500-sample ring (Config.h:178), which
    /// `kiss_indicate_channel_stats()` scales by 100*100 (Utilities.h:963)
    /// into raw 4 = 0.04 %. Decode and format both have to keep it: an
    /// integer-percent field, or one decimal, would print that as 0.00 and
    /// silently turn "heard something" into "heard nothing".
    #[test]
    fn apply_radio_stat_chtm_keeps_a_single_dcd_sample_out_of_zero() {
        let (buf, _guard) = capture_logs();
        let c = InterfaceCounters::new();
        // Same 11-byte single-interface shape as above; only the two
        // channel-load fields carry the case. Airtime is left at zero so
        // the modem under test is the LISTENING one: `total_channel_util`
        // is `local_channel_util + airtime`, so a keying modem's reading
        // would not be a clean measurement of the air.
        let chtm = |load_short: u16| {
            let s = load_short.to_be_bytes();
            [
                0x00, 0x00, // airtime_short  = 0
                0x00, 0x00, // airtime_long   = 0
                s[0], s[1], // channel_load_short
                0x00, 0x00, // channel_load_long = 0
                0xC8, // current_rssi raw 200
                100,  // noise_floor raw 100
                0xFF, // interference none
            ]
        };

        // One DCD sample busy: 1/2500 -> raw 4 -> 0.04 %.
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_CHTM,
            &chtm(4)
        ));
        assert_eq!(c.radio_stats().unwrap().channel_load_short, 0.04);

        // A whole frame busy: 1.95 s at SF10/BW125/CR4/8 is 651 of 2500
        // samples -> 26.04 %, the reading the receiver of a delivered frame
        // shows.
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_CHTM,
            &chtm(2604)
        ));
        assert_eq!(c.radio_stats().unwrap().channel_load_short, 26.04);

        // A silent ring, the reading the analysis treats as proof of no
        // preamble.
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_CHTM,
            &chtm(0)
        ));
        assert_eq!(c.radio_stats().unwrap().channel_load_short, 0.0);

        let captured = buf.lock().unwrap();
        let logs = String::from_utf8_lossy(&captured);
        let loads: Vec<&str> = logs
            .lines()
            .filter_map(|l| l.split("channel_load_short=").nth(1))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(
            loads,
            ["0.04", "26.04", "0.00"],
            "the event has to separate one busy sample from a silent ring; logs:\n{logs}"
        );
    }

    /// A CHTM frame crossing the real KISS path emits `LORA_CHTM` on the
    /// same target and level as `LORA_TX`, so a run that captures the
    /// handovers captures the modem's keying account beside them.
    ///
    /// Why this is the keying evidence and `LORA_TX` is not: the firmware
    /// sends CHTM at the end of `update_airtime()`
    /// (RNode_Firmware.ino:712), which both `flush_queue()` (:606) and
    /// `pop_queue()` (:644) call after `add_airtime()` (:751) folded the
    /// airtime of the packet `LoRa->endPacket()` just keyed into the bins.
    /// The trailing CMD_DATA frame is the barrier: one deframer processes
    /// the stream in order, so a packet on `incoming_rx` proves the CHTM
    /// before it was already handled.
    #[tokio::test]
    async fn chtm_frame_emits_lora_chtm_trace_event() {
        let (buf, _guard) = capture_logs();
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode_chtm".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                counters,
                /* flow_control = */ false,
                test_channel_access(),
                125_000,
                7,
                5,
                /* drop_direct_ingress = */ false,
                /* jitter_arm = */ JitterArm::AsIs,
                /* frame_class = */ FrameClass::default(),
                /* alock = */ AirtimeLock::default(),
            )
            .await;
        });

        // Single-interface (11-byte) CHTM: airtime_short raw 300 -> 3.00 %,
        // airtime_long 1000 -> 10.00 %, channel_load_short 200 -> 2.00 %,
        // channel_load_long 600 -> 6.00 %.
        let mut wire = Vec::new();
        for (command, payload) in [
            (
                rnode::CMD_STAT_CHTM,
                &[
                    0x01, 0x2C, 0x03, 0xE8, 0x00, 0xC8, 0x02, 0x58, 0xC8, 100, 0xFF,
                ][..],
            ),
            (rnode::CMD_DATA, &[0x00, 0x00, 0x01][..]),
        ] {
            kiss::frame(command, payload, &mut wire);
            peer.write_all(&wire)
                .await
                .expect("write mocked KISS frame");
        }

        tokio::time::timeout(Duration::from_secs(2), incoming_rx.recv())
            .await
            .expect("data frame within 2s")
            .expect("incoming channel open");
        drop(outgoing_tx);
        drop(peer);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;

        let captured = buf.lock().unwrap();
        let logs = String::from_utf8_lossy(&captured);
        let events: Vec<&str> = logs.lines().filter(|l| l.contains("LORA_CHTM")).collect();
        assert_eq!(events.len(), 1, "one LORA_CHTM event; logs:\n{logs}");
        assert!(
            events[0].contains(
                "LORA_CHTM iface=test_rnode_chtm airtime_short=3.00 airtime_long=10.00 \
                 channel_load_short=2.00 channel_load_long=6.00"
            ),
            "scalar keys, two decimals, one line: {}",
            events[0]
        );
    }

    /// CMD_STAT_BAT payload is `[state, percent]`; percent is 0..=100. Matches
    /// Python `r_battery_state`/`r_battery_percent`.
    #[test]
    fn apply_radio_stat_battery() {
        let c = InterfaceCounters::new();
        // 0x02 = Charging, 85%
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_BAT,
            &[0x02, 85]
        ));
        let r = c.radio_stats().unwrap();
        assert_eq!(r.battery_state, rnode::BatteryState::Charging);
        assert_eq!(r.battery_percent, 85);
    }

    /// CMD_STAT_TEMP payload is one byte; temperature is `raw - 120` Celsius,
    /// clamped to `[-30, 90]` and `None` outside that range, matching Python's
    /// `if temp >= -30 and temp <= 90 ... else None`.
    #[test]
    fn apply_radio_stat_temperature_clamped() {
        let c = InterfaceCounters::new();
        // 145 - 120 = 25 C (in range)
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_TEMP,
            &[145]
        ));
        assert_eq!(c.radio_stats().unwrap().cpu_temp, Some(25));
        // 250 - 120 = 130 C (> 90) -> None
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_TEMP,
            &[250]
        ));
        assert_eq!(c.radio_stats().unwrap().cpu_temp, None);
        // 80 - 120 = -40 C (< -30) -> None
        assert!(apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_STAT_TEMP,
            &[80]
        ));
        assert_eq!(c.radio_stats().unwrap().cpu_temp, None);
    }

    /// A non-stat command is not consumed by the stats parser.
    #[test]
    fn apply_radio_stat_ignores_non_stat_command() {
        let c = InterfaceCounters::new();
        assert!(!apply_radio_stat(
            "test_rnode",
            &c,
            rnode::CMD_DATA,
            &[1, 2, 3]
        ));
        assert!(c.radio_stats().is_none());
    }

    /// Capture tracing output for the duration of the returned guard. Uses a
    /// thread-local default subscriber, which sees the io task's events
    /// because the current-thread tokio test runtime runs every task on the
    /// test thread. Same pattern as the core TUNNEL event test.
    #[derive(Clone)]
    struct LogSink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
        type Writer = LogSink;
        fn make_writer(&'a self) -> LogSink {
            self.clone()
        }
    }

    fn capture_logs() -> (
        Arc<std::sync::Mutex<Vec<u8>>>,
        tracing::subscriber::DefaultGuard,
    ) {
        let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(LogSink(Arc::clone(&buf)))
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (buf, guard)
    }

    // -----------------------------------------------------------------
    // Derived board-maximum TX power vs. a board that clamps
    // -----------------------------------------------------------------

    /// Firmware stub that echoes every config command back verbatim EXCEPT
    /// `CMD_TXPOWER`, which it clamps at `ceiling` before echoing — exactly
    /// what the RNode firmware does (`RNode_Firmware.ino:861-879`: an
    /// SX127x board caps at 17 dBm, an SX1262 without an external PA at 22).
    async fn rnode_stub_clamping_txpower(mut peer: tokio::io::DuplexStream, ceiling: u8) {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut buf = [0u8; 512];
        let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
            let mut one = Vec::new();
            kiss::frame(cmd, payload, &mut one);
            reply.extend_from_slice(&one);
        };
        loop {
            let n = match peer.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut reply: Vec<u8> = Vec::new();
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    match command {
                        rnode::CMD_DETECT => {
                            push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                            push(
                                &mut reply,
                                rnode::CMD_FW_VERSION,
                                &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                            );
                            push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                            push(&mut reply, rnode::CMD_MCU, &[0x00]);
                        }
                        rnode::CMD_TXPOWER => {
                            let asked = payload.first().copied().unwrap_or(0);
                            push(&mut reply, command, &[asked.min(ceiling)]);
                        }
                        rnode::CMD_FREQUENCY
                        | rnode::CMD_BANDWIDTH
                        | rnode::CMD_SF
                        | rnode::CMD_CR
                        | rnode::CMD_RADIO_STATE => push(&mut reply, command, &payload),
                        _ => {}
                    }
                }
            }
            if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
                return;
            }
        }
    }

    fn radio_at(tx_power: u8, tx_power_derived: bool) -> RadioParams {
        RadioParams {
            frequency: 869_525_000,
            bandwidth: 125_000,
            tx_power,
            tx_power_derived,
            sf: 7,
            cr: 5,
            st_alock: None,
            lt_alock: None,
        }
    }

    /// The derived board maximum asks for 22 dBm without probing what this
    /// board can do. An SX127x RNode answers 17 — and because confirmation
    /// is otherwise an exact match, without the derived-value tolerance
    /// every such board would refuse to start rather than run at its own
    /// ceiling. Startup must succeed.
    #[tokio::test]
    async fn a_derived_board_maximum_accepts_a_board_that_clamps_lower() {
        let (mut port, peer) = tokio::io::duplex(64 * 1024);
        let stub = tokio::spawn(rnode_stub_clamping_txpower(peer, 17));

        let result = configure_stream(
            &mut port,
            &radio_at(leviculum_core::rnode::DEFAULT_TX_POWER_DBM as u8, true),
            "clamping-board",
        )
        .await;

        assert!(
            result.is_ok(),
            "a board clamping the derived maximum must still start: {:?}",
            result.err()
        );
        stub.abort();
    }

    /// An explicitly configured power keeps the strict check. 17 dBm is a
    /// value the operator chose; a board that silently delivers 14 has to
    /// say so rather than run 3 dB down in silence.
    #[tokio::test]
    async fn an_explicit_txpower_the_board_clamps_is_still_a_mismatch() {
        let (mut port, peer) = tokio::io::duplex(64 * 1024);
        let stub = tokio::spawn(rnode_stub_clamping_txpower(peer, 14));

        let result = configure_stream(&mut port, &radio_at(17, false), "clamping-board").await;

        match result {
            Err(RNodeError::RadioMismatch(m)) => {
                assert!(m.contains("tx_power"), "wrong mismatch reported: {m}");
                assert!(m.contains("17"), "mismatch must name the request: {m}");
                assert!(m.contains("14"), "mismatch must name what came back: {m}");
            }
            other => panic!("expected a tx_power RadioMismatch, got {other:?}"),
        }
        stub.abort();
    }

    /// The tolerance is one-directional. A board reporting MORE than was
    /// asked for is a mismatch even for the derived default: nothing may
    /// transmit above the requested power, that margin belongs to the
    /// operator's regulatory budget.
    #[tokio::test]
    async fn a_confirmation_above_the_request_is_a_mismatch_even_when_derived() {
        let (mut port, peer) = tokio::io::duplex(64 * 1024);
        // Ceiling above the request, so `min` never bites and the stub
        // answers with a power the interface did not ask for.
        let stub = tokio::spawn(async move {
            let mut peer = peer;
            let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
            let mut buf = [0u8; 512];
            let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
                let mut one = Vec::new();
                kiss::frame(cmd, payload, &mut one);
                reply.extend_from_slice(&one);
            };
            loop {
                let n = match peer.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let mut reply: Vec<u8> = Vec::new();
                for f in deframer.process(&buf[..n]) {
                    if let KissDeframeResult::Frame { command, payload } = f {
                        match command {
                            rnode::CMD_DETECT => {
                                push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                                push(
                                    &mut reply,
                                    rnode::CMD_FW_VERSION,
                                    &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                                );
                                push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                                push(&mut reply, rnode::CMD_MCU, &[0x00]);
                            }
                            // Answers 30 dBm to a 22 dBm request.
                            rnode::CMD_TXPOWER => push(&mut reply, command, &[30]),
                            rnode::CMD_FREQUENCY
                            | rnode::CMD_BANDWIDTH
                            | rnode::CMD_SF
                            | rnode::CMD_CR
                            | rnode::CMD_RADIO_STATE => push(&mut reply, command, &payload),
                            _ => {}
                        }
                    }
                }
                if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
                    return;
                }
            }
        });

        let result = configure_stream(&mut port, &radio_at(22, true), "overshooting-board").await;

        assert!(
            matches!(result, Err(RNodeError::RadioMismatch(_))),
            "a board answering above the request must fail startup, got {result:?}"
        );
        stub.abort();
    }

    /// The config block idles the radio before it configures it.
    ///
    /// Asserted on the literal byte sequence rather than on "a radio-state
    /// frame is present somewhere": the defect this pins down is that the
    /// trailing `RADIO_STATE_ON` was there all along and the leading
    /// `RADIO_STATE_OFF` was not, so any containment check passes on the
    /// broken block too. Position is the whole assertion.
    #[tokio::test]
    async fn test_radio_config_block_idles_the_radio_first() {
        let radio = RadioParams {
            frequency: 867_200_000,
            bandwidth: 125_000,
            tx_power: 17,
            tx_power_derived: false,
            sf: 9,
            cr: 5,
            st_alock: Some(250),
            lt_alock: Some(1000),
        };

        let mut sink: Vec<u8> = Vec::new();
        send_radio_config(&mut sink, &radio)
            .await
            .expect("writing to a Vec cannot fail");

        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            0xC0, 0x06, 0x00, 0xC0,                         // radio state OFF
            0xC0, 0x01, 0x33, 0xB0, 0x6C, 0x00, 0xC0,       // frequency 867.2 MHz
            0xC0, 0x02, 0x00, 0x01, 0xE8, 0x48, 0xC0,       // bandwidth 125 kHz
            0xC0, 0x03, 0x11, 0xC0,                         // tx power 17 dBm
            0xC0, 0x04, 0x09, 0xC0,                         // spreading factor 9
            0xC0, 0x05, 0x05, 0xC0,                         // coding rate 4/5
            0xC0, 0x0B, 0x00, 0xFA, 0xC0,                   // short-term airtime lock
            0xC0, 0x0C, 0x03, 0xE8, 0xC0,                   // long-term airtime lock
            0xC0, 0x06, 0x01, 0xC0,                         // radio state ON
        ];

        assert_eq!(
            sink, expected,
            "config block must be: radio off, parameters, radio on"
        );
    }
}
