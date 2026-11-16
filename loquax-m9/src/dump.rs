//! The frame over the debug link as text (`handheld-messenger.md`, "The
//! test plan", item 3; slice four, 1361): the board dumps what its panel
//! shows when a `FRAME_DUMP` control frame asks
//! (`leviculum_core::envelope::TYPE_FRAME_DUMP`), and the rig reads the
//! lines back into a frame and compares it with the golden the host
//! pinned. One encoder and one decoder, here, so the board and the host
//! cannot disagree about the format.
//!
//! The format, every line plain ASCII, so the dump shares the M9's one
//! UART with the log and the HDLC frames (`leviculum-nrf/serial-mux`)
//! without a byte that means something else there:
//!
//! ```text
//! PBM 320 240
//! PBR <y> <x> <run> <run> ...
//! PBE <fnv32>
//! ```
//!
//! `PBM` opens a dump of `w` x `h` RGB565 pixels. Each `PBR` line carries
//! the pixels of row `y` from column `x` on, as runs: `rrrr` is one pixel
//! of that RGB565 value in four hex digits, `rrrr*n` is `n` of them. A row
//! takes as many `PBR` lines as keep each under [`LINE_MAX`] bytes. `PBE`
//! closes it with the FNV-1a hash of the pixels, row by row, each value
//! little end first, so a lost or garbled line is caught rather than drawn.
//! Lines that start with none of the three are the firmware's other log
//! and are skipped, so a capture reads as it was taken.
//!
//! At 115200 baud, the rate periculum opens the M9's port at, 11,520 B a
//! second: the board's chats list is a 65,521 B dump and its open chat
//! 41,520 B (`tests/firmware.rs` prints them), 5.7 s and 3.6 s on the wire.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

/// The longest dump line, its line end not counted. Under the 160 B a log
/// line of the board may take, so a reader with a line buffer of that size
/// reads a dump too.
pub const LINE_MAX: usize = 120;

/// What opens a dump.
pub const OPEN: &str = "PBM ";
/// What starts a row's line.
pub const ROW: &str = "PBR ";
/// What closes a dump.
pub const CLOSE: &str = "PBE ";

/// The FNV-1a hash the closing line carries, over `pixels` row by row,
/// each value little end first.
pub fn fnv32(pixels: impl IntoIterator<Item = u16>) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for pixel in pixels {
        for byte in pixel.to_le_bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    hash
}

/// Dump the `width` x `height` frame `pixel(x, y)` returns, one line at a
/// time into `emit`, without its line end.
pub fn encode(
    width: u32,
    height: u32,
    pixel: impl Fn(u32, u32) -> u16,
    emit: &mut dyn FnMut(&str),
) {
    let mut line = String::with_capacity(LINE_MAX + 16);
    let _ = write!(line, "{OPEN}{width} {height}");
    emit(&line);
    for y in 0..height {
        line.clear();
        let _ = write!(line, "{ROW}{y} 0");
        let mut x = 0;
        while x < width {
            let value = pixel(x, y);
            let mut n = 1;
            while x + n < width && pixel(x + n, y) == value {
                n += 1;
            }
            let mut run = String::with_capacity(12);
            match n {
                1 => {
                    let _ = write!(run, " {value:04x}");
                }
                _ => {
                    let _ = write!(run, " {value:04x}*{n}");
                }
            }
            if line.len() + run.len() > LINE_MAX {
                emit(&line);
                line.clear();
                let _ = write!(line, "{ROW}{y} {x}");
            }
            line.push_str(&run);
            x += n;
        }
        emit(&line);
    }
    line.clear();
    let pixels = (0..height).flat_map(|y| (0..width).map(move |x| (x, y)));
    let _ = write!(
        line,
        "{CLOSE}{:08x}",
        fnv32(pixels.map(|(x, y)| pixel(x, y)))
    );
    emit(&line);
}

/// Why a dump did not read back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpError {
    /// A dump line that does not parse, by its number in the text (from one).
    Line(usize),
    /// A row line before any `PBM`.
    NotOpen(usize),
    /// A run that reaches past the row, by its line number.
    PastRow(usize),
    /// The dump closed with pixels no row line carried.
    Missing { pixels: usize },
    /// The pixels read do not hash to the closing line's value.
    Hash { want: u32, got: u32 },
    /// The text ended before `PBE`.
    Unclosed,
}

/// A dump read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dump {
    pub width: u32,
    pub height: u32,
    /// Row-major RGB565 values.
    pub pixels: Vec<u16>,
}

