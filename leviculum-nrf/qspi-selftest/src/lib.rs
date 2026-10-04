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
//! - the erase comparator ([`Tally`]): how many bytes are not `0xFF` and
//!   the lowest address of one;
//! - the read-back comparator ([`Compare`]): [`READS`] reads of the same
//!   written data held against the pattern AND against each other, so a
//!   mismatch is classified as unstable (the reads disagree: the read side)
//!   or stable (every read returns the same wrong byte), with the first
//!   [`MISS_LINES`] mismatches kept byte by byte;
//! - the bus-clock table ([`BusClock`]) and the diagnosis that crosses a
//!   fast and a slow read of the same data ([`Diag`]): a byte that reads
//!   right at 8 MHz and wrong at 32 MHz was programmed right and read
//!   wrong, a byte that reads wrong at both was programmed wrong;
//! - the read sweep ([`SWEEP`], [`SweepCell`]): the already-programmed
//!   pattern B read back at every SCK and `IFTIMING.RXDELAY` the bring-up
//!   could choose, so the firmware's bus constant comes from a table and
//!   not from a guess ([`SweepBest`]);
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
//! (`FASTREAD`/`PP`, the QE bit is never set) at 32 MHz, 4 MB/s, at
//! 16 MHz, 2 MB/s, or at 8 MHz, 1 MB/s ([`PLAN`], [`SWEEP`]). From those,
//! [`worst_case_ms`] and [`typical_ms`] are the run's own bounds, and the
//! per-operation timeouts are five times the datasheet maximum of the
//! operation they guard.
//!
//! Those bounds are bus and part time only. The run also compares every
//! byte it reads, and on the 2026-10-04 capture
//! (`/home/lew/rig-run/solarnode-qspi/qspi-selftest-20261004T211914Z.log`)
//! that cost 6.6 s per clean read set and up to 17.1 s per read set
//! that was wrong almost everywhere, on top of its bus time ([`COMPARE_MS_MAX`]).
//! [`wall_bound_ms`] adds that per read set, which is the number to plan a
//! capture window with.
//!
//! # Units
//!
//! Every mismatch count in every line is a count of BYTES (`unit=byte` on
//! the lines that carry one): a byte is wrong when any of its bits is. The
//! bit counts (`lost1`, `gained1`, `bit7_0`) count bits, and say so in
//! their names.

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
/// Full-part programs: pattern A at the fast clock, then its complement B
/// at the slow one.
pub const PATTERN_PASSES: usize = 2;
/// Reads of the same written data per read set, compared with each other
/// as well as with the pattern.
pub const READS: usize = 5;
/// Read sets: each written pattern read [`READS`] times at either clock.
pub const READ_SETS: usize = 4;
/// Points of the read sweep ([`SWEEP`]).
pub const SWEEP_POINTS: usize = 1 + 2 * RXDELAYS;
/// `IFTIMING.RXDELAY` values the peripheral accepts: a 3-bit field.
pub const RXDELAYS: usize = 8;
/// `IFTIMING.RXDELAY` as `embassy_nrf::qspi::Config::default()` sets it,
/// and as every read set of [`PLAN`] and the control point of [`SWEEP`]
/// runs: two 64 MHz periods, 31.25 ns after the SCK edge.
pub const DEFAULT_RXDELAY: u8 = 2;
/// Mismatched bytes kept per read set for the `MISS` lines.
pub const MISS_LINES: usize = 64;

/// The QSPI clocks this test drives the part at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusClock {
    /// 32 MHz, the nRF52840 QSPI's ceiling.
    M32,
    /// 16 MHz, the step between, swept only.
    M16,
    /// 8 MHz: a quarter of it, so a sampling-window problem at 32 MHz has
    /// four times the margin here. The clock every erase verification
    /// and the census read at.
    M8,
}

impl BusClock {
    /// The clock in MHz.
    pub const fn mhz(self) -> u32 {
        match self {
            BusClock::M32 => 32,
            BusClock::M16 => 16,
            BusClock::M8 => 8,
        }
    }

    /// The `IFCONFIG1.SCKFREQ` code: SCK = 32 MHz / (code + 1), nRF52840
    /// PS, and `embassy_nrf::qspi::Frequency` (`M32 = 0`, `M16 = 1`,
    /// `M8 = 3`).
    pub const fn sckfreq(self) -> u8 {
        match self {
            BusClock::M32 => 0,
            BusClock::M16 => 1,
            BusClock::M8 => 3,
        }
    }

    /// Single-line throughput in bytes per second.
    pub const fn bytes_per_s(self) -> u32 {
        self.mhz() * 1_000_000 / 8
    }
}

/// One read set of the plan: which pattern, the clock it was written at,
/// the clock it is read at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SetPlan {
    /// The pattern on the part.
    pub pattern: Pattern,
    /// The clock of the program pass that put it there.
    pub write: BusClock,
    /// The clock of these reads.
    pub read: BusClock,
}

/// The run, in order. Pattern A goes on at 32 MHz and is read at 32 and
/// then at 8; pattern B goes on at 8 MHz and is read at 8 and then at 32.
/// So each written image is read at both clocks without being rewritten,
/// and each clock is used to program once.
pub const PLAN: [SetPlan; READ_SETS] = [
    SetPlan {
        pattern: Pattern::A,
        write: BusClock::M32,
        read: BusClock::M32,
    },
    SetPlan {
        pattern: Pattern::A,
        write: BusClock::M32,
        read: BusClock::M8,
    },
    SetPlan {
        pattern: Pattern::B,
        write: BusClock::M8,
        read: BusClock::M8,
    },
    SetPlan {
        pattern: Pattern::B,
        write: BusClock::M8,
        read: BusClock::M32,
    },
];

/// The clock of each erase pass's verifying read, and of the census before
/// the first: 8 MHz every time, the clock every read set at it came back
/// clean on (2026-10-04). The binary sets it before the census, it is
/// what set 2 leaves the bus at, and the sweep ends on it, so the last
/// erase's `final_state` is a measurement and not a 32 MHz guess.
pub const ERASE_CLOCK: [BusClock; ERASE_PASSES] = [BusClock::M8, BusClock::M8, BusClock::M8];

/// The clock of the census read.
pub const CENSUS_CLOCK: BusClock = BusClock::M8;

/// One bus setting: SCK and the input sampling delay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timing {
    /// `IFCONFIG1.SCKFREQ`, as a clock.
    pub clock: BusClock,
    /// `IFTIMING.RXDELAY`, 0..=7, in 64 MHz periods (15.625 ns) from the
    /// SCK edge to the moment the input is sampled.
    pub rxdelay: u8,
}

/// The timing every read set of [`PLAN`], the census and every erase
/// verification run at, apart from the clock.
pub const fn at_default_rxdelay(clock: BusClock) -> Timing {
    Timing {
        clock,
        rxdelay: DEFAULT_RXDELAY,
    }
}

