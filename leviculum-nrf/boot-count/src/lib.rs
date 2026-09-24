//! The boot record that survives a power loss: one append-only page of
//! internal flash (Codeberg #380).
//!
//! # Why this exists next to `leviculum-boot-trace`
//!
//! The breadcrumb record in retained RAM answers *what the previous boot
//! was doing when it died*. It cannot answer *how many times this board
//! restarted*, because retained RAM is gone by definition after a power
//! loss — and a power loss is exactly the case that matters. Every field
//! boot we have looked at read `reset_reason=0x00000000` with
//! `prev_magic=absent`: the pack sagged, the board came up new, and the
//! only reason anyone knew it had happened at all was that someone
//! subtracted uptime stamps from a watch log afterwards. A count that
//! survives the supply going away has to live in flash.
//!
//! This crate is the pure half: the record layout, the scan that decides
//! where this boot's record goes, and the wrap that carries the boot
//! number forward past an erase. The flash I/O — one memory-mapped read
//! and one NVMC write — stays in `leviculum-nrf/src/boot_count.rs`, the
//! same split `leviculum-boot-trace` and `leviculum-log-line` use: the
//! firmware crate cross-compiles and runs no host tests, so everything
//! that can be asserted is asserted here.
//!
//! # Append, do not rewrite
//!
//! A counter kept at a fixed address and rewritten on every boot costs
//! one page erase per boot. The nRF52840's internal flash is rated
//! [`ENDURANCE_CYCLES`] erase cycles per page, so that scheme spends the
//! page's whole budget in [`ENDURANCE_CYCLES`] boots — ten thousand, a
//! number a board on a bench can reach.
//!
//! So each boot *appends* one [`RECORD_SIZE`]-byte record into the next
//! free slot of the page, which costs no erase at all: NOR programming
//! only clears bits, and an erased slot is all ones. **One erase cycle
//! covers [`RECORDS_PER_PAGE`] = 256 boots**, the page is erased only
//! when its last slot is spent, and the boot number is carried forward
//! across that erase so it never restarts at zero. Over the rated
//! endurance that is [`BOOTS_PER_ENDURANCE`] = 2 560 000 boots, 256× what
//! the rewrite-every-boot scheme buys — the arithmetic is pinned in this
//! crate's tests rather than left in this comment.
//!
//! # On-flash layout
//!
//! One page ([`PAGE_SIZE`] = 4096 B), 256 fixed records of 16 bytes, no
//! header and no index: the page describes itself, and nothing is ever
//! written to a fixed location.
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 4 | magic [`MAGIC`], u32 LE |
//! | 4 | 4 | boot number, u32 LE |
//! | 8 | 4 | `POWER.RESETREAS` as the boot trace read it, u32 LE |
//! | 12 | 1 | flags: bit 0 = retained RAM survived |
//! | 13 | 1 | format version [`FORMAT_VERSION`] |
//! | 14 | 2 | CRC-16 over bytes 0..14, u16 LE |
//!
//! 16 bytes and not 12, which would also fit 341 records: the record is
//! four whole NVMC words (the part writes 32 bits at a time), 256 of them
//! tile the page exactly with nothing left over, and the spare four bytes
//! buy a 32-bit magic. The magic is not decoration — until this batch the
//! page's address was inside the linker's FLASH region, so a board may
//! well carry application bytes there from an older image, and a 12-byte
//! record gated only by a version byte and a CRC would accept one such
//! slot in every 16 million. With the magic in front it is one in 2^48,
//! and a page of foreign bytes is erased rather than parsed (see
//! [`plan`]).
//!
//! # How a torn write is recognised
//!
//! The NVMC programs the record's four words in ascending address order,
//! so a power cut mid-record leaves a prefix written and the rest erased.
//! The CRC is in the last word: a cut before that word lands leaves
//! `0xFFFF` there, which cannot match, and a cut *inside* it leaves a
//! half-programmed word the CRC catches. A record therefore counts
//! **iff** its magic, version and CRC all check — a partial record from a
//! cut is never counted as a boot.
//!
//! The slot it occupies is not reused either. NOR programming cannot
//! raise a bit, so writing a second record over a torn one would produce
//! bytes neither record ever contained; [`plan`] skips the torn slot and
//! appends to the next one.

#![cfg_attr(not(test), no_std)]

/// Erase granularity of the nRF52840's internal flash, and the size of
/// the page this log owns (`embassy_nrf::nvmc::PAGE_SIZE`).
pub const PAGE_SIZE: usize = 4096;

