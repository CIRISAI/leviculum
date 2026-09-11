//! A forward-only record log on 4 KiB flash pages.
//!
//! # What a record is at this layer
//!
//! **A record is a byte string with a 32-byte key, a 4-byte timestamp and a
//! 1-byte tag. Nothing here knows about LXMF, messages, telemetry or
//! diagnostics.** The key is opaque: an LXMF propagation store puts a
//! transient ID in it, the field diagnostics ring (Codeberg #380) puts
//! whatever identifier its entries carry, telemetry retention puts a target
//! hash. The tag is one opaque byte — the propagation store's stamp value in
//! the concept paper's table, a severity or a kind for anyone else. This
//! layer reads neither.
//!
//! That is a deliberate line. The store is the part of a propagation node
//! that is worth having whether or not the node ever exists
//! (`docs/src/concepts/propagation-node-on-a-board.md`, §4), so it must not
//! be reachable only through an LXMF-shaped door.
//!
//! # Which part this is for, and what that changed
//!
//! This crate was written for an external QSPI NOR part. Neither board
//! carries one (#384, commit 081522b2), so the region it drives is the
//! nRF52840's own flash behind the firmware image — and three of the
//! numbers the design was argued from are different there. The argument
//! survives; the numbers had to be recomputed, and the endurance one got an
//! order of magnitude worse.
//!
//! | | External NOR (as written) | nRF52840 internal (now) |
//! |---|---|---|
//! | Erase unit | 4 KiB sector | 4 KiB page |
//! | Endurance | 100 000 cycles | **10 000 cycles** |
//! | Program unit | 1 byte (4 via QSPI DMA) | **1 word, always** |
//! | Writes per unit between erases | datasheet unknown | **2** |
//! | Erase time | 58–70 ms typ | 85 ms |
//!
//! Source for the right-hand column: nRF52840 Product Specification, NVMC
//! chapter (10 000 erase cycles per page, 85 ms page erase, 41 µs word
//! write, a word writable at most twice between erases). With the
//! SoftDevice enabled the NVMC is *Restricted* and only `sd_flash_write` /
//! `sd_flash_page_erase` may touch it (S140 SDS, Hardware peripherals),
//! which is also where the word-at-a-time rule becomes absolute.
//!
//! # Why forward-only, and why round-robin reclaim
//!
//! The argument is unchanged and the recomputed numbers make it sharper,
//! because a tenth of the erase budget is a tenth of the room for getting
//! this wrong.
//!
//! Take the region this store lives in: the **16 pages of 4 KiB**
//! `leviculum-nrf/memory.x` reserves at `0xDA000`..`0xEA000`, between the
//! firmware image and the three persistence pages. At the duty the 2026-09-09
//! field walk measured — 22.4 messages/hour, 196 224 a year — and 11
//! field-sized records to a page, that is 17 838 page erases a year:
//!
//! | | Erases/year | Budget | Life |
//! |---|---|---|---|
//! | Spread over the 16 pages | 1 115 per page | 10 000 | **9 years** |
//! | Spread over 68 pages (the whole window, for scale) | 262 per page | 10 000 | 38 years |
//! | One fixed metadata page | 196 224 | 10 000 | **18.6 days** |
//!
//! **A single fixed metadata page, rewritten on every accepted record,
//! spends its whole budget in eighteen days.** On the external part the same
//! line read six months, which is long enough to sound survivable; eighteen
//! days is not. The first two rows are the same design; the third is a
//! different one, and it is the one this format exists to avoid. (Why 16
//! pages rather than the 68 the window could hold is a separate trade —
//! image headroom against store life — and it is argued where the number
//! lives, in `memory.x`.)
//!
//! So there is no superblock, no index page, no head pointer and no sequence
//! counter at a fixed address. Everything this log needs to
//! mount itself is recovered by reading the page headers, and a page header
//! is written exactly once per erase of the page it heads — which is the
//! definition of level wear.
//!
//! Reclaim is round-robin over the region: when the active page cannot fit
//! the next record, the *next* page by index is erased and becomes active,
//! dropping the oldest records with it. Every page is therefore erased once
//! per lap, and `SimNor::erase_counts` in the tests pins that with its own
//! negative control.
//!
//! # On-flash layout
//!
//! Every offset in the region is a multiple of `PROGRAM_UNIT` (4). That is
//! not a taste, and the reason for it changed with the part: the QSPI
//! peripheral's DMA alignment assertions are gone, and in their place is
//! `sd_flash_write`, which takes a word-aligned destination and a **length
//! in words** (S140 SDS, SoC library) — there is no call that writes three
//! bytes. `nrf_softdevice::Flash` publishes exactly that as
//! `WRITE_SIZE = 4`.
//!
//! ## Page header, 12 bytes, written once per erase
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 4 | magic `LVR1` |
//! | 4 | 4 | sequence, u32 LE — strictly increasing across erases |
//! | 8 | 1 | format version |
//! | 9 | 1 | reserved, 0 |
//! | 10 | 2 | CRC-16 over bytes 0..10 |
//!
//! The sequence is what makes a fixed metadata page unnecessary: the active
//! page is the one with the highest sequence, and the oldest is the one
//! after it round-robin. Mounting is a read of `pages` × 12 bytes.
//!
//! ## Record, 42-byte header then the body then 0xFF padding to a multiple of 4
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 2 | body length, u16 LE |
//! | 2 | 32 | key |
//! | 34 | 4 | timestamp, u32 LE |
//! | 38 | 1 | tag |
//! | 39 | 1 | flags: `0xFF` uncommitted, `0xFE` live, `0xFC` purged |
//! | 40 | 2 | CRC-16 over header bytes 0..39 and the body |
//! | 42 | len | body |
//!
//! This is the 42-byte header the concept paper tabulates, with its
//! propagation-specific names generalised (transient ID → key, stamp value →
//! tag). The destination hash is not a field: it is the first bytes of the
//! body, exactly as the reference reads it back from the head of its file.
//!
//! ## How a torn write is recognised, in two writes per word
//!
//! A record is written in three programs and committed by the last:
//!
//! 1. Header bytes `0..36` — everything up to but not including the commit
//!    word.
//! 2. Header bytes `40..42` (the CRC), the body, and 0xFF padding to the
//!    record's stride. Programmed from offset 40, which is word-aligned, so
//!    the commit word at `36..40` is stepped over and left erased.
//! 3. The commit word at offset 36 — `timestamp[2..4]`, `tag`, `flags` —
//!    with `flags` already `0xFE`.
//!
//! **The skip in step 2 is the whole reason this format fits the internal
//! flash.** The obvious implementation programs the header in one run and
//! then re-programs the commit word to flip its flags byte: two writes of
//! that word, which is the entire budget the nRF52840 allows between
//! erases, leaving none for a purge. Leaving the word erased until it is the
//! commit spends one write on the commit and keeps the second for
//! [`RecordLog::purge`] — which is why `purge` is the one operation bounded
//! on [`MultiwriteNorFlash`] rather than plain `NorFlash`.
//!
//! It costs one extra flash operation per append (three rather than two).
//! At 41 µs a word and the SoftDevice's per-operation scheduling round trip,
//! that is not the cost that matters; the erase is.
//!
//! A record counts as present **iff** its flags byte reads `0xFE` or `0xFC`
//! *and* its CRC checks. Any cut before step 3 leaves `0xFF` there and the
//! record is not seen — deterministically, not with 1-in-65536 confidence.
//! The CRC is then doing what a CRC should: catching a bit the part dropped,
//! not standing in for a commit protocol.
//!
//! Recovery adds one rule. After the last valid record in the active page,
//! if the remainder of that page is not still erased, the page is *sealed* —
//! the cursor jumps to the next page — because programming over a
//! half-written record would need to raise bits, which flash cannot do. The
//! cost is at most one partly-used page per power cut; the alternative is
//! silent corruption.
//!
//! # Cancellation
//!
//! Every method here is `async` because the only legal way to write this
//! flash is `nrf_softdevice::Flash`, which is. That makes cancellation a
//! safety question rather than a style one: a `select!` that drops an
//! in-flight `append` at an `.await` leaves the record uncommitted, which is
//! byte-for-byte what a power cut leaves **on the part** — the commit word is
//! still `0xFF`, the next mount seals the page, and nothing is lost but the
//! room. There is no repair pass to run and no torn-append state to detect,
//! because the commit is one word and one word cannot be half-written.
//!
//! What a power cut also does, and a cancellation does not, is take the
//! in-RAM cursor with it. A dropped future runs no code, so it cannot move
//! the cursor on its way out, and the **same handle** then still believes the
//! record's offset is free; the next append programs over words that already
//! hold half a record, which needs bits raised. So [`RecordLog::append`]
//! seals its page *before* its first program run and puts the cursor back
//! only on a path that reaches the end — success, or a failure that landed
//! nothing. The cost is the rest of one page per cancellation, and a
//! cancellation is the only way to pay it for nothing (a drop at the first
//! await has written no byte). `a_dropped_append_seals_its_page_on_the_same_handle`
//! pins it at every drop point, with
//! `the_unsealed_cursor_programs_over_a_half_written_record` as the negative
//! control; `cancel_safety_is_the_power_cut_case` covers the reboot case, and
//! passed before the cursor was sealed precisely because a remount recomputes
//! it.
//!
//! The cheap discipline on top of that: give the store its own task and reach
//! it by channel, so no caller's timeout can drop a future mid-append. On
//! `nrf_softdevice::Flash` that is not merely cheap but mandatory — its write
//! and erase futures arm a `DropBomb` (nrf-softdevice 5949a5b,
//! `nrf-softdevice/src/flash.rs`) and **panic** if dropped in flight, so a
//! caller that could cancel an append would be a caller that could panic the
//! board. The sealing rule above is what makes the awaits *between*
//! operations safe, which are the ones a drop can actually reach there.