/// The read sweep, in order, over pattern B as write pass 2 left it
/// (programmed at 8 MHz, read clean at 8 MHz by set 3): first the 8 MHz
/// control at the default RXDELAY, then 16 MHz and 32 MHz each at every
/// RXDELAY from 0 to 7. [`READS`] reads per chunk, as in a read set.
///
/// Why RXDELAY and not only the clock: the register counts 15.625 ns
/// after the SCK edge, so the default 2 samples a quarter of the way into
/// an 8 MHz bit (125 ns), half way into a 16 MHz one (62.5 ns) and a whole
/// 32 MHz bit later (31.25 ns), right where the part's next bit is
/// arriving (`tCLQV` 7 ns after the falling edge, P25Q16H datasheet
/// Table 5-3). Which delay puts the sample in the eye at 32 MHz is the
/// board's (pad, trace and part delays), so it is measured, not computed.
pub const SWEEP: [Timing; SWEEP_POINTS] = {
    let mut s = [at_default_rxdelay(BusClock::M8); SWEEP_POINTS];
    let mut i = 0;
    while i < RXDELAYS {
        s[1 + i] = Timing {
            clock: BusClock::M16,
            rxdelay: i as u8,
        };
        s[1 + RXDELAYS + i] = Timing {
            clock: BusClock::M32,
            rxdelay: i as u8,
        };
        i += 1;
    }
    s
};

/// The pattern the sweep reads: what write pass 2 programmed.
pub const SWEEP_PATTERN: Pattern = Pattern::B;

/// `tSE`, 4 KiB sector erase, typical (datasheet Table 5-4).
pub const T_SE_TYP_MS: u32 = 8;
/// `tSE`, 4 KiB sector erase, maximum (datasheet Table 5-4).
pub const T_SE_MAX_MS: u32 = 20;
/// `tPP`, page program up to 256 bytes, typical (datasheet Table 5-4).
pub const T_PP_TYP_MS: u32 = 2;
/// `tPP`, page program up to 256 bytes, maximum (datasheet Table 5-4).
pub const T_PP_MAX_MS: u32 = 3;

/// Bytes moved per read operation by the binary.
pub const READ_CHUNK_BYTES: u32 = 4096;

/// How long one sector erase may take before the binary calls it hung.
pub const ERASE_TIMEOUT_MS: u32 = 5 * T_SE_MAX_MS;
/// How long one page program may take before the binary calls it hung.
pub const PROGRAM_TIMEOUT_MS: u32 = 5 * T_PP_MAX_MS;
/// How long one [`READ_CHUNK_BYTES`] read may take: twenty times its
/// transfer time at the slower clock, rounded up to whole milliseconds.
pub const READ_TIMEOUT_MS: u32 = 20 * ceil_div(READ_CHUNK_BYTES * 1000, BusClock::M8.bytes_per_s());

const fn ceil_div(a: u32, b: u32) -> u32 {
    a.div_ceil(b)
}

/// One full-part read on the bus at `clock`, in ms, rounded up.
pub const fn full_read_ms(clock: BusClock) -> u32 {
    ceil_div(PART_BYTES, clock.bytes_per_s() / 1000)
}

/// Full-part reads at `clock`: the census at [`CENSUS_CLOCK`], every
/// erase verification at its [`ERASE_CLOCK`], [`READS`] per read set and
/// per sweep point.
pub const fn full_reads(clock: BusClock) -> u32 {
    let mut n = 0;
    if CENSUS_CLOCK as u8 == clock as u8 {
        n += 1;
    }
    let mut i = 0;
    while i < ERASE_PASSES {
        if ERASE_CLOCK[i] as u8 == clock as u8 {
            n += 1;
        }
        i += 1;
    }
    let mut i = 0;
    while i < READ_SETS {
        if PLAN[i].read as u8 == clock as u8 {
            n += READS as u32;
        }
        i += 1;
    }
    let mut i = 0;
    while i < SWEEP_POINTS {
        if SWEEP[i].clock as u8 == clock as u8 {
            n += READS as u32;
        }
        i += 1;
    }
    n
}

/// Every full-part read of the run.
pub const FULL_READS: u32 =
    full_reads(BusClock::M32) + full_reads(BusClock::M16) + full_reads(BusClock::M8);

/// Bus time of every read and every page transfer, in ms.
const fn transfer_ms() -> u32 {
    full_reads(BusClock::M32) * full_read_ms(BusClock::M32)
        + full_reads(BusClock::M16) * full_read_ms(BusClock::M16)
        + full_reads(BusClock::M8) * full_read_ms(BusClock::M8)
        + full_read_ms(PLAN[0].write)
        + full_read_ms(PLAN[2].write)
}

/// The runtime bound from the datasheet maxima: every erase at `tSE` max,
/// every page at `tPP` max plus its transfer, every full read at bus speed.
pub const fn worst_case_ms() -> u32 {
    ERASE_PASSES as u32 * SECTORS * T_SE_MAX_MS
        + PATTERN_PASSES as u32 * PAGES * T_PP_MAX_MS
        + transfer_ms()
}

/// The same sum over the datasheet's typical values.
pub const fn typical_ms() -> u32 {
    ERASE_PASSES as u32 * SECTORS * T_SE_TYP_MS
        + PATTERN_PASSES as u32 * PAGES * T_PP_TYP_MS
        + transfer_ms()
}

/// Comparison and printing time of one read set or sweep point beyond
/// its bus time, at most. On the 2026-10-04 capture read sets 1 and 4
/// (32 MHz, about 80 % of their bytes wrong) took 18.4 s and 19.8 s of
/// wall time between the line before and their own, of which 2.74 s was
/// reads: 15.7 s and 17.1 s. The clean set 3 took 6.6 s beyond its
/// 10.6 s of reads. The worse of the two, rounded up to whole seconds.
pub const COMPARE_MS_MAX: u32 = 18_000;

/// [`worst_case_ms`] plus [`COMPARE_MS_MAX`] for every read set and every
/// sweep point: what a capture of one whole run has to cover.
pub const fn wall_bound_ms() -> u32 {
    worst_case_ms() + (READ_SETS + SWEEP_POINTS) as u32 * COMPARE_MS_MAX
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

/// Bytes of an erase verification that are not `0xFF`, and the lowest
/// address among them.
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

    /// Compare `got`, read from byte `base` on, against the erased state.
    pub fn check_erased(&mut self, base: u32, got: &[u8]) {
        for (i, b) in got.iter().enumerate() {
            if *b != 0xFF {
                self.note(base + i as u32);
            }
        }
    }
}

/// One mismatched byte of a read set: where, what was written, and what
/// each of the [`READS`] reads returned.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Miss {
    /// Part address of the byte.
    pub addr: u32,
    /// The pattern's byte.
    pub want: u8,
    /// Each read's byte, in read order.
    pub got: [u8; READS],
}

