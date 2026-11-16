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
//! is counted ([`TxRing::dropped`]), and by its [`Kind`]
//! ([`TxRing::dropped_kinds`]), for the board to say.
//!
//! A record is the whole unit the host reads, its line end included
//! (1553): the bench of 2026-11-08 (window 13) wrote a dump line and its
//! `\r\n` as two records, the loop's lossy writes after the dump dropped
//! the queued tail, and a line the UART had begun kept its rest while its
//! line end behind it went: `PBR 169 0 f79d*320PBR 188 0 f79d*320`. A
//! [`Kind::Dump`] record is never dropped for a lossy one: the new record
//! drops itself instead.
//!
//! The ring has no locking: the firmware keeps it behind the critical
//! section the interrupt and the loop share, as it does [`crate::rx::RxKeep`].

/// What a record is, for the drop rule and the drop counts (1553).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A log line, its line end included.
    Line = 0,
    /// An HDLC frame, flags included.
    Frame = 1,
    /// A frame dump's line: never dropped for another record.
    Dump = 2,
}

impl Kind {
    /// Every kind, in the order [`TxRing::dropped_kinds`] counts them.
    pub const ALL: [Kind; 3] = [Kind::Line, Kind::Frame, Kind::Dump];

    /// The kind's name, as the board says it (`tx_drop_kind=`).
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Line => "line",
            Kind::Frame => "frame",
            Kind::Dump => "dump",
        }
    }

    const fn of(byte: u8) -> Kind {
        match byte {
            1 => Kind::Frame,
            2 => Kind::Dump,
            _ => Kind::Line,
        }
    }
}

/// Up to `N` bytes in up to `R` records, oldest first.
pub struct TxRing<const N: usize, const R: usize> {
    buf: [u8; N],
    /// The first unsent byte and how many follow it.
    head: usize,
    len: usize,
    /// Each record's length, oldest first, as a ring of `R`.
    records: [u16; R],
    /// Each record's [`Kind`], beside its length.
    kinds: [u8; R],
    first: usize,
    count: usize,
    /// Bytes of the oldest record already handed to the UART.
    started: usize,
    dropped: u32,
    dropped_bytes: u32,
    /// Records dropped, by [`Kind`] in [`Kind::ALL`]'s order.
    dropped_kinds: [u32; 3],
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
            kinds: [0; R],
            first: 0,
            count: 0,
            started: 0,
            dropped: 0,
            dropped_bytes: 0,
            dropped_kinds: [0; 3],
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

    /// Records waiting, a started one included (1630): with
    /// [`TxRing::len`], the room a writer that must not make the ring
    /// drop sizes its burst by.
    pub fn records(&self) -> usize {
        self.count
    }

    /// Records dropped so far, the new ones that could not fit included.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Their bytes.
    pub fn dropped_bytes(&self) -> u32 {
        self.dropped_bytes
    }

    /// Records dropped by [`Kind`], in [`Kind::ALL`]'s order.
    pub fn dropped_kinds(&self) -> [u32; 3] {
        self.dropped_kinds
    }

    /// Whether a record of `kind` still waits, its rest included: what a
    /// dump writer waits out before the loop writes lossy again (1553).
    pub fn holds(&self, kind: Kind) -> bool {
        (0..self.count).any(|i| self.kinds[(self.first + i) % R] == kind as u8)
    }

