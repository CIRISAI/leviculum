//! The keyboard controller's answer (slice four, 1361; measured in 1427):
//! what the board reads over I2C, logged raw, and what one answer says.
//!
//! The firmware writes [`Protocol::register`], reads [`Protocol::len`]
//! bytes, and prints every answer as a `KBRAW <ms> <hex>` line before it
//! reads anything into it, so a capture replays on the host through
//! [`parse_raw`]. The capture of 2026-11-04 (`thinknode-m9.md`, "The
//! keyboard controller, measured"; `tests/keyboard/capture-20261104.txt`)
//! read the protocol: an idle answer is all zeros, a press is one answer
//! with one key code in every byte ([`Protocol::read`]), and the code is
//! the driver's ([`crate::keyboard::Key::of`]).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The longest answer the firmware reads in one poll.
pub const MAX_ANSWER: usize = 16;

/// What the firmware asks the controller each poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    /// The controller's address: 0x6D on V1.1, 0x6C on V1.0.
    pub address: u8,
    /// The register written before the read; none reads without a write.
    pub register: Option<u8>,
    /// How many bytes it reads, at most [`MAX_ANSWER`].
    pub len: usize,
}

/// The protocol the capture was taken with and read: register 0x01 of
/// 0x6D, eight bytes. Every byte of an answer carried the same code, so
/// one byte would do; whether a shorter read or register 0x00 or 0x02
/// answers the same is a bench question, and until it is asked the board
/// keeps the read the capture proved, and [`Protocol::read`] holds the
/// eight bytes to agreeing.
pub const MEASURED: Protocol = Protocol {
    address: 0x6D,
    register: Some(0x01),
    len: 8,
};

/// What one answer says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// All zeros: no key since the last read.
    Idle,
    /// One press: the key code every byte carried.
    Code(u8),
    /// The bytes disagree, which the capture never saw: no key is taken
    /// from it, the firmware says so, and the `KBRAW` line keeps it.
    Mixed,
}

impl Protocol {
    /// What `answer` says: idle, one code, or bytes that disagree.
    pub fn read(&self, answer: &[u8]) -> Read {
        match answer.split_first() {
            None | Some((0, [])) => Read::Idle,
            Some((code, rest)) if rest.iter().all(|b| b == code) => match code {
                0 => Read::Idle,
                code => Read::Code(*code),
            },
            Some(_) => Read::Mixed,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Key;

    const CAPTURE: &str = include_str!("../tests/keyboard/capture-20261104.txt");
    const DISTINCT: &str = include_str!("../tests/keyboard/distinct-20261104.txt");

    /// The key names of `capture`'s answers, in order, idle reads skipped.
    fn names(capture: &str) -> Vec<String> {
        parse_raw(capture)
            .map(|answer| answer.expect("every captured line reads"))
            .filter_map(|(_, bytes)| match MEASURED.read(&bytes) {
                Read::Idle => None,
                Read::Code(code) => Some(Key::of(code).expect("a non-zero code").name()),
                Read::Mixed => panic!("the capture has no mixed answer"),
            })
            .collect()
    }

    /// The first 60 presses of the bench capture, through the measured
    /// protocol and the driver, name the keys Lew pressed: the top letter
    /// row, the home row backwards, the bottom row, then the keys that
    /// are no character, which stay `special_` until the walk.
    #[test]
    fn the_bench_capture_replays_to_its_key_names() {
        let names = names(CAPTURE);
        let want = "q w e special_0x83 r t y special_0xb6 u enter i o p special_0x84 \
                    i u y t g f d s z x c v b n m m j h g t \
                    special_0x81 special_0x83 special_0x82 \
                    special_0x86 special_0x86 special_0x86 special_0x86 \
                    special_0xb7 special_0xb7 special_0xb6 special_0xb4 special_0xb5 \
                    special_0x86 special_0x85 special_0x84 special_0x86 e d \
                    special_0x83 special_0x82 special_0x83 space special_0x90 \
                    special_0x83 special_0x86 special_0x86";
        assert_eq!(names.join(" "), want);
        assert_eq!(names.len(), 60);
    }

    /// Every distinct answer of the capture is one code in all eight
    /// bytes, and the "state byte" the matrix decoder's `KB` line showed
    /// last was no state: it is that code's two low bits, the two matrix
    /// positions (48, 49) the bitmap table read from byte 6.
    #[test]
    fn every_captured_answer_is_one_code_and_the_kb_state_byte_was_its_low_bits() {
        let mut seen = 0;
        for line in DISTINCT.lines().filter(|l| !l.starts_with('#')) {
            let mut words = line.split_whitespace();
            let (Some(raw), Some(_count), Some(kb)) = (words.next(), words.next(), words.next())
            else {
                panic!("a distinct line has three words: {line}");
            };
            let bytes: Vec<u8> = (0..raw.len() / 2)
                .map(|i| u8::from_str_radix(&raw[2 * i..2 * i + 2], 16).expect("hex"))
                .collect();
            let Read::Code(code) = MEASURED.read(&bytes) else {
                panic!("{raw} is one code");
            };
            let last = u8::from_str_radix(&kb[12..14], 16).expect("hex");
            assert_eq!(last, code & 0x03, "{raw} -> {kb}");
            seen += 1;
        }
        assert_eq!(seen, 42, "the capture's 42 distinct codes");
    }

    #[test]
    fn an_answer_reads_idle_one_code_or_mixed() {
        assert_eq!(MEASURED.read(&[0; 8]), Read::Idle);
        assert_eq!(MEASURED.read(&[]), Read::Idle);
        assert_eq!(MEASURED.read(&[0x6b; 8]), Read::Code(0x6b));
        assert_eq!(MEASURED.read(&[0x6b]), Read::Code(0x6b));
        // The positive control: one byte unlike the others is no key.
        let mut answer = [0x6b; 8];
        answer[7] = 0x6c;
        assert_eq!(MEASURED.read(&answer), Read::Mixed);
        answer = [0; 8];
        answer[3] = 0x20;
        assert_eq!(MEASURED.read(&answer), Read::Mixed);
    }

    #[test]
    fn a_capture_line_that_does_not_read_is_named_by_number() {
        let capture = "[FW_BUILD] git_sha=x t=1\r\n\
                       KBRAW 10 0000000000000000\r\n\
                       KEY 20 code=0x6b name=k\r\n\
                       KBRAW 20 6b6b6b6b6b6b6b6b\r\n\
                       KBRAW 30 zz\r\n";
        let answers: Vec<_> = parse_raw(capture).collect();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[2], Err(5));
        assert_eq!(answers[1], Ok((20, alloc::vec![0x6b; 8])));
    }

    #[test]
    fn a_raw_line_is_the_answer_in_hex() {
        assert_eq!(raw_line(7, &[0xab, 0x01]), "KBRAW 7 ab01");
    }
}