#![no_std]

#[cfg(any(test, feature = "sim"))]
extern crate alloc;

#[cfg(any(test, feature = "sim"))]
pub mod sim;

#[cfg(test)]
mod tests;

use embedded_storage_async::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};

/// Erase granularity of the part, and the only one this log supports.
pub const SECTOR_SIZE: u32 = 4096;
/// Smallest unit that may be programmed, in bytes.
///
/// Set by `sd_flash_write`, which takes a word-aligned address and a length
/// in words.
pub const PROGRAM_UNIT: u32 = 4;
/// Length of the per-record header.
pub const HEADER_LEN: usize = 42;
/// Length of the per-page header, written once per erase of that page.
pub const SECTOR_HEADER_LEN: u32 = 12;
/// Length of a record key.
pub const KEY_LEN: usize = 32;

/// Bytes of a page available to records.
pub const SECTOR_PAYLOAD: u32 = SECTOR_SIZE - SECTOR_HEADER_LEN;
/// Largest body this layer will store. A record never straddles a page.
pub const MAX_BODY: usize = SECTOR_PAYLOAD as usize - HEADER_LEN;
/// Stride of a record with an empty body — the smallest room a record needs.
pub const MIN_STRIDE: u32 = align_up(HEADER_LEN) as u32;

/// `LVR1`, little-endian.
const MAGIC: u32 = 0x3152_564C;
const VERSION: u8 = 1;

