//! A box of an archive, local or remote, cut into a region under a size
//! cap (1089): the tiles are located first, from the directories alone, and
//! when the box's tiles down to the deepest zoom asked would fetch more than
//! the cap, the deepest zoom is lowered until they do not. Only then is
//! anything fetched, in as few ranges as the archive's layout allows.

use std::io::{Seek, Write};
use std::ops::RangeInclusive;

use leviculum_map::region::Bounds;

use crate::cut::{cut, CutOptions, CutStats};
use crate::pmtiles::{Archive, RangeRead};
use crate::{notice, source_line, Res, ATTRIBUTION};

/// What to load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub bounds: Bounds,
    /// The zooms wanted; the deepest is lowered to fit [`Want::cap`].
    pub zooms: RangeInclusive<u8>,
    /// The shallowest the deepest zoom may be lowered to.
    pub floor: u8,
    /// The most archive bytes the tiles may come to.
    pub cap: u64,
    pub profile: u8,
    /// The language names are kept in beside the local ones
    /// ([`CutOptions::locale`]).
    pub locale: Option<String>,
    /// Cut the buildings too ([`CutOptions::buildings`]).
    pub buildings: bool,
}

/// What a load fetched and wrote.
#[derive(Debug)]
pub struct Loaded {
    /// The zooms the region holds.
    pub zooms: RangeInclusive<u8>,
    /// Archive bytes fetched, the gaps between tiles included.
    pub fetched: u64,
    /// Range requests for the tiles (the directories not counted).
    pub requests: usize,
    pub stats: CutStats,
}

/// The box's tiles would fetch more than the cap even at the floor zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    /// Archive bytes at the floor zoom.
    pub bytes: u64,
    pub cap: u64,
}

impl core::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "the area is {} MB of tiles, over the {} MB a load takes",
            self.bytes.div_ceil(1 << 20),
            self.cap >> 20
        )
    }
}

impl std::error::Error for TooLarge {}

/// The source holds no tile in the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Empty;

impl core::fmt::Display for Empty {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the map source holds no tiles there")
    }
}

impl std::error::Error for Empty {}

/// The deepest zoom of `zooms` down to `floor` whose tiles (each a zoom and
/// its length) come to at most `cap` bytes, with the bytes; `Err` with the
/// bytes at the floor when none does.
pub fn deepest_within(
    tiles: &[(u8, u32)],
    zooms: &RangeInclusive<u8>,
    floor: u8,
    cap: u64,
) -> Result<(u8, u64), u64> {
    let bytes = |hi: u8| -> u64 {
        tiles
            .iter()
            .filter(|(z, _)| *z <= hi)
            .map(|(_, l)| u64::from(*l))
            .sum()
    };
    let lowest = floor.clamp(*zooms.start(), *zooms.end());
    for hi in (lowest..=*zooms.end()).rev() {
        let b = bytes(hi);
        if b <= cap {
            return Ok((hi, b));
        }
    }
    Err(bytes(lowest))
}

/// Cut `want`'s box of `archive` into `out`, the region's notice naming
/// `by` as the program that cut it.
pub fn load<R: RangeRead, W: Write + Seek>(
    archive: &Archive<R>,
    want: &Want,
    by: &str,
    threads: usize,
    out: W,
) -> Res<Loaded> {
    let located = archive.locate_in(&want.bounds, want.zooms.clone())?;
    if located.is_empty() {
        return Err(Box::new(Empty));
    }
    let sizes: Vec<(u8, u32)> = located.iter().map(|l| (l.0.z, l.2)).collect();
    let (hi, _) =
        deepest_within(&sizes, &want.zooms, want.floor, want.cap).map_err(|bytes| TooLarge {
            bytes,
            cap: want.cap,
        })?;
    let chosen: Vec<_> = located.into_iter().filter(|l| l.0.z <= hi).collect();
    let fetched = archive.fetch(&chosen)?;
    let zooms = *want.zooms.start()..=hi;
    let opts = CutOptions {
        bounds: want.bounds,
        zooms: zooms.clone(),
        profile: want.profile,
        attribution: ATTRIBUTION.to_owned(),
        notice: notice(by, want.profile, &source_line(archive)),
        threads,
        locale: want.locale.clone(),
        buildings: want.buildings,
    };
    let stats = cut(&fetched, &opts, out)?;
    Ok(Loaded {
        zooms,
        fetched: fetched.bytes,
        requests: fetched.requests,
        stats,
    })
}
