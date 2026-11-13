//! The globe hides its far side and stands on its own ground (1762).
//!
//! Lew's phone test of the globe build (2026-11-12) found the back of the
//! world drawn through the front, the space around the sphere painted in
//! the sea's colour, and, in the handover to Mercator over the southern
//! Indian Ocean, the sea filled as land. The 1710 goldens all faced the
//! front, so none of this showed. These tests hold the mechanism, not a
//! picture: a ring wholly behind the horizon adds no pixel, a ring across
//! it is cut there, what lies off the sphere is ground and not sea, and
//! the view of the second screenshot keeps its sea blue in both shapes.

use embedded_graphics::pixelcolor::raw::{RawData, RawU16};
use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::blend::{Blend, Shape};
use leviculum_map::mercator::{project, LatLon, World as Point};
use leviculum_map::outline::{self, World};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::style::{MapTheme, Style};

const THEME: MapTheme = Style::SCREEN.theme;

/// A colour as the frame holds it: Rgb565, big-endian.
fn raw(c: Rgb565) -> u16 {
    RawU16::from(c).into_inner().to_be()
}

fn land() -> u16 {
    raw(THEME.palette().background)
}

fn sea() -> u16 {
    raw(outline::water(THEME))
}

/// The world view of `world` through `blend`, Rgb565 without smoothing,
/// so a pixel is one colour exactly.
fn picture(world: &World, blend: &Blend) -> Vec<u16> {
    let (w, h) = blend.size();
    let mut fb = vec![0u16; (w * h) as usize];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let Ok(_) = outline::draw_world_view(
        world,
        blend,
        leviculum_map::draw::Ink::Colour,
        THEME,
        &mut frame,
    );
    fb
}

/// A longitude past 180° is taken round the world, so a test ring's
/// edge from 160° to 200° crosses the antimeridian, not the front.
fn at(lat: f64, lon: f64) -> Point {
    let lon = if lon > 180.0 { lon - 360.0 } else { lon };
    project(LatLon::from_degrees(lat, lon))
}

/// The pixel `p` lands on, when it lands in the frame.
fn pixel(fb: &[u16], blend: &Blend, p: Point) -> Option<u16> {
    let (w, h) = blend.size();
    let (x, y) = blend.to_screen(p);
    if x < 0.0 || y < 0.0 || x >= f64::from(w) || y >= f64::from(h) {
        return None;
    }
    Some(fb[(y as usize) * (w as usize) + x as usize])
}

/// A one-ring table of 16-bit coordinates: the polygon through
/// `corners` (latitude, longitude), each edge cut into `steps` pieces so
/// the ring bends with the projection as the coasts do.
fn one_ring(corners: &[(f64, f64)], steps: u32) -> Vec<u8> {
    const BITS: u32 = 16;
    let mut pts: Vec<(u32, u32)> = Vec::new();
    for (i, &(la, lo)) in corners.iter().enumerate() {
        let (lb, ob) = corners[(i + 1) % corners.len()];
        for s in 0..steps {
            let f = f64::from(s) / f64::from(steps);
            let p = at(la + (lb - la) * f, lo + (ob - lo) * f);
            pts.push((p.x >> (32 - BITS), p.y >> (32 - BITS)));
        }
    }
    let varint = |out: &mut Vec<u8>, mut v: u32| loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        match v {
            0 => {
                out.push(b);
                break;
            }
            _ => out.push(b | 0x80),
        }
    };
    // Deltas zigzagged, BITS + 1 bits each, least significant first.
    let width = BITS + 1;
    let mut bits: Vec<bool> = Vec::new();
    for pair in pts.windows(2) {
        for (a, b) in [(pair[0].0, pair[1].0), (pair[0].1, pair[1].1)] {
            let d = i64::from(b) - i64::from(a);
            assert!(
                d.abs() < 1 << (BITS - 1),
                "a step too long for the test table"
            );
            let z = ((d << 1) ^ (d >> 63)) as u32;
            for i in 0..width {
                bits.push(z >> i & 1 == 1);
            }
        }
    }
    let mut deltas = vec![0u8; bits.len().div_ceil(8)];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            deltas[i / 8] |= 1 << (i % 8);
        }
    }
    let (x0, y0) = pts[0];
    let west = pts.iter().map(|p| p.0).min().unwrap_or(0);
    let east = pts.iter().map(|p| p.0).max().unwrap_or(0);
    let north = pts.iter().map(|p| p.1).min().unwrap_or(0);
    let south = pts.iter().map(|p| p.1).max().unwrap_or(0);
    let mut t = b"LWO2".to_vec();
    t.push(BITS as u8);
    varint(&mut t, 1);
    varint(&mut t, (pts.len() as u32) << 1);
    varint(&mut t, deltas.len() as u32);
    for v in [x0, y0, x0 - west, y0 - north, east - x0, south - y0] {
        varint(&mut t, v);
    }
    t.push(width as u8);
    t.extend_from_slice(&deltas);
    t
}

