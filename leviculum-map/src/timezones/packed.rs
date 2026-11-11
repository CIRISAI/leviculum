//! Android's packed tzdata: every zone's TZif file in one file, found at
//! `/apex/com.android.tzdata/etc/tz/tzdata`, falling back to
//! `/system/usr/share/zoneinfo/tzdata`.
//!
//! The layout, all integers big-endian, as bionic reads it
//! (`platform/bionic`, `libc/tzcode/bionic.cpp`, `bionic_tzdata_header_t`
//! and `index_entry_t`, main at 731631f3) and as libcore does
//! (`platform/external/icu`,
//! `android_icu4j/libcore_bridge/src/java/com/android/i18n/timezone/ZoneInfoDb.java`,
//! `readHeader` and `readIndex`, main at 474ba983):
//!
//! ```text
//! u8[12]  "tzdata2026b\0": "tzdata", the version, a NUL
//! i32     index offset, from the start of the file
//! i32     data offset
//! i32     final offset (unused here)
//! at the index offset, up to the data offset, 52-byte entries:
//!     u8[40]  the zone's name, NUL padded
//!     i32     start of its TZif bytes, from the data offset
//!     i32     their length
//!     i32     unused (the raw offset before tzdata2014f)
//! ```

/// The header's length.
pub const PACKED_HEADER: usize = 24;
/// An index entry's length.
pub const PACKED_ENTRY: usize = 52;
/// The name's room in an entry.
const NAME: usize = 40;

fn be32(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at + 4)?;
    usize::try_from(i32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok()
}

/// The index offset and the data offset of a header; `None` for one that
/// is not a packed tzdata header.
pub fn packed_header(head: &[u8]) -> Option<(usize, usize)> {
    let magic = head.get(..12)?;
    if &magic[..6] != b"tzdata" || magic[11] != 0 {
        return None;
    }
    let (index, data) = (be32(head, 12)?, be32(head, 16)?);
    (index >= PACKED_HEADER && index <= data && (data - index) % PACKED_ENTRY == 0)
        .then_some((index, data))
}

/// The version the header names, `2026b`.
pub fn packed_version(packed: &[u8]) -> Option<&str> {
    packed_header(packed)?;
    core::str::from_utf8(packed.get(6..11)?).ok()
}

/// The start, from the data offset, and the length of `name`'s TZif bytes
/// in `index`, the bytes from the index offset to the data offset.
pub fn packed_find(index: &[u8], name: &str) -> Option<(usize, usize)> {
    if name.is_empty() || name.len() > NAME {
        return None;
    }
    index.chunks_exact(PACKED_ENTRY).find_map(|e| {
        let len = e[..NAME].iter().position(|&c| c == 0).unwrap_or(NAME);
        if &e[..len] != name.as_bytes() {
            return None;
        }
        Some((be32(e, NAME)?, be32(e, NAME + 4)?))
    })
}

/// `name`'s TZif bytes in the packed file `packed`.
pub fn packed_zone<'a>(packed: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let (index, data) = packed_header(packed)?;
    let (start, len) = packed_find(packed.get(index..data)?, name)?;
    let at = data.checked_add(start)?;
    packed.get(at..at.checked_add(len)?)
}