/// Uncommitted: the erased state. Not a record.
const FLAG_ERASED: u8 = 0xFF;
/// Committed and current.
pub const FLAG_LIVE: u8 = 0xFE;
/// Committed and withdrawn. Still occupies its bytes until its page is
/// reclaimed; that is the whole point of a forward-only log.
pub const FLAG_PURGED: u8 = 0xFC;

/// Offset within a record header of the 4-byte commit word.
const COMMIT_OFF: u32 = 36;
/// Index of the flags byte inside the commit word.
const COMMIT_FLAGS_IX: usize = 3;
/// First byte after the commit word: where the second program run starts.
const AFTER_COMMIT: u32 = COMMIT_OFF + PROGRAM_UNIT;

const CRC_INIT: u16 = 0xFFFF;
const CRC_POLY: u16 = 0x1021;

/// Round up to the program unit.
pub const fn align_up(n: usize) -> usize {
    (n + (PROGRAM_UNIT as usize - 1)) & !(PROGRAM_UNIT as usize - 1)
}

/// Bytes a record with `body_len` bytes of body occupies on flash.
pub const fn record_stride(body_len: usize) -> usize {
    align_up(HEADER_LEN + body_len)
}

/// CRC-16/CCITT-FALSE, incremental.
///
/// Same polynomial, seed and bit order as `leviculum_core::framing::crc16`;
/// the standard check value (`"123456789"` → `0x29B1`) is asserted in this
/// crate's tests, which is what keeps the two from drifting. Duplicated
/// rather than shared because these sixteen lines are the only thing this
/// crate would take from `leviculum-core`, and the firmware's pure crates
/// carry no dependencies for a reason.
pub fn crc16_update(mut crc: u16, data: &[u8]) -> u16 {
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

/// What went wrong. `E` is the underlying device's error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The device said no.
    Flash(E),
    /// Body longer than [`MAX_BODY`]; a record never straddles a page.
    BodyTooLarge,
    /// Region base or length is not a whole number of pages, or the region
    /// is shorter than the two pages reclaim needs.
    BadRegion,
    /// The region does not fit inside the device.
    OutOfBounds,
    /// The device's erase, program or read granularity is not one this log
    /// can drive.
    UnsupportedGeometry,
}

