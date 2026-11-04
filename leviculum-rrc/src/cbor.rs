//! The CBOR subset an RRC envelope uses (RFC 8949), written here rather
//! than taken from a crate: the workspace had none, and an envelope needs
//! two things a general crate makes awkward, maps that keep the order they
//! came in (WELCOME's capabilities arrive as keys 1, 2, 0 and a PONG hands
//! the hub's body back as it came) and heads that are always the shortest,
//! which is what the reference hub writes and what the vectors hold.
//!
//! Decoded: unsigned and negative integers, byte and text strings, arrays,
//! maps, `false`, `true`, `null`, `undefined`, and floats of 16, 32 and 64
//! bits. Refused: indefinite lengths, tags, other simple values, text that
//! is not UTF-8, a length past the input, nesting deeper than
//! [`MAX_DEPTH`], and bytes after the item. Encoded: the same, with the
//! shortest head, a float as the width it was decoded with.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// How deep arrays and maps may nest. An envelope nests three deep
/// (envelope, WELCOME body, its limits); the bound keeps a hostile input
/// from walking the stack.
pub const MAX_DEPTH: usize = 16;

/// One CBOR item.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Uint(u64),
    /// A negative integer, `-1 - n`.
    Nint(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    /// Pairs in the order they were decoded or built.
    Map(Vec<(Value, Value)>),
    Bool(bool),
    Null,
    Undefined,
    Float(Float),
}

/// A float and the width it travels in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Float {
    /// Half precision, kept as its bits.
    F16(u16),
    F32(f32),
    F64(f64),
}

impl Float {
    /// The value as an `f64`.
    pub fn to_f64(self) -> f64 {
        match self {
            Float::F16(bits) => half_to_f64(bits),
            Float::F32(v) => f64::from(v),
            Float::F64(v) => v,
        }
    }
}

/// Why bytes are not one CBOR item of the subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended inside an item.
    Truncated,
    /// Bytes are left after the item.
    Trailing,
    /// An indefinite length, a tag, or a simple value outside the subset.
    Unsupported(u8),
    /// A text string that is not UTF-8.
    BadText,
    /// Arrays and maps nest deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("the input ends inside an item"),
            DecodeError::Trailing => f.write_str("bytes follow the item"),
            DecodeError::Unsupported(b) => write!(f, "initial byte {b:#04x} is outside the subset"),
            DecodeError::BadText => f.write_str("a text string is not UTF-8"),
            DecodeError::TooDeep => f.write_str("items nest too deep"),
        }
    }
}

