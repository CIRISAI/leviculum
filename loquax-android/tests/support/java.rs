//! A request as the app's Java builds it, in the bytes `MsgPack.pack`
//! writes (`android/src/network/leviculum/loquax/MsgPack.java`, `write`):
//! a long in a fixint when it fits and as `d3` and eight bytes otherwise,
//! whatever its size; a double as `cb`; a `byte[]` as `c6`; null as `c0`. The facade's
//! tests decode these bytes, not serde's own, so a field Java leaves out
//! or writes in another width shows up as it does on the phone.

/// A value of a request map as the app builds it.
#[allow(dead_code)]
pub enum Java<'a> {
    Nil,
    Bool(bool),
    Long(i64),
    /// A `Double` (or a `Float`), always float 64 (1269).
    Real(f64),
    Str(&'a str),
    Bin(&'a [u8]),
    Longs(&'a [i64]),
    Strs(&'a [&'a str]),
    List(Vec<Java<'a>>),
    Map(Vec<(&'a str, Java<'a>)>),
}

/// The map `entries`, keys in Java's insertion order (`Core.map` builds a
/// `LinkedHashMap`).
pub fn java_map(entries: &[(&str, Java)]) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(&mut out, entries);
    out
}

fn write_map(out: &mut Vec<u8>, entries: &[(&str, Java)]) {
    header(out, entries.len(), 0x80, 0xde, 0xdf);
    for (name, value) in entries {
        string(out, name);
        write(out, value);
    }
}

fn write(out: &mut Vec<u8>, value: &Java) {
    match value {
        Java::Nil => out.push(0xc0),
        Java::Bool(b) => out.push(if *b { 0xc3 } else { 0xc2 }),
        Java::Long(n) => long(out, *n),
        Java::Real(x) => {
            out.push(0xcb);
            out.extend_from_slice(&x.to_bits().to_be_bytes());
        }
        Java::Str(s) => string(out, s),
        Java::Bin(bytes) => {
            out.push(0xc6);
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        Java::Longs(ns) => {
            header(out, ns.len(), 0x90, 0xdc, 0xdd);
            for n in *ns {
                long(out, *n);
            }
        }
        Java::Strs(ss) => {
            header(out, ss.len(), 0x90, 0xdc, 0xdd);
            for s in *ss {
                string(out, s);
            }
        }
        Java::List(items) => {
            header(out, items.len(), 0x90, 0xdc, 0xdd);
            for item in items {
                write(out, item);
            }
        }
        Java::Map(entries) => write_map(out, entries),
    }
}

fn long(out: &mut Vec<u8>, n: i64) {
    if (0..128).contains(&n) || (-32..0).contains(&n) {
        out.push(n as u8);
    } else {
        out.push(0xd3);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

fn string(out: &mut Vec<u8>, s: &str) {
    let n = s.len();
    if n < 32 {
        out.push(0xa0 | n as u8);
    } else if n < 0x100 {
        out.extend_from_slice(&[0xd9, n as u8]);
    } else if n < 0x10000 {
        out.push(0xda);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
    out.extend_from_slice(s.as_bytes());
}

fn header(out: &mut Vec<u8>, n: usize, fix: u8, b16: u8, b32: u8) {
    if n < 16 {
        out.push(fix | n as u8);
    } else if n < 0x10000 {
        out.push(b16);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(b32);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
}
