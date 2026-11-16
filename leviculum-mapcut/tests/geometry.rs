//! Connected ways stay connected, and the deepest zoom keeps its geometry
//! (1707): the cut's simplification must not open gaps where the data
//! joins ways. Measured on `fixtures/oltrarno.pmtiles` on 2026-11-11
//! (`notizen/2026-11-11-karte-luecken-und-ecken.md` of the review clone):
//! 19 of 182 joined road ends were over half a metre from their partner
//! after the cut, 7.8 tile units at the worst, none in the source; 10 of
//! 39 buffer ends lay over half a metre off the neighbour tile's copy.
//!
//! All distances here are in tile units of the 4096 extent (a z15 unit is
//! 0.22 m in Florence). The rule under test: an end that lies on another
//! line in the source (within half a unit) lies within one unit of it in
//! the cut, two copies of a line in neighbouring tiles' buffers agree to
//! one unit, and the deepest zoom keeps every source vertex to one unit.

use leviculum_map::profile::{self, ZoomLayers};
use leviculum_map::region::{Bounds, Region, TileId};
use leviculum_mapcut::cut::{cut, cut_tile, CutOptions};
use leviculum_mapcut::mvt;
use leviculum_mapcut::pmtiles::Archive;
use leviculum_mapcut::simplify::douglas_peucker;
use leviculum_mapcut::Res;
use leviculum_mapfill::fixture::{Fixture, Kind};
use leviculum_mapfill::style::Class;

const FIXTURE: &[u8] = include_bytes!("../fixtures/oltrarno.pmtiles");

/// The ceiling of [`the_held_cut_keeps_a_measured_share_of_the_z14_road_vertices`].
/// Measured 0.731 (2,358 of 3,227) on 2026-11-12; plain Douglas-Peucker
/// keeps 2,031 (0.629), an anchor test that always says near 2,424
/// (0.751), an edge strip that takes every vertex more. The ceiling lies
/// between the held cut and the nearest of those.
const MAX_Z14_SHARE: f64 = 0.74;

struct InMemory;

impl leviculum_mapcut::pmtiles::RangeRead for InMemory {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let at = (offset as usize).min(FIXTURE.len());
        let end = at.saturating_add(len as usize).min(FIXTURE.len());
        Ok(FIXTURE[at..end].to_vec())
    }
}

fn archive() -> Archive<InMemory> {
    Archive::from_source(InMemory).expect("open")
}

/// The fixture's box (`fixtures/README.md`).
fn bounds() -> Bounds {
    Bounds {
        west: 112_615_000,
        south: 437_405_000,
        east: 112_825_000,
        north: 437_545_000,
    }
}

type P2 = [f64; 2];

fn is_road(c: Class) -> bool {
    matches!(
        c,
        Class::Path | Class::MinorRoad | Class::SecondaryRoad | Class::MajorRoad
    )
}

/// The road polylines of a source tile, in units of the 4096 extent.
fn source_roads(raw: &[u8]) -> Vec<Vec<P2>> {
    let d = mvt::decode(raw).expect("decode");
    let mut out = Vec::new();
    for f in d.features {
        if f.kind != Kind::Line || !is_road(f.class) {
            continue;
        }
        for part in &f.parts {
            for line in part {
                let mut pts: Vec<P2> = Vec::new();
                for p in line {
                    let q = [f64::from(p[0]), f64::from(p[1])];
                    if pts.last() != Some(&q) {
                        pts.push(q);
                    }
                }
                if pts.len() >= 2 {
                    out.push(pts);
                }
            }
        }
    }
    out
}

/// The road polylines of a cut blob, scaled back to the 4096 extent.
fn blob_roads(blob: &[u8]) -> Vec<Vec<P2>> {
    let fx = Fixture::parse(blob).expect("fixture");
    let s = f64::from(1i32 << fx.shift().max(0));
    let mut out = Vec::new();
    for f in fx.features() {
        if f.kind != Kind::Line || !is_road(f.class) {
            continue;
        }
        for r in f.rings() {
            let pts: Vec<P2> = r
                .points()
                .map(|(x, y)| [f64::from(x) * s, f64::from(y) * s])
                .collect();
            if pts.len() >= 2 {
                out.push(pts);
            }
        }
    }
    out
}