fn land_only(table: &[u8]) -> World<'_> {
    World {
        land: table,
        lakes: &[],
        rivers: &[],
        ice: &[],
        shelves: &[],
        borders: &[],
        disputed: &[],
    }
}

/// The test table reads back as one ring.
#[test]
fn the_test_table_reads_back() {
    let t = one_ring(
        &[(10.0, 10.0), (10.0, 20.0), (-10.0, 20.0), (-10.0, 10.0)],
        4,
    );
    let rings: Vec<_> = outline::rings(&t).collect();
    assert_eq!(rings.len(), 1);
    let pts: Vec<_> = rings[0].points().collect();
    assert_eq!(pts.len(), 16);
    let first = at(10.0, 10.0);
    assert_eq!(pts[0], (first.x >> 16, first.y >> 16));
    let p8 = at(-10.0, 20.0);
    assert_eq!(pts[8], (p8.x >> 16, p8.y >> 16));
}

/// A ring wholly on the far side draws nothing: at the world (`t = 0`)
/// and in the handover. Before 1762 its points were pushed onto the
/// limb, so a ring round the antipode filled the whole disk, and in the
/// handover the Mercator term spread them across the screen.
#[test]
fn a_ring_behind_the_horizon_draws_nothing() {
    // Round the antipode of 0 N 20 W, and a smaller one off to its side.
    let tables = [
        one_ring(
            &[(15.0, 135.0), (15.0, 179.0), (-15.0, 179.0), (-15.0, 135.0)],
            16,
        ),
        one_ring(
            &[(30.0, 100.0), (30.0, 120.0), (10.0, 120.0), (10.0, 100.0)],
            8,
        ),
    ];
    for (i, table) in tables.iter().enumerate() {
        for zoom in [1.0, 2.0, 3.0, 3.5, 4.5] {
            let blend = Blend::new(Shape::Globe, at(0.0, -20.0), zoom, 480, 320);
            let fb = picture(&land_only(table), &blend);
            let n = fb.iter().filter(|&&p| p == land()).count();
            assert_eq!(
                n, 0,
                "ring {i} at z{zoom}: {n} land pixels from behind the horizon"
            );
        }
    }
}

/// A ring across the horizon keeps its near part, cut at the limb: the
/// near inside is land, the near outside sea, nothing of it beyond the
/// limb at `t = 0`.
#[test]
fn a_ring_across_the_horizon_is_cut_at_the_limb() {
    let table = one_ring(
        &[(20.0, 70.0), (20.0, 130.0), (-20.0, 130.0), (-20.0, 70.0)],
        16,
    );
    for zoom in [1.0, 2.0, 3.0, 3.5] {
        let blend = Blend::new(Shape::Globe, at(0.0, 0.0), zoom, 480, 320);
        let fb = picture(&land_only(&table), &blend);
        for (lat, lon, want) in [(0.0, 80.0, true), (10.0, 85.0, true), (0.0, 60.0, false)] {
            if let Some(p) = pixel(&fb, &blend, at(lat, lon)) {
                assert_eq!(
                    p == land(),
                    want,
                    "z{zoom} {lat}/{lon}: land {}",
                    p == land()
                );
            }
        }
        if blend.t() == 0.0 {
            let (w, h) = blend.size();
            let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
            let r = 256.0 * zoom.exp2() / std::f64::consts::TAU;
            for y in 0..h {
                for x in 0..w {
                    let d = (f64::from(x) + 0.5 - cx).hypot(f64::from(y) + 0.5 - cy);
                    if d > r + 1.5 {
                        let p = fb[(y * w + x) as usize];
                        assert_ne!(p, land(), "z{zoom} land beyond the limb at ({x}, {y})");
                    }
                }
            }
        }
    }
}

/// The sphere stands on ground that is not sea: off the limb no pixel
/// has the sea's colour, on it the sea is sea.
#[test]
fn the_globe_stands_on_ground_not_on_sea() {
    for (lat, lon) in [(50.0, 10.0), (-40.0, 75.0), (0.0, -150.0)] {
        let blend = Blend::new(Shape::Globe, at(lat, lon), 1.0, 480, 320);
        let fb = picture(&outline::WORLD, &blend);
        for (x, y) in [
            (0u32, 0u32),
            (479, 0),
            (0, 319),
            (479, 319),
            (5, 160),
            (474, 160),
        ] {
            let p = fb[(y * 480 + x) as usize];
            assert_ne!(p, sea(), "{lat}/{lon}: ({x}, {y}) off the globe is sea");
        }
    }
    // Mid-Atlantic, facing.
    let blend = Blend::new(Shape::Globe, at(20.0, -30.0), 1.0, 480, 320);
    let fb = picture(&outline::WORLD, &blend);
    assert_eq!(pixel(&fb, &blend, at(25.0, -40.0)), Some(sea()));
}

