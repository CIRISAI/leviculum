//! The first-tile measurement of 1527, shared by the phone's facade walk
//! (`loquax-android/tests/map.rs`) and the window's (`loquax/src/gui.rs`):
//! a synthetic archive of the Grunewald served throttled, the start region
//! cut from it at full speed, and what the maps directory holds.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use leviculum_map::mercator::World;
use leviculum_map::region::{Header, Region};
use leviculum_map::viewport::Viewport;
use loquax::mapload::{Loader, Source, Step, StepKind};
use loquax::picture::Maps;
use loquax::ui::map::{LoadReason, MapWant};

/// The synthetic archive's box: about 14 by 16 km around the Grunewald,
/// so a pan of a screen and a half of a phone's 1527 pixels east, or two
/// steps out of it, stays inside.
pub const FIRST_TILE_BOX: leviculum_map::region::Bounds = leviculum_map::region::Bounds {
    west: 131_600_000,
    south: 524_000_000,
    east: 133_600_000,
    north: 525_400_000,
};
/// Each tile's own bytes: a dense city tile's size.
pub const FIRST_TILE_PAD: usize = 12_000;
/// The link: 600 kB/s, 20 ms to each answer's first byte.
pub const FIRST_TILE_RATE: u64 = 600_000;
/// Where the walk starts, in the archive's box: z16 over the forest.
pub const FIRST_TILE_AT: (f64, f64) = (52.4715, 13.2280);

/// The Grunewald fixture's z15 tile in every slot of [`FIRST_TILE_BOX`]
/// from z10 to z15, padded.
pub fn first_tile_archive() -> Vec<u8> {
    use leviculum_mapcut::pmtiles::{padded, Archive};
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../leviculum-mapcut/fixtures/grunewald.pmtiles");
    let archive = Archive::open(&path).expect("the fixture");
    let z15 = archive
        .tiles()
        .expect("tiles")
        .into_iter()
        .find(|t| t.z == 15)
        .expect("a z15 tile");
    let mvt = archive.mvt(z15).expect("read").expect("the tile");
    let meta = archive.metadata().expect("metadata");
    padded(&mvt, &FIRST_TILE_BOX, 10..=15, FIRST_TILE_PAD, &meta).expect("the archive")
}

/// The archive's bytes of one tile of [`first_tile_archive`]: the tile,
/// its pad, and the pad field's tag and length.
pub fn first_tile_blob() -> u64 {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../leviculum-mapcut/fixtures/grunewald.pmtiles");
    let archive = leviculum_mapcut::pmtiles::Archive::open(&path).expect("the fixture");
    let z15 = archive
        .tiles()
        .expect("tiles")
        .into_iter()
        .find(|t| t.z == 15)
        .expect("a z15 tile");
    let mvt = archive.mvt(z15).expect("read").expect("the tile");
    (mvt.len() + FIRST_TILE_PAD + 3) as u64
}

/// The region files of `maps` with their zooms and sizes, by name.
pub fn region_files(maps: &Path) -> Vec<(String, (u8, u8), u64)> {
    let mut out: Vec<(String, (u8, u8), u64)> = std::fs::read_dir(maps)
        .expect("maps")
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let file = std::fs::File::open(e.path()).ok()?;
            let len = file.metadata().ok()?.len();
            let region = leviculum_map::region::Region::open(file).ok()?;
            let h = region.header();
            Some((name, (h.min_zoom, h.max_zoom), len))
        })
        .collect();
    out.sort();
    out
}

/// Whether a region of `maps` not named in `old` holds a tile of `view`
/// at `z`.
pub fn arrived(maps: &Path, old: &[String], view: &Viewport, z: u8) -> bool {
    let b = view.bounds();
    std::fs::read_dir(maps).expect("maps").any(|e| {
        let Ok(e) = e else { return false };
        if old.contains(&e.file_name().to_string_lossy().into_owned()) {
            return false;
        }
        let Ok(file) = std::fs::File::open(e.path()) else {
            return false;
        };
        leviculum_map::region::Region::open(file)
            .ok()
            .is_some_and(|r| r.arrived_in(&b, z).unwrap_or(false))
    })
}

