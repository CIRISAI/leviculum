//! LMR3: the building sections a region carries (1191). A section round
//! trips through its writer and reader at the format's extremes: an empty
//! tile, a footprint at the vertex cap and one over it, more footprints
//! than a `u16` counts, steps that need the escape, holes, coordinates at
//! both ends of the range. A truncated or flipped section is an error,
//! never a panic. A version 2 region still reads, without buildings, and a
//! version 3 header is refused by the version 2 reader's check.

use leviculum_map::buildings::{
    cell_of, encode_section, Bbox, BuildingError, Buildings, CELLS, CELL_LEN, COORDS, HEAD_LEN,
    MAX_RINGS, MAX_VERTICES,
};
use leviculum_map::region::{
    BuildingTables, Header, Region, RegionError, HEADER_LEN, HEADER_LEN_3, VERSION_2, VERSION_3,
};

/// The cut of the Florence region before 1291, frozen: the version 2 sample
/// the old-version reading tests hold to (1707; the live fixture is
/// version 4 now).
const OLD_V2: &[u8] = include_bytes!("../fixtures/old/firenze-v2.lmr");

type Footprint = Vec<Vec<(i16, i16)>>;

fn square(x: i16, y: i16, side: i16) -> Vec<(i16, i16)> {
    vec![(x, y), (x + side, y), (x + side, y + side), (x, y + side)]
}

/// Every footprint read back, by cell, each a list of rings.
fn read_back(b: &Buildings<'_>) -> Vec<Vec<Footprint>> {
    b.cells()
        .map(|c| {
            c.footprints()
                .map(|f| {
                    let f = f.expect("footprint");
                    let rings: Footprint = f.rings().map(|r| r.points().collect()).collect();
                    assert_eq!(usize::from(f.rings), rings.len());
                    assert_eq!(
                        usize::from(f.vertices),
                        rings.iter().map(Vec::len).sum::<usize>()
                    );
                    rings
                })
                .collect()
        })
        .collect()
}

/// The footprints sorted into cells as the writer sorts them.
fn by_cell(fps: &[Footprint]) -> Vec<Vec<Footprint>> {
    let mut cells = vec![Vec::new(); CELLS];
    for f in fps {
        let pts = f.iter().flatten();
        let b = Bbox {
            x0: pts.clone().map(|p| p.0).min().expect("x"),
            y0: pts.clone().map(|p| p.1).min().expect("y"),
            x1: pts.clone().map(|p| p.0).max().expect("x"),
            y1: pts.map(|p| p.1).max().expect("y"),
        };
        cells[cell_of(&b)].push(f.clone());
    }
    cells
}

#[test]
fn a_building_section_round_trips_at_its_extremes() {
    let mut courtyard = vec![square(1100, 1100, 300)];
    let mut hole = square(1200, 1200, 100);
    hole.reverse();
    courtyard.push(hole);
    // Steps of every size: inside i8, at its edges (127 and -128 for dy,
    // -127 for dx, the escape byte being -128, so a dx of -128 escapes),
    // and far past it.
    let steps = vec![vec![
        (2000, 2000),
        (2127, 1872),
        (2000, 1999),
        (-4096, 8191),
        (8191, -4096),
        (2001, 2127),
        (1873, 2127),
    ]];
    let fps: Vec<Footprint> = vec![
        vec![square(10, 10, 20)],
        courtyard,
        steps,
        // Across the cell edge at 1024: in the cell of its centre alone.
        vec![square(1000, 3000, 60)],
        // At both ends of the coordinate range, in the clamped corner cells.
        vec![square(-4096, -4096, 3)],
        vec![square(8188, 8188, 3)],
        vec![square(20, 40, 5)],
    ];
    let bytes = encode_section(&fps).expect("encode");
    let b = Buildings::parse(&bytes).expect("parse");
    b.check().expect("check");
    assert_eq!(b.len(), fps.len() as u32);
    assert_eq!(read_back(&b), by_cell(&fps));
    // The first and last footprint share cell 0 and keep their order.
    assert_eq!(b.cell(0).expect("cell").count, 3);
    // The footprint across the cell edge: its centre 1030, 3030 lies in
    // column 1, row 2, so cell 9.
    assert_eq!(b.cell(9).expect("cell").count, 1);
    assert!(b.cell(CELLS).is_none());
    // Its cell's box reaches over the edge into column 0, so a view of
    // column 0 alone reads that cell too.
    let column0 = Bbox {
        x0: 0,
        y0: 2048,
        x1: 1023,
        y1: 3071,
    };
    let read: Vec<u8> = b.touching(column0).map(|c| c.index).collect();
    assert!(read.contains(&9), "{read:?}");
}

