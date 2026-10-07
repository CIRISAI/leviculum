//! Lowercase hex rendering of byte strings, the one implementation behind
//! every hash the stack prints: `HexFmt` and `HexShort` in log fields,
//! the `Display` of `DestinationHash` and `LinkId`, and the host's own
//! helpers in `leviculum-std`.
//!
//! [`write_hex`] encodes through a stack buffer and hands the writer whole
//! chunks. The per-byte `write!(f, "{b:02x}")` it replaces went through the
//! formatting machinery twice a byte, and when the writer was a `String`
//! (every structured event field is rendered into one) each of those was a
//! separate `write_str` that could grow it: 508 measured that path as the
//! largest single allocation site of a node with the event log on.

use core::fmt;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Bytes encoded per `write_str` call: 64 hex digits on the stack, which
/// covers a full 32-byte hash in one call.
const CHUNK: usize = 32;

/// Write `bytes` to `w` as lowercase hex, two digits a byte, no separator.
///
/// Byte-for-byte the output of `write!(w, "{b:02x}")` over every byte, in
/// at most one `write_str` per 32 input bytes.
pub fn write_hex<W: fmt::Write + ?Sized>(w: &mut W, bytes: &[u8]) -> fmt::Result {
    let mut buf = [0u8; 2 * CHUNK];
    for chunk in bytes.chunks(CHUNK) {
        for (i, b) in chunk.iter().enumerate() {
            buf[2 * i] = DIGITS[usize::from(b >> 4)];
            buf[2 * i + 1] = DIGITS[usize::from(b & 0x0f)];
        }
        let digits = &buf[..2 * chunk.len()];
        // Every byte is an ASCII hex digit, so this never fails; mapping
        // the impossible error to fmt::Error keeps the function total.
        let text = core::str::from_utf8(digits).map_err(|_| fmt::Error)?;
        w.write_str(text)?;
    }
    Ok(())
}

/// no_std-compatible hex formatter for tracing output.
pub(crate) struct HexFmt<'a>(pub &'a [u8]);

impl fmt::Display for HexFmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, self.0)
    }
}

/// Hex formatter for hashes in log output: shows the full truncated hash
/// (16 bytes = 32 hex chars for destination hashes).
///
/// Matches Python Reticulum's log format where hashes are shown in full
/// so they can be copy-pasted into tools like `rnprobe` and `rnpath`.
pub(crate) struct HexShort<'a>(pub &'a [u8]);

impl fmt::Display for HexShort<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{write_hex, HexShort};
    use alloc::string::String;
    use core::fmt::Write as _;

    /// The rendering the shared function replaced, kept as the oracle.
    fn per_byte(bytes: &[u8]) -> String {
        let mut s = String::new();
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    #[test]
    fn matches_the_per_byte_rendering_at_every_length_and_byte() {
        let all: alloc::vec::Vec<u8> = (0..=255u8).collect();
        // Every byte value, and every length across the chunk boundaries.
        for len in [0, 1, 2, 15, 16, 31, 32, 33, 63, 64, 65, 200, 256] {
            let bytes = &all[..len];
            let mut s = String::new();
            write_hex(&mut s, bytes).unwrap();
            assert_eq!(s, per_byte(bytes), "len {len}");
        }
    }

    #[test]
    fn display_wrappers_render_through_it() {
        let h = [0x37u8, 0x3e, 0x00, 0xff];
        assert_eq!(alloc::format!("{}", HexShort(&h)), "373e00ff");
    }
}