/// One record's header, plus where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// The caller's key. Opaque here.
    pub key: [u8; KEY_LEN],
    /// The caller's timestamp. Opaque here.
    pub time: u32,
    /// The caller's tag byte. Opaque here.
    pub tag: u8,
    /// [`FLAG_LIVE`] or [`FLAG_PURGED`].
    pub flags: u8,
    /// Body length in bytes.
    pub len: u16,
    /// Absolute device offset of the record header.
    pub offset: u32,
}

impl Record {
    /// Absolute device offset of the body.
    pub fn body_offset(&self) -> u32 {
        self.offset + HEADER_LEN as u32
    }

    /// Bytes this record occupies on flash, padding included.
    pub fn stride(&self) -> u32 {
        record_stride(self.len as usize) as u32
    }

    /// Whether the record is still current.
    pub fn is_live(&self) -> bool {
        self.flags == FLAG_LIVE
    }
}

/// A 4-byte-aligned scratch buffer.
///
/// Kept aligned although the internal flash no longer demands it: the
/// firmware may yet drive a part behind a DMA engine that does, and an
/// aligned buffer costs nothing.
#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);

/// Window used for streaming reads and programs. Any multiple of 4 works;
/// 64 keeps the stack cost of a scan at one cache-line-ish buffer.
const WINDOW: usize = 64;

/// A forward-only record log over a region of flash.
pub struct RecordLog<F> {
    flash: F,
    base: u32,
    sectors: u32,
    active: u32,
    seq: u32,
    /// Offset of the next record *within* the active page.
    cursor: u32,
}

impl<F: NorFlash> RecordLog<F> {
    /// Mount the log on `[base, base + len)` without writing anything.
    ///
    /// `Ok(None)` means no page in the region carries a header this log
    /// wrote — an unformatted region, or somebody else's data. Use this
    /// where formatting would be a decision rather than a detail: the
    /// firmware's boot probe reads a region it has never driven before and
    /// must not erase whatever is on it.
    pub async fn mount(mut flash: F, base: u32, len: u32) -> Result<Option<Self>, Error<F::Error>> {
        let sectors = check_region::<F>(base, len, flash.capacity())?;
        let Some((active, seq)) = find_active(&mut flash, base, sectors).await? else {
            return Ok(None);
        };
        let cursor = scan_tail(&mut flash, base + active * SECTOR_SIZE).await?;
        Ok(Some(Self {
            flash,
            base,
            sectors,
            active,
            seq,
            cursor,
        }))
    }

    /// Mount the log on `[base, base + len)`, formatting it if no page there
    /// carries a valid header.
    ///
    /// Reads `pages` × 12 bytes of page headers plus one scan of the active
    /// page. Writes nothing unless the region is unformatted, in which case
    /// it costs one erase and one 12-byte header.
    pub async fn open(mut flash: F, base: u32, len: u32) -> Result<Self, Error<F::Error>> {
        let sectors = check_region::<F>(base, len, flash.capacity())?;
        let (active, seq) = match find_active(&mut flash, base, sectors).await? {
            Some(found) => found,
            None => {
                erase_sector(&mut flash, base).await?;
                write_sector_header(&mut flash, base, 0).await?;
                (0, 0)
            }
        };
        let cursor = scan_tail(&mut flash, base + active * SECTOR_SIZE).await?;
        Ok(Self {
            flash,
            base,
            sectors,
            active,
            seq,
            cursor,
        })
    }

    /// How many records are on the part: `(live, purged)`.
    pub async fn count(&mut self) -> Result<(u32, u32), Error<F::Error>> {
        let mut live = 0u32;
        let mut purged = 0u32;
        self.for_each(|record| {
            if record.is_live() {
                live += 1;
            } else {
                purged += 1;
            }
        })
        .await?;
        Ok((live, purged))
    }