#[test]
fn an_empty_tile_has_a_head_and_nothing_else() {
    let bytes = encode_section(&[]).expect("encode");
    assert_eq!(bytes.len(), HEAD_LEN);
    assert!(bytes.iter().all(|&b| b == 0));
    let b = Buildings::parse(&bytes).expect("parse");
    assert!(b.is_empty());
    assert_eq!(b.len(), 0);
    assert!(b.cells().all(|c| c.footprints().next().is_none()));
    assert_eq!(b.touching(Bbox::of_tile(15, 0, 0)).count(), 0);
}

#[test]
fn a_footprint_over_the_cap_is_refused_and_one_at_it_round_trips() {
    // A ring at the vertex cap: its head takes the two-byte varint.
    let ring = |n: usize| -> Vec<(i16, i16)> {
        (0..n)
            .map(|i| ((i % 64) as i16 * 4, (i / 64) as i16 * 4 + (i % 2) as i16))
            .collect()
    };
    let at_cap: Footprint = vec![ring(MAX_VERTICES)];
    let bytes = encode_section(std::slice::from_ref(&at_cap)).expect("at the cap");
    let b = Buildings::parse(&bytes).expect("parse");
    assert_eq!(read_back(&b), by_cell(&[at_cap]));

    let over = [vec![ring(MAX_VERTICES + 1)]];
    assert_eq!(
        encode_section(&over).err(),
        Some(BuildingError::Record { cell: 0, n: 0 })
    );
    let too_many_rings = [vec![square(0, 0, 3); MAX_RINGS + 1]];
    assert!(encode_section(&too_many_rings).is_err());
    let no_ring: [Footprint; 1] = [vec![]];
    assert!(encode_section(&no_ring).is_err());
    let two_vertices = [vec![square(10, 10, 20)], vec![vec![(0, 0), (5, 5)]]];
    assert_eq!(
        encode_section(&two_vertices).err(),
        Some(BuildingError::Record { cell: 0, n: 1 })
    );
    // Out of range coordinates are not i16 values the writer can be given
    // past 8191 and below -4096 only: both refused.
    assert!(encode_section(&[vec![square(8190, 0, 3)]]).is_err());
    assert!(encode_section(&[vec![square(-4097, 0, 3)]]).is_err());
    assert_eq!(COORDS, -4096..8192);
}

/// The study's worst z15 tile holds 2,228 buildings; the format counts a
/// cell in a `u32`, so a tile far past a `u16`'s worth still round trips.
#[test]
fn a_tile_over_a_u16_of_footprints_round_trips() {
    let fps: Vec<Footprint> = (0..70_000u32)
        .map(|i| {
            vec![square(
                (i % 256 * 16) as i16,
                (i / 256 % 256 * 16) as i16,
                3,
            )]
        })
        .collect();
    let bytes = encode_section(&fps).expect("encode");
    let b = Buildings::parse(&bytes).expect("parse");
    assert_eq!(b.len(), 70_000);
    assert_eq!(read_back(&b), by_cell(&fps));
}

