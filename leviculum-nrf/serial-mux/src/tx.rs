//! The send side of a board's single serial line (1507, move 6 of the
//! 2026-11-07 structure review's M9 list): what the board writes, kept in
//! memory and fed to the UART's FIFO by its interrupt, so the loop never
//! waits for the line.
//!
//! The ThinkNode M9's UART0 sends 11.5 bytes a millisecond at 115200 baud
//! out of a 128-byte FIFO. Before 1507 every log line and every frame was
//! written by the loop, which spun until the FIFO took the last byte: a
//! 200-byte census line held the loop 17 ms, a full frame 100 ms. Now the
//! loop puts each write into a [`TxRing`] and goes on; the UART's
//! interrupt moves the ring into the FIFO as it drains.
//!
//! A ring can fill: a host that stopped reading, a burst of lines larger
//! than the line's rate. Its policy is drop-oldest, by whole records: a
//! record is one write (a log line, a frame), and the mux rules (lines
//! whole between frames, frames whole) hold only if no record is cut. The
//! oldest record not yet started goes first; one the UART already began
//! keeps its rest, because the host has its first bytes. What was dropped
//! is counted ([`TxRing::dropped`]) for the board to say.
//!
//! The ring has no locking: the firmware keeps it behind the critical
//! section the interrupt and the loop share, as it does [`crate::rx::RxKeep`].

/// Up to `N` bytes in up to `R` records, oldest first.
pub struct TxRing<const N: usize, const R: usize> {
    buf: [u8; N],
    /// The first unsent byte and how many follow it.
    head: usize,
    len: usize,
    /// Each record's length, oldest first, as a ring of `R`.
    records: [u16; R],
    first: usize,
    count: usize,
    /// Bytes of the oldest record already handed to the UART.
    started: usize,
    dropped: u32,
    dropped_bytes: u32,
}

