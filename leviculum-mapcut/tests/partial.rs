//! A region read while it arrives (1291): the cut writes version 4 in
//! place, a round of tiles at a time, and a reader opening the file at any
//! moment, or a copy of it cut short anywhere, uses every tile that is
//! there whole, says the rest are not here yet, and never reads half a
//! tile.

use std::cell::RefCell;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::rc::Rc;

use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::{check_tile, Bounds, Region, RegionError, Slot};
use leviculum_map::viewport::Viewport;
use leviculum_mapcut::cut::{cut, cut_streamed, CutOptions, Progress};
use leviculum_mapcut::pmtiles::{Archive, RangeRead};
use leviculum_mapcut::Res;

const FIXTURE: &[u8] = include_bytes!("../fixtures/oltrarno.pmtiles");

struct InMemory;

impl RangeRead for InMemory {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let at = (offset as usize).min(FIXTURE.len());
        let end = at.saturating_add(len as usize).min(FIXTURE.len());
        Ok(FIXTURE[at..end].to_vec())
    }
}

fn options(round: usize) -> CutOptions {
    CutOptions {
        bounds: Bounds {
            west: 112_615_000,
            south: 437_405_000,
            east: 112_825_000,
            north: 437_545_000,
        },
        zooms: 0..=15,
        profile: 1,
        attribution: "© OpenStreetMap contributors".to_owned(),
        notice: "test".to_owned(),
        threads: 2,
        locale: None,
        buildings: true,
        pois: true,
        round,
    }
}

/// A file the cut writes and a test reads between its rounds.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Cursor<Vec<u8>>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().write(b)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for Shared {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.0.borrow_mut().seek(to)
    }
}

/// What a reader of `bytes` sees: the slots that hold their tile (or
/// say nothing is there), the ones pending, how many of those a coarser
/// tile stands in for, and whether the file is complete; every tile there
/// is read and checked whole, and a view over the box draws.
fn seen(bytes: &[u8]) -> Result<(u32, u32, u32, bool), RegionError> {
    let r = Region::from_bytes(bytes)?;
    let (mut here, mut pending, mut stood) = (0, 0, 0);
    for n in 0..r.header().tiles {
        match r.slot(n)? {
            (id, Slot::Here(e)) => {
                let end = (e.offset + u64::from(e.len)) as usize;
                assert!(end <= bytes.len(), "a slot past the bytes there");
                check_tile(id, &bytes[e.offset as usize..end])?;
                here += 1;
            }
            (id, Slot::Pending) => {
                pending += 1;
                if let Some((e, dz)) = r.stand_in(id)? {
                    assert_eq!(
                        (e.id.z, e.id.x, e.id.y),
                        (id.z - dz, id.x >> dz, id.y >> dz)
                    );
                    stood += 1;
                }
            }
            (_, Slot::Empty) => here += 1,
        }
    }
    if !r.is_complete() {
        // The names and the buildings wait for the whole file.
        assert!(r.header().labels.is_none());
        assert_eq!(r.building_sections(), 0);
        assert_eq!(r.verify().map(|_| ()).is_err(), pending > 0);
    } else {
        r.verify()?;
    }
    let centre = project(LatLon::from_degrees(43.7675, 11.2525));
    for z in 12..=17 {
        r.view(&Viewport::new(centre, z, 512, 512))?;
    }
    Ok((here, pending, stood, r.is_complete()))
}