impl Value {
    /// Decode exactly one item from `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Value, DecodeError> {
        let mut reader = Reader { bytes, at: 0 };
        let value = reader.item(0)?;
        if reader.at != bytes.len() {
            return Err(DecodeError::Trailing);
        }
        Ok(value)
    }

    /// Encode the item with the shortest heads.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Append the item's encoding to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Value::Uint(n) => head(out, 0, *n),
            Value::Nint(n) => head(out, 1, *n),
            Value::Bytes(b) => {
                head(out, 2, b.len() as u64);
                out.extend_from_slice(b);
            }
            Value::Text(s) => {
                head(out, 3, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            Value::Array(items) => {
                head(out, 4, items.len() as u64);
                items.iter().for_each(|item| item.encode_into(out));
            }
            Value::Map(pairs) => {
                head(out, 5, pairs.len() as u64);
                for (k, v) in pairs {
                    k.encode_into(out);
                    v.encode_into(out);
                }
            }
            Value::Bool(false) => out.push(0xf4),
            Value::Bool(true) => out.push(0xf5),
            Value::Null => out.push(0xf6),
            Value::Undefined => out.push(0xf7),
            Value::Float(Float::F16(bits)) => {
                out.push(0xf9);
                out.extend_from_slice(&bits.to_be_bytes());
            }
            Value::Float(Float::F32(v)) => {
                out.push(0xfa);
                out.extend_from_slice(&v.to_bits().to_be_bytes());
            }
            Value::Float(Float::F64(v)) => {
                out.push(0xfb);
                out.extend_from_slice(&v.to_bits().to_be_bytes());
            }
        }
    }

    /// The value under integer key `key` of a map.
    pub fn get(&self, key: u64) -> Option<&Value> {
        match self {
            Value::Map(pairs) => pairs
                .iter()
                .find(|(k, _)| *k == Value::Uint(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Value::Uint(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

/// Write a head of major type `major` with argument `n`, shortest form.
fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    if n < 24 {
        out.push(m | n as u8);
    } else if n <= u64::from(u8::MAX) {
        out.extend_from_slice(&[m | 24, n as u8]);
    } else if n <= u64::from(u16::MAX) {
        out.push(m | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= u64::from(u32::MAX) {
        out.push(m | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at.checked_add(n).ok_or(DecodeError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(DecodeError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn uint(&mut self, width: usize) -> Result<u64, DecodeError> {
        Ok(self
            .take(width)?
            .iter()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
    }

    /// The argument of a head whose low five bits are `info`.
    fn argument(&mut self, initial: u8) -> Result<u64, DecodeError> {
        match initial & 0x1f {
            n @ 0..=23 => Ok(u64::from(n)),
            24 => self.uint(1),
            25 => self.uint(2),
            26 => self.uint(4),
            27 => self.uint(8),
            _ => Err(DecodeError::Unsupported(initial)),
        }
    }

    /// A length that cannot claim more than what is left: one byte per
    /// item at least, so an array of 2^60 empty slots is refused before
    /// anything is allocated for it.
    fn length(&mut self, initial: u8) -> Result<usize, DecodeError> {
        let n = self.argument(initial)?;
        let left = (self.bytes.len() - self.at) as u64;
        if n > left {
            return Err(DecodeError::Truncated);
        }
        Ok(n as usize)
    }

    fn item(&mut self, depth: usize) -> Result<Value, DecodeError> {
        let initial = self.byte()?;
        match initial >> 5 {
            0 => Ok(Value::Uint(self.argument(initial)?)),
            1 => Ok(Value::Nint(self.argument(initial)?)),
            2 => {
                let n = self.length(initial)?;
                Ok(Value::Bytes(self.take(n)?.to_vec()))
            }
            3 => {
                let n = self.length(initial)?;
                let raw = self.take(n)?;
                let text = core::str::from_utf8(raw).map_err(|_| DecodeError::BadText)?;
                Ok(Value::Text(String::from(text)))
            }
            4 => {
                let n = self.length(initial)?;
                let depth = deeper(depth)?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.item(depth)?);
                }
                Ok(Value::Array(items))
            }
            5 => {
                let n = self.length(initial)?;
                let depth = deeper(depth)?;
                let mut pairs = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.item(depth)?;
                    let v = self.item(depth)?;
                    pairs.push((k, v));
                }
                Ok(Value::Map(pairs))
            }
            6 => Err(DecodeError::Unsupported(initial)),
            _ => match initial {
                0xf4 => Ok(Value::Bool(false)),
                0xf5 => Ok(Value::Bool(true)),
                0xf6 => Ok(Value::Null),
                0xf7 => Ok(Value::Undefined),
                0xf9 => Ok(Value::Float(Float::F16(self.uint(2)? as u16))),
                0xfa => Ok(Value::Float(Float::F32(f32::from_bits(
                    self.uint(4)? as u32
                )))),
                0xfb => Ok(Value::Float(Float::F64(f64::from_bits(self.uint(8)?)))),
                _ => Err(DecodeError::Unsupported(initial)),
            },
        }
    }
}

fn deeper(depth: usize) -> Result<usize, DecodeError> {
    if depth + 1 > MAX_DEPTH {
        return Err(DecodeError::TooDeep);
    }
    Ok(depth + 1)
}

/// IEEE 754 half precision to `f64` (RFC 8949 Appendix D).
fn half_to_f64(bits: u16) -> f64 {
    let exp = (bits >> 10) & 0x1f;
    let mant = f64::from(bits & 0x3ff);
    let magnitude = match exp {
        0 => mant * pow2(-24),
        31 if mant == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (mant + 1024.0) * pow2(i32::from(e) - 25),
    };
    if bits & 0x8000 != 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// `2^e` without `std`'s `powi`.
fn pow2(e: i32) -> f64 {
    // The exponents here are within -24..=6, well inside f64's normal range.
    f64::from_bits(((1023 + e) as u64) << 52)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn round(bytes: &[u8]) -> Value {
        let value = Value::decode(bytes).expect("decodes");
        assert_eq!(value.encode(), bytes, "re-encodes byte for byte");
        value
    }

    #[test]
    fn heads_are_the_shortest_at_every_boundary() {
        for (n, bytes) in [
            (0u64, vec![0x00]),
            (23, vec![0x17]),
            (24, vec![0x18, 24]),
            (255, vec![0x18, 0xff]),
            (256, vec![0x19, 0x01, 0x00]),
            (65_535, vec![0x19, 0xff, 0xff]),
            (65_536, vec![0x1a, 0, 1, 0, 0]),
            (u64::from(u32::MAX) + 1, vec![0x1b, 0, 0, 0, 1, 0, 0, 0, 0]),
        ] {
            assert_eq!(Value::Uint(n).encode(), bytes, "{n}");
            assert_eq!(round(&bytes), Value::Uint(n));
        }
        assert_eq!(round(&[0x20]), Value::Nint(0), "-1");
    }

    #[test]
    fn a_map_keeps_its_order() {
        // WELCOME's capabilities as the reference hub writes them.
        let value = round(&[0xa3, 0x01, 0xf5, 0x02, 0xf5, 0x00, 0xf5]);
        let Value::Map(pairs) = value else {
            panic!("a map");
        };
        let keys: Vec<_> = pairs.iter().map(|(k, _)| k.as_uint()).collect();
        assert_eq!(keys, [Some(1), Some(2), Some(0)]);
    }

    #[test]
    fn floats_keep_their_width() {
        round(&[0xf9, 0x3c, 0x00]);
        round(&[0xfa, 0x3f, 0x80, 0x00, 0x00]);
        round(&[0xfb, 0x41, 0x22, 0x51, 0x1a, 0x1e, 0xd2, 0x66, 0x61]);
        assert_eq!(Float::F16(0x3c00).to_f64(), 1.0);
        assert_eq!(Float::F16(0xc000).to_f64(), -2.0);
        assert_eq!(Float::F16(0x0001).to_f64(), 5.960_464_477_539_063e-8);
        assert_eq!(Float::F16(0x7bff).to_f64(), 65504.0);
        assert!(Float::F16(0x7c00).to_f64().is_infinite());
    }

    #[test]
    fn what_is_outside_the_subset_is_refused() {
        assert_eq!(Value::decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(Value::decode(&[0x00, 0x00]), Err(DecodeError::Trailing));
        assert_eq!(
            Value::decode(&[0x9f, 0xff]),
            Err(DecodeError::Unsupported(0x9f))
        );
        assert_eq!(
            Value::decode(&[0xc1, 0x00]),
            Err(DecodeError::Unsupported(0xc1))
        );
        assert_eq!(
            Value::decode(&[0xf8, 0x20]),
            Err(DecodeError::Unsupported(0xf8))
        );
        assert_eq!(
            Value::decode(&[0x62, 0xff, 0xfe]),
            Err(DecodeError::BadText)
        );
        // A length past the input is refused before anything is allocated.
        assert_eq!(
            Value::decode(&[0x9b, 0x10, 0, 0, 0, 0, 0, 0, 0]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            Value::decode(&[0x5a, 0, 0, 0, 9, 1]),
            Err(DecodeError::Truncated)
        );
        let deep = [0x81u8; MAX_DEPTH + 1];
        let mut bytes = deep.to_vec();
        bytes.push(0x00);
        assert_eq!(Value::decode(&bytes), Err(DecodeError::TooDeep));
        // The control: one level less decodes.
        assert!(Value::decode(&bytes[1..]).is_ok());
    }
}