fn seg_dist(p: P2, a: P2, b: P2) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
    };
    ((p[0] - (a[0] + t * dx)).powi(2) + (p[1] - (a[1] + t * dy)).powi(2)).sqrt()
}

/// The nearest distance from `p` to any line but `skip`, up to 64 units.
fn near(p: P2, lines: &[Vec<P2>], skip: Option<usize>) -> f64 {
    let mut best = 64.0f64;
    for (i, l) in lines.iter().enumerate() {
        if Some(i) == skip {
            continue;
        }
        for w in l.windows(2) {
            best = best.min(seg_dist(p, w[0], w[1]));
        }
    }
    best
}

fn inside(p: P2) -> bool {
    (0.0..4096.0).contains(&p[0]) && (0.0..4096.0).contains(&p[1])
}

fn simplified(z: u8) -> ZoomLayers {
    profile::layers(profile::CUT_1, z).expect("profile")
}

/// `src` simplified as the cut did before 1707: each line alone by
/// plain Douglas-Peucker at the zoom's profile 1 tolerance, nothing held.
fn plain_cut(src: &[Vec<P2>], z: u8) -> Vec<Vec<P2>> {
    let tol = i64::from(simplified(z).tolerance);
    src.iter()
        .map(|l| {
            let pts: Vec<[i32; 2]> = l.iter().map(|p| [p[0] as i32, p[1] as i32]).collect();
            douglas_peucker(&pts, tol)
                .into_iter()
                .map(|p| [f64::from(p[0]), f64::from(p[1])])
                .collect()
        })
        .collect()
}

/// The joined ends of `src` (an end within half a unit of another line)
/// and what `cut` leaves between the end and the rest: `(end, gap)` per
/// joined end whose line the cut still has.
fn joined_gaps(src: &[Vec<P2>], cut: &[Vec<P2>]) -> Vec<(P2, f64)> {
    let mut out = Vec::new();
    for (i, line) in src.iter().enumerate() {
        let n = line.len();
        for &e in &[line[0], line[n - 1]] {
            if !inside(e) || near(e, src, Some(i)) > 0.5 {
                continue;
            }
            // The cut's line that ends here: ends are kept by the cut.
            let own = cut.iter().enumerate().min_by(|(_, a), (_, b)| {
                let d = |l: &[P2]| {
                    let m = l.len();
                    let at = |q: P2| (q[0] - e[0]).powi(2) + (q[1] - e[1]).powi(2);
                    at(l[0]).min(at(l[m - 1]))
                };
                d(a).total_cmp(&d(b))
            });
            let Some((own, l)) = own else { continue };
            let m = l.len();
            let at = |q: P2| (q[0] - e[0]).powi(2) + (q[1] - e[1]).powi(2);
            let end = match at(l[0]) <= at(l[m - 1]) {
                true => l[0],
                false => l[m - 1],
            };
            if at(end) > 64.0 {
                continue; // its line vanished from the cut
            }
            out.push((e, near(end, cut, Some(own))));
        }
    }
    out
}

