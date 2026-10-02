#![no_std]
//! The arithmetic of the SolarNode's external-flash self-test.
//!
//! OTA stage 2 (`docs/src/concepts/ota-stage-2-mesh-image.md`) wants a
//! staged and a golden image on the 2 MiB PUYA P25Q16H the SolarNode
//! carries. The boot line proves the part answers `9Fh`; it proves nothing
//! about erase, program, read-back, addressing across the whole part, or
//! speed. The `qspi-selftest` binary of `leviculum-nrf` finds that out, and
//! everything it decides lives here, where a host test can hold it:
//!
//! - the test pattern ([`word`], [`fill`]): address-derived and bijective
//!   over the part, so a read that aliases two addresses, wraps at 1 MiB or
//!   lands on the wrong word reads back a different value instead of the
//!   same one a constant fill would give;
//! - the comparator ([`Tally`]): how many bytes are wrong and the lowest
//!   address of one;
//! - the census of what was on the part before the first erase
//!   ([`Census`]), per 64 KiB window, with the same FNV-1a as
//!   `leviculum_nrf::qspi::log_head`;
//! - the status-register reading ([`Status`]) that decides whether the
//!   part is write-protected and the run must not start;
//! - the verdict over a whole run ([`Run::verdict`]) and the byte-exact
//!   `[QSPI-TEST]` lines.
//!
//! The binary is the thin rest: six pins, the QSPI peripheral, a clock.
//!
//! # Time
//!
//! The P25Q16H datasheet (`Flash_P25Q16H-UXH-IR_Datasheet.pdf`, Table 5-4
//! "AC parameters for program and erase") gives a 4 KiB sector erase `tSE`
//! of 8 ms typical and 20 ms maximum, and a page program `tPP` (up to 256
//! bytes) of 2 ms typical and 3 ms maximum. The bus runs single-line
//! (`FASTREAD`/`PP`, the QE bit is never set) at 32 MHz, 4 MB/s. From
//! those, [`worst_case_ms`] and [`typical_ms`] are the run's own bounds, and
//! the per-operation timeouts are five times the datasheet maximum of the
//! operation they guard.

use core::fmt;

/// Bytes on the part: 16 Mbit.
pub const PART_BYTES: u32 = 2 * 1024 * 1024;
/// The erase unit the driver offers (`NorFlash::ERASE_SIZE`), and the
/// part's "Sector".
pub const SECTOR_BYTES: u32 = 4096;
/// Sectors on the part.
pub const SECTORS: u32 = PART_BYTES / SECTOR_BYTES;
/// The program unit: one page, and the boundary no single program
/// operation of this test crosses.
pub const PAGE_BYTES: u32 = 256;
/// Pages on the part.
pub const PAGES: u32 = PART_BYTES / PAGE_BYTES;
/// Width of one census window.
pub const WINDOW_BYTES: u32 = 64 * 1024;
/// Census windows over the part.
pub const WINDOWS: usize = (PART_BYTES / WINDOW_BYTES) as usize;

/// Full-part erases: before pattern A, before pattern B, and the one that
/// leaves the part blank for whatever mounts it next.
pub const ERASE_PASSES: usize = 3;
/// Full-part programs: pattern A, then its complement B.
pub const PATTERN_PASSES: usize = 2;
/// Full-part reads: the census, one verify after every erase, one after
/// every program.
pub const FULL_READS: u32 = 1 + ERASE_PASSES as u32 + PATTERN_PASSES as u32;

/// `tSE`, 4 KiB sector erase, typical (datasheet Table 5-4).
pub const T_SE_TYP_MS: u32 = 8;
/// `tSE`, 4 KiB sector erase, maximum (datasheet Table 5-4).
pub const T_SE_MAX_MS: u32 = 20;
/// `tPP`, page program up to 256 bytes, typical (datasheet Table 5-4).
pub const T_PP_TYP_MS: u32 = 2;
/// `tPP`, page program up to 256 bytes, maximum (datasheet Table 5-4).
pub const T_PP_MAX_MS: u32 = 3;

/// Single-line bus throughput at 32 MHz, in bytes per second.
pub const BUS_BYTES_PER_S: u32 = 32_000_000 / 8;

/// Bytes moved per read operation by the binary.
pub const READ_CHUNK_BYTES: u32 = 4096;

/// How long one sector erase may take before the binary calls it hung.
pub const ERASE_TIMEOUT_MS: u32 = 5 * T_SE_MAX_MS;
/// How long one page program may take before the binary calls it hung.
pub const PROGRAM_TIMEOUT_MS: u32 = 5 * T_PP_MAX_MS;
/// How long one [`READ_CHUNK_BYTES`] read may take: twenty times its
/// transfer time, rounded up to whole milliseconds.
pub const READ_TIMEOUT_MS: u32 = 20 * ceil_div(READ_CHUNK_BYTES * 1000, BUS_BYTES_PER_S);

