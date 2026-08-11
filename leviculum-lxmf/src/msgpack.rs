use alloc::vec::Vec;
use rmp::{
    decode::{
        bytes::{Bytes, BytesReadError},
        NumValueReadError, ValueReadError,
    },
    Marker,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Type,
    Overflow,
    Trailing,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "msgpack value is truncated"),
            Self::Type => write!(f, "unexpected msgpack type"),
            Self::Overflow => write!(f, "msgpack value does not fit its target type"),
            Self::Trailing => write!(f, "trailing bytes after the msgpack value"),
        }
    }
}

impl core::error::Error for Error {}

/// Coarse wire type used to decode protocol unions without exposing `rmp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Nil,
    False,
    True,
    Array,
    Float,
    Other,
}

pub fn array(out: &mut Vec<u8>, n: usize) {
    let _ = rmp::encode::write_array_len(out, n as u32);
}
pub fn map(out: &mut Vec<u8>, n: usize) {
    let _ = rmp::encode::write_map_len(out, n as u32);
}
pub fn bin(out: &mut Vec<u8>, v: &[u8]) {
    let _ = rmp::encode::write_bin(out, v);
}
pub fn string(out: &mut Vec<u8>, value: &str) {
    let _ = rmp::encode::write_str(out, value);
}
pub fn f64(out: &mut Vec<u8>, v: f64) {
    let _ = rmp::encode::write_f64(out, v);
}
pub fn uint(out: &mut Vec<u8>, v: u64) {
    let _ = rmp::encode::write_uint(out, v);
}
pub fn int(out: &mut Vec<u8>, v: i64) {
    if v >= 0 {
        uint(out, v as u64);
    } else {
        let _ = rmp::encode::write_sint(out, v);
    }
}
pub fn nil(out: &mut Vec<u8>) {
    let _ = rmp::encode::write_nil(out);
}
pub fn bool(out: &mut Vec<u8>, value: bool) {
    let _ = rmp::encode::write_bool(out, value);
}
/// Append one already-validated MessagePack value without changing its wire type.
pub fn append_raw(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(value);
}

fn take<'a>(d: &'a [u8], p: &mut usize, n: usize) -> Result<&'a [u8], Error> {
    let e = p.checked_add(n).ok_or(Error::Overflow)?;
    let v = d.get(*p..e).ok_or(Error::Truncated)?;
    *p = e;
    Ok(v)
}

fn take_u16(d: &[u8], p: &mut usize) -> Result<u16, Error> {
    let bytes = take(d, p, 2)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn take_u32(d: &[u8], p: &mut usize) -> Result<u32, Error> {
    let bytes = take(d, p, 4)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn map_value_error(error: ValueReadError<BytesReadError>) -> Error {
    match error {
        ValueReadError::InvalidMarkerRead(_) | ValueReadError::InvalidDataRead(_) => {
            Error::Truncated
        }
        ValueReadError::TypeMismatch(_) => Error::Type,
    }
}

fn map_number_error(error: NumValueReadError<BytesReadError>) -> Error {
    match error {
        NumValueReadError::InvalidMarkerRead(_) | NumValueReadError::InvalidDataRead(_) => {
            Error::Truncated
        }
        NumValueReadError::TypeMismatch(_) => Error::Type,
        NumValueReadError::OutOfRange => Error::Overflow,
    }
}

/// Inspect the next MessagePack value kind without consuming it.
pub fn peek_kind(d: &[u8], p: usize) -> Result<Kind, Error> {
    let marker = d
        .get(p)
        .copied()
        .map(Marker::from_u8)
        .ok_or(Error::Truncated)?;
    Ok(match marker {
        Marker::Null => Kind::Nil,
        Marker::False => Kind::False,
        Marker::True => Kind::True,
        Marker::FixArray(_) | Marker::Array16 | Marker::Array32 => Kind::Array,
        Marker::F32 | Marker::F64 => Kind::Float,
        _ => Kind::Other,
    })
}

pub fn array_len(d: &[u8], p: &mut usize) -> Result<usize, Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let n = rmp::decode::read_array_len(&mut rd).map_err(map_value_error)?;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    Ok(n as usize)
}
pub fn map_len(d: &[u8], p: &mut usize) -> Result<usize, Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let n = rmp::decode::read_map_len(&mut rd).map_err(map_value_error)?;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    Ok(n as usize)
}
pub fn read_bin<'a>(d: &'a [u8], p: &mut usize) -> Result<&'a [u8], Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let n = rmp::decode::read_bin_len(&mut rd).map_err(map_value_error)? as usize;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    take(d, p, n)
}
pub fn read_str<'a>(d: &'a [u8], p: &mut usize) -> Result<&'a str, Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let n = rmp::decode::read_str_len(&mut rd).map_err(map_value_error)? as usize;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    core::str::from_utf8(take(d, p, n)?).map_err(|_| Error::Type)
}
pub fn read_f64(d: &[u8], p: &mut usize) -> Result<f64, Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let value = rmp::decode::read_f64(&mut rd).map_err(map_value_error)?;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    Ok(value)
}
/// One MessagePack number together with the family the wire used for it.
///
/// Callers that only need the value take [`read_number_f64`]. The family
/// matters where a decoded value has to be packed again in the form the
/// reference's `msgpack.packb` would produce: an int goes back as a
/// minimal-width int, a float as float64.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    Int(i64),
    Float(f64),
}

impl Number {
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(v) => v as f64,
            Self::Float(v) => v,
        }
    }
}

