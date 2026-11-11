//! The committed Florence region: read in place, refused whole when cut
//! short or corrupted (an error, never a panic), and drawn at three zooms
//! with pinned counts, as `leviculum-mapfill/tests/fixture.rs` pins the
//! single-view fixture.

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use leviculum_map::draw::{draw, draw_in, draw_tiles, Ink};
use leviculum_map::mercator::{project, x_of_lon, y_of_lat, LatLon};
use leviculum_map::region::{Entry, Region, RegionError, TileId, ENTRY_LEN, HEADER_LEN_2};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Profile, Style, BACKGROUND};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

/// On the seam of the region's two z15 tiles, inside its box.
const CENTRE: LatLon = LatLon {
    lat_e7: 437_750_000,
    lon_e7: 112_610_000,
};

fn render(region: &Region<&[u8]>, zoom: u8, fb: &mut [u16]) -> (usize, RenderStats) {
    let view = Viewport::new(project(CENTRE), zoom, 240, 320);
    let area = region.area(&view);
    draw_view(region, &view, area, fb)
}

/// `view` of `region` drawn into `area` of a frame cleared to the
/// background, as a frontend draws it over the outline.
fn draw_view(
    region: &Region<&[u8]>,
    view: &Viewport,
    area: Rectangle,
    fb: &mut [u16],
) -> (usize, RenderStats) {
    let (w, h) = view.size();
    let tiles = region.view(view).expect("tiles");
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(fb, w, h).expect("frame");
    let Ok(()) = frame.clear(BACKGROUND);
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(st) = draw_in(
        &tiles,
        area,
        Profile::Full,
        Ink::Colour,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    (tiles.len(), st)
}

fn not_background(fb: &mut [u16]) -> u32 {
    let frame = Frame::new(fb, 240, 320).expect("frame");
    let mut n = 0;
    for y in 0..320 {
        for x in 0..240 {
            if frame.pixel(x, y) != Some(BACKGROUND) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn the_fixture_opens_and_verifies() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    r.verify().expect("verify");
    let h = r.header();
    assert_eq!((h.min_zoom, h.max_zoom, h.profile, h.tiles), (11, 15, 1, 6));
    assert_eq!(h.len, FLORENCE.len() as u64);
    assert_eq!(r.attribution(), "© OpenStreetMap contributors");
    assert!(r.notice().contains("Open Database License (ODbL) 1.0"));
    assert!(r.notice().contains("Protomaps Basemap 3.7.1"));
    let z15 = TileId {
        z: 15,
        x: 17408,
        y: 11943,
    };
    assert!(r.find(z15).expect("find").is_some());
    assert!(r.find(TileId { y: 11944, ..z15 }).expect("find").is_none());
    assert!(r.fixture(z15).expect("tile").is_some());
}

/// The numbers a change to the cut, the filler, the stroker, the draw
/// order or the line widths moves. Background is the earth colour, so a
/// pixel counted in the last column is one some feature other than earth
/// painted. The per-zoom widths (1065) left z15 alone and thinned z13 from
/// (10_732, 15_986, 40_763) stroked and 36_187 painted, z11 from
/// (3044, 4717, 21_777) and 22_397. The draw into the region's box (1077)
/// culls what lies past the box and paints nothing there: z15 from 282
/// culled, (1220, 3200, 151_645) filled, (2353, 7415, 14_735) stroked and
/// 63_842 painted; z13 from 126, (1260, 2266, 78_167), (10_651, 15_332,
/// 33_138) and 36_072; z11 from 79, (1832, 2809, 59_154), (2952, 3632,
/// 9574) and 20_279, the box being about 28 x 12 pixels there. The
/// secondary roads apart from the major ones (1099) stroke a step thinner
/// below z15: z13 from (1375, 1654, 3313), z11 from (21, 20, 40).
#[test]
fn florence_region_draws_three_zooms() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let mut fb = vec![0u16; 240 * 320];
    let mut rows = Vec::new();
    for zoom in [15, 13, 11] {
        let (tiles, st) = render(&r, zoom, &mut fb);
        assert_eq!(st.overflows, 0, "z{zoom}");
        rows.push((
            zoom,
            tiles,
            st.features,
            st.culled,
            st.vertices,
            (st.fill.edges, st.fill.spans, st.fill.pixels),
            (st.stroke.edges, st.stroke.spans, st.stroke.pixels),
            not_background(&mut fb),
        ));
    }
    assert_eq!(
        rows,
        [
            (
                15,
                2,
                617,
                435,
                4868,
                (727, 2160, 106_801),
                (1513, 4992, 9348),
                44_160
            ),
            (
                13,
                1,
                528,
                458,
                10_301,
                (282, 375, 11_509),
                (1375, 1646, 3245),
                5198
            ),
            (11, 1, 325, 310, 5365, (8, 26, 627), (21, 17, 25), 308),
        ]
    );
}

/// The drawn data ends at the region's box, the bbox it was cut with, at
/// every zoom, within one pixel of that zoom: its tiles cover the box
/// rounded out to each zoom's grid, one z11 tile reaching far past it, and
/// drawn whole they moved the edge as the reader zoomed (1077, Lew: the
/// Frankfurt rectangle wandered and changed shape). Drawn whole, the z11
/// edge is off by tens of pixels, which the test checks too, so it can
/// tell the two apart.
#[test]
fn the_data_edge_is_the_regions_box_at_every_zoom() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let b = r.header().bounds;
    let box_centre = LatLon {
        lat_e7: (b.north + b.south) / 2,
        lon_e7: (b.west + b.east) / 2,
    };
    // West, north, east, south of the box in world units.
    let edges = [
        x_of_lon(b.west),
        y_of_lat(b.north),
        x_of_lon(b.east),
        y_of_lat(b.south),
    ];
    let (w, h) = (600u32, 400u32);
    let mut fb = vec![0u16; (w * h) as usize];
    // The inked pixels' outer edges in world units: west, north, east,
    // south.
    let drawn = |fb: &mut [u16], view: &Viewport| {
        let frame = Frame::new(fb, w, h).expect("frame");
        let inked = |x: u32, y: u32| frame.pixel(x, y) != Some(BACKGROUND);
        let col = |x: &u32| (0..h).any(|y| inked(*x, y));
        let row = |y: &u32| (0..w).any(|x| inked(x, *y));
        let left = (0..w).find(col).expect("ink") as i32;
        let right = (0..w).rev().find(col).expect("ink") as i32 + 1;
        let top = (0..h).find(row).expect("ink") as i32;
        let bottom = (0..h).rev().find(row).expect("ink") as i32 + 1;
        let (nw, se) = (view.to_world(left, top), view.to_world(right, bottom));
        [nw.x, nw.y, se.x, se.y]
    };
    for zoom in [11, 13, 15] {
        let view = Viewport::new(project(box_centre), zoom, w, h);
        let pixel = 1i64 << (24 - u32::from(zoom));
        let area = r.area(&view);
        assert!(area.size.width < w && area.size.height < h, "z{zoom}");
        draw_view(&r, &view, area, &mut fb);
        let got = drawn(&mut fb, &view);
        for (side, (g, e)) in got.iter().zip(edges).enumerate() {
            let off = (i64::from(*g) - i64::from(e)).abs();
            assert!(
                off <= pixel,
                "z{zoom} side {side}: {off} units, a pixel is {pixel}"
            );
        }
        if zoom == 11 {
            let all = Rectangle::new(Point::zero(), Size::new(w, h));
            draw_view(&r, &view, all, &mut fb);
            let whole = drawn(&mut fb, &view);
            let worst = whole
                .iter()
                .zip(edges)
                .map(|(g, e)| (i64::from(*g) - i64::from(e)).abs())
                .max()
                .unwrap_or(0);
            assert!(worst > 10 * pixel, "unclipped z11 edge off by {worst}");
        }
    }
}

/// The same view read through `Storage` into buffers, as a file or a card
/// is read, is the same set of tiles as the in-place one.
#[test]
fn a_view_read_into_buffers_matches_the_view_in_place() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let mut read = Vec::new();
    for zoom in [11, 13, 15, 16] {
        let view = Viewport::new(project(CENTRE), zoom, 240, 320);
        r.read_view(&view, &mut read).expect("read");
        let buffered = draw_tiles(&read).expect("tiles");
        let in_place = r.view(&view).expect("tiles");
        assert_eq!(buffered.len(), in_place.len(), "z{zoom}");
        for (a, b) in buffered.iter().zip(&in_place) {
            assert_eq!(a.tile, b.tile);
            assert_eq!(a.fixture.len(), b.fixture.len());
        }
    }
}