const fn ceil_div(a: u32, b: u32) -> u32 {
    a.div_ceil(b)
}

/// One full-part read on the bus, in ms, rounded up.
pub const fn full_read_ms() -> u32 {
    ceil_div(PART_BYTES, BUS_BYTES_PER_S / 1000)
}

/// The runtime bound from the datasheet maxima: every erase at `tSE` max,
/// every page at `tPP` max plus its transfer, every full read at bus speed.
pub const fn worst_case_ms() -> u32 {
    ERASE_PASSES as u32 * SECTORS * T_SE_MAX_MS
        + PATTERN_PASSES as u32 * (PAGES * T_PP_MAX_MS + full_read_ms())
        + FULL_READS * full_read_ms()
}

/// The same sum over the datasheet's typical values.
pub const fn typical_ms() -> u32 {
    ERASE_PASSES as u32 * SECTORS * T_SE_TYP_MS
        + PATTERN_PASSES as u32 * (PAGES * T_PP_TYP_MS + full_read_ms())
        + FULL_READS * full_read_ms()
}

/// The bound under which the binary still completes without calling any
/// operation hung: every operation one tick short of its timeout.
pub const fn timeout_bound_ms() -> u32 {
    ERASE_PASSES as u32 * SECTORS * ERASE_TIMEOUT_MS
        + PATTERN_PASSES as u32 * PAGES * PROGRAM_TIMEOUT_MS
        + FULL_READS * (PART_BYTES / READ_CHUNK_BYTES) * READ_TIMEOUT_MS
}

/// Which of the two test patterns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    /// The address-derived words.
    A,
    /// Their bitwise complement, so every bit of the part is programmed to
    /// 0 in one of the two passes.
    B,
}

impl Pattern {
    fn letter(self) -> &'static str {
        match self {
            Pattern::A => "A",
            Pattern::B => "B",
        }
    }
}

/// Offset added before the mix. Chosen so neither pattern contains an
/// all-ones or all-zero word anywhere on the part (held by a test): a
/// word that reads `ffffffff` must never be one that was meant that way.
const SEED: u32 = 0x9E37_79B9;

/// MurmurHash3's 32-bit finaliser. Every step is a bijection on `u32` —
/// xor with a right shift of itself, multiplication by an odd constant —
/// so the whole is one, and distinct word indices give distinct words.
const fn fmix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    h
}

/// The 32-bit word pattern `p` puts at byte offset `offset` (a multiple
/// of 4). Pattern A is a bijection of the word index, so no two addresses
/// on the part carry the same word; B is A's complement.
pub const fn word(p: Pattern, offset: u32) -> u32 {
    let a = fmix32((offset >> 2).wrapping_add(SEED));
    match p {
        Pattern::A => a,
        Pattern::B => !a,
    }
}

/// Fill `buf` with pattern `p` as it lies on the part from byte `base`
/// on: little-endian words. `base` and `buf.len()` are multiples of 4,
/// which is also what the QSPI peripheral demands of every transfer; a
/// trailing partial word is left untouched.
pub fn fill(p: Pattern, base: u32, buf: &mut [u8]) {
    let mut offset = base;
    for chunk in buf.chunks_exact_mut(4) {
        chunk.copy_from_slice(&word(p, offset).to_le_bytes());
        offset = offset.wrapping_add(4);
    }
}

/// Bytes that did not read back what was expected, and the lowest address
/// among them.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tally {
    /// Count of wrong bytes.
    pub bad: u32,
    /// Lowest address of a wrong byte, `None` while there is none.
    pub first_bad: Option<u32>,
}

impl Tally {
    fn note(&mut self, addr: u32) {
        self.bad = self.bad.saturating_add(1);
        if self.first_bad.is_none_or(|f| addr < f) {
            self.first_bad = Some(addr);
        }
    }

    /// Compare `got`, read from byte `base` on, against pattern `p`.
    /// `base` is a multiple of 4.
    pub fn check_pattern(&mut self, p: Pattern, base: u32, got: &[u8]) {
        let mut offset = base;
        for chunk in got.chunks_exact(4) {
            let want = word(p, offset).to_le_bytes();
            for (i, (g, w)) in chunk.iter().zip(want.iter()).enumerate() {
                if g != w {
                    self.note(offset + i as u32);
                }
            }
            offset = offset.wrapping_add(4);
        }
    }

    /// Compare `got`, read from byte `base` on, against the erased state.
    pub fn check_erased(&mut self, base: u32, got: &[u8]) {
        for (i, b) in got.iter().enumerate() {
            if *b != 0xFF {
                self.note(base + i as u32);
            }
        }
    }
}

/// FNV-1a 32-bit offset basis, as `leviculum_nrf::qspi::log_head` uses it.
pub const FNV_OFFSET: u32 = 0x811C_9DC5;
/// FNV-1a 32-bit prime, as `leviculum_nrf::qspi::log_head` uses it.
pub const FNV_PRIME: u32 = 0x0100_0193;

