//! The bench image's per-op flash records (1613, 1622): a ring of the
//! last flash ops, written by the op's core after each op and read by the
//! same core's loop, which says each as a `[FLASH] op=<kind> ms=<n> at=<ms>`
//! line. The ring lives here so the host can pin it: the board keeps one
//! in a static (`leviculum_esp::flash::park`).
//!
//! Window 24 (1622) found the ring of 32 overwriting silently: drained
//! once per loop turn, it lost every record of a turn past its 32nd, and
//! a forget's turn parks hundreds (its 128 erases came out as 7 `[FLASH]`
//! lines against 551 `park_ops`). Two things changed. The ring is sized
//! for a forget's turn ([`KEEP`]), and what a drain still finds
//! overwritten is counted ([`OpLog::take`] returns it, [`OpLog::lost`]
//! sums it), so a window's distribution says what it is missing.
//!
//! One writer and one reader on one core; atomics only so the ring can
//! sit in a `static`. The indices are free-running `u32`s and the slot is
//! the index modulo `N`, which stays consistent across their wrap only
//! for a power of two: [`OpLog::new`] refuses any other `N` at compile
//! time.

use core::sync::atomic::{AtomicU32, Ordering};

/// The records the bench image keeps: a forget and the reopen's rebuild
/// run in one loop turn (window 24: 551 ops in the forget's `[IDLE]`
/// window), so the ring holds a turn of that size whole, with room. 8 KiB
/// of the bench image's internal RAM (two words a record); the release
/// image keeps none.
pub const KEEP: usize = 1024;

/// What an op is, for the per-kind distribution (1613): an erase's
/// masked window is the sector's 35 to 57 ms, a write's is per 512-byte
/// chunk and well under a millisecond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Erase,
    Write,
}

impl Op {
    /// The word the `[FLASH]` line says.
    pub fn name(self) -> &'static str {
        match self {
            Op::Erase => "erase",
            Op::Write => "write",
        }
    }
}

/// The top bit of a record's microseconds word: set for a write. No op
/// parks for 2^31 µs (35 minutes); a longer one is clamped below it.
const WRITE_BIT: u32 = 1 << 31;

/// The ring: `N` records of the op's kind and length and when it ended.
pub struct OpLog<const N: usize> {
    at_ms: [AtomicU32; N],
    /// The op's microseconds, its kind in [`WRITE_BIT`].
    us: [AtomicU32; N],
    write: AtomicU32,
    read: AtomicU32,
    lost: AtomicU32,
}

impl<const N: usize> Default for OpLog<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> OpLog<N> {
    const POWER_OF_TWO: () = assert!(N.is_power_of_two() && N <= 1 << 30);

    /// An empty ring.
    pub const fn new() -> Self {
        let () = Self::POWER_OF_TWO;
        Self {
            at_ms: [const { AtomicU32::new(0) }; N],
            us: [const { AtomicU32::new(0) }; N],
            write: AtomicU32::new(0),
            read: AtomicU32::new(0),
            lost: AtomicU32::new(0),
        }
    }

    /// Keep one op: its kind, how long it parked, and when it ended. The
    /// oldest record goes when the ring is full; the next [`OpLog::take`]
    /// counts it.
    pub fn keep(&self, op: Op, us: u32, at_ms: u32) {
        let w = self.write.load(Ordering::Relaxed);
        let slot = (w as usize) % N;
        let kind = match op {
            Op::Erase => 0,
            Op::Write => WRITE_BIT,
        };
        self.at_ms[slot].store(at_ms, Ordering::Relaxed);
        self.us[slot].store(us.min(WRITE_BIT - 1) | kind, Ordering::Relaxed);
        self.write.store(w.wrapping_add(1), Ordering::Relaxed);
    }

    /// Say every op kept since the last call, `(op, us, at_ms)`, oldest
    /// first, and return how many were overwritten before this call
    /// could say them.
    pub fn take(&self, say: impl FnMut(Op, u32, u32)) -> u32 {
        self.take_at_most(u32::MAX, say)
    }