#[test]
fn a_region_cut_in_place_reads_after_every_round() {
    let archive = Archive::from_source(InMemory).expect("open");
    let mut whole = Cursor::new(Vec::new());
    cut(&archive, &options(256), &mut whole).expect("cut");
    let whole = whole.into_inner();

    let file = Shared::default();
    let mut snaps: Vec<(Progress, Vec<u8>)> = Vec::new();
    let mine = file.clone();
    cut_streamed(&archive, &options(1), file.clone(), &mut |p| {
        snaps.push((p, mine.0.borrow().get_ref().clone()));
    })
    .expect("cut");
    // The round only says when a reader sees the tiles, not what is
    // written.
    assert_eq!(*file.0.borrow().get_ref(), whole);
    assert!(
        snaps.len() >= 4,
        "{} rounds of {:?}",
        snaps.len(),
        snaps.last().map(|s| s.0)
    );

    let mut last = 0;
    let mut states = Vec::new();
    let mut stood_in = 0;
    for (p, bytes) in &snaps {
        let (here, pending, stood, complete) = seen(bytes).expect("a file still arriving reads");
        stood_in += stood;
        assert_eq!(complete, p.complete, "{p:?}");
        assert_eq!(here as usize, p.done, "{p:?}");
        assert_eq!((here + pending) as usize, p.tiles);
        assert!(here >= last, "coverage only grows");
        last = here;
        states.push(here);
    }
    states.dedup();
    assert!(states.len() >= 3, "{states:?}");
    assert!(stood_in > 0, "a z15 tile not there yet drew from its z14");
    // Coarse first: the zooms arrive in order, the shallowest first.
    let zooms: Vec<u8> = snaps.iter().map(|s| s.0.zoom).collect();
    assert!(zooms.windows(2).all(|w| w[0] <= w[1]), "{zooms:?}");
    let first = Region::from_bytes(&snaps[0].1).expect("open");
    assert!(!first.is_complete());
    assert_eq!(
        first.slot(0).expect("slot").0.z,
        *zooms.first().unwrap_or(&0)
    );
    println!("rounds {} coverage {states:?}", snaps.len());
}

#[test]
fn a_region_cut_short_anywhere_reads_what_arrived() {
    let archive = Archive::from_source(InMemory).expect("open");
    let mut whole = Cursor::new(Vec::new());
    cut(&archive, &options(256), &mut whole).expect("cut");
    let whole = whole.into_inner();
    let r = Region::from_bytes(&whole).expect("open");
    let index_end = r.header().index as usize + r.header().tiles as usize * 20;

    let mut at: Vec<usize> = (0..whole.len()).step_by(whole.len() / 211 + 1).collect();
    at.extend([
        3,
        64,
        111,
        112,
        index_end - 1,
        index_end,
        index_end + 1,
        whole.len() - 1,
        whole.len(),
    ]);
    at.sort_unstable();
    at.dedup();
    let mut last_pending = u32::MAX;
    let mut opened = 0;
    for &n in &at {
        match seen(&whole[..n]) {
            Ok((_, pending, _, complete)) => {
                opened += 1;
                assert!(n >= index_end, "opened at {n} before the slots were there");
                assert_eq!(complete, n == whole.len(), "at {n}");
                assert!(
                    pending <= last_pending,
                    "at {n}: {pending} > {last_pending}"
                );
                last_pending = pending;
            }
            Err(e) => {
                assert!(n < index_end, "at {n}: {e:?}");
                assert!(matches!(e, RegionError::Truncated), "at {n}: {e:?}");
            }
        }
    }
    assert!(opened > 100, "{opened} of {}", at.len());
    assert_eq!(last_pending, 0);
}

/// A round ends where its zoom does, and holds at most the round's tiles:
/// the shallow zooms reach a reader each on their own.
#[test]
fn a_round_ends_with_its_zoom() {
    use leviculum_map::region::TileId;
    use leviculum_mapcut::cut::rounds;
    let t = |z, x| TileId { z, x, y: 0 };
    let ids = [
        t(0, 0),
        t(1, 0),
        t(1, 1),
        t(2, 0),
        t(2, 1),
        t(2, 2),
        t(2, 3),
    ];
    let zooms = |r: &[TileId]| r.iter().map(|t| (t.z, t.x)).collect::<Vec<_>>();
    let got: Vec<_> = rounds(&ids, 3).into_iter().map(zooms).collect();
    assert_eq!(
        got,
        [
            vec![(0, 0)],
            vec![(1, 0), (1, 1)],
            vec![(2, 0), (2, 1), (2, 2)],
            vec![(2, 3)],
        ]
    );
    assert!(rounds(&[], 3).is_empty());
    assert_eq!(rounds(&ids, 0).len(), ids.len(), "a round of 0 is one tile");
}
