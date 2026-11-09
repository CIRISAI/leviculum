//! The receive side of a board's single serial line (1440): what the host
//! sent, kept in memory between the UART's small FIFO and the board's loop,
//! and the counts that say where bytes were lost.
//!
//! The ThinkNode M9's UART0 has a 128-byte receive FIFO, 11 ms of the line
//! at 115200 baud. Until 1440 the board emptied it only when its loop wrote
//! or read the line, so any stretch of the loop longer than 11 ms without a
//! log line (checking an announce, drawing) let the FIFO overflow; esp-hal
//! then resets the FIFO and reports `FifoOverflowed`, which the board threw
//! away. The 2026-11-06 bench lost three `/sync` answers that way, each
//! a few milliseconds behind a backbone announce. Since 1440 an interrupt
//! empties the FIFO into an [`RxKeep`] whenever it holds more than a
//! threshold, whatever the loop is doing, and [`RxCounts`] says every
//! loss on the board's `[UART]` line.
//!
//! [`RxKeep`] is a plain ring with no locking: the firmware keeps it behind
//! a critical section shared by the interrupt and the loop. The model in
//! this module's tests drives the same ring with the FIFO's timing and
//! esp-hal's overflow rule, so the mechanism is pinned on the host.

extern crate alloc;

use core::fmt;

/// The bytes the host sent that the board's loop has not read yet, at
/// most `N`; what does not fit is counted, never kept.
pub struct RxKeep<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
    lost: u32,
}

impl<const N: usize> Default for RxKeep<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RxKeep<N> {
    /// An empty keep; `const` so it can be a static.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            len: 0,
            lost: 0,
        }
    }

    /// Keep `bytes`, oldest first; the ones past `N` are counted as lost.
    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.len == N {
                self.lost = self.lost.saturating_add(1);
                continue;
            }
            self.buf[(self.head + self.len) % N] = byte;
            self.len += 1;
        }
    }

    /// Move the oldest kept bytes into `out`: how many.
    pub fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for slot in &mut out[..n] {
            *slot = self.buf[self.head];
            self.head = (self.head + 1) % N;
        }
        self.len -= n;
        n
    }

    /// Bytes kept and not read.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes lost because the keep was full.
    pub fn lost(&self) -> u32 {
        self.lost
    }

    /// Move every kept byte, oldest first, to `take`, which takes what it
    /// has room for of each chunk and says how many: the bytes it refused,
    /// which are gone (1597). The M9's pump (`pump_rx`) spills the keep
    /// into a larger buffer on the heap while its loop cannot read, so a
    /// keep left unread through the boot's census wait (3.4 s, window
    /// 21, `rx_lost=149` before the first key) never fills.
    pub fn spill(&mut self, mut take: impl FnMut(&[u8]) -> usize) -> u32 {
        let mut refused = 0u32;
        let mut chunk = [0u8; 128];
        loop {
            let n = self.pop_into(&mut chunk);
            if n == 0 {
                return refused;
            }
            let took = take(&chunk[..n]).min(n);
            refused = refused.saturating_add((n - took) as u32);
        }
    }
}

/// The most an [`RxHeld`] keeps (1603): 35 s of the line at 115,200
/// baud, past the 25 s of a boot that rebuilds the M9's ring of 4464
/// entries (window 22) with room to spare. Until 1603 the firmware kept
/// 256 KiB, 22 s, and only through the forget's reopen; the boot's
/// rebuild pumped nothing and lost 7916, 3656 and 1440 bytes.
pub const HELD_MAX: usize = 384 * 1024;

/// One block of an [`RxHeld`]: the largest piece it asks the heap for.
pub const HELD_BLOCK: usize = 2048;

/// What the host sent while the board's loop is held in one long op, on
/// the heap in [`HELD_BLOCK`] pieces, at most [`HELD_MAX`] bytes (1594,
/// 1603): the M9's `pump_rx` spills its keep here between the steps of
/// a restore or a forget, and the loop reads it first once it reads
/// again. In pieces, never one buffer: a `VecDeque<u8>` doubles, and the
/// 256 KiB it would reach is one block of the size whose failure
/// panicked window 22's reopen. No locking, as [`RxKeep`].
pub struct RxHeld {
    blocks: alloc::collections::VecDeque<alloc::vec::Vec<u8>>,
    /// Bytes of the front block already read.
    head: usize,
    len: usize,
}

impl Default for RxHeld {
    fn default() -> Self {
        Self::new()
    }
}