/// One census window: bytes that are not erased, and the FNV-1a of all
/// of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    /// Bytes other than `0xFF`.
    pub nonff: u32,
    /// FNV-1a over the window's bytes, in address order.
    pub fnv1a: u32,
}

impl Window {
    const EMPTY: Window = Window {
        nonff: 0,
        fnv1a: FNV_OFFSET,
    };
}

/// What was on the part before this test erased it, per 64 KiB window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Census {
    windows: [Window; WINDOWS],
}

impl Default for Census {
    fn default() -> Self {
        Self::new()
    }
}

impl Census {
    /// A census over nothing yet.
    pub const fn new() -> Self {
        Census {
            windows: [Window::EMPTY; WINDOWS],
        }
    }

    /// Account `bytes`, read from byte `base` on. Chunks have to arrive in
    /// address order within a window, since FNV-1a is order-dependent; a
    /// chunk may straddle a window boundary. Bytes past the part are
    /// ignored.
    pub fn feed(&mut self, base: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            let idx = ((base as usize) + i) / WINDOW_BYTES as usize;
            let Some(w) = self.windows.get_mut(idx) else {
                return;
            };
            w.fnv1a = (w.fnv1a ^ u32::from(*b)).wrapping_mul(FNV_PRIME);
            if *b != 0xFF {
                w.nonff += 1;
            }
        }
    }

    /// The windows, lowest address first.
    pub fn windows(&self) -> &[Window; WINDOWS] {
        &self.windows
    }
}

/// Read Status Register 1, S7..S0 (datasheet §10.5).
pub const OP_RDSR: u8 = 0x05;
/// Read Status Register 2, S15..S8 (datasheet §10.5).
pub const OP_RDSR2: u8 = 0x35;
/// Read Configure Register (datasheet §10.6).
pub const OP_RDCR: u8 = 0x15;

/// What the three read-only register reads returned. `None` for a read the
/// peripheral refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    /// S7..S0: SRP0, BP4..BP0, WEL, WIP.
    pub sr1: u8,
    /// S15..S8: SUS1, CMP, LB3..LB1, SUS2, QE, SRP1.
    pub sr2: Option<u8>,
    /// Configure register: DP in bit 7.
    pub cr: Option<u8>,
}

/// Whether the run may erase.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protection {
    /// No block-protect bit and CMP clear: nothing of the array is
    /// protected (datasheet Table 6-1, first row).
    Clear,
    /// Some protection is configured.
    Protected,
    /// SR2 could not be read, so CMP is unknown.
    Unknown,
}

impl Status {
    /// BP4..BP0, as a five-bit number.
    pub fn bp(&self) -> u8 {
        (self.sr1 >> 2) & 0x1F
    }

    /// The CMP bit, S14.
    pub fn cmp(&self) -> Option<bool> {
        self.sr2.map(|s| s & 0x40 != 0)
    }

    /// Conservative: any block-protect bit, or CMP, refuses the run.
    ///
    /// CMP has to be in here. With CMP=1 and every BP bit clear, Table 6-1
    /// "Protected Area Sizes (CMP bit = 1)" protects ALL of the array, so a
    /// test of the BP bits alone would read that part as unprotected and
    /// then fail every erase silently (the part ignores an erase of a
    /// protected area, note 2 of the table). The rule also refuses the few
    /// combinations the table maps to NONE (BP2=BP1=1 under CMP=1, or BP4
    /// and BP3 alone under CMP=0); refusing a part that was writable costs a
    /// reviewer one look, writing past a protection we misread costs the
    /// measurement.
    pub fn protection(&self) -> Protection {
        match self.cmp() {
            None => Protection::Unknown,
            Some(cmp) if cmp || self.bp() != 0 => Protection::Protected,
            Some(_) => Protection::Clear,
        }
    }
}

/// Hex byte or `na`.
struct HexOr(Option<u8>);

impl fmt::Display for HexOr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{v:02x}"),
            None => f.write_str("na"),
        }
    }
}

/// Boolean as `0`/`1`, or `na`.
struct BitOr(Option<bool>);

impl fmt::Display for BitOr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{}", u8::from(v)),
            None => f.write_str("na"),
        }
    }
}

/// A part address, or `none`.
struct Addr(Option<u32>);

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(a) => write!(f, "0x{a:06x}"),
            None => f.write_str("none"),
        }
    }
}

/// Whole ms from µs, rounded down.
fn ms(us: u64) -> u64 {
    us / 1000
}

/// KiB per second for `bytes` moved in `us`; 0 for a zero duration.
pub fn kib_per_s(bytes: u32, us: u64) -> u64 {
    if us == 0 {
        return 0;
    }
    u64::from(bytes) * 1_000_000 / 1024 / us
}

