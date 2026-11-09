//! A region area (1477): the regions a board carries in a flash partition
//! of their own, beside the image, and the directory that names them.
//!
//! The ThinkNode M9 keeps one at 0xC12000 (`leviculum-esp/partitions-m9.csv`,
//! the `regions` partition, 3.93 MiB). `lnflash --region` builds the area
//! with [`build`] and writes it whole; the board reads the directory with
//! [`directory`] at boot and each region's bytes after it. One format, one
//! parser, both sides.
//!
//! Layout, little-endian: the first [`SECTOR`] is the directory, `LMRA`,
//! the version byte [`VERSION`], the count of entries, two reserved bytes,
//! then per entry its offset from the area's start (`u32`), its length
//! (`u32`), a checksum of its bytes ([`sum`], `u32`) and its name, 20
//! bytes of UTF-8 padded with zeros. The regions follow, each at a
//! sector boundary, in the order of the directory. An erased area (all
//! `0xFF`) has no directory and holds no region.

use alloc::string::String;
use alloc::vec::Vec;

/// The flash's erase unit, and the alignment of every region in the area.
pub const SECTOR: usize = 4096;

/// The directory's magic.
pub const MAGIC: &[u8; 4] = b"LMRA";

/// The format's version.
pub const VERSION: u8 = 1;

/// The bytes of one directory entry.
pub const ENTRY: usize = 32;

/// The bytes of a name in an entry.
pub const NAME: usize = 20;

/// The most regions an area names.
pub const MAX_REGIONS: usize = 32;

/// The directory's head: magic, version, count, two reserved bytes.
const HEAD: usize = 8;

const _: () = assert!(HEAD + MAX_REGIONS * ENTRY <= SECTOR);

/// One region the directory names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// From the area's start, a multiple of [`SECTOR`].
    pub offset: u32,
    pub len: u32,
    /// [`sum`] of its bytes.
    pub sum: u32,
    pub name: String,
}

/// Why an area is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaError {
    /// Erased, or no directory at all.
    Empty,
    /// A directory of another version.
    Version(u8),
    /// More regions than [`MAX_REGIONS`].
    TooMany,
    /// An entry outside the area, unaligned, or over another.
    Misplaced,
    /// The regions do not fit the area's capacity.
    Full { need: usize, capacity: usize },
    /// A name longer than [`NAME`] bytes or empty.
    Name,
    /// A region's bytes do not match its checksum.
    Sum,
}

impl core::fmt::Display for AreaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty"),
            Self::Version(v) => write!(f, "version-{v}"),
            Self::TooMany => f.write_str("too-many"),
            Self::Misplaced => f.write_str("misplaced"),
            Self::Full { need, capacity } => write!(f, "full need={need} capacity={capacity}"),
            Self::Name => f.write_str("name"),
            Self::Sum => f.write_str("sum"),
        }
    }
}

/// The checksum the directory keeps of a region: FNV-1a, 32 bits. A torn
/// or half written region reads as damaged, not as a map.
pub fn sum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// `n` rounded up to whole sectors.
fn sectors(n: usize) -> usize {
    n.div_ceil(SECTOR) * SECTOR
}

/// The area holding `regions` (name, bytes), at most `capacity` bytes:
/// the directory, then each region at a sector boundary. Only the bytes
/// up to the last region's end; the rest of the area is left as it is.
pub fn build(regions: &[(&str, &[u8])], capacity: usize) -> Result<Vec<u8>, AreaError> {
    if regions.len() > MAX_REGIONS {
        return Err(AreaError::TooMany);
    }
    let need = SECTOR + regions.iter().map(|(_, b)| sectors(b.len())).sum::<usize>();
    if need > capacity {
        return Err(AreaError::Full { need, capacity });
    }
    let mut out = alloc::vec![0xFFu8; SECTOR];
    out[..4].copy_from_slice(MAGIC);
    out[4] = VERSION;
    out[5] = regions.len() as u8;
    out[6] = 0;
    out[7] = 0;
    for (i, (name, bytes)) in regions.iter().enumerate() {
        if name.is_empty() || name.len() > NAME {
            return Err(AreaError::Name);
        }
        let offset = out.len();
        let at = HEAD + i * ENTRY;
        let len = u32::try_from(bytes.len()).map_err(|_| AreaError::Misplaced)?;
        let offset = u32::try_from(offset).map_err(|_| AreaError::Misplaced)?;
        out[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&len.to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&sum(bytes).to_le_bytes());
        let mut field = [0u8; NAME];
        field[..name.len()].copy_from_slice(name.as_bytes());
        out[at + 12..at + 12 + NAME].copy_from_slice(&field);
        out.extend_from_slice(bytes);
        out.resize(sectors(out.len()), 0xFF);
    }
    Ok(out)
}