    /// Keep `record`, of `kind`, whole, dropping the oldest records not
    /// yet started until it fits. `false` when it was dropped itself:
    /// longer than the ring, or no room even with every unstarted record
    /// gone, or the next to go is a [`Kind::Dump`] record.
    pub fn push(&mut self, kind: Kind, record: &[u8]) -> bool {
        if record.is_empty() {
            return true;
        }
        if record.len() > Self::max_record() {
            self.count_drop(kind, record.len());
            return false;
        }
        while !self.fits(record.len()) {
            if !self.drop_oldest() {
                self.count_drop(kind, record.len());
                return false;
            }
        }
        for (i, &byte) in record.iter().enumerate() {
            self.buf[(self.head + self.len + i) % N] = byte;
        }
        self.len += record.len();
        self.records[(self.first + self.count) % R] = record.len() as u16;
        self.kinds[(self.first + self.count) % R] = kind as u8;
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

    fn count_drop(&mut self, kind: Kind, bytes: usize) {
        self.dropped = self.dropped.saturating_add(1);
        self.dropped_kinds[kind as usize] = self.dropped_kinds[kind as usize].saturating_add(1);
        self.dropped_bytes = self
            .dropped_bytes
            .saturating_add(u32::try_from(bytes).unwrap_or(u32::MAX));
    }

    /// Drop the oldest record the UART has not begun. One it began keeps
    /// its rest, which moves up over the dropped one. `false` when there
    /// is none to drop, or it is a [`Kind::Dump`] record.
    fn drop_oldest(&mut self) -> bool {
        if self.count == 0 {
            return false;
        }
        if self.started == 0 {
            let kind = Kind::of(self.kinds[self.first]);
            if kind == Kind::Dump {
                return false;
            }
            let gone = usize::from(self.records[self.first]);
            self.head = (self.head + gone) % N;
            self.len -= gone;
            self.first = (self.first + 1) % R;
            self.count -= 1;
            self.count_drop(kind, gone);
            return true;
        }
        if self.count < 2 {
            return false;
        }
        // The head record's unsent rest sits at `head`, the second record
        // right behind it: move the rest up by the second's length.
        let rest = usize::from(self.records[self.first]) - self.started;
        let second = (self.first + 1) % R;
        let kind = Kind::of(self.kinds[second]);
        if kind == Kind::Dump {
            return false;
        }
        let gone = usize::from(self.records[second]);
        for i in (0..rest).rev() {
            self.buf[(self.head + gone + i) % N] = self.buf[(self.head + i) % N];
        }
        self.head = (self.head + gone) % N;
        self.len -= gone;
        self.records[second] = self.records[self.first];
        self.kinds[second] = self.kinds[self.first];
        self.first = second;
        self.count -= 1;
        self.count_drop(kind, gone);
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
        assert!(ring.push(Kind::Line, b"abcdefghij"));
        let mut out = drain(&mut ring, 3);
        assert!(ring.push(Kind::Line, b"klmnop"));
        assert!(ring.push(Kind::Line, b"qrstu"));
        out.extend(drain(&mut ring, 5));
        assert_eq!(out, b"abcdefghijklmnopqrstu");
        assert_eq!(ring.dropped(), 0);
    }

    /// A full ring drops its oldest records whole, never a part of one.
    #[test]
    fn a_full_ring_drops_the_oldest_record_whole() {
        let mut ring = TxRing::<16, 8>::new();
        assert!(ring.push(Kind::Line, b"one\n"));
        assert!(ring.push(Kind::Line, b"two\n"));
        assert!(ring.push(Kind::Line, b"three\n"));
        assert!(ring.push(Kind::Line, b"four----\n"), "drops one and two");
        assert_eq!(ring.dropped(), 2);
        assert_eq!(ring.dropped_bytes(), 8);
        assert_eq!(drain(&mut ring, 4), b"three\nfour----\n");
    }

    /// A record the UART began keeps its rest; the next one goes instead.
    #[test]
    fn a_started_record_keeps_its_rest() {
        let mut ring = TxRing::<16, 8>::new();
        assert!(ring.push(Kind::Line, b"AAAAAA"));
        ring.consume(2);
        assert!(ring.push(Kind::Line, b"BBBB"));
        assert!(ring.push(Kind::Line, b"CCCC"));
        assert!(ring.push(Kind::Line, b"DDDDDD"), "drops B, behind A's rest");
        assert_eq!(ring.dropped(), 1);
        assert_eq!(drain(&mut ring, 16), b"AAAACCCCDDDDDD");
    }

    /// Nothing to drop but the started record: the new one is dropped.
    #[test]
    fn a_record_that_cannot_fit_is_dropped_itself() {
        let mut ring = TxRing::<8, 4>::new();
        assert!(ring.push(Kind::Line, b"12345678"));
        ring.consume(1);
        assert!(!ring.push(Kind::Line, b"xy"));
        assert!(!ring.push(Kind::Line, &[0u8; 9]), "longer than the ring");
        assert_eq!(ring.dropped(), 2);
        assert_eq!(drain(&mut ring, 8), b"2345678");
    }

    /// The record count is a limit of its own.
    #[test]
    fn too_many_records_drop_the_oldest() {
        let mut ring = TxRing::<64, 2>::new();
        assert!(ring.push(Kind::Line, b"a"));
        assert!(ring.push(Kind::Line, b"b"));
        assert!(!ring.fits(1));
        assert!(ring.push(Kind::Line, b"c"));
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
            ring.push(Kind::Line, line.as_bytes());
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

    /// The UART's FIFO, as the model drains it: up to `step` bytes.
    fn tick<const N: usize, const R: usize>(
        ring: &mut TxRing<N, R>,
        out: &mut Vec<u8>,
        step: usize,
    ) {
        let part = ring.peek();
        let n = part.len().min(step);
        out.extend_from_slice(&part[..n]);
        ring.consume(n);
    }

    /// A dump line, its line end in the same record (1553).
    fn dump_line(i: usize) -> std::string::String {
        std::format!("PBR {i} 0 f79d*{} 0000*{}\r\n", 300 - i % 17, 20 + i % 17)
    }

    /// The dump writer's way: wait for room, then push, the FIFO
    /// draining `step` bytes a turn.
    fn push_waiting<const N: usize, const R: usize>(
        ring: &mut TxRing<N, R>,
        out: &mut Vec<u8>,
        kind: Kind,
        record: &[u8],
    ) {
        while !ring.fits(record.len()) {
            tick(ring, out, 12);
        }
        assert!(ring.push(kind, record));
        tick(ring, out, 12);
    }

    /// The dump's lines as the host reads them, split on line ends and
    /// frame flags.
    fn dump_lines_read(out: &[u8]) -> Vec<std::string::String> {
        std::string::String::from_utf8_lossy(out)
            .split("\r\n")
            .flat_map(|l| l.split('\x7e'))
            .filter(|l| l.starts_with("PBR "))
            .map(|l| std::format!("{l}\r\n"))
            .collect()
    }

    /// 1553, window 13's case: a 600-line dump through a ring smaller than
    /// the dump, the writer waiting for room per line, a `[UART]` banner
    /// and a frame written lossy in its middle, and the loop's burst of
    /// lossy lines right after it while the dump's tail still waits. At
    /// 40b81a33 (line end a record of its own, kept by nothing) 526 of the
    /// 600 lines arrived. Now every dump line arrives, whole and in
    /// order, and what was dropped instead is counted by its kind.
    #[test]
    fn a_dump_through_a_small_ring_loses_no_line() {
        let mut ring = TxRing::<8192, 512>::new();
        let mut out = Vec::new();
        for i in 0..600 {
            push_waiting(&mut ring, &mut out, Kind::Dump, dump_line(i).as_bytes());
            if i == 300 {
                ring.push(Kind::Line, b"[UART] rx_lost=0 overrun=0 tx_drop=0\r\n");
                ring.push(Kind::Frame, b"\x7e\x01\x02\x03\x7e");
            }
        }
        for i in 0..400 {
            let line = std::format!("[LOOP] n={i} pad={}\r\n", "x".repeat(i % 40));
            ring.push(Kind::Line, line.as_bytes());
            tick(&mut ring, &mut out, 1);
        }
        out.extend(drain(&mut ring, 12));
        let want: Vec<std::string::String> = (0..600).map(dump_line).collect();
        assert_eq!(dump_lines_read(&out), want);
        let [line, frame, dump] = ring.dropped_kinds();
        assert_eq!(dump, 0);
        assert!(line > 0, "the burst behind the dump's tail drops itself");
        assert_eq!(line + frame, ring.dropped());
        // Every [LOOP] line that arrived is whole.
        let text = std::string::String::from_utf8_lossy(&out).into_owned();
        for l in text.split("\r\n").filter(|l| l.contains("[LOOP]")) {
            let n: usize = l["[LOOP] n=".len()..]
                .split(' ')
                .next()
                .and_then(|n| n.parse().ok())
                .expect("number");
            assert_eq!(l.split("pad=").nth(1).map(str::len), Some(n % 40), "{l}");
        }
    }

    /// The board's dump writer also waits out its tail before the loop
    /// writes lossy again (`TxRing::holds`): then nothing at all is lost.
    #[test]
    fn a_dump_waited_out_loses_nothing() {
        let mut ring = TxRing::<8192, 512>::new();
        let mut out = Vec::new();
        for i in 0..600 {
            push_waiting(&mut ring, &mut out, Kind::Dump, dump_line(i).as_bytes());
        }
        while ring.holds(Kind::Dump) {
            tick(&mut ring, &mut out, 12);
        }
        assert!(ring.push(Kind::Line, b"[UART] tx_drop=0\r\n"));
        assert!(ring.push(Kind::Frame, b"\x7e\x01\x7e"));
        for i in 0..100 {
            assert!(ring.push(Kind::Line, std::format!("[LOOP] n={i}\r\n").as_bytes()));
        }
        out.extend(drain(&mut ring, 12));
        assert_eq!(ring.dropped(), 0);
        let want: Vec<std::string::String> = (0..600).map(dump_line).collect();
        assert_eq!(dump_lines_read(&out), want);
    }

    /// A lossy record behind a dump record drops itself, started or not,
    /// and is counted by its kind.
    #[test]
    fn a_dump_record_is_never_dropped_for_a_lossy_one() {
        let mut ring = TxRing::<16, 8>::new();
        assert!(ring.push(Kind::Dump, b"DDDDDDDD"));
        assert!(!ring.push(Kind::Line, b"LLLLLLLLLL"));
        ring.consume(2);
        assert!(ring.push(Kind::Dump, b"EEEE"));
        assert!(!ring.push(Kind::Frame, b"FFFFFFFF"));
        assert_eq!(ring.dropped_kinds(), [1, 1, 0]);
        assert_eq!(drain(&mut ring, 3), b"DDDDDDEEEE");
        assert!(!ring.holds(Kind::Dump));
    }
}
