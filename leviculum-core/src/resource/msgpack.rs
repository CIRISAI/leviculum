//! Hand-rolled msgpack encode/decode helpers for the Resource protocol.
//!
//! These handle only the exact msgpack types needed by ResourceAdvertisement
//! serialization. Not a general-purpose msgpack library.
//!
//! Pattern follows `destination.rs` module-private helpers, extended with
//! integer encoding and map support needed by Resource.

use alloc::vec::Vec;

// Encoding helpers
/// Write a fixmap header (count ≤ 15).
pub(crate) fn write_fixmap_header(buf: &mut Vec<u8>, count: u8) {
    debug_assert!(count <= 15);
    buf.push(0x80 | count);
}

/// Write a fixstr (len ≤ 31).
pub(crate) fn write_fixstr(buf: &mut Vec<u8>, s: &str) {
    let len = s.len();
    debug_assert!(len <= 31);
    buf.push(0xa0 | (len as u8));
    buf.extend_from_slice(s.as_bytes());
}

/// Write binary data as bin8 or bin16.
pub(crate) fn write_bin(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len();
    if len <= 0xFF {
        buf.push(0xc4); // bin8
        buf.push(len as u8);
    } else if len <= 0xFFFF {
        buf.push(0xc5); // bin16
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xc6); // bin32
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(data);
}

/// Write an unsigned integer in compact msgpack format (Python-compatible).
///
/// Encoding rules:
/// - 0..=127: positive fixint (1 byte)
/// - 128..=255: uint8 (0xcc + 1 byte)
/// - 256..=65535: uint16 (0xcd + 2 bytes BE)
/// - 65536..=4294967295: uint32 (0xce + 4 bytes BE)
/// - larger: uint64 (0xcf + 8 bytes BE)
pub(crate) fn write_uint(buf: &mut Vec<u8>, val: u64) {
    if val <= 127 {
        buf.push(val as u8);
    } else if val <= 255 {
        buf.push(0xcc);
        buf.push(val as u8);
    } else if val <= 65535 {
        buf.push(0xcd);
        buf.extend_from_slice(&(val as u16).to_be_bytes());
    } else if val <= 4_294_967_295 {
        buf.push(0xce);
        buf.extend_from_slice(&(val as u32).to_be_bytes());
    } else {
        buf.push(0xcf);
        buf.extend_from_slice(&val.to_be_bytes());
    }
}

/// Write nil (0xc0).
pub(crate) fn write_nil(buf: &mut Vec<u8>) {
    buf.push(0xc0);
}

/// Write a fixarray header (count ≤ 15).
pub(crate) fn write_fixarray_header(buf: &mut Vec<u8>, count: usize) {
    debug_assert!(count <= 15);
    buf.push(0x90 | (count as u8));
}

/// Write a float64 value.
pub(crate) fn write_float64(buf: &mut Vec<u8>, val: f64) {
    buf.push(0xcb); // float64 tag
    buf.extend_from_slice(&val.to_be_bytes());
}

/// Write a boolean value.
#[cfg(test)]
pub(crate) fn write_bool(buf: &mut Vec<u8>, val: bool) {
    buf.push(if val { 0xc3 } else { 0xc2 });
}

// Decoding helpers
/// Read one byte, advancing position.
pub(crate) fn read_byte(data: &[u8], pos: &mut usize) -> Option<u8> {
    let b = *data.get(*pos)?;
    *pos += 1;
    Some(b)
}

/// Read a big-endian u16, advancing position.
pub(crate) fn read_be_u16(data: &[u8], pos: &mut usize) -> Option<u16> {
    if *pos + 2 > data.len() {
        return None;
    }
    let val = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Some(val)
}

/// Read a big-endian u32, advancing position.
pub(crate) fn read_be_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    if *pos + 4 > data.len() {
        return None;
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Some(val)
}