    /// Append a record and commit it. Returns its absolute device offset.
    ///
    /// Reclaims the next page round-robin if the active one cannot hold the
    /// record; the records in that page are gone when it returns.
    pub async fn append(
        &mut self,
        key: &[u8; KEY_LEN],
        time: u32,
        tag: u8,
        body: &[u8],
    ) -> Result<u32, Error<F::Error>> {
        if body.len() > MAX_BODY {
            return Err(Error::BodyTooLarge);
        }
        let stride = record_stride(body.len()) as u32;
        if stride > SECTOR_SIZE - self.cursor {
            self.advance().await?;
        }
        let offset = self.base + self.active * SECTOR_SIZE + self.cursor;

        let mut header = [FLAG_ERASED; HEADER_LEN];
        header[0..2].copy_from_slice(&(body.len() as u16).to_le_bytes());
        header[2..34].copy_from_slice(key);
        header[34..38].copy_from_slice(&time.to_le_bytes());
        header[38] = tag;
        header[39] = FLAG_ERASED;
        let crc = crc16_update(crc16_update(CRC_INIT, &header[0..39]), body);
        header[40..42].copy_from_slice(&crc.to_le_bytes());

        let mut commit = Aligned([0u8; PROGRAM_UNIT as usize]);
        commit.0.copy_from_slice(
            &header[COMMIT_OFF as usize..COMMIT_OFF as usize + PROGRAM_UNIT as usize],
        );
        commit.0[COMMIT_FLAGS_IX] = FLAG_LIVE;

        // Seal the page BEFORE the first program run, and put the cursor
        // back only on a path that runs to its end.
        //
        // A future dropped at one of the awaits below runs no code of its
        // own: there is no `Err` to inspect and no destructor on this
        // function's body. Whatever the cursor holds at the moment of the
        // drop is what the next call on this handle believes, so it has to
        // already be the sealed value — the same place a failure and a power
        // cut leave it. Set it afterwards and a cancelled append leaves
        // `cursor` pointing at an offset whose words may already hold half a
        // record, and the next append on the **same handle** programs over
        // them, which needs bits raised; the remount a reboot performs is
        // what used to hide that
        // (`a_dropped_append_seals_its_page_on_the_same_handle`, and its
        // negative control next to it).
        //
        // The cost is the rest of one page per cancellation, paid even by a
        // drop at the very first await, which has put nothing down. That is
        // the price of a state that is correct without running code, and the
        // discipline in §Cancellation — give the store its own task, reach it
        // by channel — is what keeps the case off the field in the first
        // place. On `nrf_softdevice::Flash` it is not even reachable inside
        // an operation: its write and erase futures arm a `DropBomb` and
        // panic if dropped, so the only droppable awaits there are the ones
        // between operations.
        let resume = self.cursor;
        self.cursor = SECTOR_SIZE;

        // Everything except the commit word, which stays erased so that its
        // one write is the commit and its second is a later purge. `touched`
        // is what tells a failure that landed nothing from one that left
        // bytes behind; see below.
        let mut touched = false;
        let mut outcome = self
            .program_run(offset, &header[0..COMMIT_OFF as usize], &[], &mut touched)
            .await;
        if outcome.is_ok() {
            outcome = self
                .program_run(
                    offset + AFTER_COMMIT,
                    &header[AFTER_COMMIT as usize..HEADER_LEN],
                    body,
                    &mut touched,
                )
                .await;
        }
        if outcome.is_ok() {
            outcome = self
                .flash
                .write(offset + COMMIT_OFF, &commit.0)
                .await
                .map_err(Error::Flash);
        }

        if let Err(error) = outcome {
            // A failed operation on this part leaves no bytes behind — the
            // SoftDevice's timeout means the write did not happen — but the
            // operations *before* it did, and their words have spent one of
            // their two writes. Retrying at the same offset would spend the
            // second on bytes that already hold the right value, and a third
            // if it ever failed again; worse, a caller that retries with a
            // different record would be programming over a half-written one,
            // which needs bits raised.
            //
            // So a partly-written record keeps the page sealed, exactly as a
            // power cut does, and the retry lands in a fresh one — the page
            // is already sealed above, and this is the one path that may
            // un-seal it. The cost is the rest of one page per failed append
            // that got as far as its first program; a failure on that first
            // program costs nothing, which is the common case when the radio
            // is busy enough to make the SoftDevice refuse.
            if !touched {
                self.cursor = resume;
            }
            return Err(error);
        }

        self.cursor = resume + stride;
        Ok(offset)
    }

    /// Visit every record still on the part, oldest page first.
    ///
    /// Purged records are visited too — the caller decides. Reading a body
    /// needs `&mut self`, so the closure gets the header and the caller
    /// reads bodies afterwards from the [`Record`] it kept.
    pub async fn for_each(
        &mut self,
        mut visit: impl FnMut(&Record),
    ) -> Result<(), Error<F::Error>> {
        for step in 1..=self.sectors {
            let idx = (self.active + step) % self.sectors;
            let sector = self.base + idx * SECTOR_SIZE;
            if read_sector_header(&mut self.flash, sector).await?.is_none() {
                continue;
            }
            let mut off = SECTOR_HEADER_LEN;
            while SECTOR_SIZE - off >= MIN_STRIDE {
                match probe_record(&mut self.flash, sector + off, SECTOR_SIZE - off).await? {
                    Some((record, intact)) => {
                        off += record.stride();
                        if intact {
                            visit(&record);
                        }
                    }
                    None => break,
                }
            }
        }
        Ok(())
    }

