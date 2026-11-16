//! Shared fixture: the grunewald region (z12..=15, four zooms over the
//! committed PMTiles fixture), cut in memory exactly as
//! `leviculum-map-tool cut` would cut it — the store tests then split,
//! sign and serve a real region file, not a synthetic byte blob.

use std::io::Cursor;

use leviculum_map::region::Bounds;
use leviculum_mapcut::cut::{cut, CutOptions};
use leviculum_mapcut::pmtiles::{Archive, RangeRead};
use leviculum_mapcut::Res;

const FIXTURE: &[u8] = include_bytes!("../../../leviculum-mapcut/fixtures/grunewald.pmtiles");

struct InMemory;

impl RangeRead for InMemory {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let at = (offset as usize).min(FIXTURE.len());
        let end = at.saturating_add(len as usize).min(FIXTURE.len());
        Ok(FIXTURE[at..end].to_vec())
    }
}

/// The fixture's box (the extract command in
/// `leviculum-mapcut/fixtures/README.md`), degrees x 10^7.
pub fn bounds() -> Bounds {
    Bounds {
        west: 132_250_000,
        south: 524_700_000,
        east: 132_310_000,
        north: 524_730_000,
    }
}

/// Cut the fixture into a complete region file (version 4 or newer:
/// the tile index first, the tiles coarse to fine).
pub fn grunewald_region() -> Vec<u8> {
    let archive = Archive::from_source(InMemory).expect("open fixture archive");
    let options = CutOptions {
        bounds: bounds(),
        zooms: 12..=15,
        profile: 1,
        attribution: "© OpenStreetMap contributors".to_owned(),
        notice: "ODbL; test fixture".to_owned(),
        threads: 2,
        locale: None,
        buildings: true,
        pois: true,
        round: 64,
    };
    let mut out = Cursor::new(Vec::new());
    cut(&archive, &options, &mut out).expect("cut the fixture region");
    out.into_inner()
}