/// [`READS`] reads of the same written data, compared with the pattern and
/// with each other. Every count is of bytes unless its name says bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Compare {
    /// Bytes where at least one read differs from the pattern.
    pub mismatches: u32,
    /// Of those, bytes where the reads disagree among themselves: the same
    /// cell returned different values, which only the read side can do.
    pub unstable: u32,
    /// Of those, bytes where every read returned the same wrong value:
    /// what the part holds, or a read error that repeats exactly.
    pub stable: u32,
    /// Bytes wrong in each read, in read order.
    pub per_read: [u32; READS],
    /// Lowest address of a mismatched byte.
    pub first_bad: Option<u32>,
    /// Bits written 1 that read 0, summed over every read.
    pub lost1: u32,
    /// Bits written 0 that read 1, summed over every read.
    pub gained1: u32,
    /// Wrong bits by position within the byte, summed over every read,
    /// bit 7 first: the order the single-line bus clocks them, so a
    /// sampling-edge problem shows as a position, not as a pin (on a
    /// single-line bus every read bit arrives on IO1 and every written one
    /// leaves on IO0).
    pub bit7_0: [u32; 8],
    /// The first [`MISS_LINES`] mismatched bytes, lowest address first.
    pub misses: [Miss; MISS_LINES],
    /// How many of `misses` are filled.
    pub n_misses: usize,
}

impl Default for Compare {
    fn default() -> Self {
        Compare {
            mismatches: 0,
            unstable: 0,
            stable: 0,
            per_read: [0; READS],
            first_bad: None,
            lost1: 0,
            gained1: 0,
            bit7_0: [0; 8],
            misses: [Miss::default(); MISS_LINES],
            n_misses: 0,
        }
    }
}

impl Compare {
    /// Compare [`READS`] reads of the same range, each read from byte
    /// `base` on, against pattern `p` and against each other. `base` is a
    /// multiple of 4; the reads are compared over the shortest one's whole
    /// words. Chunks are fed in ascending address order, which is what
    /// makes `misses` the lowest-addressed ones.
    pub fn check(&mut self, p: Pattern, base: u32, reads: [&[u8]; READS]) {
        let len = reads.iter().map(|r| r.len()).min().unwrap_or(0) / 4 * 4;
        for w in (0..len).step_by(4) {
            let want = word(p, base.wrapping_add(w as u32)).to_le_bytes();
            for (i, want) in want.iter().enumerate() {
                let at = w + i;
                let got = core::array::from_fn(|r| reads[r][at]);
                self.byte(base.wrapping_add(at as u32), *want, got);
            }
        }
    }

    fn byte(&mut self, addr: u32, want: u8, got: [u8; READS]) {
        let mut wrong = false;
        for (r, g) in got.iter().enumerate() {
            let x = g ^ want;
            if x == 0 {
                continue;
            }
            wrong = true;
            self.per_read[r] = self.per_read[r].saturating_add(1);
            self.lost1 = self.lost1.saturating_add((want & !g).count_ones());
            self.gained1 = self.gained1.saturating_add((!want & g).count_ones());
            for (pos, n) in self.bit7_0.iter_mut().enumerate() {
                if x & (0x80 >> pos) != 0 {
                    *n = n.saturating_add(1);
                }
            }
        }
        if !wrong {
            return;
        }
        self.mismatches = self.mismatches.saturating_add(1);
        if got.iter().all(|g| *g == got[0]) {
            self.stable = self.stable.saturating_add(1);
        } else {
            self.unstable = self.unstable.saturating_add(1);
        }
        if self.first_bad.is_none_or(|f| addr < f) {
            self.first_bad = Some(addr);
        }
        if let Some(slot) = self.misses.get_mut(self.n_misses) {
            *slot = Miss { addr, want, got };
            self.n_misses += 1;
        }
    }

    /// The kept mismatches.
    pub fn misses(&self) -> &[Miss] {
        &self.misses[..self.n_misses.min(MISS_LINES)]
    }
}

/// What a fast and a slow read set of the same written image say together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// Both read sets match the pattern.
    None,
    /// The slow reads are stable and right, the fast ones are not: the part
    /// holds the pattern and the fast clock reads it wrong.
    Read,
    /// The slow reads are stable and wrong: the part holds something other
    /// than the pattern, so the program pass put it there.
    Program,
    /// Even the slow reads disagree among themselves, so no read is a
    /// trustworthy picture of the part's content.
    Unresolved,
}

/// The `DIAG` line: one written image, its reads at both clocks, the side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Diag {
    /// The pattern on the part.
    pub pattern: Pattern,
    /// The clock it was programmed at.
    pub write: BusClock,
    /// The read set at 32 MHz.
    pub fast: Compare,
    /// The read set at 8 MHz.
    pub slow: Compare,
}

impl Diag {
    /// The side the error is on.
    ///
    /// The 8 MHz reads are the reference picture of the part, but only
    /// while they agree with each other. Bytes the slow reads get wrong
    /// AND agree on are on the part, whatever the fast reads say, so
    /// `Program` wins over `Read` when both kinds are present; the counts
    /// on the line say how many of each.
    pub fn side(&self) -> Side {
        if self.slow.unstable != 0 {
            Side::Unresolved
        } else if self.slow.mismatches != 0 {
            Side::Program
        } else if self.fast.mismatches != 0 {
            Side::Read
        } else {
            Side::None
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
    /// The bus clock it went on at.
    pub clock: BusClock,
    /// Sum of the page program durations.
    pub us: u64,
}

impl fmt::Display for WritePass {
    /// `WRITE pass=<n> pattern=<A|B> mhz=<32|8> bytes=2097152 ms=<n> kib_s=<n>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WRITE pass={} pattern={} mhz={} bytes={} ms={} kib_s={}",
            self.pass,
            self.pattern.letter(),
            self.clock.mhz(),
            PART_BYTES,
            ms(self.us),
            kib_per_s(PART_BYTES, self.us),
        )
    }
}

/// [`READS`] full-part reads of one written image at one clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReadSet {
    /// 1-based set number, the index into [`PLAN`] plus one.
    pub set: u8,
    /// What was written, at which clock, and the clock of these reads.
    pub plan: SetPlan,
    /// Sum of all [`READS`] reads' durations, without the comparison.
    pub us: u64,
    /// What did not match.
    pub cmp: Compare,
}

/// Comma-separated list.
struct List<'a, T>(&'a [T]);

impl<T: fmt::Display> fmt::Display for List<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, v) in self.0.iter().enumerate() {
            if i != 0 {
                f.write_str(",")?;
            }
            write!(f, "{v}")?;
        }
        Ok(())
    }
}

/// Comma-separated hex bytes.
struct HexList<'a>(&'a [u8]);