/// Lew's second screenshot (2026-11-12 22:21): from the globe over the
/// southern Indian Ocean, centre about 40 S 75 E, in the handover. The
/// sea between Africa and Australia was filled in the land's colour with
/// lens-shaped slivers, and a wedge spanned from Kerguelen to Antarctica.
/// Sea points stay sea, land points land, at every zoom of the handover,
/// in both shapes and at the phone's portrait size.
#[test]
fn the_southern_indian_ocean_stays_sea_through_the_handover() {
    let sea_points = [
        (-42.0, 75.0),
        (-30.0, 85.0),
        (-45.0, 95.0),
        (-25.0, 65.0),
        (-50.0, 110.0),
        (-35.0, 100.0),
        (-20.0, 90.0),
        (-58.0, 70.0),
        (-60.0, 90.0),
        (-62.0, 40.0),
    ];
    let land_points = [(-25.0, 25.0), (-25.0, 135.0), (-20.0, 46.5)];
    let mut bad = Vec::new();
    for shape in [Shape::Globe, Shape::EqualEarth] {
        for (w, h) in [(1224u32, 2100u32), (480, 320)] {
            for zoom in [1.0, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5] {
                let blend = Blend::new(shape, at(-40.0, 75.0), zoom, w, h);
                let fb = picture(&outline::WORLD, &blend);
                for &(lat, lon) in &sea_points {
                    if !blend.facing(at(lat, lon)) {
                        continue;
                    }
                    if let Some(p) = pixel(&fb, &blend, at(lat, lon)) {
                        if p != sea() {
                            bad.push(format!("{shape:?} {w}x{h} z{zoom} sea {lat}/{lon}"));
                        }
                    }
                }
                for &(lat, lon) in &land_points {
                    if !blend.facing(at(lat, lon)) {
                        continue;
                    }
                    if let Some(p) = pixel(&fb, &blend, at(lat, lon)) {
                        if p != land() {
                            bad.push(format!("{shape:?} {w}x{h} z{zoom} land {lat}/{lon}"));
                        }
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "wrong fills:\n{}", bad.join("\n"));
}

/// Near the south pole Antarctica is whole: across the meridian the
/// projection is cut on (the view's far meridian, which near a pole is
/// on the near side) it is neither torn nor cancelled by a second copy,
/// and the sea north of it stays sea.
#[test]
fn near_the_pole_antarctica_is_whole() {
    let mut bad = Vec::new();
    for shape in [Shape::Globe, Shape::EqualEarth] {
        for zoom in [1.0, 2.0, 2.5, 3.0] {
            let blend = Blend::new(shape, at(-70.0, 0.0), zoom, 480, 320);
            let fb = picture(&outline::WORLD, &blend);
            for (lat, lon) in [
                (-80.0, 180.0),
                (-80.0, 170.0),
                (-80.0, -170.0),
                (-82.0, 90.0),
                (-75.0, 0.0),
                (-88.0, 0.0),
                (-87.0, 120.0),
            ] {
                if let Some(p) = pixel(&fb, &blend, at(lat, lon)) {
                    if p == sea() {
                        bad.push(format!("{shape:?} z{zoom} {lat}/{lon} is sea"));
                    }
                }
            }
            for (lat, lon) in [(-60.0, 0.0), (-55.0, -30.0)] {
                if let Some(p) = pixel(&fb, &blend, at(lat, lon)) {
                    if p != sea() {
                        bad.push(format!("{shape:?} z{zoom} {lat}/{lon} is not sea"));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A ring across the meridian the projection is cut on (the far side of
/// the view, 170 W here, on the near side past the pole) is filled once: before
/// 1762 the drawer added a second copy a turn over, which on the
/// periodic globe lay exactly on the first and the even-odd fill
/// cancelled both.
#[test]
fn a_ring_across_the_far_meridian_is_filled_once() {
    let table = one_ring(
        &[
            (-74.0, -178.0),
            (-74.0, -160.0),
            (-80.0, -160.0),
            (-80.0, -178.0),
        ],
        16,
    );
    for zoom in [1.0, 2.0, 3.0] {
        let blend = Blend::new(Shape::Globe, at(-70.0, 10.0), zoom, 480, 320);
        let fb = picture(&land_only(&table), &blend);
        for (lat, lon) in [(-77.0, -175.0), (-77.0, -170.0), (-77.0, -165.0)] {
            if let Some(p) = pixel(&fb, &blend, at(lat, lon)) {
                assert_eq!(p, land(), "z{zoom} {lat}/{lon} not land");
            }
        }
    }
}
