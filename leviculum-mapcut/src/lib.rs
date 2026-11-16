//! `leviculum-mapcut`: makes region files for `leviculum-map` (Codeberg
//! #474, `docs/src/concepts/loquax-map.md`, section 4), for the cut by hand
//! (`leviculum-map-tool cut`) and for the messenger's autoload (`loquax`,
//! 1089) alike: one cut path, not a copy.
//!
//! - [`pmtiles`]: a PMTiles v3 archive read by ranges, from a file or a
//!   server, only the directories and tiles a box needs.
//! - [`http`] (feature `http`): the server, by HTTP range requests.
//! - [`mvt`]: vector tiles to classed geometry, the tree's one MVT decoder.
//! - [`simplify`]: Douglas-Peucker, quantisation, areas, all integer.
//! - [`cut`]: a bounding box and zoom range of a source under a layer
//!   profile, written as a region file.
//! - [`load`]: a box of a remote archive cut into a region under a size
//!   cap, which lowers the deepest zoom rather than asking.
//! - [`serve`]: a loopback HTTP server of archives with range requests,
//!   the map source of the tests.
//!
//! `std`, and in no `no_std` graph: prost and the MVT decoder stay here,
//! out of `leviculum-map` and the loquax core.

pub mod cut;
#[cfg(feature = "http")]
pub mod http;
pub mod load;
pub mod mvt;
pub mod pmtiles;
pub mod serve;
pub mod simplify;

use std::ops::RangeInclusive;

use leviculum_map::region::{Bounds, TileId};

use crate::pmtiles::{Archive, RangeRead};

/// The error type of the cut: anything, sendable across its threads.
pub type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Where the cut reads tiles from: a PMTiles archive, tiles fetched from
/// one, or a test's tiles.
pub trait TileSource: Sync {
    /// The tiles the source holds in `bounds` at `zooms`, in any order.
    fn tiles_in(&self, bounds: &Bounds, zooms: RangeInclusive<u8>) -> Res<Vec<TileId>>;
    /// The uncompressed MVT bytes of `t`; `None` when the source has none.
    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>>;
    /// The cut is about to ask for `tiles` and nothing before them again:
    /// a source that fetches as the cut goes fetches them now (1291).
    fn want(&self, tiles: &[TileId]) -> Res<()> {
        let _ = tiles;
        Ok(())
    }
}

impl<R: RangeRead> TileSource for Archive<R> {
    fn tiles_in(&self, bounds: &Bounds, zooms: RangeInclusive<u8>) -> Res<Vec<TileId>> {
        Ok(self
            .locate_in(bounds, zooms)?
            .into_iter()
            .map(|l| l.0)
            .collect())
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        Archive::mvt(self, t)
    }
}

impl TileSource for pmtiles::Fetched {
    fn tiles_in(&self, bounds: &Bounds, zooms: RangeInclusive<u8>) -> Res<Vec<TileId>> {
        Ok(self
            .tiles()
            .filter(|t| zooms.contains(&t.z) && cut::in_bounds(*t, bounds))
            .collect())
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        pmtiles::Fetched::mvt(self, t)
    }
}

/// Several archives read as one, for a region downloaded in parts: a tile
/// is read from the first archive that holds it. The parts of one build
/// hold the same bytes for a tile on their shared edge, so which one is
/// read does not change the cut.
impl<R: RangeRead> TileSource for [Archive<R>] {
    fn tiles_in(&self, bounds: &Bounds, zooms: RangeInclusive<u8>) -> Res<Vec<TileId>> {
        let mut all = Vec::new();
        for a in self {
            all.extend(a.tiles_in(bounds, zooms.clone())?);
        }
        Ok(all)
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        for a in self {
            if let Some(b) = a.mvt(t)? {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }
}

/// The attribution every region carries and every map view shows.
pub const ATTRIBUTION: &str = "© OpenStreetMap contributors";

/// The words naming an archive in a region's notice: its name and version
/// and the date of its OpenStreetMap data, as its metadata says them.
pub fn source_line<R: RangeRead>(a: &Archive<R>) -> String {
    let m = a.metadata().unwrap_or(serde_json::Value::Null);
    let field = |k: &str| m.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let name = field("name").unwrap_or_else(|| "a PMTiles archive".into());
    let version = field("version")
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    let osm = field("planetiler:osm:osmosisreplicationtime")
        .or_else(|| field("planetiler:osm:replicationtime"))
        .map(|t| format!(", OpenStreetMap data of {t}"))
        .unwrap_or_default();
    format!("{name}{version}{osm}")
}

/// The licence notice a region carries: ODbL, the derived database, and
/// the program `by` that cut it, the profile and `source` it was cut under
/// and from.
pub fn notice(by: &str, profile: u8, source: &str) -> String {
    format!(
        "Map data {ATTRIBUTION}, https://www.openstreetmap.org/copyright. \
         Available under the Open Database License (ODbL) 1.0, \
         https://opendatacommons.org/licenses/odbl/1-0/. This region file is a \
         derived database of it and is under the same licence. Low zooms may \
         carry Natural Earth data, which is in the public domain. Cut by \
         {by} under layer profile {profile} from {source}."
    )
}