#[test]
fn every_truncation_is_an_error() {
    for cut in 0..FLORENCE.len() {
        let r = Region::from_bytes(&FLORENCE[..cut]);
        assert!(r.is_err(), "cut at {cut} opened");
    }
}

/// Open, verify, and draw every tile at every zoom: whatever the bytes,
/// an error or a picture, never a panic.
fn survives(bytes: &[u8]) -> Result<(), RegionError> {
    let r = Region::from_bytes(bytes)?;
    let verified = r.verify();
    // The names, as a frontend reads them: every section, every string.
    for n in 0..r.header().labels.map_or(0, |l| l.sections) {
        if let Ok(e) = r.label_entry(n) {
            if let Ok(Some(s)) = r.labels(e.id) {
                for (_, p) in s.points() {
                    let _ = r.string(p.string);
                }
                for (_, l) in s.lines() {
                    let _ = r.string(l.string);
                }
            }
        }
    }
    let mut fb = vec![0u16; 240 * 320];
    for zoom in [11, 15, 16] {
        let view = Viewport::new(project(CENTRE), zoom, 240, 320);
        if let Ok(tiles) = r.view(&view) {
            let mut edges = vec![Edge::default(); 4096];
            let mut active = vec![0u16; 4096];
            let mut heads = vec![0u16; 320];
            let mut points = vec![Point::zero(); 4096];
            let mut ring_ends = vec![0u16; 1024];
            let mut frame = Frame::new(&mut fb, 240, 320).expect("frame");
            let mut scratch = Scratch {
                edges: &mut edges,
                heads: &mut heads,
                active: &mut active,
            };
            let mut work = Work {
                points: &mut points,
                ring_ends: &mut ring_ends,
            };
            let Ok(_) = draw(
                &tiles,
                Profile::Full,
                Ink::Colour,
                &mut frame,
                &mut scratch,
                &mut work,
                &mut NoClock,
            );
        }
    }
    verified
}