/// Read the first complete dump in `text`; every other line is skipped.
pub fn decode(text: &str) -> Result<Dump, DumpError> {
    let mut open: Option<(Dump, Vec<bool>)> = None;
    for (number, line) in text.lines().enumerate() {
        let number = number + 1;
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(OPEN) {
            let mut words = rest.split_whitespace();
            let (Some(w), Some(h), None) = (words.next(), words.next(), words.next()) else {
                return Err(DumpError::Line(number));
            };
            let (Ok(width), Ok(height)) = (w.parse::<u32>(), h.parse::<u32>()) else {
                return Err(DumpError::Line(number));
            };
            let size = (width as usize) * (height as usize);
            open = Some((
                Dump {
                    width,
                    height,
                    pixels: vec![0; size],
                },
                vec![false; size],
            ));
        } else if let Some(rest) = line.strip_prefix(ROW) {
            let (dump, seen) = open.as_mut().ok_or(DumpError::NotOpen(number))?;
            row(dump, seen, rest, number)?;
        } else if let Some(rest) = line.strip_prefix(CLOSE) {
            let (dump, seen) = open.take().ok_or(DumpError::NotOpen(number))?;
            let want = u32::from_str_radix(rest.trim(), 16).map_err(|_| DumpError::Line(number))?;
            let missing = seen.iter().filter(|s| !**s).count();
            if missing > 0 {
                return Err(DumpError::Missing { pixels: missing });
            }
            let got = fnv32(dump.pixels.iter().copied());
            if got != want {
                return Err(DumpError::Hash { want, got });
            }
            return Ok(dump);
        }
    }
    Err(DumpError::Unclosed)
}

fn row(dump: &mut Dump, seen: &mut [bool], rest: &str, number: usize) -> Result<(), DumpError> {
    let bad = DumpError::Line(number);
    let mut words = rest.split_whitespace();
    let y: u32 = words.next().and_then(|w| w.parse().ok()).ok_or(bad)?;
    let mut x: u32 = words.next().and_then(|w| w.parse().ok()).ok_or(bad)?;
    if y >= dump.height {
        return Err(DumpError::PastRow(number));
    }
    for run in words {
        let (value, n) = match run.split_once('*') {
            Some((value, n)) => (value, n.parse::<u32>().map_err(|_| bad)?),
            None => (run, 1),
        };
        if value.len() != 4 {
            return Err(bad);
        }
        let value = u16::from_str_radix(value, 16).map_err(|_| bad)?;
        if x.checked_add(n).is_none_or(|end| end > dump.width) {
            return Err(DumpError::PastRow(number));
        }
        for _ in 0..n {
            let at = (y * dump.width + x) as usize;
            dump.pixels[at] = value;
            seen[at] = true;
            x += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(width: u32, height: u32, pixel: impl Fn(u32, u32) -> u16) -> Vec<String> {
        let mut lines = Vec::new();
        encode(width, height, pixel, &mut |line| lines.push(line.into()));
        lines
    }

    #[test]
    fn a_dump_reads_back_as_the_frame_it_was_made_of() {
        let pixel = |x: u32, y: u32| {
            if (x / 3 + y).is_multiple_of(4) {
                0xffff
            } else {
                (x * 7 + y) as u16
            }
        };
        let lines = lines_of(320, 4, pixel);
        assert_eq!(lines[0], "PBM 320 4");
        assert!(
            lines.iter().all(|l| l.len() <= LINE_MAX),
            "a line past LINE_MAX"
        );
        assert!(
            lines.len() > 4 + 2,
            "rows of distinct pixels take several lines"
        );
        let mut text = String::from("[FW_BUILD] x\n");
        for line in &lines {
            text.push_str(line);
            text.push_str(" \r\n");
            text.push_str("KB 10 00000000000000\n");
        }
        let dump = decode(&text).expect("the dump reads");
        assert_eq!((dump.width, dump.height), (320, 4));
        for y in 0..4 {
            for x in 0..320 {
                assert_eq!(dump.pixels[(y * 320 + x) as usize], pixel(x, y));
            }
        }
    }

    #[test]
    fn a_flat_row_is_one_run() {
        let lines = lines_of(320, 1, |_, _| 0x1234);
        assert_eq!(lines[1], "PBR 0 0 1234*320");
    }

    #[test]
    fn a_lost_or_changed_line_is_caught() {
        let pixel = |x: u32, y: u32| (x ^ y) as u16;
        let lines = lines_of(16, 3, pixel);
        let without: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !l.starts_with("PBR 1 "))
            .collect();
        assert_eq!(
            decode(&without.join("\n")),
            Err(DumpError::Missing { pixels: 16 })
        );
        let changed = lines.join("\n").replace("PBR 2 0 0002", "PBR 2 0 0003");
        assert!(matches!(decode(&changed), Err(DumpError::Hash { .. })));
        let unclosed: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !l.starts_with("PBE"))
            .collect();
        assert_eq!(decode(&unclosed.join("\n")), Err(DumpError::Unclosed));
    }
}