impl RxHeld {
    /// An empty buffer; `const` so it can be a static, and nothing on the
    /// heap until a byte comes.
    pub const fn new() -> Self {
        Self {
            blocks: alloc::collections::VecDeque::new(),
            head: 0,
            len: 0,
        }
    }

    /// Keep what fits of `bytes` under [`HELD_MAX`], oldest first: how
    /// many were taken; the rest are the caller's to count as lost.
    pub fn push(&mut self, bytes: &[u8]) -> usize {
        let take = bytes.len().min(HELD_MAX.saturating_sub(self.len));
        let mut rest = &bytes[..take];
        while !rest.is_empty() {
            let room = match self.blocks.back() {
                Some(block) => HELD_BLOCK - block.len(),
                None => 0,
            };
            if room == 0 {
                self.blocks
                    .push_back(alloc::vec::Vec::with_capacity(HELD_BLOCK));
                continue;
            }
            let n = room.min(rest.len());
            if let Some(block) = self.blocks.back_mut() {
                block.extend_from_slice(&rest[..n]);
            }
            rest = &rest[n..];
        }
        self.len += take;
        take
    }

    /// Move the oldest held bytes into `out`: how many. A buffer read
    /// empty gives its blocks back.
    pub fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        while n < out.len() {
            let Some(block) = self.blocks.front() else {
                break;
            };
            let k = (block.len() - self.head).min(out.len() - n);
            out[n..n + k].copy_from_slice(&block[self.head..self.head + k]);
            n += k;
            self.head += k;
            if self.head == block.len() && (block.len() == HELD_BLOCK || self.blocks.len() > 1) {
                self.blocks.pop_front();
                self.head = 0;
            } else if self.head == block.len() {
                break;
            }
        }
        self.len -= n;
        if self.len == 0 {
            self.blocks = alloc::collections::VecDeque::new();
            self.head = 0;
        }
        n
    }

    /// Bytes held and not read.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Where the host's bytes were lost on the board, as its `[UART]` line
/// says them: `rx_lost` the keep was full, `overrun` the hardware FIFO
/// overflowed before anything emptied it (each one up to the FIFO's
/// 128 bytes and every byte that came while it was full), `deframe_drop`
/// frames the HDLC reader dropped as empty or oversized, `rx_err` bytes
/// the UART flagged (framing, parity, glitch). `loop_gap_max_ms` is the
/// longest the board's loop went without reading the line since the last
/// line: under the interrupt it costs nothing, without it every gap past
/// 11 ms is an overrun when the host sends meanwhile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RxCounts {
    pub rx_lost: u32,
    pub overrun: u32,
    pub deframe_drop: u32,
    pub rx_err: u32,
    pub loop_gap_max_ms: u32,
}

impl RxCounts {
    /// Whether a loss count differs from `other`'s: the board says the line
    /// again on every change, not on a longer gap alone.
    pub fn losses_differ(&self, other: &Self) -> bool {
        (self.rx_lost, self.overrun, self.deframe_drop, self.rx_err)
            != (
                other.rx_lost,
                other.overrun,
                other.deframe_drop,
                other.rx_err,
            )
    }
}