impl fmt::Display for Status {
    /// `SR sr1=<hh> sr2=<hh|na> cr=<hh|na> bp=<0|1> cmp=<0|1|na>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SR sr1={:02x} sr2={} cr={} bp={} cmp={}",
            self.sr1,
            HexOr(self.sr2),
            HexOr(self.cr),
            u8::from(self.bp() != 0),
            BitOr(self.cmp()),
        )
    }
}

/// One `CENSUS` line.
pub struct CensusLine {
    /// Window index, 0 = the lowest 64 KiB.
    pub win: usize,
    /// What the window held.
    pub window: Window,
}

impl fmt::Display for CensusLine {
    /// `CENSUS win=<n> nonff=<n> fnv1a=<hhhhhhhh>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CENSUS win={} nonff={} fnv1a={:08x}",
            self.win, self.window.nonff, self.window.fnv1a
        )
    }
}

/// One full-part erase and the read that verified it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErasePass {
    /// 1-based pass number.
    pub pass: u8,
    /// Sum of the sector erase durations.
    pub us: u64,
    /// The slowest sector.
    pub max_sector_us: u64,
    /// Bytes not `0xFF` in the verifying read.
    pub not_ff: Tally,
}

impl fmt::Display for ErasePass {
    /// `ERASE pass=<n> sectors=512 ms=<n> max_sector_ms=<n> not_ff=<n> first_bad=0x<hhhhhh|none>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ERASE pass={} sectors={} ms={} max_sector_ms={} not_ff={} first_bad={}",
            self.pass,
            SECTORS,
            ms(self.us),
            ms(self.max_sector_us),
            self.not_ff.bad,
            Addr(self.not_ff.first_bad),
        )
    }
}

/// One full-part program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WritePass {
    /// 1-based pass number.
    pub pass: u8,
    /// Which pattern went on.
    pub pattern: Pattern,
    /// Sum of the page program durations.
    pub us: u64,
}

impl fmt::Display for WritePass {
    /// `WRITE pass=<n> pattern=<A|B> bytes=2097152 ms=<n> kib_s=<n>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WRITE pass={} pattern={} bytes={} ms={} kib_s={}",
            self.pass,
            self.pattern.letter(),
            PART_BYTES,
            ms(self.us),
            kib_per_s(PART_BYTES, self.us),
        )
    }
}

/// One full-part read compared against a pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReadPass {
    /// 1-based pass number, the same as the write it checks.
    pub pass: u8,
    /// Sum of the read durations, without the comparison.
    pub us: u64,
    /// What did not match.
    pub tally: Tally,
}

impl fmt::Display for ReadPass {
    /// `READ pass=<n> bytes=2097152 ms=<n> kib_s=<n> mismatches=<n> first_bad=0x<hhhhhh|none>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "READ pass={} bytes={} ms={} kib_s={} mismatches={} first_bad={}",
            self.pass,
            PART_BYTES,
            ms(self.us),
            kib_per_s(PART_BYTES, self.us),
            self.tally.bad,
            Addr(self.tally.first_bad),
        )
    }
}

/// Which operation failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// A register read before anything was touched.
    Status,
    /// A sector erase.
    Erase,
    /// A page program.
    Write,
    /// A read.
    Read,
}

/// Why it failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cause {
    /// The driver returned an error.
    Driver,
    /// The operation did not complete within its timeout.
    Timeout,
}

/// The one operation that stopped the run. Nothing is retried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Failure {
    /// What was being done.
    pub op: Op,
    /// Where on the part.
    pub addr: u32,
    /// Driver error or timeout.
    pub cause: Cause,
}

impl fmt::Display for Failure {
    /// `ERROR op=<status|erase|write|read> addr=0x<hhhhhh> cause=<driver|timeout>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.op {
            Op::Status => "status",
            Op::Erase => "erase",
            Op::Write => "write",
            Op::Read => "read",
        };
        let cause = match self.cause {
            Cause::Driver => "driver",
            Cause::Timeout => "timeout",
        };
        write!(
            f,
            "ERROR op={op} addr={} cause={cause}",
            Addr(Some(self.addr))
        )
    }
}

/// Everything one run produced, in the order it was produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Run {
    /// The part did not identify, so nothing was attempted.
    pub no_part: bool,
    /// The register reads, once made.
    pub status: Option<Status>,
    /// The census, once complete.
    pub census: Option<Census>,
    /// Set before the first erase is issued: from here on the part's
    /// previous contents are no longer guaranteed.
    pub erase_issued: bool,
    /// The erase passes, in order.
    pub erases: [Option<ErasePass>; ERASE_PASSES],
    /// The program passes, in order.
    pub writes: [Option<WritePass>; PATTERN_PASSES],
    /// The read-back passes, in order.
    pub reads: [Option<ReadPass>; PATTERN_PASSES],
    /// The operation that stopped the run, if one did.
    pub failure: Option<Failure>,
    /// Wall time from the start of the run to its end.
    pub total_us: u64,
}