impl fmt::Display for HexList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, v) in self.0.iter().enumerate() {
            if i != 0 {
                f.write_str(",")?;
            }
            write!(f, "{v:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ReadSet {
    /// `READSET set=<n> pattern=<A|B> write_mhz=<n> read_mhz=<n> reads=5 unit=byte
    /// mismatches=<n> unstable=<n> stable=<n> per_read=<n>,.. first_bad=0x<hhhhhh|none>
    /// lost1=<bits> gained1=<bits> bit7_0=<n>,.. ms=<n> kib_s=<n>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.cmp;
        write!(
            f,
            "READSET set={} pattern={} write_mhz={} read_mhz={} reads={} unit=byte \
             mismatches={} unstable={} stable={} per_read={} first_bad={} \
             lost1={} gained1={} bit7_0={} ms={} kib_s={}",
            self.set,
            self.plan.pattern.letter(),
            self.plan.write.mhz(),
            self.plan.read.mhz(),
            READS,
            c.mismatches,
            c.unstable,
            c.stable,
            List(&c.per_read),
            Addr(c.first_bad),
            c.lost1,
            c.gained1,
            List(&c.bit7_0),
            ms(self.us),
            kib_per_s(PART_BYTES.saturating_mul(READS as u32), self.us),
        )
    }
}

/// One `MISS` line.
pub struct MissLine {
    /// The read set it belongs to.
    pub set: u8,
    /// Its index among the set's kept mismatches.
    pub n: usize,
    /// The byte.
    pub miss: Miss,
}

impl fmt::Display for MissLine {
    /// `MISS set=<n> n=<k> addr=0x<hhhhhh> want=<hh> got=<hh>,.. xor=<hh>,..`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = &self.miss;
        let xor: [u8; READS] = core::array::from_fn(|r| m.got[r] ^ m.want);
        write!(
            f,
            "MISS set={} n={} addr={} want={:02x} got={} xor={}",
            self.set,
            self.n,
            Addr(Some(m.addr)),
            m.want,
            HexList(&m.got),
            HexList(&xor),
        )
    }
}

impl fmt::Display for Diag {
    /// `DIAG pattern=<A|B> write_mhz=<n> fast_mismatches=<n> fast_unstable=<n>
    /// slow_mismatches=<n> slow_unstable=<n> side=<none|read|program|unresolved>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = match self.side() {
            Side::None => "none",
            Side::Read => "read",
            Side::Program => "program",
            Side::Unresolved => "unresolved",
        };
        write!(
            f,
            "DIAG pattern={} write_mhz={} fast_mismatches={} fast_unstable={} \
             slow_mismatches={} slow_unstable={} side={side}",
            self.pattern.letter(),
            self.write.mhz(),
            self.fast.mismatches,
            self.fast.unstable,
            self.slow.mismatches,
            self.slow.unstable,
        )
    }
}

/// The QSPI interface registers that set the bus timing, read back raw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bus {
    /// `IFCONFIG1`: SCKDELAY in 7:0, SCKFREQ in 31:28.
    pub ifconfig1: u32,
    /// `IFTIMING`: RXDELAY in 10:8.
    pub iftiming: u32,
}

impl Bus {
    /// `IFCONFIG1.SCKFREQ`.
    pub fn sckfreq(&self) -> u8 {
        ((self.ifconfig1 >> 28) & 0xF) as u8
    }

    /// The SCK the register asks for, in kHz: 32 MHz / (SCKFREQ + 1).
    pub fn sck_khz(&self) -> u32 {
        32_000 / (u32::from(self.sckfreq()) + 1)
    }

    /// `IFTIMING.RXDELAY`, in 64 MHz periods (15.625 ns) after the SCK
    /// edge the input is sampled.
    pub fn rxdelay(&self) -> u8 {
        ((self.iftiming >> 8) & 0x7) as u8
    }

    /// `IFCONFIG1.SCKDELAY`, in 16 MHz periods of CSN high between
    /// operations.
    pub fn sckdelay(&self) -> u8 {
        (self.ifconfig1 & 0xFF) as u8
    }

    /// `IFCONFIG1` with SCKFREQ set for `clock` and every other bit kept.
    pub fn with_clock(ifconfig1: u32, clock: BusClock) -> u32 {
        (ifconfig1 & !(0xF << 28)) | (u32::from(clock.sckfreq()) << 28)
    }

    /// `IFTIMING` with RXDELAY set to `rxdelay` (its low three bits) and
    /// every other bit kept.
    pub fn with_rxdelay(iftiming: u32, rxdelay: u8) -> u32 {
        (iftiming & !(0x7 << 8)) | (u32::from(rxdelay & 0x7) << 8)
    }

    /// Whether these registers carry `t`.
    pub fn carries(&self, t: Timing) -> bool {
        self.sckfreq() == t.clock.sckfreq() && self.rxdelay() == t.rxdelay
    }
}

/// One point of the read sweep: what was asked, what the registers read
/// back, and what [`READS`] reads of the whole part at it returned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SweepCell {
    /// Index into [`SWEEP`].
    pub n: u8,
    /// The timing asked for.
    pub want: Timing,
    /// The timing registers as they read back after the switch.
    pub bus: Bus,
    /// Sum of the reads' durations, without the comparison.
    pub us: u64,
    /// Bytes where at least one read differs from [`SWEEP_PATTERN`].
    pub mismatches: u32,
    /// Of those, bytes the reads disagree on.
    pub unstable: u32,
    /// Of those, bytes every read returned the same wrong value for.
    pub stable: u32,
    /// Bits written 1 that read 0, over every read.
    pub lost1: u32,
    /// Bits written 0 that read 1, over every read.
    pub gained1: u32,
    /// Wrong bits by position, bit 7 first, over every read.
    pub bit7_0: [u32; 8],
}

impl SweepCell {
    /// The cell for sweep point `n`, from its comparison.
    pub fn new(n: u8, want: Timing, bus: Bus, us: u64, cmp: &Compare) -> Self {
        SweepCell {
            n,
            want,
            bus,
            us,
            mismatches: cmp.mismatches,
            unstable: cmp.unstable,
            stable: cmp.stable,
            lost1: cmp.lost1,
            gained1: cmp.gained1,
            bit7_0: cmp.bit7_0,
        }
    }

    /// Whether the registers carried the asked timing. A cell that was not
    /// applied measured some other setting and says nothing about this one.
    pub fn applied(&self) -> bool {
        self.bus.carries(self.want)
    }

    /// Applied, and every byte of every read right.
    pub fn clean(&self) -> bool {
        self.applied() && self.mismatches == 0
    }
}

