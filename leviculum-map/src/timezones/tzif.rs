//! TZif, the zone files of the tz database (RFC 8536, versions 1 to 3,
//! and 4 of RFC 9636, which reads the same).
//!
//! A file is a header and a data block of 32-bit times (version 1), then,
//! from version 2, a second header and block of 64-bit times and a footer,
//! a POSIX TZ string between two newlines. A time before the first
//! transition keeps time type 0; a time at or after the last keeps the
//! footer's rule where there is one, the last transition's type where
//! there is not; in between, the type of the last transition at or before
//! it. A file without transitions is its footer, or type 0. Leap second
//! records are skipped: the system's zone files are the `posix/` ones, whose
//! times are UTC seconds without leap seconds, the times this reads.

use super::posix::{self, Offset};

/// The header's length.
const HEADER: usize = 44;

/// One block: where its parts start, and their counts.
struct Block<'a> {
    data: &'a [u8],
    /// 4 or 8.
    time_size: usize,
    times: usize,
    indices: usize,
    types: usize,
    chars: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
    /// The block's length.
    len: usize,
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The header at `at` and the block after it, for times of `time_size`
/// bytes; the version byte too.
fn block(data: &[u8], at: usize, time_size: usize) -> Option<(u8, Block<'_>)> {
    if data.get(at..at + 4)? != b"TZif" {
        return None;
    }
    let version = *data.get(at + 4)?;
    if !matches!(version, 0 | b'2'..=b'4') {
        return None;
    }
    let count = |i: usize| -> Option<usize> { usize::try_from(be32(data, at + 20 + 4 * i)?).ok() };
    let (isutcnt, isstdcnt, leapcnt) = (count(0)?, count(1)?, count(2)?);
    let (timecnt, typecnt, charcnt) = (count(3)?, count(4)?, count(5)?);
    if typecnt == 0 || charcnt == 0 {
        return None;
    }
    let times = at + HEADER;
    let indices = times.checked_add(timecnt.checked_mul(time_size)?)?;
    let types = indices.checked_add(timecnt)?;
    let chars = types.checked_add(typecnt.checked_mul(6)?)?;
    let leaps = chars.checked_add(charcnt)?;
    let end = leaps
        .checked_add(leapcnt.checked_mul(time_size + 4)?)?
        .checked_add(isstdcnt)?
        .checked_add(isutcnt)?;
    if end > data.len() {
        return None;
    }
    Some((
        version,
        Block {
            data,
            time_size,
            times,
            indices,
            types,
            chars,
            timecnt,
            typecnt,
            charcnt,
            len: end - at,
        },
    ))
}

impl<'a> Block<'a> {
    fn time(&self, i: usize) -> Option<i64> {
        let at = self.times + i * self.time_size;
        match self.time_size {
            8 => {
                let b = self.data.get(at..at + 8)?;
                Some(i64::from_be_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ]))
            }
            _ => Some(i64::from(be32(self.data, at)? as i32)),
        }
    }

    /// Time type `i`: its offset, DST flag and abbreviation.
    fn kind(&self, i: usize) -> Option<Offset<'a>> {
        if i >= self.typecnt {
            return None;
        }
        let at = self.types + 6 * i;
        let utc_offset = be32(self.data, at)? as i32;
        let dst = *self.data.get(at + 4)? != 0;
        let idx = usize::from(*self.data.get(at + 5)?);
        if idx >= self.charcnt {
            return None;
        }
        let chars = self.data.get(self.chars + idx..self.chars + self.charcnt)?;
        let len = chars.iter().position(|&c| c == 0)?;
        let abbr = core::str::from_utf8(&chars[..len]).ok()?;
        Some(Offset {
            utc_offset,
            dst,
            abbr,
        })
    }

    /// The type of the last transition at or before `unix`, type 0 before
    /// the first; `None` past the last, or with no transitions.
    fn inside(&self, unix: i64) -> Option<Option<Offset<'a>>> {
        if self.timecnt == 0 {
            return None;
        }
        if unix >= self.time(self.timecnt - 1)? {
            return None;
        }
        if unix < self.time(0)? {
            return Some(self.kind(0));
        }
        // The largest i with time(i) <= unix, in [0, timecnt - 1).
        let (mut lo, mut hi) = (0usize, self.timecnt - 1);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match self.time(mid) {
                Some(t) if t <= unix => lo = mid,
                Some(_) => hi = mid,
                None => return Some(None),
            }
        }
        let idx = self.data.get(self.indices + lo).copied();
        Some(idx.and_then(|i| self.kind(usize::from(i))))
    }

    /// The type of the last transition, or type 0 without any.
    fn last(&self) -> Option<Offset<'a>> {
        match self.timecnt {
            0 => self.kind(0),
            n => self.kind(usize::from(*self.data.get(self.indices + n - 1)?)),
        }
    }
}

/// The footer's TZ string; `None` for a version 1 file or a file without
/// one, `Some("")` for an empty footer.
pub fn footer(data: &[u8]) -> Option<&str> {
    let (version, v1) = block(data, 0, 4)?;
    if version == 0 {
        return None;
    }
    let (_, v2) = block(data, v1.len, 8)?;
    let at = v1.len + v2.len;
    if data.get(at) != Some(&b'\n') {
        return None;
    }
    let rest = data.get(at + 1..)?;
    let len = rest.iter().position(|&c| c == b'\n')?;
    core::str::from_utf8(&rest[..len]).ok()
}

/// The local time the zone file `data` gives at `unix` seconds since the
/// epoch; `None` for bytes that are not a TZif file.
pub fn offset_at(data: &[u8], unix: i64) -> Option<Offset<'_>> {
    let (version, v1) = block(data, 0, 4)?;
    let b = match version {
        0 => v1,
        _ => block(data, v1.len, 8)?.1,
    };
    if let Some(inside) = b.inside(unix) {
        return inside;
    }
    match footer(data).filter(|f| !f.is_empty()) {
        Some(tz) => posix::offset_at(tz, unix).or_else(|| b.last()),
        None => b.last(),
    }
}