/// The regions the directory `sector` names, checked against an area of
/// `capacity` bytes: inside it, aligned, in order and apart.
pub fn directory(sector: &[u8], capacity: usize) -> Result<Vec<Entry>, AreaError> {
    if sector.len() < HEAD || &sector[..4] != MAGIC {
        return Err(AreaError::Empty);
    }
    if sector[4] != VERSION {
        return Err(AreaError::Version(sector[4]));
    }
    let count = usize::from(sector[5]);
    if count > MAX_REGIONS || sector.len() < HEAD + count * ENTRY {
        return Err(AreaError::TooMany);
    }
    let word = |at: usize| {
        u32::from_le_bytes([sector[at], sector[at + 1], sector[at + 2], sector[at + 3]])
    };
    let mut out: Vec<Entry> = Vec::with_capacity(count);
    let mut end = SECTOR;
    for i in 0..count {
        let at = HEAD + i * ENTRY;
        let (offset, len, sum) = (word(at), word(at + 4), word(at + 8));
        let start = offset as usize;
        let stop = start
            .checked_add(len as usize)
            .ok_or(AreaError::Misplaced)?;
        if !start.is_multiple_of(SECTOR) || start < end || stop > capacity {
            return Err(AreaError::Misplaced);
        }
        end = sectors(stop);
        let raw = &sector[at + 12..at + 12 + NAME];
        let used = raw.iter().position(|&b| b == 0).unwrap_or(NAME);
        let name = core::str::from_utf8(&raw[..used]).map_err(|_| AreaError::Name)?;
        if name.is_empty() {
            return Err(AreaError::Name);
        }
        out.push(Entry {
            offset,
            len,
            sum,
            name: String::from(name),
        });
    }
    Ok(out)
}

/// Whether `bytes` are the region `entry` names.
pub fn check(entry: &Entry, bytes: &[u8]) -> Result<(), AreaError> {
    match bytes.len() == entry.len as usize && sum(bytes) == entry.sum {
        true => Ok(()),
        false => Err(AreaError::Sum),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two regions in, the same two out: names, places, lengths and sums;
    /// the bytes at each entry's place are the region's. An erased area
    /// is empty; a flipped byte fails its sum; regions past the capacity
    /// are refused before anything is built.
    #[test]
    fn an_area_reads_back_what_it_was_built_of() {
        let a: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let b = alloc::vec![7u8; 100];
        let area = build(&[("firenze", &a), ("berlin-mitte", &b)], 64 * SECTOR).expect("built");
        assert_eq!(area.len(), SECTOR + 2 * SECTOR + SECTOR);
        let dir = directory(&area[..SECTOR], 64 * SECTOR).expect("read");
        assert_eq!(dir.len(), 2);
        assert_eq!(
            (dir[0].name.as_str(), dir[0].offset, dir[0].len),
            ("firenze", 4096, 5000)
        );
        assert_eq!(
            (dir[1].name.as_str(), dir[1].offset, dir[1].len),
            ("berlin-mitte", 12288, 100)
        );
        for (entry, bytes) in dir.iter().zip([&a[..], &b[..]]) {
            let at = entry.offset as usize;
            let got = &area[at..at + entry.len as usize];
            assert_eq!(got, bytes);
            assert_eq!(check(entry, got), Ok(()));
        }
        let mut torn = area[12288..12288 + 100].to_vec();
        torn[50] ^= 1;
        assert_eq!(check(&dir[1], &torn), Err(AreaError::Sum));
        assert_eq!(
            directory(&[0xFF; SECTOR], 64 * SECTOR),
            Err(AreaError::Empty)
        );
        assert_eq!(
            build(&[("big", &alloc::vec![0u8; 3 * SECTOR])], 3 * SECTOR),
            Err(AreaError::Full {
                need: 4 * SECTOR,
                capacity: 3 * SECTOR
            })
        );
        assert_eq!(build(&[("", &b)], 64 * SECTOR), Err(AreaError::Name));
        // A directory that points past the area is refused.
        assert_eq!(
            directory(&area[..SECTOR], 2 * SECTOR),
            Err(AreaError::Misplaced)
        );
    }
}