/// Cut the start region at full speed from the archive at `url` into
/// `maps`: the screen of `size` at z16 around `at` and 600 pixels past it,
/// so at rest nothing is next to it (`AHEAD_PX`) and what loads after a
/// gesture is for it alone; z13 to z15, named by hand as `start.lmr`, so
/// it is no cache. Its header.
pub fn start_region(maps: &Path, url: &str, at: World, size: (u32, u32)) -> Header {
    let mut loader = Loader::new(
        Maps::new(Some(maps.to_path_buf())),
        Source::Url(url.to_string()),
        1 << 30,
    );
    let grown = Viewport::new(at, 16, size.0 + 1200, size.1 + 1200);
    loader.ask(
        loquax::ui::map::MapWant {
            bounds: grown.bounds(),
            zooms: (13, 15),
            floor: 15,
            reason: loquax::ui::map::LoadReason::Place,
            centre: at,
        },
        0,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let landed = loop {
        let steps = loader.poll(0);
        if let Some(step) = steps.iter().find(|s| !matches!(s, Step::Started(_))) {
            break format!("{step:?}");
        }
        assert!(Instant::now() < deadline, "the start region never landed");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(landed.starts_with("Done"), "{landed}");
    let files = region_files(maps);
    let auto: Vec<_> = files.iter().filter(|f| f.0.starts_with("auto-")).collect();
    assert_eq!(auto.len(), 1, "{files:?}");
    std::fs::rename(maps.join(&auto[0].0), maps.join("start.lmr")).expect("by hand");
    *Region::open(std::fs::File::open(maps.join("start.lmr")).expect("open"))
        .expect("the start region")
        .header()
}

/// A loader step as the walks keep it (1616): when on the walk's clock,
/// what it did, which box.
pub type Stepped = (u64, StepKind, MapWant);

/// Print the walk `name`'s loader steps, each from the uncovered frame at
/// `uncovered_ms`.
pub fn print_steps(name: &str, steps: &[Stepped], uncovered_ms: u64) {
    for (at, kind, want) in steps {
        eprintln!(
            "{name}: step {:>6} ms {kind:?} {} z{}-{}",
            *at as i64 - uncovered_ms as i64,
            want.reason.word(),
            want.zooms.0,
            want.zooms.1
        );
    }
}

/// Why the loader's `steps` break the screen-first order of 1527, from
/// the uncovered frame at `uncovered_ms` (1616): each cell started after
/// it before the screen's box asked last had landed. A box of the screen
/// the view left runs on (a load cannot stop) and a cell may start beside
/// it once the box of the view now has landed; a cell started at the
/// uncovered frame or before is the pan's ahead (1292) and may come first.
/// Read from the loader's order, not from the disk: a cell started before
/// the uncovered frame writes its file after it on a host a parallel build
/// starves (1613).
pub fn out_of_order(steps: &[Stepped], uncovered_ms: u64) -> Vec<String> {
    // The screen's box asked last, and whether it has landed.
    let mut last: Option<(&MapWant, bool)> = None;
    let mut wrong = Vec::new();
    for (at, kind, want) in steps {
        let screen = want.reason == LoadReason::Screen;
        match (kind, screen) {
            (StepKind::Started, true) => last = Some((want, false)),
            (StepKind::Done | StepKind::Failed, true) => {
                if let Some((asked, ended)) = &mut last {
                    *ended |= *asked == want;
                }
            }
            (StepKind::Started, false) if *at > uncovered_ms => {
                if let Some((asked, false)) = last {
                    wrong.push(format!(
                        "a cell z{}-{} started {} ms after the uncovered frame \
                         while the screen's box z{}-{} had not landed",
                        want.zooms.0,
                        want.zooms.1,
                        at - uncovered_ms,
                        asked.zooms.0,
                        asked.zooms.1
                    ));
                }
            }
            _ => {}
        }
    }
    wrong
}