#[test]
fn a_corrupt_section_is_an_error_never_a_panic() {
    let fps: Vec<Footprint> = (0..40)
        .map(|i| vec![square(i * 100, i * 97, 30 + i)])
        .chain([vec![vec![(0, 0), (4000, 0), (4000, 4000), (5, 3)]]])
        .collect();
    let bytes = encode_section(&fps).expect("encode");
    let walk = |b: &[u8]| -> Result<(), BuildingError> {
        let s = Buildings::parse(b)?;
        for c in s.cells() {
            for f in c.footprints() {
                let f = f?;
                for r in f.rings() {
                    let _ = r.points().count();
                }
            }
        }
        Ok(())
    };
    assert_eq!(walk(&bytes), Ok(()));
    // Every truncation: the head no longer runs up to the end.
    for n in 0..bytes.len() {
        assert!(walk(&bytes[..n]).is_err(), "truncated at {n}");
    }
    // Every byte flipped: an error or a still valid section, never a panic.
    for at in 0..bytes.len() {
        let mut b = bytes.clone();
        b[at] ^= 0xA5;
        let _ = walk(&b);
    }
    // A cell whose count says more than its bytes hold.
    let mut b = bytes.clone();
    let c = (0..CELLS)
        .find(|&c| {
            u32::from_le_bytes(b[c * CELL_LEN + 4..c * CELL_LEN + 8].try_into().unwrap()) > 0
        })
        .expect("a cell");
    b[c * CELL_LEN + 4] += 1;
    assert!(matches!(walk(&b), Err(BuildingError::Record { .. })));
    // A cell's box that does not hold its footprints.
    let mut b = bytes.clone();
    b[c * CELL_LEN + 12] = 0;
    b[c * CELL_LEN + 13] = 0;
    let shrunk = Buildings::parse(&b).map(|s| s.check());
    assert!(matches!(shrunk, Ok(Err(_)) | Err(_)), "{shrunk:?}");
    // An empty cell with a box.
    let empty = encode_section(&[]).expect("encode");
    let mut b = empty.clone();
    b[8] = 1;
    assert_eq!(Buildings::parse(&b).err(), Some(BuildingError::Head));
}

#[test]
fn the_cells_a_deeper_tile_reads_are_its_part_of_the_z15_tile() {
    assert_eq!(
        Bbox::of_tile(15, 17408, 11943),
        Bbox {
            x0: 0,
            y0: 0,
            x1: 4096,
            y1: 4096
        }
    );
    // z16 tile (1, 0) of its z15 parent: the right top quarter.
    assert_eq!(
        Bbox::of_tile(16, 2 * 17408 + 1, 2 * 11943),
        Bbox {
            x0: 2048,
            y0: 0,
            x1: 4096,
            y1: 2048
        }
    );
    // One footprint in every cell, none across an edge: a z16 tile reads
    // its four cells, a z17 tile its one (and the touching edges'
    // neighbours only where a box reaches them).
    let fps: Vec<Footprint> = (0..16)
        .map(|c| vec![square((c % 4) * 1024 + 100, (c / 4) * 1024 + 100, 50)])
        .collect();
    let bytes = encode_section(&fps).expect("encode");
    let b = Buildings::parse(&bytes).expect("parse");
    let read = |z: u8, x: u32, y: u32| -> Vec<u8> {
        b.touching(Bbox::of_tile(z, x, y))
            .map(|c| c.index)
            .collect()
    };
    assert_eq!(read(16, 1, 0), [2, 3, 6, 7]);
    assert_eq!(read(17, 3, 3), [15]);
    assert_eq!(read(15, 0, 0).len(), 16);
}

#[test]
fn a_version_2_region_still_reads_without_buildings() {
    let r = Region::from_bytes(OLD_V2).expect("open");
    assert_eq!(r.header().version_of(), VERSION_2);
    assert_eq!(r.header().buildings, None);
    assert_eq!(r.building_sections(), 0);
    r.verify().expect("verify");
    for n in 0..r.header().tiles {
        let id = r.entry(n).expect("entry").id;
        assert!(matches!(r.buildings(id), Ok(None)));
        assert_eq!(r.find_buildings(id), Ok(None));
    }
}

#[test]
fn a_version_3_header_is_refused_by_the_version_2_reader() {
    let r = Region::from_bytes(OLD_V2).expect("open");
    let h = Header {
        buildings: Some(BuildingTables {
            index: 1000,
            sections: 7,
        }),
        ..*r.header()
    };
    let bytes = h.encode();
    assert_eq!(bytes.len(), HEADER_LEN_3);
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), VERSION_3);
    assert_eq!(Header::parse(&bytes), Ok(h));
    let head: &[u8; HEADER_LEN] = bytes[..HEADER_LEN].try_into().expect("head");
    // The check a version 2 reader runs first, at its newest version.
    assert_eq!(
        Header::version_up_to(head, VERSION_2),
        Err(RegionError::Version(VERSION_3))
    );
    assert_eq!(Header::version(head), Ok(VERSION_3));
    // The reserved word after the building count must stay 0.
    let mut b = bytes.clone();
    b[108] = 1;
    assert_eq!(Header::parse(&b), Err(RegionError::Header("reserved")));
}