    /// Records kept and not yet said, at most `N`.
    pub fn waiting(&self) -> u32 {
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Relaxed);
        w.wrapping_sub(r).min(N as u32)
    }

    /// Say at most `max` of the ops kept since the last call, oldest
    /// first, as [`OpLog::take`] says them, and leave the rest for the
    /// next call (1630). Window 25's forget handed 550 records to one
    /// drain, which put them on the line faster than UART0 carries them,
    /// and the send ring dropped 388; the board now asks for no more than
    /// the ring has room for ([`sayable`]). What was overwritten before
    /// this call is counted and returned as [`OpLog::take`] counts it.
    pub fn take_at_most(&self, max: u32, mut say: impl FnMut(Op, u32, u32)) -> u32 {
        let w = self.write.load(Ordering::Relaxed);
        let mut r = self.read.load(Ordering::Relaxed);
        let behind = w.wrapping_sub(r);
        let lost = behind.saturating_sub(N as u32);
        if lost > 0 {
            // The oldest still-held records are the truth.
            r = w.wrapping_sub(N as u32);
            self.lost.fetch_add(lost, Ordering::Relaxed);
        }
        let end = r.wrapping_add(w.wrapping_sub(r).min(max));
        while r != end {
            let slot = (r as usize) % N;
            let word = self.us[slot].load(Ordering::Relaxed);
            let op = match word & WRITE_BIT {
                0 => Op::Erase,
                _ => Op::Write,
            };
            say(
                op,
                word & !WRITE_BIT,
                self.at_ms[slot].load(Ordering::Relaxed),
            );
            r = r.wrapping_add(1);
        }
        self.read.store(r, Ordering::Relaxed);
        lost
    }

    /// Records overwritten unsaid since boot, wrapping: a reader takes
    /// deltas, as the `[IDLE]` line's `flash_lost=`.
    pub fn lost(&self) -> u32 {
        self.lost.load(Ordering::Relaxed)
    }
}

/// The longest `[FLASH] op=` record the board writes, its stamp and line
/// end included, and the line end a frame before it adds (1630): prefix
/// 8, `op=erase` 8, ` ms=` and `2147483.647` 15, ` at=` and a `u32` 14,
/// ` t=` and a `u64` 23, `\r\n` twice 4. Nothing in it is escaped. The
/// bench's lines run 45 to 50 bytes (`[FLASH] op=write ms=0.312
/// at=123456 t=123456`); the bound is the worst case, so the drain fits
/// by construction, not by the typical line.
pub const RECORD_MAX: usize = 72;

/// What the drain leaves free of the send ring (1630): half the board's
/// `TX_RING` (8 KiB) and half its `TX_RECORDS` (512), for the lines the
/// loop writes lossy before the UART carried the records out (`[IDLE]`,
/// `[UART]`, `[LOOP]`, a frame: under 1 KiB a turn). The firmware pins
/// both halves against its ring at compile time.
pub const RESERVE_BYTES: usize = 4 * 1024;
/// The records of the send ring the drain leaves free, as
/// [`RESERVE_BYTES`] leaves its bytes.
pub const RESERVE_RECORDS: usize = 256;

/// How many records a drain may write into a send ring with `free_bytes`
/// and `free_records` of room so that none of them, and none of the
/// loop's lines after them, makes the ring drop (1630): the room past
/// the reserve, in worst-case records.
pub fn sayable(free_bytes: usize, free_records: usize) -> u32 {
    let by_bytes = free_bytes.saturating_sub(RESERVE_BYTES) / RECORD_MAX;
    let by_records = free_records.saturating_sub(RESERVE_RECORDS);
    u32::try_from(by_bytes.min(by_records)).unwrap_or(u32::MAX)
}

/// A record's `[FLASH]` body: `op=<kind> ms=<ms>.<frac> at=<ms>`.
pub struct Body {
    pub op: Op,
    pub us: u32,
    pub at_ms: u32,
}

impl core::fmt::Display for Body {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "op={} ms={}.{:03} at={}",
            self.op.name(),
            self.us / 1000,
            self.us % 1000,
            self.at_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Erases at the even ops, writes at the odd.
    fn kind(i: u32) -> Op {
        match i % 2 {
            0 => Op::Erase,
            _ => Op::Write,
        }
    }

    #[test]
    fn a_power_of_two_ring_says_every_op_across_the_index_wrap() {
        let log: OpLog<8> = OpLog::new();
        log.write.store(u32::MAX - 3, Ordering::Relaxed);
        log.read.store(u32::MAX - 3, Ordering::Relaxed);
        for i in 0..8 {
            log.keep(kind(i), 100 + i, i);
        }
        let mut said = Vec::new();
        let lost = log.take(|op, us, at| said.push((op, us, at)));
        assert_eq!(lost, 0);
        assert_eq!(said.len(), 8);
        for (i, (op, us, at)) in said.into_iter().enumerate() {
            let i = i as u32;
            assert_eq!(op, kind(i));
            assert_eq!((us, at), (100 + i, i));
        }
    }
}