impl fmt::Display for SweepCell {
    /// `READSWEEP n=<k> sck_khz=<n> rxdelay=<n> applied=<0|1> pattern=B reads=5 unit=byte
    /// mismatches=<n> unstable=<n> stable=<n> lost1=<bits> gained1=<bits> bit7_0=<n>,..
    /// ms=<n> kib_s=<n>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "READSWEEP n={} sck_khz={} rxdelay={} applied={} pattern={} reads={} unit=byte \
             mismatches={} unstable={} stable={} lost1={} gained1={} bit7_0={} ms={} kib_s={}",
            self.n,
            self.want.clock.mhz() * 1000,
            self.want.rxdelay,
            u8::from(self.applied()),
            SWEEP_PATTERN.letter(),
            READS,
            self.mismatches,
            self.unstable,
            self.stable,
            self.lost1,
            self.gained1,
            List(&self.bit7_0),
            ms(self.us),
            kib_per_s(PART_BYTES.saturating_mul(READS as u32), self.us),
        )
    }
}

/// What the sweep recommends: the fastest clock with any clean RXDELAY,
/// and of its clean delays the middle of the longest unbroken run, the
/// one furthest from both edges of the window the sweep found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SweepBest {
    /// The recommended timing, `None` when no cell was clean.
    pub best: Option<Timing>,
    /// Bit `d` set when RXDELAY `d` was clean at the recommended clock.
    pub clean_mask: u8,
}

impl SweepBest {
    /// Read the recommendation off the cells that ran.
    pub fn of(cells: &[Option<SweepCell>]) -> Self {
        for clock in [BusClock::M32, BusClock::M16, BusClock::M8] {
            let mut mask = 0u8;
            for c in cells.iter().flatten() {
                if c.want.clock == clock && c.clean() {
                    mask |= 1 << (c.want.rxdelay & 0x7);
                }
            }
            if mask == 0 {
                continue;
            }
            // Longest run of set bits; the first one on a tie.
            let (mut best_start, mut best_len) = (0u8, 0u8);
            let mut d = 0u8;
            while d < RXDELAYS as u8 {
                if mask & (1 << d) == 0 {
                    d += 1;
                    continue;
                }
                let start = d;
                while d < RXDELAYS as u8 && mask & (1 << d) != 0 {
                    d += 1;
                }
                if d - start > best_len {
                    (best_start, best_len) = (start, d - start);
                }
            }
            return SweepBest {
                best: Some(Timing {
                    clock,
                    rxdelay: best_start + (best_len - 1) / 2,
                }),
                clean_mask: mask,
            };
        }
        SweepBest::default()
    }
}