/// One record, four NVMC words.
pub const RECORD_SIZE: usize = 16;

/// How many boots one erase cycle of the page covers.
pub const RECORDS_PER_PAGE: usize = PAGE_SIZE / RECORD_SIZE;

/// Record-valid marker, and the version gate for the layout above: bump
/// [`FORMAT_VERSION`] when a field moves, so a page written by an older
/// image reads as empty instead of misparsed.
pub const MAGIC: u32 = 0xB007_C0DE;

/// Layout version stored in every record.
pub const FORMAT_VERSION: u8 = 1;

/// Erase cycles the nRF52840's internal flash is rated for, per page.
///
/// From Nordic's nRF52840 Product Specification (NVMC chapter, flash
/// endurance). **No copy of that document is on these machines**, so the
/// figure is carried here as a stated assumption rather than as a
/// citation we can re-read — the same honesty the QSPI record log applies
/// to its own program-per-page question. What the tests pin is the
/// arithmetic built on it; if the true figure differs, one constant
/// changes and every derived number follows.
pub const ENDURANCE_CYCLES: u32 = 10_000;

/// Boots one page survives over its rated endurance: 2 560 000.
pub const BOOTS_PER_ENDURANCE: u32 = RECORDS_PER_PAGE as u32 * ENDURANCE_CYCLES;

/// An erased byte of NOR flash.
const ERASED: u8 = 0xFF;

/// CRC-16/CCITT-FALSE polynomial and seed, as in
/// `leviculum_core::framing::crc16` and `leviculum_record_log`.
const CRC_POLY: u16 = 0x1021;
const CRC_INIT: u16 = 0xFFFF;

/// One boot, as it sits in the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BootRecord {
    /// Monotonic across erases of the page: the wrap carries it forward.
    /// Starts at 1 on a page that holds no readable record.
    pub boot: u32,
    /// Raw `POWER.RESETREAS` at entry to `main`, exactly as
    /// `leviculum-nrf/src/boot_trace.rs` read and cleared it. Zero on a
    /// power-on or brownout reset, which latch no bit — which is why the
    /// flag below is what separates a power loss from a watchdog or a
    /// commanded reset.
    pub reset_reason: u32,
    /// Whether the previous boot's breadcrumb record in retained RAM was
    /// still there. False means RAM lost its contents: the supply went
    /// away, or an image with a different record layout ran in between.
    pub retained: bool,
}

/// Bit 0 of the flags byte.
const FLAG_RETAINED: u8 = 1 << 0;

/// Serialise a record into the bytes the NVMC writes.
pub fn encode(record: &BootRecord) -> [u8; RECORD_SIZE] {
    let mut buf = [0u8; RECORD_SIZE];
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&record.boot.to_le_bytes());
    buf[8..12].copy_from_slice(&record.reset_reason.to_le_bytes());
    buf[12] = if record.retained { FLAG_RETAINED } else { 0 };
    buf[13] = FORMAT_VERSION;
    let crc = crc16(&buf[0..14]);
    buf[14..16].copy_from_slice(&crc.to_le_bytes());
    buf
}

/// Read one slot back. `None` for an erased slot, a torn write, foreign
/// bytes, or a record this image's layout version does not own.
pub fn decode(slot: &[u8]) -> Option<BootRecord> {
    if slot.len() < RECORD_SIZE {
        return None;
    }
    if u32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]) != MAGIC {
        return None;
    }
    if slot[13] != FORMAT_VERSION {
        return None;
    }
    let stored = u16::from_le_bytes([slot[14], slot[15]]);
    if crc16(&slot[0..14]) != stored {
        return None;
    }
    Some(BootRecord {
        boot: u32::from_le_bytes([slot[4], slot[5], slot[6], slot[7]]),
        reset_reason: u32::from_le_bytes([slot[8], slot[9], slot[10], slot[11]]),
        retained: slot[12] & FLAG_RETAINED != 0,
    })
}

/// What the page says right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scan {
    /// The newest readable record, or `None` on a page that holds none.
    pub last: Option<BootRecord>,
    /// How many readable records precede the free slot.
    pub records: usize,
    /// Byte offset of the next free slot, or `None` when the page is
    /// spent (or unusable, see [`plan`]) and has to be erased first.
    pub free: Option<usize>,
}