/// Item 2 of 1707: a road end that lies on another line in the source
/// (within half a unit) lies within one unit of it in the simplified cut
/// tile, at z15 and z14. Positive control: measured against the source
/// itself, no end is parted.
#[test]
fn a_joined_road_end_stays_within_one_unit_in_the_cut() {
    let a = archive();
    for z in [15u8, 14] {
        let tiles: Vec<TileId> = a
            .tiles()
            .expect("tiles")
            .into_iter()
            .filter(|t| t.z == z)
            .collect();
        assert!(!tiles.is_empty(), "no z{z} tiles");
        let mut ends = 0usize;
        let mut control_parted = 0usize;
        let mut parted: Vec<(P2, f64)> = Vec::new();
        for t in tiles {
            let raw = a.mvt(t).expect("mvt").expect("tile");
            let src = source_roads(&raw);
            // Positive control (1727): the same measure on a plain
            // Douglas-Peucker cut of the same lines, which holds no
            // joined end, parts some.
            control_parted += joined_gaps(&src, &plain_cut(&src, z))
                .iter()
                .filter(|(_, g)| *g > 1.0)
                .count();
            let blob = cut_tile(&raw, simplified(z)).expect("cut").0.expect("blob");
            let gaps = joined_gaps(&src, &blob_roads(&blob));
            ends += gaps.len();
            parted.extend(gaps.into_iter().filter(|(_, g)| *g > 1.0));
        }
        assert!(ends >= 100, "only {ends} joined ends measured at z{z}");
        assert!(
            control_parted >= 1,
            "control: a plain Douglas-Peucker cut parts no joined end at z{z}, \
             so the measure cannot see a gap"
        );
        parted.sort_by(|a, b| b.1.total_cmp(&a.1));
        assert!(
            parted.is_empty(),
            "z{z}: {} of {} joined road ends over one unit from their \
             partner after the cut; worst {:?}",
            parted.len(),
            ends,
            &parted[..parted.len().min(5)],
        );
    }
}

/// Item 2 of 1707, the tile edge: a road line end in a z15 tile's buffer
/// lies within one unit of the neighbour tile's copy of the line, as it
/// does in the source (positive control). Today each copy is simplified
/// alone and they drift apart, which draws as a doubled line.
#[test]
fn a_buffer_end_lies_on_the_neighbour_tiles_copy() {
    let a = archive();
    let tiles: Vec<TileId> = a
        .tiles()
        .expect("tiles")
        .into_iter()
        .filter(|t| t.z == 15)
        .collect();
    let src_of = |t: TileId| -> Option<Vec<Vec<P2>>> { Some(source_roads(&a.mvt(t).ok()??)) };
    let cut_of = |t: TileId| -> Option<Vec<Vec<P2>>> {
        let raw = a.mvt(t).ok()??;
        let blob = cut_tile(&raw, simplified(15)).expect("cut").0?;
        Some(blob_roads(&blob))
    };
    // A buffer end and the neighbour tile it reaches into, translated.
    let buffer_ends = |lines: &[Vec<P2>], t: TileId| -> Vec<(P2, TileId, P2)> {
        let mut out = Vec::new();
        for l in lines {
            for &e in &[l[0], l[l.len() - 1]] {
                if inside(e) {
                    continue;
                }
                let nx = i64::from(t.x) + (e[0] / 4096.0).floor() as i64;
                let ny = i64::from(t.y) + (e[1] / 4096.0).floor() as i64;
                let n = TileId {
                    z: t.z,
                    x: nx as u32,
                    y: ny as u32,
                };
                let q = [
                    e[0] - (nx - i64::from(t.x)) as f64 * 4096.0,
                    e[1] - (ny - i64::from(t.y)) as f64 * 4096.0,
                ];
                out.push((e, n, q));
            }
        }
        out
    };
    let mut ends = 0usize;
    let mut frayed: Vec<(P2, f64)> = Vec::new();
    for &t in &tiles {
        let (Some(src), Some(cut)) = (src_of(t), cut_of(t)) else {
            continue;
        };
        // Positive control: in the source the two copies agree.
        for (e, n, q) in buffer_ends(&src, t) {
            let Some(nsrc) = src_of(n) else { continue };
            assert!(
                near(q, &nsrc, None) <= 1.0,
                "control: the source's own copies disagree at {e:?}"
            );
        }
        for (e, n, q) in buffer_ends(&cut, t) {
            let Some(ncut) = cut_of(n) else { continue };
            ends += 1;
            let d = near(q, &ncut, None);
            if d > 1.0 {
                frayed.push((e, d));
            }
        }
    }
    assert!(ends >= 20, "only {ends} buffer ends measured");
    frayed.sort_by(|a, b| b.1.total_cmp(&a.1));
    assert!(
        frayed.is_empty(),
        "{} of {} buffer ends over one unit off the neighbour's copy; \
         worst {:?}",
        frayed.len(),
        ends,
        &frayed[..frayed.len().min(5)],
    );
}