/// Why a run passed or did not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// Every pass complete and clean.
    Ok,
    /// The part did not identify.
    NoPart,
    /// A block-protect bit or CMP is set; nothing was erased.
    BlockProtected,
    /// SR2 was unreadable, so the protection is unknown; nothing erased.
    StatusUnknown,
    /// An operation failed or hung; see the `ERROR` line.
    Error,
    /// A read-back differed from the pattern.
    Mismatch,
    /// An erase left bytes that are not `0xFF`.
    NotErased,
    /// The run stopped without a failure and without finishing. A bug in
    /// the binary, not in the part.
    Incomplete,
}

/// What the part is left holding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FinalState {
    /// Nothing was erased: the part holds what it held before the run.
    Untouched,
    /// The last erase pass verified all `0xFF`.
    Erased,
    /// The last erase pass ran and found bytes that are not `0xFF`.
    NotErased,
    /// The run stopped somewhere between the first erase and the last.
    Unknown,
}

/// The `RESULT` line's content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Verdict {
    /// Green.
    pub pass: bool,
    /// Why.
    pub reason: Reason,
    /// Wrong bytes over every read-back pass.
    pub mismatches: u32,
    /// Non-`0xFF` bytes over every erase verification.
    pub not_ff: u32,
    /// What the part is left holding.
    pub final_state: FinalState,
    /// Wall time of the run.
    pub total_us: u64,
}

impl Run {
    /// Classify the run. Green only when every pass ran and every byte of
    /// every pass read back as intended.
    pub fn verdict(&self) -> Verdict {
        let mismatches = self
            .reads
            .iter()
            .flatten()
            .fold(0u32, |n, r| n.saturating_add(r.tally.bad));
        let not_ff = self
            .erases
            .iter()
            .flatten()
            .fold(0u32, |n, e| n.saturating_add(e.not_ff.bad));
        let final_state = if !self.erase_issued {
            FinalState::Untouched
        } else {
            match (&self.erases[ERASE_PASSES - 1], self.failure) {
                (Some(last), None) if last.not_ff.bad == 0 => FinalState::Erased,
                (Some(_), None) => FinalState::NotErased,
                _ => FinalState::Unknown,
            }
        };
        let complete = self.erases.iter().all(Option::is_some)
            && self.writes.iter().all(Option::is_some)
            && self.reads.iter().all(Option::is_some);
        let protection = self.status.map(|s| s.protection());
        let reason = if self.no_part {
            Reason::NoPart
        } else if protection == Some(Protection::Protected) {
            Reason::BlockProtected
        } else if protection == Some(Protection::Unknown) {
            Reason::StatusUnknown
        } else if self.failure.is_some() {
            Reason::Error
        } else if mismatches != 0 {
            Reason::Mismatch
        } else if not_ff != 0 {
            Reason::NotErased
        } else if !complete {
            Reason::Incomplete
        } else {
            Reason::Ok
        };
        Verdict {
            pass: reason == Reason::Ok,
            reason,
            mismatches,
            not_ff,
            final_state,
            total_us: self.total_us,
        }
    }
}

