#![no_std]
//! Debug text and HDLC frames on one serial line.
//!
//! A board with a single host port (the ThinkNode M9's UART0 behind a CH340K)
//! carries its debug log and its Reticulum/control transport on the same byte
//! stream. The rules, both halves of which live here so a host test holds them
//! together:
//!
//! * The board writes whole log lines **between** HDLC frames, never inside an
//!   open one, and renders the HDLC specials and the flow-control bytes inside
//!   text as four-character literals (`\x7e`, `\x7d`, `\x11`, `\x13`,
//!   [`escape_text`]). Text therefore never contains a raw [`FLAG`], and every
//!   host-side HDLC reader (the core `Deframer` drops out-of-frame bytes)
//!   ignores it unchanged.
//! * The line carries XON/XOFF software flow control: the board's UART sends
//!   [`XOFF`] when its receive FIFO fills and [`XON`] when it drains, at any
//!   byte boundary. Neither byte travels raw for any other reason: both senders
//!   HDLC-escape them inside frames ([`escape_flow`]), and the host removes the
//!   board's flow bytes before anything else reads the stream ([`strip_flow`]).
//!   The core `Deframer` undoes any `ESCAPE`d byte, so no receiver changes.
//!   Only the board sends them raw, and its UART obeys a received XOFF in
//!   hardware: one raw 0x13 from the host mutes it until an XON or a reset
//!   (1238). The host senders that keep this: periculum, for every frame it
//!   writes to an LNode (periculum 0c455f7 and 1b81660,
//!   `periculum-wire/src/hdlc.rs`), and lnsd, for a `SerialInterface` whose
//!   board states `serial_mux=1` in its capability report (or whose block
//!   says `serial_mux = yes`), which also strips the board's flow bytes on
//!   read (`leviculum-std/src/interfaces/serial_mux.rs`, 1242, 1249). A
//!   plain `SerialInterface` keeps its bytes: Python's reader does not undo
//!   an escaped flow byte.
//! * The host separates the two with [`Splitter`]: out-of-frame bytes are
//!   [`Event::Text`], `FLAG..FLAG` runs are [`Event::Frame`], forwarded
//!   verbatim (still escaped, flags included) to whatever speaks HDLC.
//!
//! The framing itself is `leviculum_core::framing::hdlc`, unchanged.

pub mod park;
pub mod rx;
pub mod tx;

use leviculum_core::framing::hdlc::{max_framed_size, ESCAPE_XOR};
pub use leviculum_core::framing::hdlc::{ESCAPE, FLAG};

/// Resume sending: the board's receive FIFO drained below its XON threshold.
pub const XON: u8 = 0x11;
/// Stop sending: the board's receive FIFO passed its XOFF threshold.
pub const XOFF: u8 = 0x13;

/// What [`FLAG`] becomes inside log text.
pub const ESCAPED_FLAG: &[u8; 4] = b"\\x7e";
/// What [`ESCAPE`] becomes inside log text.
pub const ESCAPED_ESCAPE: &[u8; 4] = b"\\x7d";
/// What [`XON`] becomes inside log text.
pub const ESCAPED_XON: &[u8; 4] = b"\\x11";
/// What [`XOFF`] becomes inside log text.
pub const ESCAPED_XOFF: &[u8; 4] = b"\\x13";

/// The HW_MTU every Leviculum serial interface declares, on the board
/// (`SERIAL_HW_MTU` in the rp/nrf `usb.rs`) and in lnsd.
pub const SERIAL_HW_MTU: usize = 564;

/// The longest frame on the wire, flags included: every byte escaped.
pub const MAX_FRAME_WIRE: usize = max_framed_size(SERIAL_HW_MTU);

/// Write `text` to `out` with [`FLAG`], [`ESCAPE`], [`XON`] and [`XOFF`]
/// rendered as their four-character literals; every other byte passes unchanged.
///
/// `out` sees runs of the input and the escape literals, in order, so a
/// writer can stream straight into its UART without a buffer.
pub fn escape_text(text: &[u8], out: &mut impl FnMut(&[u8])) {
    let mut start = 0;
    for (i, &byte) in text.iter().enumerate() {
        let literal: &[u8] = match byte {
            FLAG => ESCAPED_FLAG,
            ESCAPE => ESCAPED_ESCAPE,
            XON => ESCAPED_XON,
            XOFF => ESCAPED_XOFF,
            _ => continue,
        };
        if start < i {
            out(&text[start..i]);
        }
        out(literal);
        start = i + 1;
    }
    if start < text.len() {
        out(&text[start..]);
    }
}

/// How many bytes [`escape_text`] writes for `text`.
pub fn escaped_len(text: &[u8]) -> usize {
    text.len()
        + 3 * text
            .iter()
            .filter(|&&byte| matches!(byte, FLAG | ESCAPE | XON | XOFF))
            .count()
}