/// Read the page: count the records, find the newest, find where the next
/// one goes.
///
/// Scanning stops at the first slot that is not a readable record,
/// because that is the only place one can be: records are appended in
/// order and nothing is ever written past the free slot. If that slot is
/// still erased it is the free one. If it is not, it holds a torn write
/// (or bytes that were there before this page became a boot log), the
/// slot is retired unread, and the one after it is the free one — if
/// *that* one is not erased either, the page is not something to append
/// to and [`Scan::free`] is `None`.
pub fn scan(page: &[u8]) -> Scan {
    let slots = page.len() / RECORD_SIZE;
    let mut last = None;
    let mut records = 0usize;
    for i in 0..slots {
        let slot = &page[i * RECORD_SIZE..(i + 1) * RECORD_SIZE];
        match decode(slot) {
            Some(record) => {
                last = Some(record);
                records += 1;
            }
            None => {
                let free = if is_erased(slot) {
                    Some(i)
                } else if i + 1 < slots
                    && is_erased(&page[(i + 1) * RECORD_SIZE..(i + 2) * RECORD_SIZE])
                {
                    Some(i + 1)
                } else {
                    None
                };
                return Scan {
                    last,
                    records,
                    free: free.map(|s| s * RECORD_SIZE),
                };
            }
        }
    }
    Scan {
        last,
        records,
        free: None,
    }
}

/// What this boot must do to the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    /// Erase the whole page before writing. True when the last slot is
    /// spent, and when the page holds bytes that cannot be appended to.
    pub erase_first: bool,
    /// Byte offset within the page for this boot's record.
    pub offset: usize,
    /// The record to write there.
    pub record: BootRecord,
    /// How many slots of the page are spent once this record is in,
    /// counting this one and counting a slot retired by a torn write:
    /// `offset / RECORD_SIZE + 1`. This is the `since_erase=` of the
    /// `BOOT_COUNT` line, and it counts *down* to the next erase from
    /// [`RECORDS_PER_PAGE`].
    pub since_erase: u32,
}

/// Decide this boot's record and where it goes.
///
/// The boot number is the newest readable record's plus one, or 1 on a
/// page that holds none — and it is taken from the scan of the *whole*
/// page, so an erase carries it forward rather than restarting the count.
/// `wrapping_add` and not a saturating one: the counter is 32 bits wide
/// and the page it lives on wears out after [`BOOTS_PER_ENDURANCE`]
/// boots, three orders of magnitude before `u32` could wrap, so the
/// wrap-vs-saturate question is not one a real board reaches (pinned in
/// `the_counter_outlives_the_page_it_lives_on`).
pub fn plan(page: &[u8], reset_reason: u32, retained: bool) -> Plan {
    let scan = scan(page);
    let record = BootRecord {
        boot: scan.last.map_or(1, |r| r.boot.wrapping_add(1)),
        reset_reason,
        retained,
    };
    let (erase_first, offset) = match scan.free {
        Some(offset) => (false, offset),
        None => (true, 0),
    };
    Plan {
        erase_first,
        offset,
        record,
        since_erase: (offset / RECORD_SIZE) as u32 + 1,
    }
}

/// Whether every byte of `slot` is still erased.
fn is_erased(slot: &[u8]) -> bool {
    slot.iter().all(|b| *b == ERASED)
}

/// CRC-16/CCITT-FALSE over `data`.
///
/// Same polynomial, seed and bit order as `leviculum_core::framing::crc16`
/// and `leviculum_record_log::crc16_update`; the standard check value
/// (`"123456789"` → `0x29B1`) is asserted in this crate's tests, which is
/// what keeps the copies from drifting. Duplicated rather than shared for
/// the reason the record log gives: these sixteen lines are the only
/// thing this crate would take from `leviculum-core`, and the firmware's
/// pure crates carry no dependencies.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = CRC_INIT;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ CRC_POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// The body of the `BOOT_COUNT` line the firmware emits once per boot,
/// ungated, on the debug port:
///
/// ```text
/// BOOT_COUNT n=42 reset_reason=0x00000004 retained=1 since_erase=17 t=118
/// ```
///
/// Stable keys, scalar values, one line — the structured-event-log format
/// (`docs/src/structured-event-logs.md`). The ` t=<ms>` stamp is the log
/// formatter's, like on every other line.
pub struct BootCountLine {
    pub record: BootRecord,
    pub since_erase: u32,
}

impl core::fmt::Display for BootCountLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "BOOT_COUNT n={} reset_reason=0x{:08x} retained={} since_erase={}",
            self.record.boot,
            self.record.reset_reason,
            self.record.retained as u8,
            self.since_erase
        )
    }
}

#[cfg(test)]
mod tests;