/// The whole fixture cut as the product cuts it (profile 2, 1707).
fn product_region() -> Vec<u8> {
    region_of(profile::CUT_2)
}

/// The whole fixture cut, z14 to z15, under `profile`.
fn region_of(profile: u8) -> Vec<u8> {
    let opts = CutOptions {
        bounds: bounds(),
        zooms: 14..=15,
        profile,
        attribution: "© OpenStreetMap contributors".to_owned(),
        notice: "test".to_owned(),
        threads: 2,
        locale: None,
        buildings: false,
        pois: false,
        round: leviculum_mapcut::cut::ROUND,
    };
    let mut out = std::io::Cursor::new(Vec::new());
    cut(&archive(), &opts, &mut out).expect("cut");
    out.into_inner()
}

/// Item 4 of 1707: building footprints are drawn from z16 only, always
/// overzoomed, so by item 1's rule they keep the source's corners —
/// every source corner of a kept footprint lies within one unit of the
/// cut footprint's outer ring. At the old half-pixel tolerance corners
/// sat up to 8 units off, 4 px at a z18 view under the sharp building
/// edge. Measured cost of tolerance 0 on `frankfurt/roemer`
/// (2026-11-12): section bytes 78,368 to 106,569 (+36 %), 42,424
/// corners all exact, 66 footprints that vanished as sub-pixel return.
#[test]
fn building_corners_stay_in_the_cut() {
    use leviculum_mapcut::cut::{footprints, ZoomStats};

    let a = archive();
    let mut corners = 0usize;
    for t in a.tiles().expect("tiles") {
        if t.z != 15 {
            continue;
        }
        let raw = a.mvt(t).expect("mvt").expect("tile");
        let d = mvt::decode(&raw).expect("decode");
        for f in d.features {
            if f.class != Class::Building || f.kind != Kind::Area {
                continue;
            }
            for part in &f.parts {
                let mut st = ZoomStats::default();
                let fp = footprints(std::slice::from_ref(part), &mut st);
                let Some(rings) = fp.first() else { continue };
                let outer: Vec<P2> = rings[0]
                    .iter()
                    .map(|&(x, y)| [f64::from(x), f64::from(y)])
                    .collect();
                // The ring closed: measure against its boundary.
                let mut ring = outer.clone();
                ring.push(outer[0]);
                for p in &part[0] {
                    corners += 1;
                    let q = [f64::from(p[0]), f64::from(p[1])];
                    let mut d = f64::INFINITY;
                    for w in ring.windows(2) {
                        d = d.min(seg_dist(q, w[0], w[1]));
                    }
                    assert!(
                        d <= 1.0,
                        "a building corner at {q:?} in {t:?} lies {d:.2} units \
                         off its cut footprint",
                    );
                }
            }
        }
    }
    assert!(corners >= 2000, "only {corners} corners measured");
}

