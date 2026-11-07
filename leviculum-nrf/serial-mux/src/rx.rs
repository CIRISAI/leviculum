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
}