/// Read a big-endian u64, advancing position.
fn read_be_u64(data: &[u8], pos: &mut usize) -> Option<u64> {
    if *pos + 8 > data.len() {
        return None;
    }
    let val = u64::from_be_bytes([
        data[*pos],
        data[*pos + 1],
        data[*pos + 2],
        data[*pos + 3],
        data[*pos + 4],
        data[*pos + 5],
        data[*pos + 6],
        data[*pos + 7],
    ]);
    *pos += 8;
    Some(val)
}

/// Read a msgpack float64 (or float32, promoted).
pub(crate) fn read_float64(data: &[u8], pos: &mut usize) -> Option<f64> {
    let tag = read_byte(data, pos)?;
    match tag {
        0xcb => {
            // float64
            if *pos + 8 > data.len() {
                return None;
            }
            let val = f64::from_be_bytes([
                data[*pos],
                data[*pos + 1],
                data[*pos + 2],
                data[*pos + 3],
                data[*pos + 4],
                data[*pos + 5],
                data[*pos + 6],
                data[*pos + 7],
            ]);
            *pos += 8;
            Some(val)
        }
        0xca => {
            // float32 (Python could send this)
            if *pos + 4 > data.len() {
                return None;
            }
            let val =
                f32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
            *pos += 4;
            Some(val as f64)
        }
        _ => None,
    }
}

/// Read a msgpack boolean.
#[cfg(test)]
pub(crate) fn read_bool(data: &[u8], pos: &mut usize) -> Option<bool> {
    let tag = read_byte(data, pos)?;
    match tag {
        0xc2 => Some(false),
        0xc3 => Some(true),
        _ => None,
    }
}

/// Read one complete msgpack value as raw bytes (for opaque data passthrough).
pub(crate) fn read_msgpack_raw_value<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let start = *pos;
    skip_msgpack_value(data, pos)?;
    Some(&data[start..*pos])
}

/// Read a msgpack string (fixstr, str8, str16).
pub(crate) fn read_msgpack_str<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let tag = read_byte(data, pos)?;
    let len = if tag & 0xe0 == 0xa0 {
        // fixstr: 0xa0..0xbf, length in lower 5 bits
        (tag & 0x1f) as usize
    } else if tag == 0xd9 {
        // str8
        read_byte(data, pos)? as usize
    } else if tag == 0xda {
        // str16
        read_be_u16(data, pos)? as usize
    } else {
        return None;
    };

    if *pos + len > data.len() {
        return None;
    }
    let s = &data[*pos..*pos + len];
    *pos += len;
    Some(s)
}

/// Read a msgpack binary (bin8, bin16, bin32).
pub(crate) fn read_msgpack_bin<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let tag = read_byte(data, pos)?;
    let len = if tag == 0xc4 {
        // bin8
        read_byte(data, pos)? as usize
    } else if tag == 0xc5 {
        // bin16
        read_be_u16(data, pos)? as usize
    } else if tag == 0xc6 {
        // bin32
        read_be_u32(data, pos)? as usize
    } else {
        return None;
    };

    if *pos + len > data.len() {
        return None;
    }
    let b = &data[*pos..*pos + len];
    *pos += len;
    Some(b)
}

/// Read a msgpack unsigned integer (fixint, uint8, uint16, uint32, uint64).
pub(crate) fn read_msgpack_uint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let tag = read_byte(data, pos)?;
    if tag <= 0x7f {
        // positive fixint
        Some(tag as u64)
    } else if tag == 0xcc {
        // uint8
        Some(read_byte(data, pos)? as u64)
    } else if tag == 0xcd {
        // uint16
        Some(read_be_u16(data, pos)? as u64)
    } else if tag == 0xce {
        // uint32
        Some(read_be_u32(data, pos)? as u64)
    } else if tag == 0xcf {
        // uint64
        read_be_u64(data, pos)
    } else {
        None
    }
}