/// The two places of 1707's findings, drawn in the PC finish from the
/// product's cut (profile 2) and held to their goldens, pixel for pixel:
/// the junction at 43.753791, 11.268163 where the side road ended a line
/// width short of the through road, at view z18; and the path at
/// 43.747425, 11.272150 along the tile edge 17409/17410, which two
/// drifted buffer copies drew as a doubled line. The pictures of
/// 2026-11-11 (`notizen/mapgeom-2026-11-11/`) are the before.
/// `LEVICULUM_WRITE_GOLDEN=1` rewrites the goldens.
#[test]
fn the_junction_and_the_tile_edge_draw_their_goldens() {
    use embedded_graphics::prelude::*;
    use leviculum_map::draw::{draw_in_scaled, Ink};
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::Viewport;
    use leviculum_mapfill::fill::{Edge, Scratch};
    use leviculum_mapfill::frame::RgbaFrame;
    use leviculum_mapfill::render::{NoClock, Work};
    use leviculum_mapfill::style::{Profile, Style};

    let region = product_region();
    let r = Region::from_bytes(&region).expect("region");
    let (w, h) = (320u32, 240u32);
    for (name, lat, lon) in [
        ("junction-z18-pc.png", 43.753791, 11.268163),
        ("tile-edge-path-z18-pc.png", 43.747425, 11.272150),
    ] {
        let view = Viewport::new(project(LatLon::from_degrees(lat, lon)), 18, w, h);
        let tiles = r.view(&view).expect("tiles");
        let mut px = vec![0u8; (w * h * 4) as usize];
        let mut edges = vec![Edge::default(); 60000];
        let mut active = vec![0u16; 60000];
        let mut heads = vec![0u16; h as usize];
        let mut points = vec![Point::zero(); 8192];
        let mut ring_ends = vec![0u16; 2048];
        let mut frame = RgbaFrame::new(&mut px, w, h).expect("frame").smoothed(true);
        let Ok(()) = frame.clear(Style::PC.theme.palette().background);
        let mut scratch = Scratch {
            edges: &mut edges,
            heads: &mut heads,
            active: &mut active,
        };
        let mut work = Work {
            points: &mut points,
            ring_ends: &mut ring_ends,
        };
        let Ok(_) = draw_in_scaled(
            &tiles,
            r.area(&view),
            Profile::Full,
            Ink::Colour,
            Style::PC,
            100,
            &mut frame,
            &mut scratch,
            &mut work,
            &mut NoClock,
        );
        let rgb: Vec<u8> = px
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        same_as(name, &rgb, w, h);
    }
}

fn golden(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn png(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    let mut writer = e.write_header().expect("header");
    writer.write_image_data(rgb).expect("data");
    writer.finish().expect("finish");
    out
}

/// Hold `rgb` to the PNG golden `name`, pixel for pixel, as
/// `leviculum-map/tests/golden.rs` does; a picture that moved is left
/// under `$TMPDIR/leviculum-golden/` for a look.
fn same_as(name: &str, rgb: &[u8], w: u32, h: u32) {
    if std::env::var_os("LEVICULUM_WRITE_GOLDEN").is_some() {
        std::fs::write(golden(name), png(rgb, w, h)).expect("write the golden");
        return;
    }
    let kept = |bytes: &[u8]| {
        let dir = std::env::temp_dir().join("leviculum-golden");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        path
    };
    let Ok(bytes) = std::fs::read(golden(name)) else {
        let path = kept(&png(rgb, w, h));
        panic!("no golden {name}; this run's is {}", path.display());
    };
    let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes[..]))
        .read_info()
        .expect("png");
    let mut want = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut want).expect("frame");
    want.truncate(info.buffer_size());
    if (info.width, info.height) != (w, h) || want != rgb {
        let differ = want
            .chunks(3)
            .zip(rgb.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        let path = kept(&png(rgb, w, h));
        panic!(
            "{name} moved: {differ} pixels differ; this run's is {}",
            path.display()
        );
    }
}