/// Read any MessagePack numeric representation, keeping its family.
///
/// Non-numeric markers give [`Error::Type`]; an unsigned value above
/// `i64::MAX` gives [`Error::Overflow`] rather than a silently rounded double.
pub fn read_number(d: &[u8], p: &mut usize) -> Result<Number, Error> {
    match d.get(*p).copied().ok_or(Error::Truncated)? {
        0xca => {
            let marker = take(d, p, 1)?[0];
            debug_assert_eq!(marker, 0xca);
            let bits = f32::from_be_bytes(take(d, p, 4)?.try_into().map_err(|_| Error::Truncated)?);
            Ok(Number::Float(bits as f64))
        }
        0xcb => read_f64(d, p).map(Number::Float),
        _ => read_int(d, p).map(Number::Int),
    }
}

/// Read any MessagePack numeric representation as `f64`.
pub fn read_number_f64(d: &[u8], p: &mut usize) -> Result<f64, Error> {
    read_number(d, p).map(Number::as_f64)
}
pub fn read_bool(d: &[u8], p: &mut usize) -> Result<bool, Error> {
    match take(d, p, 1)?[0] {
        0xc2 => Ok(false),
        0xc3 => Ok(true),
        _ => Err(Error::Type),
    }
}
pub fn read_nil(d: &[u8], p: &mut usize) -> Result<(), Error> {
    if take(d, p, 1)?[0] == 0xc0 {
        Ok(())
    } else {
        Err(Error::Type)
    }
}
pub fn read_uint(d: &[u8], p: &mut usize) -> Result<u64, Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let value = rmp::decode::read_int::<u64, _>(&mut rd).map_err(map_number_error)?;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    Ok(value)
}
pub fn read_int(d: &[u8], p: &mut usize) -> Result<i64, Error> {
    let mut rd = Bytes::new(d.get(*p..).ok_or(Error::Truncated)?);
    let value = rmp::decode::read_int::<i64, _>(&mut rd).map_err(map_number_error)?;
    *p = p
        .checked_add(rd.position() as usize)
        .ok_or(Error::Overflow)?;
    Ok(value)
}
pub fn raw<'a>(d: &'a [u8], p: &mut usize) -> Result<&'a [u8], Error> {
    let s = *p;
    skip(d, p)?;
    Ok(&d[s..*p])
}
pub fn skip(d: &[u8], p: &mut usize) -> Result<(), Error> {
    let marker = Marker::from_u8(take(d, p, 1)?[0]);
    match marker {
        Marker::FixPos(_) | Marker::FixNeg(_) | Marker::Null | Marker::False | Marker::True => {
            Ok(())
        }
        Marker::FixMap(n) => {
            for _ in 0..n {
                skip(d, p)?;
                skip(d, p)?
            }
            Ok(())
        }
        Marker::FixArray(n) => {
            for _ in 0..n {
                skip(d, p)?
            }
            Ok(())
        }
        Marker::FixStr(n) => {
            take(d, p, n as usize)?;
            Ok(())
        }
        Marker::Bin8 | Marker::Str8 => {
            let n = take(d, p, 1)?[0] as usize;
            take(d, p, n)?;
            Ok(())
        }
        Marker::Bin16 | Marker::Str16 => {
            let n = take_u16(d, p)? as usize;
            take(d, p, n)?;
            Ok(())
        }
        Marker::Bin32 | Marker::Str32 => {
            let n = take_u32(d, p)? as usize;
            take(d, p, n)?;
            Ok(())
        }
        Marker::F32 | Marker::U32 | Marker::I32 => {
            take(d, p, 4)?;
            Ok(())
        }
        Marker::F64 | Marker::U64 | Marker::I64 => {
            take(d, p, 8)?;
            Ok(())
        }
        Marker::U8 | Marker::I8 => {
            take(d, p, 1)?;
            Ok(())
        }
        Marker::U16 | Marker::I16 => {
            take(d, p, 2)?;
            Ok(())
        }
        Marker::Array16 => {
            let n = take_u16(d, p)?;
            for _ in 0..n {
                skip(d, p)?
            }
            Ok(())
        }
        Marker::Array32 => {
            let n = take_u32(d, p)?;
            for _ in 0..n {
                skip(d, p)?
            }
            Ok(())
        }
        Marker::Map16 => {
            let n = take_u16(d, p)?;
            for _ in 0..n {
                skip(d, p)?;
                skip(d, p)?
            }
            Ok(())
        }
        Marker::Map32 => {
            let n = take_u32(d, p)?;
            for _ in 0..n {
                skip(d, p)?;
                skip(d, p)?
            }
            Ok(())
        }
        Marker::FixExt1 => skip_ext(d, p, 1),
        Marker::FixExt2 => skip_ext(d, p, 2),
        Marker::FixExt4 => skip_ext(d, p, 4),
        Marker::FixExt8 => skip_ext(d, p, 8),
        Marker::FixExt16 => skip_ext(d, p, 16),
        Marker::Ext8 => {
            let n = take(d, p, 1)?[0] as usize;
            skip_ext(d, p, n)
        }
        Marker::Ext16 => {
            let n = take_u16(d, p)? as usize;
            skip_ext(d, p, n)
        }
        Marker::Ext32 => {
            let n = take_u32(d, p)? as usize;
            skip_ext(d, p, n)
        }
        Marker::Reserved => Err(Error::Type),
    }
}

fn skip_ext(d: &[u8], p: &mut usize, payload_len: usize) -> Result<(), Error> {
    take(d, p, payload_len.checked_add(1).ok_or(Error::Overflow)?)?;
    Ok(())
}
