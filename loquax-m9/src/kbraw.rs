//! The keyboard controller's answer to a [`Scan`] (slice four, 1361): what
//! the board reads over I2C, logged raw, and the table that turns it into
//! the matrix the driver ([`crate::keyboard`]) debounces.
//!
//! The controller's register map is unread (`thinknode-m9.md`, "Keyboard"
//! and "Open"): all that is measured is that it ACKs at 0x6D on our V1.1
//! unit and that the community port reads register 0x01 on a key press.
//! So nothing here claims to know the answer's shape. The firmware reads
//! [`Protocol::len`] bytes after writing [`Protocol::register`], prints
//! every answer as a `KBRAW <ms> <hex>` line before it decodes anything,
//! and decodes it by a table, [`Decode`]. Until the reviewer has read a
//! capture with each key pressed once, the board runs [`PROVISIONAL`]: the
//! answer taken as the matrix's bitmap, bit for bit. A capture replays on
//! the host through [`parse_raw`] and any other table, so the right table
//! is found without a flash per guess.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::keyboard::{Cap, Layout, Scan, COLUMNS, POSITIONS, ROWS};
use crate::keys::M9Key;

/// The longest answer the firmware reads in one poll.
pub const MAX_ANSWER: usize = 16;

/// A table entry for an answer bit or code that is no key.
pub const NO_KEY: u8 = 0xFF;

/// How the answer becomes a scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decode {
    /// The answer is a bitmap of keys down: bit `b` of byte `k` is answer
    /// bit `8 * k + b`, and the table names the matrix position of each
    /// answer bit, or [`NO_KEY`].
    Bitmap(&'static [u8]),
    /// Each answer byte is the code of one key down, `0` for none, and the
    /// table maps a code to its matrix position.
    Codes(&'static [(u8, u8)]),
}

/// What the firmware asks the controller each poll, and how it reads the
/// answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    /// The controller's address: 0x6D on V1.1, 0x6C on V1.0.
    pub address: u8,
    /// The register written before the read; none reads without a write.
    pub register: Option<u8>,
    /// How many bytes it reads, at most [`MAX_ANSWER`].
    pub len: usize,
    pub decode: Decode,
}

/// Answer bit `i` is matrix position `i`, past the matrix none.
pub const IDENTITY: [u8; 8 * MAX_ANSWER] = identity();

const fn identity() -> [u8; 8 * MAX_ANSWER] {
    let mut table = [NO_KEY; 8 * MAX_ANSWER];
    let mut i = 0;
    while i < POSITIONS {
        table[i] = i as u8;
        i += 1;
    }
    table
}

/// What the board runs until a capture reads the controller: register 0x01,
/// the one the community port reads, eight bytes, taken as the matrix bit
/// for bit. Unproven; the `KBRAW` lines are what proves or replaces it.
pub const PROVISIONAL: Protocol = Protocol {
    address: 0x6D,
    register: Some(0x01),
    len: 8,
    decode: Decode::Bitmap(&IDENTITY),
};

impl Protocol {
    /// The scan `answer` reads as.
    pub fn scan(&self, answer: &[u8]) -> Scan {
        let mut down: Vec<(usize, usize)> = Vec::new();
        let mut mark = |position: u8| {
            let position = usize::from(position);
            if position < POSITIONS {
                down.push((position / COLUMNS, position % COLUMNS));
            }
        };
        match self.decode {
            Decode::Bitmap(table) => {
                for (k, byte) in answer.iter().enumerate() {
                    for b in 0..8 {
                        if byte & (1 << b) != 0 {
                            mark(table.get(8 * k + b).copied().unwrap_or(NO_KEY));
                        }
                    }
                }
            }
            Decode::Codes(table) => {
                for code in answer.iter().filter(|c| **c != 0) {
                    if let Some((_, position)) = table.iter().find(|(c, _)| c == code) {
                        mark(*position);
                    }
                }
            }
        }
        Scan::of(&down)
    }
}

fn hex(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
}

/// The raw line: `KBRAW <ms> <hex>`, the answer as it came.
pub fn raw_line(ms: u32, answer: &[u8]) -> String {
    let mut out = String::with_capacity(8 + 11 + 2 * answer.len());
    let _ = write!(out, "KBRAW {ms} ");
    hex(&mut out, answer);
    out
}

/// The recording line of slice three: `KB <ms> <14 hex>`, the scan the
/// table made of the answer ([`crate::keyboard::parse_recording`] reads it).
pub fn scan_line(ms: u32, scan: Scan) -> String {
    let mut out = String::with_capacity(32);
    let _ = write!(out, "KB {ms} ");
    hex(&mut out, &scan.frame());
    out
}