    /// Read a record's body into `out`. Returns the number of bytes read,
    /// which is `min(out.len(), record.len)`.
    pub async fn read_body(
        &mut self,
        record: &Record,
        out: &mut [u8],
    ) -> Result<usize, Error<F::Error>> {
        let want = core::cmp::min(out.len(), record.len as usize);
        let mut written = 0usize;
        read_span(&mut self.flash, record.body_offset(), want, |chunk| {
            out[written..written + chunk.len()].copy_from_slice(chunk);
            written += chunk.len();
        })
        .await?;
        Ok(want)
    }

    /// Erase the next page round-robin and make it active.
    async fn advance(&mut self) -> Result<(), Error<F::Error>> {
        let next = (self.active + 1) % self.sectors;
        let seq = self.seq.wrapping_add(1);
        let sector = self.base + next * SECTOR_SIZE;
        erase_sector(&mut self.flash, sector).await?;
        write_sector_header(&mut self.flash, sector, seq).await?;
        self.active = next;
        self.seq = seq;
        self.cursor = SECTOR_HEADER_LEN;
        Ok(())
    }

    /// Program `head` followed by `tail` as one forward run of word-sized
    /// writes out of an aligned window, padding the last word with `0xFF`.
    ///
    /// `offset` must be word-aligned. Padding is free: programming `0xFF`
    /// clears no bit. `touched` is set once any write has succeeded, so the
    /// caller can tell a failure that left bytes on the part from one that
    /// did not.
    async fn program_run(
        &mut self,
        offset: u32,
        head: &[u8],
        tail: &[u8],
        touched: &mut bool,
    ) -> Result<(), Error<F::Error>> {
        let mut window = Aligned([FLAG_ERASED; WINDOW]);
        let mut filled = 0usize;
        let mut at = offset;
        for part in [head, tail] {
            let mut rest = part;
            while !rest.is_empty() {
                let take = core::cmp::min(WINDOW - filled, rest.len());
                window.0[filled..filled + take].copy_from_slice(&rest[..take]);
                filled += take;
                rest = &rest[take..];
                if filled == WINDOW {
                    self.flash
                        .write(at, &window.0)
                        .await
                        .map_err(Error::Flash)?;
                    *touched = true;
                    at += WINDOW as u32;
                    filled = 0;
                }
            }
        }
        if filled > 0 {
            let total = align_up(filled);
            window.0[filled..total].fill(FLAG_ERASED);
            self.flash
                .write(at, &window.0[..total])
                .await
                .map_err(Error::Flash)?;
            *touched = true;
        }
        Ok(())
    }

    /// Index of the page records are currently appended to.
    pub fn active_sector(&self) -> u32 {
        self.active
    }

    /// Sequence number of the active page: how many pages this log has
    /// erased since it was formatted.
    pub fn sequence(&self) -> u32 {
        self.seq
    }

    /// Pages in the region.
    pub fn sectors(&self) -> u32 {
        self.sectors
    }

    /// Bytes still free in the active page.
    pub fn sector_room(&self) -> u32 {
        SECTOR_SIZE - self.cursor
    }

    /// Bytes that can be appended before reclaim has to erase a page that
    /// still holds records: the room left in the active page plus the pages
    /// this log has never reached.
    ///
    /// "Free" is a slightly awkward word for a forward-only log, and this is
    /// the honest reading of it: after the first lap there are no unused
    /// pages left, every further append costs the oldest one, and the number
    /// is therefore just the active page's room — which is what a board
    /// reporting `free_bytes=` should say rather than a figure that hides the
    /// wrap. Derived from [`Self::sequence`] because that is what records how
    /// far round the region the log has come: page *n* is first headed at
    /// sequence *n*, so `sectors - 1 - seq` pages are still untouched until
    /// the count runs out.
    pub fn free_bytes(&self) -> u32 {
        let virgin = self.sectors.saturating_sub(1 + self.seq);
        self.sector_room() + virgin * SECTOR_PAYLOAD
    }

    /// The device, for an owner that needs it for something else — reading
    /// a JEDEC id, driving a second region. Writing inside this log's
    /// region behind its back corrupts it.
    pub fn flash_mut(&mut self) -> &mut F {
        &mut self.flash
    }

    /// Give the device back.
    pub fn into_flash(self) -> F {
        self.flash
    }
}