/// The worst distance of a z15 source road vertex from the cut lines of
/// `region` at z15, and how many vertices were measured.
fn deepest_vertex_worst(region: &[u8]) -> (f64, usize) {
    let a = archive();
    let r = Region::from_bytes(region).expect("region");
    let (mut worst, mut vertices) = (0.0f64, 0usize);
    for n in 0..r.header().tiles {
        let e = r.entry(n).expect("entry");
        if e.id.z != 15 {
            continue;
        }
        let fx = r.fixture_of(&e).expect("fixture").expect("blob");
        let s = f64::from(1i32 << fx.shift().max(0));
        let mut cut_lines = Vec::new();
        for f in fx.features() {
            if f.kind != Kind::Line || !is_road(f.class) {
                continue;
            }
            for ring in f.rings() {
                let pts: Vec<P2> = ring
                    .points()
                    .map(|(x, y)| [f64::from(x) * s, f64::from(y) * s])
                    .collect();
                if pts.len() >= 2 {
                    cut_lines.push(pts);
                }
            }
        }
        let raw = a.mvt(e.id).expect("mvt").expect("tile");
        for line in source_roads(&raw) {
            for p in line {
                vertices += 1;
                worst = worst.max(near(p, &cut_lines, None));
            }
        }
    }
    (worst, vertices)
}

/// Item 1 of 1707: the region's deepest zoom keeps the source's geometry.
/// Every source vertex of a z15 road line lies within one unit of the cut
/// line; under profile 1 the half-pixel tolerance left vertices 8 units
/// off, which every view deeper than z15 enlarges.
#[test]
fn the_deepest_zoom_keeps_every_source_vertex() {
    let (worst, vertices) = deepest_vertex_worst(&product_region());
    assert!(vertices >= 1000, "only {vertices} source vertices measured");
    assert!(
        worst <= 1.0,
        "a z15 source vertex lies {worst:.2} units off the cut line"
    );
}

/// The positive control of [`the_deepest_zoom_keeps_every_source_vertex`]
/// (1727, the audit of 1707): the same measure on a profile 1 cut of the
/// same z14-15 box, whose deepest zoom is simplified, finds a vertex over
/// one unit off, so the measure can see what profile 2 prevents.
#[test]
fn the_deepest_zoom_check_fails_on_a_profile_one_cut() {
    let (worst, vertices) = deepest_vertex_worst(&region_of(profile::CUT_1));
    assert!(vertices >= 1000, "only {vertices} source vertices measured");
    assert!(
        worst > 1.0,
        "profile 1 left every z15 source vertex within one unit ({worst:.2}): \
         the check cannot fail"
    );
}

/// The held cut's price at z14 (1727, the audit of 1707): the share of
/// the source's road vertices the simplified tile keeps. Holding the
/// joined ends and the edge strip keeps more than plain Douglas-Peucker,
/// and a held cut that kept every vertex would pass every gap test above
/// while simplifying nothing; this test pins the share under a measured
/// ceiling, so a hold that grows to everything (an anchor test that
/// always says near, an edge strip that takes the whole tile) goes red.
/// Measured on 2026-11-12: 2,358 of 3,227 (0.731), ceiling [`MAX_Z14_SHARE`].
#[test]
fn the_held_cut_keeps_a_measured_share_of_the_z14_road_vertices() {
    let a = archive();
    let (mut source, mut kept, mut plain) = (0usize, 0usize, 0usize);
    for t in a.tiles().expect("tiles").into_iter().filter(|t| t.z == 14) {
        let raw = a.mvt(t).expect("mvt").expect("tile");
        let src = source_roads(&raw);
        source += src.iter().map(Vec::len).sum::<usize>();
        plain += plain_cut(&src, 14).iter().map(Vec::len).sum::<usize>();
        let blob = cut_tile(&raw, simplified(14))
            .expect("cut")
            .0
            .expect("blob");
        kept += blob_roads(&blob).iter().map(Vec::len).sum::<usize>();
    }
    assert!(source >= 1000, "only {source} source road vertices at z14");
    let share = kept as f64 / source as f64;
    println!("z14 road vertices: kept {kept} of {source}, share {share:.4}, plain {plain}");
    assert!(
        kept >= plain,
        "the held cut keeps fewer than plain: {kept} < {plain}"
    );
    assert!(
        share <= MAX_Z14_SHARE,
        "the held cut keeps {kept} of {source} z14 road vertices ({share:.3}), \
         over the ceiling {MAX_Z14_SHARE}"
    );
}