/// The answers of a capture, `(ms, bytes)` per `KBRAW` line; every other
/// line is skipped and a `KBRAW` line that does not read is too, by number.
pub fn parse_raw(text: &str) -> impl Iterator<Item = Result<(u32, Vec<u8>), usize>> + '_ {
    text.lines().enumerate().filter_map(|(number, line)| {
        let rest = line.trim().strip_prefix("KBRAW ")?;
        let mut words = rest.split_whitespace();
        let mut read = || -> Option<(u32, Vec<u8>)> {
            let ms = words.next()?.parse().ok()?;
            let hex = words.next()?;
            if words.next().is_some() || hex.len() % 2 != 0 {
                return None;
            }
            let bytes = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            Some((ms, bytes))
        };
        Some(read().ok_or(number + 1))
    })
}

/// The layout the board types with until its own is read: the test layout
/// of slice three (`keyboard.rs`), not the board's. q to p on row 0, a to l
/// and Backspace on row 1, Shift, z to m, Sym on row 2, the arrows, Enter,
/// Back and the space on row 3.
pub fn provisional_layout() -> Layout {
    let mut cells = [[Cap::None; COLUMNS]; ROWS];
    for (column, (plain, sym)) in "qwertyuiop".chars().zip("1234567890".chars()).enumerate() {
        cells[0][column] = Cap::letter(plain, sym);
    }
    for (column, (plain, sym)) in "asdfghjkl".chars().zip("@#&*-+=()".chars()).enumerate() {
        cells[1][column] = Cap::letter(plain, sym);
    }
    cells[1][9] = Cap::Key(M9Key::Backspace);
    cells[2][0] = Cap::Shift;
    for (column, (plain, sym)) in "zxcvbnm".chars().zip("!?,.;:'".chars()).enumerate() {
        cells[2][column + 1] = Cap::letter(plain, sym);
    }
    cells[2][9] = Cap::Sym;
    let keys = [M9Key::Up, M9Key::Down, M9Key::Left, M9Key::Right];
    for (column, key) in keys.into_iter().enumerate() {
        cells[3][column] = Cap::Key(key);
    }
    cells[3][4] = Cap::Key(M9Key::Enter);
    cells[3][5] = Cap::Key(M9Key::Back);
    cells[3][6] = Cap::Char {
        plain: ' ',
        shifted: ' ',
        sym: ' ',
    };
    Layout(cells)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::parse_recording;

    #[test]
    fn the_provisional_table_reads_the_answer_as_the_matrix() {
        let answer = [0x00, 0x80, 0, 0, 0, 0, 0x04, 0xff];
        let scan = PROVISIONAL.scan(&answer);
        // Bit 15 is (1, 5); bit 50 is past the matrix, as is byte 7.
        assert_eq!(scan, Scan::of(&[(1, 5)]));
        assert_eq!(scan_line(190, scan), "KB 190 00800000000000");
    }

    #[test]
    fn a_code_table_maps_each_code_to_its_position() {
        const CODES: [(u8, u8); 2] = [(0x21, 7), (0x48, 49)];
        let protocol = Protocol {
            decode: Decode::Codes(&CODES),
            len: 3,
            ..PROVISIONAL
        };
        assert_eq!(
            protocol.scan(&[0x48, 0x21, 0x00]),
            Scan::of(&[(0, 7), (4, 9)])
        );
        assert_eq!(
            protocol.scan(&[0x99, 0, 0]),
            Scan::NONE,
            "an unknown code is no key"
        );
    }

    #[test]
    fn a_capture_replays_through_another_table() {
        let capture = "[FW_BUILD] git_sha=x t=1\r\n\
                       KBRAW 10 0000000000000000\r\n\
                       KB 10 00000000000000\r\n\
                       KBRAW 20 0080000000000000\r\n\
                       KBRAW 30 zz\r\n";
        let answers: Vec<_> = parse_raw(capture).collect();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[2], Err(5));
        let (ms, bytes) = answers[1].clone().expect("the second answer reads");
        assert_eq!((ms, PROVISIONAL.scan(&bytes)), (20, Scan::of(&[(1, 5)])));
        // And the scan line the board prints reads as slice three's format.
        let line = scan_line(ms, PROVISIONAL.scan(&bytes));
        let read: Vec<_> = parse_recording(&line).collect();
        assert_eq!(read, [Ok((20, Scan::of(&[(1, 5)])))]);
    }

    #[test]
    fn a_raw_line_is_the_answer_in_hex() {
        assert_eq!(raw_line(7, &[0xab, 0x01]), "KBRAW 7 ab01");
    }
}