#[test]
fn corrupted_bytes_are_an_error_not_a_panic() {
    assert_eq!(survives(FLORENCE), Ok(()));
    let h = Region::from_bytes(FLORENCE).expect("open");
    let index = h.header().index as usize;

    let mut b = FLORENCE.to_vec();
    b[0] = b'X';
    assert_eq!(survives(&b).err(), Some(RegionError::Magic));
    let mut b = FLORENCE.to_vec();
    b[4] = 6;
    assert_eq!(survives(&b).err(), Some(RegionError::Version(6)));
    let mut b = FLORENCE.to_vec();
    b[25] = 17;
    assert_eq!(survives(&b).err(), Some(RegionError::Header("zoom")));
    let mut b = FLORENCE.to_vec();
    b.push(0);
    assert_eq!(survives(&b).err(), Some(RegionError::Truncated));

    // Two entries swapped: out of order.
    let mut b = FLORENCE.to_vec();
    let (e0, e1) = (index, index + ENTRY_LEN);
    let first: Vec<u8> = b[e0..e1].to_vec();
    b.copy_within(e1..e1 + ENTRY_LEN, e0);
    b[e1..e1 + ENTRY_LEN].copy_from_slice(&first);
    assert_eq!(survives(&b).err(), Some(RegionError::Entry(1)));

    // An entry pointing past the data, and into the header.
    let entry = |b: &[u8]| {
        let mut e = [0u8; ENTRY_LEN];
        e.copy_from_slice(&b[index..index + ENTRY_LEN]);
        Entry::parse(&e).expect("entry")
    };
    for offset in [index as u64, 3] {
        let mut b = FLORENCE.to_vec();
        let mut e = entry(&b);
        e.offset = offset;
        b[index..index + ENTRY_LEN].copy_from_slice(&e.encode());
        assert_eq!(survives(&b).err(), Some(RegionError::Entry(0)));
    }

    // Bytes flipped across header, text, tiles, the string table and both
    // indexes: never a panic. Most flips inside a tile's geometry are still
    // a valid picture, so only the outcome being reached is asserted.
    for at in (0..FLORENCE.len())
        .step_by(1009)
        .chain(0..HEADER_LEN_2)
        .chain((h.header().labels.map_or(index, |l| l.table as usize)..index).step_by(31))
    {
        let mut b = FLORENCE.to_vec();
        b[at] ^= 0xA5;
        let _ = survives(&b);
    }
}

/// The lines' colour in [`lines`], which nothing else in its frame has.
const INK: embedded_graphics::pixelcolor::Rgb565 =
    embedded_graphics::pixelcolor::Rgb565::new(31, 0, 0);

/// How many pixels of `fb` are [`INK`].
fn inked(fb: &mut [u16], w: u32, h: u32) -> usize {
    let frame = Frame::new(fb, w, h).expect("frame");
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.pixel(x, y) == Some(INK))
        .count()
}

/// The view of `region` in lines alone, through `draw_in` (`None`) or
/// `draw_in_scaled` at `Some(percent)`.
fn lines(region: &Region<&[u8]>, view: &Viewport, percent: Option<u32>) -> Vec<u16> {
    use leviculum_map::draw::draw_in_scaled;
    let (w, h) = view.size();
    let mut fb = vec![0u16; (w * h) as usize];
    let tiles = region.view(view).expect("tiles");
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let (area, ink) = (region.area(view), Ink::Lines(INK));
    let (s, w2, wk) = (&mut scratch, &mut work, &mut NoClock);
    let Ok(_) = match percent {
        None => draw_in(&tiles, area, Profile::Full, ink, &mut frame, s, w2, wk),
        Some(p) => {
            let m9 = Style::M9;
            draw_in_scaled(
                &tiles,
                area,
                Profile::Full,
                ink,
                m9,
                p,
                &mut frame,
                s,
                w2,
                wk,
            )
        }
    };
    fb
}

/// A dense screen's scale: the same view with lines twice as wide inks
/// well over as many pixels, and 100 per cent is the plain draw exactly.
#[test]
fn scaled_lines_are_wider_and_one_hundred_per_cent_is_unchanged() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let view = Viewport::new(project(CENTRE), 15, 240, 320);
    let plain = lines(&r, &view, None);
    assert_eq!(lines(&r, &view, Some(100)), plain);
    let (mut once, mut twice) = (plain, lines(&r, &view, Some(200)));
    let (a, b) = (inked(&mut once, 240, 320), inked(&mut twice, 240, 320));
    assert!(a > 0, "the view has lines");
    assert!(b > a * 3 / 2, "200 per cent inks {b}, 100 inks {a}");
}