/// Write HDLC-framed `wire` to `out` with every [`XON`] and [`XOFF`] escaped
/// as `ESCAPE, byte ^ 0x20`, so neither reaches the line raw.
pub fn escape_flow(wire: &[u8], out: &mut impl FnMut(&[u8])) {
    let mut start = 0;
    for (i, &byte) in wire.iter().enumerate() {
        if byte != XON && byte != XOFF {
            continue;
        }
        if start < i {
            out(&wire[start..i]);
        }
        out(&[ESCAPE, byte ^ ESCAPE_XOR]);
        start = i + 1;
    }
    if start < wire.len() {
        out(&wire[start..]);
    }
}

/// The board's flow-control request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// [`XOFF`]: hold what is not sent yet.
    Stop,
    /// [`XON`]: send again.
    Go,
}

/// Write `input` to `out` without its [`XON`] and [`XOFF`] bytes; the last of
/// them, if any, is the board's current request.
pub fn strip_flow(input: &[u8], out: &mut impl FnMut(&[u8])) -> Option<Flow> {
    let mut last = None;
    let mut start = 0;
    for (i, &byte) in input.iter().enumerate() {
        let flow = match byte {
            XON => Flow::Go,
            XOFF => Flow::Stop,
            _ => continue,
        };
        if start < i {
            out(&input[start..i]);
        }
        last = Some(flow);
        start = i + 1;
    }
    if start < input.len() {
        out(&input[start..]);
    }
    last
}

/// One piece of the split stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// Out-of-frame bytes: at most one line, ending at its `\n` when the line
    /// is complete. A line cut by the end of a read continues in the next
    /// `Text`, so a consumer joins pieces until it sees the newline.
    Text(&'a [u8]),
    /// One frame exactly as it was on the wire, opening and closing [`FLAG`]
    /// included and the payload still escaped.
    Frame(&'a [u8]),
}

/// Separates a single-port board's byte stream into text and frames.
///
/// `N` bounds one frame on the wire, flags included ([`MAX_FRAME_WIRE`] for a
/// Leviculum serial link). Resynchronisation, because one lost [`FLAG`] would
/// otherwise swap text and frames for good:
///
/// * an empty frame (`FLAG FLAG`) keeps the second flag as an opener, exactly
///   as the core `Deframer` does, so back-to-back frames resync at once;
/// * a "frame" that is a complete line of text (valid UTF-8, no control bytes
///   but tab/CR/LF, ending in `\n`) was text read after a lost flag: it is
///   emitted as [`Event::Text`] and its closing flag opens the next frame;
/// * a "frame" that outgrows `N` was text too: its bytes are emitted as
///   [`Event::Text`] and the splitter returns to text.
pub struct Splitter<const N: usize> {
    frame: [u8; N],
    /// 0 in text; otherwise the bytes held, `frame[0]` being the opening flag.
    len: usize,
}

impl<const N: usize> Default for Splitter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Splitter<N> {
    const ROOM: () = assert!(N >= 2, "a frame needs room for both flags");

    pub const fn new() -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::ROOM;
        Self {
            frame: [0; N],
            len: 0,
        }
    }

    /// Whether an opening flag has been seen and its frame is not closed yet.
    pub fn in_frame(&self) -> bool {
        self.len > 0
    }

    /// Drop a half-read frame, e.g. after the port went away.
    pub fn reset(&mut self) {
        self.len = 0;
    }

    /// Split `input`, handing every event to `sink` in stream order.
    pub fn feed(&mut self, input: &[u8], mut sink: impl FnMut(Event<'_>)) {
        let mut i = 0;
        while i < input.len() {
            if self.len == 0 {
                let start = i;
                while i < input.len() && input[i] != FLAG {
                    i += 1;
                }
                emit_lines(&input[start..i], &mut sink);
                if i < input.len() {
                    self.frame[0] = FLAG;
                    self.len = 1;
                    i += 1;
                }
                continue;
            }
            let byte = input[i];
            i += 1;
            if byte != FLAG {
                if self.len == N - 1 {
                    // No room left for the closing flag: this was never a frame.
                    emit_lines(&self.frame[1..self.len], &mut sink);
                    self.len = 0;
                    i -= 1;
                    continue;
                }
                self.frame[self.len] = byte;
                self.len += 1;
                continue;
            }
            if self.len == 1 {
                continue;
            }
            let payload = &self.frame[1..self.len];
            if is_text_line(payload) {
                emit_lines(payload, &mut sink);
                self.len = 1;
                continue;
            }
            self.frame[self.len] = FLAG;
            sink(Event::Frame(&self.frame[..=self.len]));
            self.len = 0;
        }
    }
}

fn emit_lines(mut text: &[u8], sink: &mut impl FnMut(Event<'_>)) {
    while !text.is_empty() {
        let end = text
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(text.len(), |pos| pos + 1);
        sink(Event::Text(&text[..end]));
        text = &text[end..];
    }
}

/// A complete line a board could have logged; random frame payloads are not.
fn is_text_line(bytes: &[u8]) -> bool {
    bytes.last() == Some(&b'\n')
        && core::str::from_utf8(bytes).is_ok()
        && bytes
            .iter()
            .all(|&byte| byte >= 0x20 || matches!(byte, b'\t' | b'\r' | b'\n'))
        && !bytes.contains(&0x7F)
}

#[cfg(test)]
mod tests;