impl fmt::Display for RxCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rx_lost={} overrun={} deframe_drop={} rx_err={} loop_gap_max_ms={}",
            self.rx_lost, self.overrun, self.deframe_drop, self.rx_err, self.loop_gap_max_ms
        )
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    extern crate std;

    use alloc::collections::VecDeque;
    use alloc::vec::Vec;
    use std::format;

    use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};

    use super::*;

    #[test]
    fn the_keep_is_a_ring_and_counts_what_does_not_fit() {
        let mut keep = RxKeep::<4>::new();
        keep.push(&[1, 2, 3]);
        let mut out = [0u8; 2];
        assert_eq!(keep.pop_into(&mut out), 2);
        assert_eq!(out, [1, 2]);
        keep.push(&[4, 5, 6, 7]);
        assert_eq!((keep.len(), keep.lost()), (4, 1));
        let mut out = [0u8; 8];
        assert_eq!(keep.pop_into(&mut out), 4);
        assert_eq!(&out[..4], &[3, 4, 5, 6]);
        assert!(keep.is_empty());
    }

    /// 1603: the held buffer keeps bytes in order across its blocks,
    /// takes no more than [`HELD_MAX`], and gives its blocks back once
    /// read empty.
    #[test]
    fn the_held_buffer_keeps_order_in_blocks_and_counts_its_cap() {
        let mut held = RxHeld::new();
        let sent: Vec<u8> = (0..(3 * HELD_BLOCK + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        for chunk in sent.chunks(100) {
            assert_eq!(held.push(chunk), chunk.len());
        }
        assert_eq!(held.len(), sent.len());
        let mut read = Vec::new();
        let mut buf = [0u8; 333];
        loop {
            let n = held.pop_into(&mut buf);
            if n == 0 {
                break;
            }
            read.extend_from_slice(&buf[..n]);
        }
        assert_eq!(read, sent);
        assert!(held.is_empty() && held.blocks.capacity() == 0);
        // Read and written in turns, the order holds too.
        let mut read = Vec::new();
        for chunk in sent.chunks(700) {
            held.push(chunk);
            let n = held.pop_into(&mut buf[..300]);
            read.extend_from_slice(&buf[..n]);
        }
        loop {
            let n = held.pop_into(&mut buf);
            if n == 0 {
                break;
            }
            read.extend_from_slice(&buf[..n]);
        }
        assert_eq!(read, sent);
        let big = alloc::vec![7u8; HELD_MAX + 10];
        assert_eq!(held.push(&big), HELD_MAX);
        assert_eq!(held.push(&[1]), 0);
        assert!(held.blocks.iter().all(|b| b.capacity() == HELD_BLOCK));
    }

    /// 1603, window 22: a boot that rebuilds the ring holds core 0's loop
    /// for 25 s (`restore_ms=25490`), and the host sent through it: the
    /// first `[UART]` line said `rx_lost=7916`, the boot's restore fed
    /// nothing. Here the line runs at its full rate for the whole 25 s,
    /// the worst the host can do. The red half reads nothing until the
    /// loop starts, as that boot did; the green half pumps the keep into
    /// the held buffer at each of the rebuild's entries (4.5 ms apart on
    /// the board, `per_1000_ms step=4518`), as the boot's restore now
    /// does through its clock: nothing lost, every byte in order, and no
    /// block on the heap larger than [`HELD_BLOCK`].
    #[test]
    fn a_rebuild_at_line_rate_loses_nothing_when_pumped_per_entry() {
        let rebuild_us = 25_500_000u64;
        let sent: Vec<u8> = (0..rebuild_us / BYTE_US).map(|i| (i % 251) as u8).collect();
        for pumped in [false, true] {
            let mut keep = RxKeep::<8192>::new();
            let mut held = RxHeld::new();
            let mut refused = 0u32;
            let mut next = 0usize;
            let mut now = 0u64;
            while now < rebuild_us {
                // The interrupt keeps up with the line.
                while next < sent.len() && (next as u64 + 1) * BYTE_US <= now {
                    keep.push(&sent[next..next + 1]);
                    next += 1;
                }
                if pumped && now.is_multiple_of(4_500) {
                    refused += keep.spill(|bytes| held.push(bytes));
                }
                now += 100;
            }
            let mut read = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                let n = held.pop_into(&mut buf);
                if n == 0 {
                    break;
                }
                read.extend_from_slice(&buf[..n]);
            }
            loop {
                let n = keep.pop_into(&mut buf);
                if n == 0 {
                    break;
                }
                read.extend_from_slice(&buf[..n]);
            }
            let lost = keep.lost() + refused;
            std::eprintln!("pumped={pumped} lost={lost} read={}", read.len());
            match pumped {
                false => assert!(lost > 0, "the control loses what the keep cannot hold"),
                true => {
                    assert_eq!(lost, 0, "a pumped rebuild lost bytes");
                    assert_eq!(read, sent[..next], "every byte, in the order sent");
                }
            }
        }
    }

    #[test]
    fn the_counts_line_names_every_loss() {
        let counts = RxCounts {
            rx_lost: 1,
            overrun: 2,
            deframe_drop: 3,
            rx_err: 4,
            loop_gap_max_ms: 37,
        };
        assert_eq!(
            format!("{counts}"),
            "rx_lost=1 overrun=2 deframe_drop=3 rx_err=4 loop_gap_max_ms=37"
        );
        let longer_gap = RxCounts {
            loop_gap_max_ms: 90,
            ..counts
        };
        assert!(!counts.losses_differ(&longer_gap));
        assert!(counts.losses_differ(&RxCounts {
            overrun: 3,
            ..counts
        }));
    }

    /// The ESP32-S3's UART receive FIFO.
    const FIFO: usize = 128;
    /// One byte at 115200 baud, 8N1: ten bits, in microseconds (86.8).
    const BYTE_US: u64 = 87;
    /// The interrupt's threshold in the firmware (`leviculum-esp`,
    /// `RX_FIFO_THRESHOLD`).
    const THRESHOLD: usize = 64;

    /// How the board empties its FIFO.
    #[derive(Clone, Copy)]
    enum Drain {
        /// Only when the loop touches the line, never while it works on a
        /// frame: the driver before 1440.
        Polled,
        /// An interrupt above the threshold, whatever the loop does, and
        /// the loop's own reads besides: since 1440.
        Interrupt,
    }

    /// The UART as esp-hal sees it: a FIFO that stops taking bytes when
    /// full and flags it, and a read that, on the flag, resets the FIFO
    /// and reports the overflow (`check_for_errors_and_reset_fifo`).
    struct Uart {
        fifo: VecDeque<u8>,
        overflowed: bool,
    }

    impl Uart {
        fn arrive(&mut self, byte: u8) {
            if self.fifo.len() == FIFO {
                self.overflowed = true;
            } else {
                self.fifo.push_back(byte);
            }
        }

        /// One read into `keep`; `true` on an overflow, the FIFO emptied
        /// unread.
        fn read_into<const N: usize>(&mut self, keep: &mut RxKeep<N>) -> bool {
            if self.overflowed {
                self.overflowed = false;
                self.fifo.clear();
                return true;
            }
            let bytes: Vec<u8> = self.fifo.drain(..).collect();
            keep.push(&bytes);
            false
        }
    }

    /// The host's frames on the line, each `(payload, gap before it in
    /// µs)`, read by a board that spends `busy_us` on every frame it takes
    /// without touching the line. The frames the board's deframer gave,
    /// and the counts.
    fn run(frames: &[(Vec<u8>, u64)], busy_us: u64, drain: Drain) -> (Vec<Vec<u8>>, RxCounts) {
        let mut arrivals = Vec::new();
        let mut t = 0;
        for (payload, gap) in frames {
            t += gap;
            let mut wire = Vec::new();
            frame(payload, &mut wire);
            for byte in wire {
                t += BYTE_US;
                arrivals.push((t, byte));
            }
        }
        let end = t + 1_000_000;
        let mut uart = Uart {
            fifo: VecDeque::new(),
            overflowed: false,
        };
        let mut keep = RxKeep::<8192>::new();
        let mut deframer = Deframer::new();
        let mut got = Vec::new();
        let mut counts = RxCounts::default();
        let mut next = arrivals.into_iter().peekable();
        let mut busy_until = 0;
        // Microsecond steps: the line, the interrupt, then the loop.
        for now in 0..end {
            while let Some((_, byte)) = next.next_if(|(at, _)| *at <= now) {
                uart.arrive(byte);
            }
            if matches!(drain, Drain::Interrupt)
                && (uart.fifo.len() > THRESHOLD || uart.overflowed)
                && uart.read_into(&mut keep)
            {
                counts.overrun += 1;
            }
            if now < busy_until {
                continue;
            }
            // The loop's turn: read what came, take the frames.
            if uart.read_into(&mut keep) {
                counts.overrun += 1;
            }
            let mut bytes = [0u8; 64];
            let n = keep.pop_into(&mut bytes);
            for result in deframer.process(&bytes[..n]) {
                match result {
                    DeframeResult::Frame(data) => {
                        got.push(data);
                        busy_until = now + busy_us;
                    }
                    DeframeResult::TooShort | DeframeResult::Oversized => {
                        counts.deframe_drop += 1;
                    }
                    DeframeResult::NeedMore => {}
                }
            }
        }
        counts.rx_lost = keep.lost();
        (got, counts)
    }

    /// The bench's pattern (2026-11-06): a backbone announce, and 5 ms
    /// after it the page the board waits for, 400 bytes, while the board
    /// works 30 ms on the announce.
    fn announce_then_page() -> [(Vec<u8>, u64); 2] {
        let announce: Vec<u8> = (0..167u32).map(|i| (i % 251) as u8).collect();
        let page: Vec<u8> = (0..400u32).map(|i| (i * 7 % 253) as u8).collect();
        [(announce, 0), (page, 5_000)]
    }

    /// The mvr of 1440, red half: the driver that empties the FIFO only
    /// from the loop loses the page behind the announce, and says so in
    /// `overrun`.
    #[test]
    fn a_polled_fifo_loses_the_page_behind_an_announce() {
        let frames = announce_then_page();
        let (got, counts) = run(&frames, 30_000, Drain::Polled);
        std::eprintln!("polled, 30 ms a frame: frames={} {counts}", got.len());
        assert_eq!(got.first(), Some(&frames[0].0), "the announce arrives");
        assert!(
            !got.contains(&frames[1].0),
            "the page arrives whole, so the model shows no loss: {counts}"
        );
        assert!(counts.overrun > 0, "{counts}");
    }

    /// The mvr of 1440, green half: with the interrupt both frames arrive
    /// and nothing is counted lost, however long the board works on a
    /// frame, up to a second.
    #[test]
    fn an_interrupt_drained_fifo_keeps_both_frames() {
        let frames = announce_then_page();
        for busy_us in [0, 5_000, 30_000, 200_000, 1_000_000] {
            let (got, counts) = run(&frames, busy_us, Drain::Interrupt);
            std::eprintln!(
                "interrupt, {busy_us} us a frame: frames={} {counts}",
                got.len()
            );
            assert_eq!(
                got,
                [frames[0].0.clone(), frames[1].0.clone()],
                "busy {busy_us} µs"
            );
            assert_eq!(counts, RxCounts::default(), "busy {busy_us} µs");
        }
    }

    /// The bound the polled driver lived within: a board that works less
    /// than the FIFO's 11 ms per frame loses nothing either way, which is
    /// why the loss needed traffic in front of the page to show.
    #[test]
    fn a_short_turn_loses_nothing_even_polled() {
        let frames = announce_then_page();
        let (got, counts) = run(&frames, 5_000, Drain::Polled);
        assert_eq!(got.len(), 2, "{counts}");
        assert_eq!(counts.overrun, 0);
    }

    /// 1597, window 21's boot: the host sent 15,310 bytes between the
    /// reset and the board's first `[UART]` line (`HOSTTX` in the proxy's
    /// log, three 4095-byte writes at the reset and eight more), and core
    /// 0 read nothing while it waited 3,374 ms for the UI core's census:
    /// the interrupt filled the 8 KiB keep and `rx_lost=149`. The red half
    /// is that boot with no reader; the green half pumps the keep into a
    /// held buffer every 10 ms of the wait, as the firmware's census wait
    /// now does (`pump_rx`, [`RxKeep::spill`]): nothing lost, every byte
    /// read in the order sent.
    #[test]
    fn the_boots_census_wait_loses_nothing_when_pumped() {
        // (ms after the interrupt is on, bytes): the window's HOSTTX lines.
        let writes = [
            (0u64, 4095usize),
            (345, 4095),
            (698, 4095),
            (819, 1390),
            (2709, 212),
            (2748, 217),
            (3409, 186),
            (3559, 246),
            (3638, 219),
            (3823, 217),
            (4196, 338),
        ];
        let mut arrivals: VecDeque<(u64, u8)> = VecDeque::new();
        let mut t = 0u64;
        let mut n = 0u32;
        for (at, len) in writes {
            t = t.max(at * 1000);
            for _ in 0..len {
                t += BYTE_US;
                arrivals.push_back((t, (n % 251) as u8));
                n += 1;
            }
        }
        let sent: Vec<u8> = arrivals.iter().map(|(_, b)| *b).collect();
        let loop_starts_us = 4_600_000;
        for pumped in [false, true] {
            let mut keep = RxKeep::<8192>::new();
            let mut held: VecDeque<u8> = VecDeque::new();
            let mut refused = 0u32;
            let mut queue = arrivals.clone();
            let mut now = 0u64;
            while now < loop_starts_us {
                // The interrupt keeps up with the line: each byte goes to
                // the keep as it comes.
                while let Some((_, byte)) = queue.pop_front_if(|(at, _)| *at <= now) {
                    keep.push(&[byte]);
                }
                if pumped && now.is_multiple_of(10_000) {
                    refused += keep.spill(|bytes| {
                        let room = (256 * 1024usize).saturating_sub(held.len());
                        let took = bytes.len().min(room);
                        held.extend(&bytes[..took]);
                        took
                    });
                }
                now += 100;
            }
            // The loop reads the held bytes first, then the keep.
            let mut read: Vec<u8> = held.drain(..).collect();
            let mut buf = [0u8; 256];
            loop {
                let n = keep.pop_into(&mut buf);
                if n == 0 {
                    break;
                }
                read.extend_from_slice(&buf[..n]);
            }
            let lost = keep.lost() + refused;
            std::eprintln!("pumped={pumped} lost={lost} read={}", read.len());
            match pumped {
                false => assert!(lost > 0, "the control loses what the keep cannot hold"),
                true => {
                    assert_eq!(lost, 0, "a pumped keep lost bytes");
                    assert_eq!(read, sent, "every byte, in the order sent");
                }
            }
        }
    }
}