impl fmt::Display for SweepBest {
    /// `SWEEPBEST sck_khz=<n|none> rxdelay=<n|none> clean_rxdelays=<d>,..|none`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(t) = self.best else {
            return f.write_str("SWEEPBEST sck_khz=none rxdelay=none clean_rxdelays=none");
        };
        write!(
            f,
            "SWEEPBEST sck_khz={} rxdelay={} clean_rxdelays=",
            t.clock.mhz() * 1000,
            t.rxdelay
        )?;
        let mut first = true;
        for d in 0..RXDELAYS as u8 {
            if self.clean_mask & (1 << d) != 0 {
                if !first {
                    f.write_str(",")?;
                }
                write!(f, "{d}")?;
                first = false;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Bus {
    /// `BUS sck_khz=<n> sckfreq=<n> rxdelay=<n> sckdelay=<n> ifconfig1=<hhhhhhhh> iftiming=<hhhhhhhh>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BUS sck_khz={} sckfreq={} rxdelay={} sckdelay={} ifconfig1={:08x} iftiming={:08x}",
            self.sck_khz(),
            self.sckfreq(),
            self.rxdelay(),
            self.sckdelay(),
            self.ifconfig1,
            self.iftiming,
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
    /// The bus timing registers as the bring-up left them.
    pub bus: Option<Bus>,
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
    /// The read sets, in [`PLAN`] order.
    pub reads: [Option<ReadSet>; READ_SETS],
    /// The sweep, in [`SWEEP`] order.
    pub sweep: [Option<SweepCell>; SWEEP_POINTS],
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
    /// A sweep point's registers did not read back as set, so the sweep
    /// table has a cell that measured something else.
    SweepNotApplied,
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
    /// Mismatched bytes summed over every read set (a byte counts once
    /// per set however many of its reads were wrong).
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
    ///
    /// The sweep's mismatches are not in it: the sweep exists to find the
    /// settings that read wrong, so a red cell is its result and not a
    /// failure. What the sweep can fail is completeness, and a cell whose
    /// registers did not take (`sweep-not-applied`).
    pub fn verdict(&self) -> Verdict {
        let mismatches = self
            .reads
            .iter()
            .flatten()
            .fold(0u32, |n, r| n.saturating_add(r.cmp.mismatches));
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
            && self.reads.iter().all(Option::is_some)
            && self.sweep.iter().all(Option::is_some);
        let not_applied = self.sweep.iter().flatten().any(|c| !c.applied());
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
        } else if not_applied {
            Reason::SweepNotApplied
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

impl Run {
    /// The two `DIAG` lines: for each written image, its 32 MHz and its
    /// 8 MHz read set. `None` while either set of an image is missing.
    pub fn diags(&self) -> [Option<Diag>; PATTERN_PASSES] {
        core::array::from_fn(|i| {
            let (x, y) = (self.reads[2 * i]?, self.reads[2 * i + 1]?);
            let (fast, slow) = if x.plan.read == BusClock::M32 {
                (x, y)
            } else {
                (y, x)
            };
            Some(Diag {
                pattern: x.plan.pattern,
                write: x.plan.write,
                fast: fast.cmp,
                slow: slow.cmp,
            })
        })
    }
}

impl fmt::Display for Verdict {
    /// `RESULT pass=<0|1> reason=<..> mismatches=<n> not_ff=<n> unit=byte total_ms=<n> final_state=<..>`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            Reason::Ok => "ok",
            Reason::NoPart => "no-part",
            Reason::BlockProtected => "block-protected",
            Reason::StatusUnknown => "status-unknown",
            Reason::Error => "error",
            Reason::Mismatch => "mismatch",
            Reason::NotErased => "not-erased",
            Reason::SweepNotApplied => "sweep-not-applied",
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
            "RESULT pass={} reason={reason} mismatches={} not_ff={} unit=byte total_ms={} final_state={final_state}",
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

    /// [`READS`] copies of pattern `p` over `len` bytes from `base`.
    fn reads_of(p: Pattern, base: u32, len: usize) -> Vec<Vec<u8>> {
        (0..READS)
            .map(|_| {
                let mut b = vec![0u8; len];
                fill(p, base, &mut b);
                b
            })
            .collect()
    }

    fn check(c: &mut Compare, p: Pattern, base: u32, reads: &[Vec<u8>]) {
        let r: [&[u8]; READS] = core::array::from_fn(|i| reads[i].as_slice());
        c.check(p, base, r);
    }

    #[test]
    fn clean_reads_compare_clean() {
        let base = 0x0A_3000;
        let reads = reads_of(Pattern::B, base, 4096);
        let mut c = Compare::default();
        check(&mut c, Pattern::B, base, &reads);
        assert_eq!(c, Compare::default());
    }

    #[test]
    fn a_byte_wrong_the_same_way_in_every_read_is_stable() {
        let base = 0x1000;
        let mut reads = reads_of(Pattern::A, base, 64);
        let want = reads[0][17];
        for r in reads.iter_mut() {
            r[17] ^= 0x81; // bit 7 and bit 0, in every read
        }
        let mut c = Compare::default();
        check(&mut c, Pattern::A, base, &reads);
        assert_eq!(c.mismatches, 1);
        assert_eq!(c.stable, 1);
        assert_eq!(c.unstable, 0);
        assert_eq!(c.per_read, [1; READS]);
        assert_eq!(c.first_bad, Some(base + 17));
        assert_eq!(c.bit7_0, [5, 0, 0, 0, 0, 0, 0, 5]);
        let lost: u32 = (want & 0x81).count_ones() * READS as u32;
        assert_eq!(c.lost1, lost);
        assert_eq!(c.gained1, 2 * READS as u32 - lost);
        assert_eq!(
            c.misses(),
            &[Miss {
                addr: base + 17,
                want,
                got: [want ^ 0x81; READS]
            }]
        );
    }

    #[test]
    fn a_byte_the_reads_disagree_on_is_unstable() {
        let base = 0x2000;
        let mut reads = reads_of(Pattern::A, base, 64);
        let want = reads[0][40];
        reads[2][40] ^= 0x10;
        let mut c = Compare::default();
        check(&mut c, Pattern::A, base, &reads);
        assert_eq!(c.mismatches, 1);
        assert_eq!(c.unstable, 1);
        assert_eq!(c.stable, 0);
        assert_eq!(c.per_read, [0, 0, 1, 0, 0]);
        assert_eq!(c.bit7_0, [0, 0, 0, 1, 0, 0, 0, 0]);
        // Two reads that are wrong DIFFERENTLY are unstable too.
        let mut reads = reads_of(Pattern::A, base, 64);
        for r in reads.iter_mut() {
            r[3] ^= 0x01;
        }
        reads[4][3] ^= 0x02;
        let mut d = Compare::default();
        check(&mut d, Pattern::A, base, &reads);
        assert_eq!((d.mismatches, d.unstable, d.stable), (1, 1, 0));
        assert_eq!(d.misses()[0].got[0], want_at(Pattern::A, base + 3) ^ 0x01);
        assert_eq!(want, want_at(Pattern::A, base + 40));
    }

    fn want_at(p: Pattern, addr: u32) -> u8 {
        word(p, addr & !3).to_le_bytes()[(addr & 3) as usize]
    }

    #[test]
    fn misses_keep_the_lowest_addressed_ones_and_stop_at_the_cap() {
        let len = 4096;
        let mut c = Compare::default();
        // Fed as two chunks in address order, every byte wrong.
        for base in [0u32, 4096] {
            let mut reads = reads_of(Pattern::A, base, len);
            for r in reads.iter_mut() {
                for b in r.iter_mut() {
                    *b = !*b;
                }
            }
            check(&mut c, Pattern::A, base, &reads);
        }
        assert_eq!(c.mismatches, 8192);
        assert_eq!(c.stable, 8192);
        assert_eq!(c.misses().len(), MISS_LINES);
        for (i, m) in c.misses().iter().enumerate() {
            assert_eq!(m.addr, i as u32);
            assert_eq!(m.got, [!m.want; READS]);
        }
        // Every bit of every byte of every read, inverted.
        assert_eq!(c.lost1 + c.gained1, 8192 * 8 * READS as u32);
        assert_eq!(c.bit7_0, [8192 * READS as u32; 8]);
    }

    fn set_with(mismatches: u32, unstable: u32) -> Compare {
        Compare {
            mismatches,
            unstable,
            stable: mismatches - unstable,
            ..Compare::default()
        }
    }

    #[test]
    fn the_diagnosis_reads_the_slow_set_as_the_picture_of_the_part() {
        let diag = |fast, slow| Diag {
            pattern: Pattern::A,
            write: BusClock::M32,
            fast,
            slow,
        };
        let clean = Compare::default();
        assert_eq!(diag(clean, clean).side(), Side::None);
        // Wrong at 32 MHz, right and stable at 8: the part holds the
        // pattern, so the fast read is what is wrong. Stable or not.
        assert_eq!(diag(set_with(11_445, 11_445), clean).side(), Side::Read);
        assert_eq!(diag(set_with(11_445, 0), clean).side(), Side::Read);
        // Wrong the same way at 8 MHz: the part holds the error.
        assert_eq!(
            diag(set_with(11_445, 0), set_with(11_445, 0)).side(),
            Side::Program
        );
        assert_eq!(diag(clean, set_with(3, 0)).side(), Side::Program);
        // The slow reads disagree with each other: nothing is a reference.
        assert_eq!(
            diag(set_with(9, 0), set_with(9, 1)).side(),
            Side::Unresolved
        );
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

    /// Sweep point `n`, applied, with `mismatches` unstable bytes.
    fn cell(n: usize, mismatches: u32) -> SweepCell {
        let want = SWEEP[n];
        let bus = Bus {
            ifconfig1: Bus::with_clock(0x0004_0450, want.clock),
            iftiming: Bus::with_rxdelay(0, want.rxdelay),
        };
        SweepCell::new(
            n as u8,
            want,
            bus,
            2_740_000,
            &set_with(mismatches, mismatches),
        )
    }

    fn green_run() -> Run {
        let erase = |pass| ErasePass {
            pass,
            us: 4_100_000,
            max_sector_us: 9_000,
            not_ff: Tally::default(),
        };
        let write = |pass, pattern, clock| WritePass {
            pass,
            pattern,
            clock,
            us: 16_400_000,
        };
        let read = |i: usize| ReadSet {
            set: i as u8 + 1,
            plan: PLAN[i],
            us: 2_650_000,
            cmp: Compare::default(),
        };
        Run {
            no_part: false,
            bus: Some(Bus {
                ifconfig1: 0x0000_0050,
                iftiming: 0x0000_0200,
            }),
            status: Some(Status {
                sr1: 0,
                sr2: Some(0),
                cr: Some(0),
            }),
            census: Some(Census::new()),
            erase_issued: true,
            erases: [Some(erase(1)), Some(erase(2)), Some(erase(3))],
            writes: [
                Some(write(1, Pattern::A, BusClock::M32)),
                Some(write(2, Pattern::B, BusClock::M8)),
            ],
            reads: [Some(read(0)), Some(read(1)), Some(read(2)), Some(read(3))],
            sweep: core::array::from_fn(|n| Some(cell(n, 0))),
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
        let mut reads = reads_of(Pattern::A, 0, 4096);
        reads[3][1234] ^= 0x10;
        let mut c = Compare::default();
        check(&mut c, Pattern::A, 0, &reads);
        if let Some(r) = run.reads[0].as_mut() {
            r.cmp = c;
        }
        let v = run.verdict();
        assert!(!v.pass);
        assert_eq!(v.reason, Reason::Mismatch);
        assert_eq!(v.mismatches, 1);
        assert_eq!(run.diags()[0].map(|d| d.side()), Some(Side::Read));
        assert_eq!(run.diags()[1].map(|d| d.side()), Some(Side::None));
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
        run.reads[3] = None;
        assert_eq!(run.verdict().reason, Reason::Incomplete);
        assert_eq!(run.diags()[1], None);
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
            "WRITE pass=2 pattern=B mhz=8 bytes=2097152 ms=16400 kib_s=124"
        );
        assert_eq!(
            format!("{}", run.reads[0].unwrap()),
            "READSET set=1 pattern=A write_mhz=32 read_mhz=32 reads=5 unit=byte \
             mismatches=0 unstable=0 stable=0 per_read=0,0,0,0,0 first_bad=none \
             lost1=0 gained1=0 bit7_0=0,0,0,0,0,0,0,0 ms=2650 kib_s=3864"
        );
        let base = 0x10_0000;
        let mut reads = reads_of(Pattern::A, base, 64);
        let want = reads[0][1];
        reads[1][1] ^= 0x40;
        reads[2][1] ^= 0x40;
        let mut r = run.reads[3].unwrap();
        check(&mut r.cmp, Pattern::A, base, &reads);
        let lost = u32::from(want & 0x40 != 0) * 2;
        assert_eq!(
            format!("{r}"),
            format!(
                "READSET set=4 pattern=B write_mhz=8 read_mhz=32 reads=5 unit=byte \
                 mismatches=1 unstable=1 stable=0 per_read=0,1,1,0,0 first_bad=0x100001 \
                 lost1={lost} gained1={} bit7_0=0,2,0,0,0,0,0,0 ms=2650 kib_s=3864",
                2 - lost
            )
        );
        assert_eq!(
            format!(
                "{}",
                MissLine {
                    set: 4,
                    n: 0,
                    miss: Miss {
                        addr: 0x10_0001,
                        want: 0x5a,
                        got: [0x5a, 0x1a, 0x1a, 0x5a, 0x5a]
                    }
                }
            ),
            "MISS set=4 n=0 addr=0x100001 want=5a got=5a,1a,1a,5a,5a xor=00,40,40,00,00"
        );
        let mut run2 = run;
        run2.reads[3] = Some(r);
        assert_eq!(
            format!("{}", run2.diags()[1].unwrap()),
            "DIAG pattern=B write_mhz=8 fast_mismatches=1 fast_unstable=1 \
             slow_mismatches=0 slow_unstable=0 side=read"
        );
        let mut c = cell(1 + RXDELAYS + 3, 0);
        c.mismatches = 7;
        c.unstable = 6;
        c.stable = 1;
        c.lost1 = 1;
        c.gained1 = 9;
        c.bit7_0 = [0, 3, 0, 0, 0, 0, 7, 0];
        assert_eq!(
            format!("{c}"),
            "READSWEEP n=12 sck_khz=32000 rxdelay=3 applied=1 pattern=B reads=5 unit=byte \
             mismatches=7 unstable=6 stable=1 lost1=1 gained1=9 bit7_0=0,3,0,0,0,0,7,0 \
             ms=2740 kib_s=3737"
        );
        assert_eq!(
            format!("{}", run.bus.unwrap()),
            "BUS sck_khz=32000 sckfreq=0 rxdelay=2 sckdelay=80 ifconfig1=00000050 iftiming=00000200"
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
            "RESULT pass=1 reason=ok mismatches=0 not_ff=0 unit=byte total_ms=49000 final_state=erased"
        );
    }

    #[test]
    fn geometry_matches_the_part() {
        assert_eq!(SECTORS, 512);
        assert_eq!(PAGES, 8192);
        assert_eq!(WINDOWS, 32);
        // Sets 1 and 4 and eight sweep points at 32 MHz; eight sweep
        // points at 16; the census, three erases, sets 2 and 3 and the
        // sweep's control at 8.
        assert_eq!(full_reads(BusClock::M32), (2 + 8) * READS as u32);
        assert_eq!(full_reads(BusClock::M16), 8 * READS as u32);
        assert_eq!(full_reads(BusClock::M8), 1 + 3 + (2 + 1) * READS as u32);
        assert_eq!(FULL_READS, 109);
        // Read chunks and sectors are page-aligned multiples, and a chunk
        // never crosses a census window.
        assert_eq!(SECTOR_BYTES % PAGE_BYTES, 0);
        assert_eq!(WINDOW_BYTES % READ_CHUNK_BYTES, 0);
    }

    #[test]
    fn the_plan_reads_each_written_image_at_both_clocks() {
        for i in 0..PATTERN_PASSES {
            let (x, y) = (PLAN[2 * i], PLAN[2 * i + 1]);
            assert_eq!(x.pattern, y.pattern);
            assert_eq!(x.write, y.write);
            assert_ne!(x.read, y.read);
        }
        assert_ne!(PLAN[0].pattern, PLAN[2].pattern);
        assert_ne!(PLAN[0].write, PLAN[2].write);
        // Each erase verifies at 8 MHz, the clock the bus is left at by
        // what runs before it: the census, set 2, the sweep's restore.
        assert_eq!(ERASE_CLOCK, [BusClock::M8; ERASE_PASSES]);
        assert_eq!(ERASE_CLOCK[0], CENSUS_CLOCK);
        assert_eq!(ERASE_CLOCK[1], PLAN[1].read);
    }

    #[test]
    fn the_sweep_is_the_control_then_every_rxdelay_at_16_and_32() {
        assert_eq!(SWEEP[0], at_default_rxdelay(BusClock::M8));
        for d in 0..RXDELAYS {
            assert_eq!(SWEEP[1 + d].clock, BusClock::M16);
            assert_eq!(SWEEP[1 + d].rxdelay, d as u8);
            assert_eq!(SWEEP[1 + RXDELAYS + d].clock, BusClock::M32);
            assert_eq!(SWEEP[1 + RXDELAYS + d].rxdelay, d as u8);
        }
        // It reads what write pass 2 left on the part, which set 3 has
        // already read clean at 8 MHz.
        assert_eq!(SWEEP_PATTERN, PLAN[2].pattern);
        assert_eq!(PLAN[2].read, BusClock::M8);
    }

    #[test]
    fn rxdelay_moves_alone_and_the_bus_line_reads_it_back() {
        let t = Bus::with_rxdelay(0xFFFF_FFFF, 5);
        assert_eq!(t | 0x0000_0700, 0xFFFF_FFFF);
        let bus = Bus {
            ifconfig1: Bus::with_clock(0x0004_0450, BusClock::M16),
            iftiming: t,
        };
        assert_eq!(bus.rxdelay(), 5);
        assert_eq!(bus.sck_khz(), 16_000);
        assert!(bus.carries(Timing {
            clock: BusClock::M16,
            rxdelay: 5
        }));
        assert!(!bus.carries(Timing {
            clock: BusClock::M32,
            rxdelay: 5
        }));
        assert!(!bus.carries(at_default_rxdelay(BusClock::M16)));
        // Only the low three bits exist.
        assert_eq!(Bus::with_rxdelay(0, 9), 0x0000_0100);
    }

    #[test]
    fn the_sweep_recommends_the_middle_of_the_fastest_clean_window() {
        // Everything red but the control: 8 MHz at the default delay.
        let mut cells: [Option<SweepCell>; SWEEP_POINTS] =
            core::array::from_fn(|n| Some(cell(n, 1000)));
        cells[0] = Some(cell(0, 0));
        let b = SweepBest::of(&cells);
        assert_eq!(b.best, Some(at_default_rxdelay(BusClock::M8)));
        assert_eq!(
            format!("{b}"),
            "SWEEPBEST sck_khz=8000 rxdelay=2 clean_rxdelays=2"
        );
        // 16 MHz clean at 1..=4 and 6: the longest run is 1..=4, its
        // middle (rounded down) 2.
        for d in [1, 2, 3, 4, 6] {
            cells[1 + d] = Some(cell(1 + d, 0));
        }
        let b = SweepBest::of(&cells);
        assert_eq!(
            b.best,
            Some(Timing {
                clock: BusClock::M16,
                rxdelay: 2
            })
        );
        assert_eq!(
            format!("{b}"),
            "SWEEPBEST sck_khz=16000 rxdelay=2 clean_rxdelays=1,2,3,4,6"
        );
        // One clean 32 MHz cell beats any number of 16 MHz ones.
        cells[1 + RXDELAYS + 7] = Some(cell(1 + RXDELAYS + 7, 0));
        let b = SweepBest::of(&cells);
        assert_eq!(
            b.best,
            Some(Timing {
                clock: BusClock::M32,
                rxdelay: 7
            })
        );
        // A clean cell whose registers did not take is not clean.
        let mut c = cell(1 + RXDELAYS + 7, 0);
        c.bus.iftiming = Bus::with_rxdelay(0, 2);
        assert!(!c.applied());
        cells[1 + RXDELAYS + 7] = Some(c);
        assert_eq!(
            SweepBest::of(&cells).best.map(|t| t.clock),
            Some(BusClock::M16)
        );
        // Nothing clean at all.
        let none: [Option<SweepCell>; SWEEP_POINTS] = core::array::from_fn(|n| Some(cell(n, 1)));
        assert_eq!(
            format!("{}", SweepBest::of(&none)),
            "SWEEPBEST sck_khz=none rxdelay=none clean_rxdelays=none"
        );
    }

    #[test]
    fn red_sweep_cells_do_not_redden_the_run_but_a_missed_one_does() {
        let mut run = green_run();
        run.sweep[9] = Some(cell(9, 1_700_000));
        assert_eq!(run.verdict().reason, Reason::Ok);
        let mut c = cell(9, 0);
        c.bus.ifconfig1 = Bus::with_clock(c.bus.ifconfig1, BusClock::M8);
        run.sweep[9] = Some(c);
        assert_eq!(run.verdict().reason, Reason::SweepNotApplied);
        run.sweep[9] = None;
        assert_eq!(run.verdict().reason, Reason::Incomplete);
    }

    #[test]
    fn bus_clock_codes_are_the_peripherals() {
        // SCK = 32 MHz / (SCKFREQ + 1).
        for c in [BusClock::M32, BusClock::M16, BusClock::M8] {
            let bus = Bus {
                ifconfig1: Bus::with_clock(0xFFFF_FFFF, c),
                iftiming: 0,
            };
            assert_eq!(bus.sck_khz(), c.mhz() * 1000);
            // Only SCKFREQ moves.
            assert_eq!(bus.ifconfig1 | 0xF000_0000, 0xFFFF_FFFF);
        }
        assert_eq!(BusClock::M8.bytes_per_s(), 1_000_000);
    }

    #[test]
    fn time_bounds_follow_from_the_datasheet() {
        // 2 MiB at 4 MB/s is 524.288 ms, at 1 MB/s 2097.152 ms; rounded up.
        // At 2 MB/s 1048.576 ms.
        assert_eq!(full_read_ms(BusClock::M32), 525);
        assert_eq!(full_read_ms(BusClock::M16), 1049);
        assert_eq!(full_read_ms(BusClock::M8), 2098);
        // 50 reads at 32, 40 at 16, 19 at 8, one program transfer at 32
        // and one at 8.
        let transfer = 50 * 525 + 40 * 1049 + 19 * 2098 + 525 + 2098;
        // 3 x 512 x 20 + 2 x 8192 x 3 + transfers
        assert_eq!(worst_case_ms(), 30_720 + 49_152 + transfer);
        assert_eq!(worst_case_ms(), 190_567);
        // The sweep alone: 40 x 525 + 40 x 1049 + 5 x 2098 ms of reads,
        // 73.45 s, against the 2026-10-04 run's 134 s in all.
        assert_eq!(40 * 525 + 40 * 1049 + 5 * 2098, 73_450);
        // With comparison time per read set and sweep point: 9.5 min.
        assert_eq!(wall_bound_ms(), 190_567 + 21 * 18_000);
        assert_eq!(wall_bound_ms(), 568_567);
        // 3 x 512 x 8 + 2 x 8192 x 2 + transfers
        assert_eq!(typical_ms(), 12_288 + 32_768 + transfer);
        // Every timeout sits above the datasheet maximum of its operation,
        // a page program above tPP max plus its 260-byte transfer at the
        // slow clock (260 us).
        const { assert!(ERASE_TIMEOUT_MS > T_SE_MAX_MS) };
        const { assert!(PROGRAM_TIMEOUT_MS * 1000 > T_PP_MAX_MS * 1000 + 260) };
        // 4096 B at 1 MB/s is 4.096 ms, rounded up to 5, times 20.
        assert_eq!(READ_TIMEOUT_MS, 100);
        assert_eq!(
            timeout_bound_ms(),
            3 * 512 * 100 + 2 * 8192 * 15 + 109 * 512 * 100
        );
    }

    #[test]
    fn rate_is_kib_per_second() {
        assert_eq!(kib_per_s(PART_BYTES, 1_000_000), 2048);
        assert_eq!(kib_per_s(PART_BYTES, 0), 0);
    }
}