/// Read a msgpack binary or nil value.
///
/// Returns `Some(Some(bytes))` for binary, `Some(None)` for nil, `None` for error.
pub(crate) fn read_msgpack_bin_or_nil<'a>(
    data: &'a [u8],
    pos: &mut usize,
) -> Option<Option<&'a [u8]>> {
    let saved_pos = *pos;
    let tag = read_byte(data, pos)?;
    if tag == 0xc0 {
        // nil
        return Some(None);
    }
    // Restore position and try reading as bin
    *pos = saved_pos;
    Some(Some(read_msgpack_bin(data, pos)?))
}

/// Read a fixmap header, returning the entry count.
pub(crate) fn read_fixmap_len(data: &[u8], pos: &mut usize) -> Option<usize> {
    let tag = read_byte(data, pos)?;
    if tag & 0xf0 == 0x80 {
        Some((tag & 0x0f) as usize)
    } else {
        None
    }
}

/// Read a fixarray header, returning the element count.
pub(crate) fn read_fixarray_len(data: &[u8], pos: &mut usize) -> Option<usize> {
    let tag = read_byte(data, pos)?;
    if tag & 0xf0 == 0x90 {
        Some((tag & 0x0f) as usize)
    } else {
        None
    }
}

/// Skip a single msgpack value at the current position.
///
/// Needed for forward-compatibility: unknown keys in a map are skipped.
pub(crate) fn skip_msgpack_value(data: &[u8], pos: &mut usize) -> Option<()> {
    let tag = read_byte(data, pos)?;

    match tag {
        // nil, false, true
        0xc0 | 0xc2 | 0xc3 => Some(()),
        // positive fixint
        0x00..=0x7f => Some(()),
        // negative fixint
        0xe0..=0xff => Some(()),
        // uint8 / int8
        0xcc | 0xd0 => {
            *pos += 1;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // uint16 / int16
        0xcd | 0xd1 => {
            *pos += 2;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // uint32 / int32 / float32
        0xce | 0xd2 | 0xca => {
            *pos += 4;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // uint64 / int64 / float64
        0xcf | 0xd3 | 0xcb => {
            *pos += 8;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // fixstr
        tag if tag & 0xe0 == 0xa0 => {
            let len = (tag & 0x1f) as usize;
            *pos += len;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // str8
        0xd9 => {
            let len = read_byte(data, pos)? as usize;
            *pos += len;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // str16
        0xda => {
            let len = read_be_u16(data, pos)? as usize;
            *pos += len;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // bin8
        0xc4 => {
            let len = read_byte(data, pos)? as usize;
            *pos += len;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // bin16
        0xc5 => {
            let len = read_be_u16(data, pos)? as usize;
            *pos += len;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // bin32
        0xc6 => {
            let len = read_be_u32(data, pos)? as usize;
            *pos += len;
            if *pos > data.len() {
                None
            } else {
                Some(())
            }
        }
        // fixarray
        tag if tag & 0xf0 == 0x90 => {
            let count = (tag & 0x0f) as usize;
            for _ in 0..count {
                skip_msgpack_value(data, pos)?;
            }
            Some(())
        }
        // fixmap
        tag if tag & 0xf0 == 0x80 => {
            let count = (tag & 0x0f) as usize;
            for _ in 0..count {
                skip_msgpack_value(data, pos)?; // key
                skip_msgpack_value(data, pos)?; // value
            }
            Some(())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uint_encoding_fixint() {
        let mut buf = Vec::new();
        write_uint(&mut buf, 0);
        assert_eq!(buf, [0x00]);

        buf.clear();
        write_uint(&mut buf, 42);
        assert_eq!(buf, [42]);

        buf.clear();
        write_uint(&mut buf, 127);
        assert_eq!(buf, [127]);
    }

    #[test]
    fn test_uint_encoding_uint8() {
        let mut buf = Vec::new();
        write_uint(&mut buf, 128);
        assert_eq!(buf, [0xcc, 128]);

        buf.clear();
        write_uint(&mut buf, 255);
        assert_eq!(buf, [0xcc, 255]);
    }

    #[test]
    fn test_uint_encoding_uint16() {
        let mut buf = Vec::new();
        write_uint(&mut buf, 256);
        assert_eq!(buf, [0xcd, 0x01, 0x00]);

        buf.clear();
        write_uint(&mut buf, 65535);
        assert_eq!(buf, [0xcd, 0xFF, 0xFF]);
    }

    #[test]
    fn test_uint_encoding_uint32() {
        let mut buf = Vec::new();
        write_uint(&mut buf, 65536);
        assert_eq!(buf, [0xce, 0x00, 0x01, 0x00, 0x00]);

        buf.clear();
        write_uint(&mut buf, 4_294_967_295);
        assert_eq!(buf, [0xce, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_uint_encoding_uint64() {
        let mut buf = Vec::new();
        write_uint(&mut buf, 4_294_967_296);
        assert_eq!(buf, [0xcf, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);

        buf.clear();
        write_uint(&mut buf, u64::MAX);
        assert_eq!(buf, [0xcf, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_uint_roundtrip() {
        let test_values: &[u64] = &[
            0,
            1,
            42,
            127,
            128,
            255,
            256,
            65535,
            65536,
            4_294_967_295,
            4_294_967_296,
            u64::MAX,
        ];
        for &val in test_values {
            let mut buf = Vec::new();
            write_uint(&mut buf, val);
            let mut pos = 0;
            let decoded = read_msgpack_uint(&buf, &mut pos).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {val}");
            assert_eq!(pos, buf.len(), "position mismatch for {val}");
        }
    }

    #[test]
    fn test_nil_roundtrip() {
        let mut buf = Vec::new();
        write_nil(&mut buf);
        assert_eq!(buf, [0xc0]);

        let mut pos = 0;
        let result = read_msgpack_bin_or_nil(&buf, &mut pos).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_bin_roundtrip() {
        let data = b"hello world";
        let mut buf = Vec::new();
        write_bin(&mut buf, data);

        let mut pos = 0;
        let decoded = read_msgpack_bin(&buf, &mut pos).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_bin_or_nil_with_bin() {
        let data = b"test";
        let mut buf = Vec::new();
        write_bin(&mut buf, data);

        let mut pos = 0;
        let result = read_msgpack_bin_or_nil(&buf, &mut pos).unwrap();
        assert_eq!(result, Some(data.as_slice()));
    }

    #[test]
    fn test_fixstr_roundtrip() {
        let mut buf = Vec::new();
        write_fixstr(&mut buf, "t");
        assert_eq!(buf, [0xa1, b't']);

        let mut pos = 0;
        let decoded = read_msgpack_str(&buf, &mut pos).unwrap();
        assert_eq!(decoded, b"t");
    }

    #[test]
    fn test_fixmap_header() {
        let mut buf = Vec::new();
        write_fixmap_header(&mut buf, 11);
        assert_eq!(buf, [0x8b]);

        let mut pos = 0;
        let count = read_fixmap_len(&buf, &mut pos).unwrap();
        assert_eq!(count, 11);
    }

    #[test]
    fn test_fixarray_header() {
        let mut buf = Vec::new();
        write_fixarray_header(&mut buf, 2);
        assert_eq!(buf, [0x92]);

        let mut pos = 0;
        let count = read_fixarray_len(&buf, &mut pos).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_fixarray_roundtrip() {
        // Build a fixarray [42, bin8("hello")]
        let mut buf = Vec::new();
        write_fixarray_header(&mut buf, 2);
        write_uint(&mut buf, 42);
        write_bin(&mut buf, b"hello");

        let mut pos = 0;
        let count = read_fixarray_len(&buf, &mut pos).unwrap();
        assert_eq!(count, 2);
        let val = read_msgpack_uint(&buf, &mut pos).unwrap();
        assert_eq!(val, 42);
        let bin = read_msgpack_bin(&buf, &mut pos).unwrap();
        assert_eq!(bin, b"hello");
        assert_eq!(pos, buf.len());
    }

    #[test]
    fn test_skip_various_types() {
        // Build a buffer with various types
        let mut buf = Vec::new();
        write_uint(&mut buf, 42); // fixint
        write_uint(&mut buf, 300); // uint16
        write_nil(&mut buf); // nil
        write_bin(&mut buf, b"data"); // bin8
        write_fixstr(&mut buf, "key"); // fixstr

        let mut pos = 0;
        skip_msgpack_value(&buf, &mut pos).unwrap(); // skip fixint
        skip_msgpack_value(&buf, &mut pos).unwrap(); // skip uint16
        skip_msgpack_value(&buf, &mut pos).unwrap(); // skip nil
        skip_msgpack_value(&buf, &mut pos).unwrap(); // skip bin8
        skip_msgpack_value(&buf, &mut pos).unwrap(); // skip fixstr
        assert_eq!(pos, buf.len());
    }

    #[test]
    fn test_bin_size_variants() {
        // bin8 (len <= 255)
        let data_small = [0u8; 100];
        let mut buf = Vec::new();
        write_bin(&mut buf, &data_small);
        assert_eq!(buf[0], 0xc4); // bin8
        let mut pos = 0;
        let decoded = read_msgpack_bin(&buf, &mut pos).unwrap();
        assert_eq!(decoded.len(), 100);

        // bin16 (len > 255)
        let data_medium = [0u8; 300];
        buf.clear();
        write_bin(&mut buf, &data_medium);
        assert_eq!(buf[0], 0xc5); // bin16
        pos = 0;
        let decoded = read_msgpack_bin(&buf, &mut pos).unwrap();
        assert_eq!(decoded.len(), 300);
    }

    #[test]
    fn test_float64_roundtrip() {
        let test_values: &[f64] = &[0.0, 1.0, -1.0, 2.5, 1_700_000_000.123, f64::MAX, f64::MIN];
        for &val in test_values {
            let mut buf = Vec::new();
            write_float64(&mut buf, val);
            let mut pos = 0;
            let decoded = read_float64(&buf, &mut pos).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {val}");
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn test_float32_promotion() {
        // Manually encode a float32 value and verify read_float64 promotes it
        let val: f32 = 1.5;
        let mut buf = Vec::new();
        buf.push(0xca); // float32 tag
        buf.extend_from_slice(&val.to_be_bytes());
        let mut pos = 0;
        let decoded = read_float64(&buf, &mut pos).unwrap();
        assert!((decoded - val as f64).abs() < 1e-6);
        assert_eq!(pos, buf.len());
    }

    #[test]
    fn test_bool_roundtrip() {
        for &val in &[true, false] {
            let mut buf = Vec::new();
            write_bool(&mut buf, val);
            let mut pos = 0;
            let decoded = read_bool(&buf, &mut pos).unwrap();
            assert_eq!(decoded, val);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn test_read_msgpack_raw_value() {
        // Build a buffer with multiple values
        let mut buf = Vec::new();
        write_uint(&mut buf, 42); // fixint
        write_bin(&mut buf, b"hello"); // bin8

        // Read first raw value (fixint 42 = single byte)
        let mut pos = 0;
        let raw1 = read_msgpack_raw_value(&buf, &mut pos).unwrap();
        assert_eq!(raw1, &[42]);

        // Read second raw value (bin8 "hello")
        let raw2 = read_msgpack_raw_value(&buf, &mut pos).unwrap();
        let mut expected = Vec::new();
        write_bin(&mut expected, b"hello");
        assert_eq!(raw2, expected.as_slice());
        assert_eq!(pos, buf.len());
    }
}