impl<F: MultiwriteNorFlash> RecordLog<F> {
    /// Mark a record withdrawn. Clears one further bit of its flags byte;
    /// the bytes stay put until the page is reclaimed.
    ///
    /// This is the second and last write the commit word gets between
    /// erases, which is the whole of the nRF52840's budget for it — hence
    /// the [`MultiwriteNorFlash`] bound, and hence the commit word being
    /// skipped rather than pre-programmed during the append.
    pub async fn purge(&mut self, record: &Record) -> Result<(), Error<F::Error>> {
        if record.flags == FLAG_PURGED {
            return Ok(());
        }
        let mut commit = Aligned([0u8; PROGRAM_UNIT as usize]);
        commit.0[0..2].copy_from_slice(&record.time.to_le_bytes()[2..4]);
        commit.0[2] = record.tag;
        commit.0[COMMIT_FLAGS_IX] = FLAG_PURGED;
        self.flash
            .write(record.offset + COMMIT_OFF, &commit.0)
            .await
            .map_err(Error::Flash)
    }
}

/// Whether `[base, base + len)` already carries this log's format.
///
/// Reads only, and borrows the device rather than taking it — which is
/// what lets a caller ask the question before deciding whether formatting
/// is its to do, and what lets a test assert that asking cost nothing.
pub async fn is_formatted<F: NorFlash>(
    flash: &mut F,
    base: u32,
    len: u32,
) -> Result<bool, Error<F::Error>> {
    let sectors = check_region::<F>(base, len, flash.capacity())?;
    Ok(find_active(flash, base, sectors).await?.is_some())
}

/// Check the region against the device's geometry and its own bounds,
/// returning the number of pages in it.
fn check_region<F: NorFlash>(base: u32, len: u32, capacity: usize) -> Result<u32, Error<F::Error>> {
    let unit = PROGRAM_UNIT as usize;
    if F::ERASE_SIZE != SECTOR_SIZE as usize
        || F::WRITE_SIZE > unit
        || !unit.is_multiple_of(F::WRITE_SIZE)
        || F::READ_SIZE > unit
        || !unit.is_multiple_of(F::READ_SIZE)
    {
        return Err(Error::UnsupportedGeometry);
    }
    if !base.is_multiple_of(SECTOR_SIZE)
        || !len.is_multiple_of(SECTOR_SIZE)
        || len < 2 * SECTOR_SIZE
    {
        return Err(Error::BadRegion);
    }
    if u64::from(base) + u64::from(len) > capacity as u64 {
        return Err(Error::OutOfBounds);
    }
    Ok(len / SECTOR_SIZE)
}

/// The active page and its sequence: the highest sequence on the part.
///
/// This read is the whole of the log's mount state. Nothing on the part
/// records where the head is, which is exactly why no page wears faster
/// than the rest.
async fn find_active<F: ReadNorFlash>(
    flash: &mut F,
    base: u32,
    sectors: u32,
) -> Result<Option<(u32, u32)>, Error<F::Error>> {
    let mut best: Option<(u32, u32)> = None;
    for idx in 0..sectors {
        if let Some(seq) = read_sector_header(flash, base + idx * SECTOR_SIZE).await? {
            if best.is_none_or(|(_, s)| seq > s) {
                best = Some((idx, seq));
            }
        }
    }
    Ok(best)
}

/// Read a page header. `Some(sequence)` iff it is intact.
async fn read_sector_header<F: ReadNorFlash>(
    flash: &mut F,
    sector: u32,
) -> Result<Option<u32>, Error<F::Error>> {
    let mut buf = Aligned([0u8; SECTOR_HEADER_LEN as usize]);
    flash.read(sector, &mut buf.0).await.map_err(Error::Flash)?;
    if u32::from_le_bytes([buf.0[0], buf.0[1], buf.0[2], buf.0[3]]) != MAGIC || buf.0[8] != VERSION
    {
        return Ok(None);
    }
    let stored = u16::from_le_bytes([buf.0[10], buf.0[11]]);
    if crc16_update(CRC_INIT, &buf.0[0..10]) != stored {
        return Ok(None);
    }
    Ok(Some(u32::from_le_bytes([
        buf.0[4], buf.0[5], buf.0[6], buf.0[7],
    ])))
}