impl fmt::Display for Verdict {
    /// `RESULT pass=<0|1> reason=<..> mismatches=<n> not_ff=<n> total_ms=<n> final_state=<..>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            Reason::Ok => "ok",
            Reason::NoPart => "no-part",
            Reason::BlockProtected => "block-protected",
            Reason::StatusUnknown => "status-unknown",
            Reason::Error => "error",
            Reason::Mismatch => "mismatch",
            Reason::NotErased => "not-erased",
            Reason::Incomplete => "incomplete",
        };
        let final_state = match self.final_state {
            FinalState::Untouched => "untouched",
            FinalState::Erased => "erased",
            FinalState::NotErased => "not-erased",
            FinalState::Unknown => "unknown",
        };
        write!(
            f,
            "RESULT pass={} reason={reason} mismatches={} not_ff={} total_ms={} final_state={final_state}",
            u8::from(self.pass),
            self.mismatches,
            self.not_ff,
            ms(self.total_us),
        )
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn pattern_a_is_bijective_over_the_part() {
        let mut words: Vec<u32> = (0..PART_BYTES)
            .step_by(4)
            .map(|o| word(Pattern::A, o))
            .collect();
        let n = words.len();
        assert_eq!(n, (PART_BYTES / 4) as usize);
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), n, "two addresses carry the same word");
    }

    #[test]
    fn no_word_of_either_pattern_looks_erased_or_blank() {
        for o in (0..PART_BYTES).step_by(4) {
            for p in [Pattern::A, Pattern::B] {
                let w = word(p, o);
                assert_ne!(w, u32::MAX, "{p:?} at {o:#x} reads like erased flash");
                assert_ne!(w, 0, "{p:?} at {o:#x} is all zeros");
            }
        }
    }

    #[test]
    fn pattern_b_is_the_complement_of_a() {
        for o in (0..PART_BYTES).step_by(4) {
            assert_eq!(word(Pattern::B, o), !word(Pattern::A, o));
        }
        let mut a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];
        fill(Pattern::A, 0x1F_F000, &mut a);
        fill(Pattern::B, 0x1F_F000, &mut b);
        assert!(a.iter().zip(&b).all(|(x, y)| *x == !*y));
    }

    #[test]
    fn a_wrap_at_one_mib_or_a_swapped_address_bit_reads_back_wrong() {
        // What a part that ignores A20 returns for the upper half is the
        // lower half: every word of it has to differ from what was written.
        let mib = 1024 * 1024;
        for o in (0..mib).step_by(4) {
            assert_ne!(word(Pattern::A, o), word(Pattern::A, o + mib));
        }
        // Each single address bit from the word bits upward, flipped.
        for bit in 2..21 {
            for o in (0..PART_BYTES).step_by(4096) {
                assert_ne!(word(Pattern::A, o), word(Pattern::A, o ^ (1 << bit)));
            }
        }
    }

    #[test]
    fn fill_writes_little_endian_words_from_the_base_on() {
        let mut buf = [0u8; 12];
        fill(Pattern::A, 0x100, &mut buf);
        for i in 0..3u32 {
            let w = word(Pattern::A, 0x100 + 4 * i).to_le_bytes();
            assert_eq!(&buf[(4 * i) as usize..(4 * i + 4) as usize], &w);
        }
    }

    #[test]
    fn comparator_counts_and_locates_injected_single_bit_errors() {
        let base = 0x0A_3000;
        let mut buf = vec![0u8; 4096];
        fill(Pattern::B, base, &mut buf);
        let mut clean = Tally::default();
        clean.check_pattern(Pattern::B, base, &buf);
        assert_eq!(clean, Tally::default());

        let flips = [(4000usize, 7u8), (17, 0), (2048, 3), (18, 5)];
        for (pos, bit) in flips {
            buf[pos] ^= 1 << bit;
        }
        let mut t = Tally::default();
        t.check_pattern(Pattern::B, base, &buf);
        assert_eq!(t.bad, 4);
        assert_eq!(t.first_bad, Some(base + 17));
    }

    #[test]
    fn first_bad_is_the_lowest_address_whatever_the_feed_order() {
        let mut t = Tally::default();
        let mut hi = vec![0xFFu8; 8];
        hi[3] = 0xFE;
        let mut lo = vec![0xFFu8; 8];
        lo[6] = 0x00;
        t.check_erased(0x2000, &hi);
        t.check_erased(0x1000, &lo);
        assert_eq!(t.bad, 2);
        assert_eq!(t.first_bad, Some(0x1006));
    }

    #[test]
    fn an_all_ff_region_classifies_as_erased() {
        let mut t = Tally::default();
        t.check_erased(0, &vec![0xFFu8; SECTOR_BYTES as usize]);
        assert_eq!(t.bad, 0);
        assert_eq!(t.first_bad, None);
        let mut buf = vec![0xFFu8; 64];
        buf[63] = 0x7F;
        t.check_erased(0x40, &buf);
        assert_eq!(t.bad, 1);
        assert_eq!(t.first_bad, Some(0x7F));
    }

    fn reference_fnv1a(bytes: &[u8]) -> u32 {
        // The loop of `leviculum_nrf::qspi::log_head`, verbatim in effect.
        let mut digest: u32 = 0x811C_9DC5;
        for byte in bytes {
            digest = (digest ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
        }
        digest
    }

    #[test]
    fn census_hash_is_fnv1a_as_published() {
        // Known FNV-1a 32 answers: "" and "a".
        assert_eq!(reference_fnv1a(b""), 0x811C_9DC5);
        assert_eq!(reference_fnv1a(b"a"), 0xE40C_292C);
        let mut c = Census::new();
        c.feed(0, b"a");
        assert_eq!(c.windows()[0].fnv1a, 0xE40C_292C);
        assert_eq!(c.windows()[0].nonff, 1);
        assert_eq!(c.windows()[1], Window::EMPTY);
    }

    #[test]
    fn census_is_the_same_in_chunks_and_across_a_window_boundary() {
        let len = 3 * WINDOW_BYTES as usize;
        let mut data = vec![0xFFu8; len];
        // Some "foreign firmware" straddling windows 0/1 and inside 2.
        for (i, b) in data.iter_mut().enumerate().skip(65_000).take(3_000) {
            *b = (i % 251) as u8;
        }
        data[2 * WINDOW_BYTES as usize + 5] = 0x12;

        let mut whole = Census::new();
        whole.feed(0, &data);
        let mut chunked = Census::new();
        for (k, chunk) in data.chunks(4096 - 4).enumerate() {
            chunked.feed((k * (4096 - 4)) as u32, chunk);
        }
        assert_eq!(whole, chunked);

        let w = WINDOW_BYTES as usize;
        for win in 0..3 {
            let slice = &data[win * w..(win + 1) * w];
            let nonff = slice.iter().filter(|b| **b != 0xFF).count() as u32;
            assert_eq!(whole.windows()[win].nonff, nonff, "window {win}");
            assert_eq!(whole.windows()[win].fnv1a, reference_fnv1a(slice));
        }
        // Bytes past the part are ignored rather than wrapped.
        let mut edge = Census::new();
        edge.feed(PART_BYTES - 1, &[0x00, 0x00]);
        assert_eq!(edge.windows()[WINDOWS - 1].nonff, 1);
        assert_eq!(edge.windows()[0], Window::EMPTY);
    }

    #[test]
    fn status_refuses_every_bp_bit_and_cmp_and_nothing_else() {
        let clear = Status {
            sr1: 0,
            sr2: Some(0),
            cr: Some(0),
        };
        assert_eq!(clear.protection(), Protection::Clear);
        // SRP0, WEL, WIP, and every SR2 bit but CMP: no protection.
        let busy = Status {
            sr1: 0x83,
            sr2: Some(0xBF),
            cr: None,
        };
        assert_eq!(busy.protection(), Protection::Clear);
        for bit in 2..7 {
            let s = Status {
                sr1: 1 << bit,
                sr2: Some(0),
                cr: None,
            };
            assert_eq!(s.protection(), Protection::Protected, "BP bit S{bit}");
        }
        // CMP=1 with BP all clear protects the whole array (Table 6-1).
        let cmp = Status {
            sr1: 0,
            sr2: Some(0x40),
            cr: None,
        };
        assert_eq!(cmp.protection(), Protection::Protected);
        let unknown = Status {
            sr1: 0,
            sr2: None,
            cr: None,
        };
        assert_eq!(unknown.protection(), Protection::Unknown);
    }

    fn green_run() -> Run {
        let erase = |pass| ErasePass {
            pass,
            us: 4_100_000,
            max_sector_us: 9_000,
            not_ff: Tally::default(),
        };
        let write = |pass, pattern| WritePass {
            pass,
            pattern,
            us: 16_400_000,
        };
        let read = |pass| ReadPass {
            pass,
            us: 530_000,
            tally: Tally::default(),
        };
        Run {
            no_part: false,
            status: Some(Status {
                sr1: 0,
                sr2: Some(0),
                cr: Some(0),
            }),
            census: Some(Census::new()),
            erase_issued: true,
            erases: [Some(erase(1)), Some(erase(2)), Some(erase(3))],
            writes: [Some(write(1, Pattern::A)), Some(write(2, Pattern::B))],
            reads: [Some(read(1)), Some(read(2))],
            failure: None,
            total_us: 49_000_000,
        }
    }

    #[test]
    fn a_clean_complete_run_is_green_and_leaves_the_part_erased() {
        let v = green_run().verdict();
        assert!(v.pass);
        assert_eq!(v.reason, Reason::Ok);
        assert_eq!(v.final_state, FinalState::Erased);
    }

    #[test]
    fn one_flipped_bit_in_a_read_back_makes_the_run_red() {
        let mut run = green_run();
        let mut buf = vec![0u8; 4096];
        fill(Pattern::A, 0, &mut buf);
        buf[1234] ^= 0x10;
        let mut t = Tally::default();
        t.check_pattern(Pattern::A, 0, &buf);
        if let Some(r) = run.reads[0].as_mut() {
            r.tally = t;
        }
        let v = run.verdict();
        assert!(!v.pass);
        assert_eq!(v.reason, Reason::Mismatch);
        assert_eq!(v.mismatches, 1);
        // The final erase was still clean.
        assert_eq!(v.final_state, FinalState::Erased);
    }

    #[test]
    fn one_unerased_byte_makes_the_run_red() {
        let mut run = green_run();
        let mut t = Tally::default();
        t.check_erased(0x1F_FFFC, &[0xFF, 0xFF, 0xEF, 0xFF]);
        if let Some(e) = run.erases[2].as_mut() {
            e.not_ff = t;
        }
        let v = run.verdict();
        assert!(!v.pass);
        assert_eq!(v.reason, Reason::NotErased);
        assert_eq!(v.not_ff, 1);
        assert_eq!(v.final_state, FinalState::NotErased);
    }

    #[test]
    fn a_protected_part_is_red_and_untouched() {
        let run = Run {
            status: Some(Status {
                sr1: 0x1C,
                sr2: Some(0),
                cr: None,
            }),
            ..Run::default()
        };
        let v = run.verdict();
        assert!(!v.pass);
        assert_eq!(v.reason, Reason::BlockProtected);
        assert_eq!(v.final_state, FinalState::Untouched);
    }

    #[test]
    fn a_failure_after_the_first_erase_leaves_the_state_unknown() {
        let mut run = green_run();
        run.erases[2] = None;
        run.failure = Some(Failure {
            op: Op::Erase,
            addr: 0x1F_E000,
            cause: Cause::Timeout,
        });
        let v = run.verdict();
        assert!(!v.pass);
        assert_eq!(v.reason, Reason::Error);
        assert_eq!(v.final_state, FinalState::Unknown);
        // During the census nothing was erased yet.
        let census_fail = Run {
            status: green_run().status,
            failure: Some(Failure {
                op: Op::Read,
                addr: 0,
                cause: Cause::Driver,
            }),
            ..Run::default()
        };
        assert_eq!(census_fail.verdict().final_state, FinalState::Untouched);
    }

    #[test]
    fn a_run_that_stops_without_a_failure_is_not_green() {
        let mut run = green_run();
        run.reads[1] = None;
        assert_eq!(run.verdict().reason, Reason::Incomplete);
    }

    #[test]
    fn lines_are_byte_exact() {
        let s = Status {
            sr1: 0x00,
            sr2: Some(0x02),
            cr: Some(0x00),
        };
        assert_eq!(format!("{s}"), "SR sr1=00 sr2=02 cr=00 bp=0 cmp=0");
        let s = Status {
            sr1: 0x04,
            sr2: None,
            cr: None,
        };
        assert_eq!(format!("{s}"), "SR sr1=04 sr2=na cr=na bp=1 cmp=na");
        assert_eq!(
            format!(
                "{}",
                CensusLine {
                    win: 31,
                    window: Window {
                        nonff: 12,
                        fnv1a: 0x0000_abcd
                    }
                }
            ),
            "CENSUS win=31 nonff=12 fnv1a=0000abcd"
        );
        let run = green_run();
        assert_eq!(
            format!("{}", run.erases[0].unwrap()),
            "ERASE pass=1 sectors=512 ms=4100 max_sector_ms=9 not_ff=0 first_bad=none"
        );
        assert_eq!(
            format!("{}", run.writes[1].unwrap()),
            "WRITE pass=2 pattern=B bytes=2097152 ms=16400 kib_s=124"
        );
        let mut r = run.reads[0].unwrap();
        r.tally = Tally {
            bad: 3,
            first_bad: Some(0x10_0000),
        };
        assert_eq!(
            format!("{r}"),
            "READ pass=1 bytes=2097152 ms=530 kib_s=3864 mismatches=3 first_bad=0x100000"
        );
        assert_eq!(
            format!(
                "{}",
                Failure {
                    op: Op::Write,
                    addr: 0xAB00,
                    cause: Cause::Driver
                }
            ),
            "ERROR op=write addr=0x00ab00 cause=driver"
        );
        assert_eq!(
            format!("{}", run.verdict()),
            "RESULT pass=1 reason=ok mismatches=0 not_ff=0 total_ms=49000 final_state=erased"
        );
    }

    #[test]
    fn geometry_matches_the_part() {
        assert_eq!(SECTORS, 512);
        assert_eq!(PAGES, 8192);
        assert_eq!(WINDOWS, 32);
        assert_eq!(FULL_READS, 6);
        // Read chunks and sectors are page-aligned multiples, and a chunk
        // never crosses a census window.
        assert_eq!(SECTOR_BYTES % PAGE_BYTES, 0);
        assert_eq!(WINDOW_BYTES % READ_CHUNK_BYTES, 0);
    }

    #[test]
    fn time_bounds_follow_from_the_datasheet() {
        // 2 MiB at 4 MB/s is 524.288 ms, rounded up.
        assert_eq!(full_read_ms(), 525);
        // 3 x 512 x 20 + 2 x (8192 x 3 + 525) + 6 x 525
        assert_eq!(worst_case_ms(), 30_720 + 2 * (24_576 + 525) + 6 * 525);
        assert_eq!(worst_case_ms(), 84_072);
        // 3 x 512 x 8 + 2 x (8192 x 2 + 525) + 6 x 525
        assert_eq!(typical_ms(), 49_256);
        // Every timeout sits above the datasheet maximum of its operation,
        // a page program above tPP max plus its 260-byte transfer (65 us).
        const { assert!(ERASE_TIMEOUT_MS > T_SE_MAX_MS) };
        const { assert!(PROGRAM_TIMEOUT_MS * 1000 > T_PP_MAX_MS * 1000 + 65) };
        // 4096 B at 4 MB/s is 1.024 ms.
        assert_eq!(READ_TIMEOUT_MS, 40);
        assert_eq!(
            timeout_bound_ms(),
            3 * 512 * 100 + 2 * 8192 * 15 + 6 * 512 * 40
        );
    }

    #[test]
    fn rate_is_kib_per_second() {
        assert_eq!(kib_per_s(PART_BYTES, 1_000_000), 2048);
        assert_eq!(kib_per_s(PART_BYTES, 0), 0);
    }
}