impl<const N: usize, const R: usize> Default for TxRing<N, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const R: usize> TxRing<N, R> {
    /// An empty ring; `const` so it can be a static.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            len: 0,
            records: [0; R],
            first: 0,
            count: 0,
            started: 0,
            dropped: 0,
            dropped_bytes: 0,
        }
    }

    /// The largest record a ring takes: all of it, at most `u16::MAX`.
    pub const fn max_record() -> usize {
        if N < u16::MAX as usize {
            N
        } else {
            u16::MAX as usize
        }
    }

    /// Whether `bytes` would go in without dropping anything: what a
    /// writer that would rather wait than lose asks first.
    pub fn fits(&self, bytes: usize) -> bool {
        bytes <= N - self.len && self.count < R
    }

    /// Nothing left to send.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes waiting.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Records dropped so far, the new ones that could not fit included.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Their bytes.
    pub fn dropped_bytes(&self) -> u32 {
        self.dropped_bytes
    }

    /// Keep `record` whole, dropping the oldest records not yet started
    /// until it fits. `false` when it was dropped itself: longer than the
    /// ring, or no room even with every unstarted record gone.
    pub fn push(&mut self, record: &[u8]) -> bool {
        if record.is_empty() {
            return true;
        }
        if record.len() > Self::max_record() {
            self.count_drop(record.len());
            return false;
        }
        while !self.fits(record.len()) {
            if !self.drop_oldest() {
                self.count_drop(record.len());
                return false;
            }
        }
        for (i, &byte) in record.iter().enumerate() {
            self.buf[(self.head + self.len + i) % N] = byte;
        }
        self.len += record.len();
        self.records[(self.first + self.count) % R] = record.len() as u16;
        self.count += 1;
        true
    }

    /// The bytes to send next, as one slice: up to the ring's end.
    pub fn peek(&self) -> &[u8] {
        let n = self.len.min(N - self.head);
        &self.buf[self.head..self.head + n]
    }

    /// `n` bytes of [`TxRing::peek`] went to the UART.
    pub fn consume(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % N;
        self.len -= n;
        self.started += n;
        while self.count > 0 && self.started >= usize::from(self.records[self.first]) {
            self.started -= usize::from(self.records[self.first]);
            self.first = (self.first + 1) % R;
            self.count -= 1;
        }
        if self.count == 0 {
            self.started = 0;
        }
    }

    fn count_drop(&mut self, bytes: usize) {
        self.dropped = self.dropped.saturating_add(1);
        self.dropped_bytes = self
            .dropped_bytes
            .saturating_add(u32::try_from(bytes).unwrap_or(u32::MAX));
    }

    /// Drop the oldest record the UART has not begun. One it began keeps
    /// its rest, which moves up over the dropped one. `false` when there
    /// is none to drop.
    fn drop_oldest(&mut self) -> bool {
        if self.count == 0 {
            return false;
        }
        if self.started == 0 {
            let gone = usize::from(self.records[self.first]);
            self.head = (self.head + gone) % N;
            self.len -= gone;
            self.first = (self.first + 1) % R;
            self.count -= 1;
            self.count_drop(gone);
            return true;
        }
        if self.count < 2 {
            return false;
        }
        // The head record's unsent rest sits at `head`, the second record
        // right behind it: move the rest up by the second's length.
        let rest = usize::from(self.records[self.first]) - self.started;
        let second = (self.first + 1) % R;
        let gone = usize::from(self.records[second]);
        for i in (0..rest).rev() {
            self.buf[(self.head + gone + i) % N] = self.buf[(self.head + i) % N];
        }
        self.head = (self.head + gone) % N;
        self.len -= gone;
        self.records[second] = self.records[self.first];
        self.first = second;
        self.count -= 1;
        self.count_drop(gone);
        true
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// Everything the ring holds, as the UART would take it.
    fn drain<const N: usize, const R: usize>(ring: &mut TxRing<N, R>, step: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while !ring.is_empty() {
            let part = ring.peek();
            let n = part.len().min(step);
            out.extend_from_slice(&part[..n]);
            ring.consume(n);
        }
        out
    }

    #[test]
    fn records_leave_in_order_across_the_rings_end() {
        let mut ring = TxRing::<16, 4>::new();
        assert!(ring.push(b"abcdefghij"));
        let mut out = drain(&mut ring, 3);
        assert!(ring.push(b"klmnop"));
        assert!(ring.push(b"qrstu"));
        out.extend(drain(&mut ring, 5));
        assert_eq!(out, b"abcdefghijklmnopqrstu");
        assert_eq!(ring.dropped(), 0);
    }

    /// A full ring drops its oldest records whole, never a part of one.
    #[test]
    fn a_full_ring_drops_the_oldest_record_whole() {
        let mut ring = TxRing::<16, 8>::new();
        assert!(ring.push(b"one\n"));
        assert!(ring.push(b"two\n"));
        assert!(ring.push(b"three\n"));
        assert!(ring.push(b"four----\n"), "drops one and two");
        assert_eq!(ring.dropped(), 2);
        assert_eq!(ring.dropped_bytes(), 8);
        assert_eq!(drain(&mut ring, 4), b"three\nfour----\n");
    }

    /// A record the UART began keeps its rest; the next one goes instead.
    #[test]
    fn a_started_record_keeps_its_rest() {
        let mut ring = TxRing::<16, 8>::new();
        assert!(ring.push(b"AAAAAA"));
        ring.consume(2);
        assert!(ring.push(b"BBBB"));
        assert!(ring.push(b"CCCC"));
        assert!(ring.push(b"DDDDDD"), "drops B, behind A's rest");
        assert_eq!(ring.dropped(), 1);
        assert_eq!(drain(&mut ring, 16), b"AAAACCCCDDDDDD");
    }

    /// Nothing to drop but the started record: the new one is dropped.
    #[test]
    fn a_record_that_cannot_fit_is_dropped_itself() {
        let mut ring = TxRing::<8, 4>::new();
        assert!(ring.push(b"12345678"));
        ring.consume(1);
        assert!(!ring.push(b"xy"));
        assert!(!ring.push(&[0u8; 9]), "longer than the ring");
        assert_eq!(ring.dropped(), 2);
        assert_eq!(drain(&mut ring, 8), b"2345678");
    }

    /// The record count is a limit of its own.
    #[test]
    fn too_many_records_drop_the_oldest() {
        let mut ring = TxRing::<64, 2>::new();
        assert!(ring.push(b"a"));
        assert!(ring.push(b"b"));
        assert!(!ring.fits(1));
        assert!(ring.push(b"c"));
        assert_eq!(ring.dropped(), 1);
        assert_eq!(drain(&mut ring, 1), b"bc");
    }

    /// The model: lines and frames written faster than the line drains,
    /// read back on the host: every one that arrives is whole.
    #[test]
    fn under_overload_every_record_that_arrives_is_whole() {
        let mut ring = TxRing::<256, 32>::new();
        let mut out = Vec::new();
        for i in 0..400u32 {
            let line = std::format!("[LINE] n={i} pad={}\n", "x".repeat((i % 37) as usize));
            ring.push(line.as_bytes());
            // The FIFO takes 12 bytes a write.
            let part = ring.peek();
            let n = part.len().min(12);
            out.extend_from_slice(&part[..n]);
            ring.consume(n);
        }
        out.extend(drain(&mut ring, 12));
        let text = std::string::String::from_utf8(out).expect("ascii");
        let lines: Vec<&str> = text.lines().collect();
        assert!(ring.dropped() > 0, "the model overloads the ring");
        assert_eq!(lines.len() as u32 + ring.dropped(), 400);
        for line in lines {
            let pad = line.split("pad=").nth(1).expect("whole");
            let n: u32 = line["[LINE] n=".len()..]
                .split(' ')
                .next()
                .and_then(|n| n.parse().ok())
                .expect("number");
            assert_eq!(pad.len() as u32, n % 37, "{line}");
        }
    }
}