async fn write_sector_header<F: NorFlash>(
    flash: &mut F,
    sector: u32,
    seq: u32,
) -> Result<(), Error<F::Error>> {
    let mut buf = Aligned([0u8; SECTOR_HEADER_LEN as usize]);
    buf.0[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf.0[4..8].copy_from_slice(&seq.to_le_bytes());
    buf.0[8] = VERSION;
    buf.0[9] = 0;
    let crc = crc16_update(CRC_INIT, &buf.0[0..10]);
    buf.0[10..12].copy_from_slice(&crc.to_le_bytes());
    flash.write(sector, &buf.0).await.map_err(Error::Flash)
}

async fn erase_sector<F: NorFlash>(flash: &mut F, sector: u32) -> Result<(), Error<F::Error>> {
    flash
        .erase(sector, sector + SECTOR_SIZE)
        .await
        .map_err(Error::Flash)
}

/// Read `len` bytes from `off` — which need not be aligned — handing them to
/// `sink` in windows.
async fn read_span<F: ReadNorFlash>(
    flash: &mut F,
    mut off: u32,
    mut len: usize,
    mut sink: impl FnMut(&[u8]),
) -> Result<(), Error<F::Error>> {
    let mut buf = Aligned([0u8; WINDOW]);
    while len > 0 {
        let start = off & !(PROGRAM_UNIT - 1);
        let skip = (off - start) as usize;
        let span = align_up(core::cmp::min(len + skip, WINDOW));
        flash
            .read(start, &mut buf.0[..span])
            .await
            .map_err(Error::Flash)?;
        let take = core::cmp::min(len, span - skip);
        sink(&buf.0[skip..skip + take]);
        off += take as u32;
        len -= take;
    }
    Ok(())
}

/// Read the record at `at`.
///
/// `None` means there is no record here and the scan of this page ends: the
/// flags byte is still erased (nothing was ever committed here) or it holds
/// a value no commit produces, or the length is one no record could have
/// had. `Some((record, intact))` means a record was committed here and
/// occupies `record.stride()` bytes whether or not its CRC still checks —
/// `intact` says whether it does. A record whose body lost a bit therefore
/// costs itself and not the records behind it.
///
/// `room` is what is left of the page from `at`; a header claiming a body
/// that would straddle the page boundary is not a record.
async fn probe_record<F: ReadNorFlash>(
    flash: &mut F,
    at: u32,
    room: u32,
) -> Result<Option<(Record, bool)>, Error<F::Error>> {
    debug_assert!(room >= MIN_STRIDE);
    let mut buf = Aligned([0u8; align_up(HEADER_LEN)]);
    flash.read(at, &mut buf.0).await.map_err(Error::Flash)?;

    let flags = buf.0[39];
    if flags != FLAG_LIVE && flags != FLAG_PURGED {
        return Ok(None);
    }
    let len = u16::from_le_bytes([buf.0[0], buf.0[1]]);
    if len as usize > MAX_BODY || record_stride(len as usize) as u32 > room {
        return Ok(None);
    }

    let stored = u16::from_le_bytes([buf.0[40], buf.0[41]]);
    let mut crc = crc16_update(CRC_INIT, &buf.0[0..39]);
    read_span(flash, at + HEADER_LEN as u32, len as usize, |chunk| {
        crc = crc16_update(crc, chunk);
    })
    .await?;

    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&buf.0[2..34]);
    let record = Record {
        key,
        time: u32::from_le_bytes([buf.0[34], buf.0[35], buf.0[36], buf.0[37]]),
        tag: buf.0[38],
        flags,
        len,
        offset: at,
    };
    Ok(Some((record, crc == stored)))
}

/// Where the next record goes in `sector`, given what survived.
///
/// Walks the committed records, then checks that the rest of the page is
/// still erased. If it is not — a cut left a half-written record there — the
/// page is sealed by returning [`SECTOR_SIZE`], because programming over
/// those bytes would have to raise bits.
async fn scan_tail<F: ReadNorFlash>(flash: &mut F, sector: u32) -> Result<u32, Error<F::Error>> {
    let mut off = SECTOR_HEADER_LEN;
    while SECTOR_SIZE - off >= MIN_STRIDE {
        match probe_record(flash, sector + off, SECTOR_SIZE - off).await? {
            // A record with a bad CRC still owns its bytes, so the cursor
            // steps over it exactly like an intact one.
            Some((record, _)) => off += record.stride(),
            None => break,
        }
    }
    if off < SECTOR_SIZE && !span_is_erased(flash, sector + off, SECTOR_SIZE - off).await? {
        return Ok(SECTOR_SIZE);
    }
    Ok(off)
}

async fn span_is_erased<F: ReadNorFlash>(
    flash: &mut F,
    off: u32,
    len: u32,
) -> Result<bool, Error<F::Error>> {
    let mut clean = true;
    read_span(flash, off, len as usize, |chunk| {
        if chunk.iter().any(|b| *b != FLAG_ERASED) {
            clean = false;
        }
    })
    .await?;
    Ok(clean)
}
