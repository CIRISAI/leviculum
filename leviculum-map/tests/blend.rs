//! The world view's blended projection (1710): the centre never moves,
//! `t = 1` is Mercator exactly, the inverse closes, areas hold at
//! `t = 0`, the zoom ramp moves the middle third only as far as the
//! shapes themselves require, and panning crosses the antimeridian
//! without a jump.

use leviculum_map::blend::{t_of_zoom, Blend, Shape};
use leviculum_map::mercator::{project, unproject, LatLon, World};
use leviculum_map::outline::{self, WORLD_50M};
use leviculum_map::viewport::{ExactView, MILLI};

/// The proof's five centres (the order's ramp): 0/0, Berlin, Merzouga,
/// Singapore, 65 N in Norway.
fn centres() -> [World; 5] {
    [
        project(LatLon::from_degrees(0.0, 0.0)),
        project(LatLon::from_degrees(52.52, 13.405)),
        project(LatLon::from_degrees(31.08, -4.01)),
        project(LatLon::from_degrees(1.35, 103.82)),
        project(LatLon::from_degrees(65.0, 13.0)),
    ]
}

const SHAPES: [Shape; 2] = [Shape::EqualEarth, Shape::Globe];

#[test]
fn t_is_a_smooth_step() {
    assert_eq!(t_of_zoom(0.0, 2.0, 5.0), 0.0);
    assert_eq!(t_of_zoom(2.0, 2.0, 5.0), 0.0);
    assert_eq!(t_of_zoom(5.0, 2.0, 5.0), 1.0);
    assert_eq!(t_of_zoom(9.0, 2.0, 5.0), 1.0);
    let mut last = 0.0;
    for i in 0..=300 {
        let t = t_of_zoom(2.0 + f64::from(i) / 100.0, 2.0, 5.0);
        assert!(t >= last, "monotone at {i}");
        last = t;
    }
    // No kink at either end: the first step away from each bound is tiny
    // (the smooth step's slope vanishes there).
    assert!(t_of_zoom(2.1, 2.0, 5.0) < 0.001);
    assert!(t_of_zoom(4.9, 2.0, 5.0) > 0.999);
}

#[test]
fn the_centre_never_moves() {
    for shape in SHAPES {
        for centre in centres() {
            for z in [2.0, 2.5, 3.0, 3.7, 4.4, 5.0, 6.0] {
                let b = Blend::new(shape, centre, z, 480, 320);
                let (x, y) = b.to_screen(centre);
                assert!(
                    (x - 240.0).abs() < 1e-6 && (y - 160.0).abs() < 1e-6,
                    "{shape:?} z{z} centre at ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn t_one_is_mercator_exactly() {
    for shape in SHAPES {
        for centre in centres() {
            let b = Blend::new(shape, centre, 6.0, 480, 320);
            assert_eq!(b.t(), 1.0);
            let view = ExactView::new(centre, 6 * MILLI, 480, 320);
            for (dlat, dlon) in [(0.0, 0.0), (0.3, 0.4), (-0.2, 0.7), (0.5, -0.9)] {
                let at = unproject(centre);
                let w = project(LatLon::from_degrees(
                    f64::from(at.lat_e7) * 1e-7 + dlat,
                    f64::from(at.lon_e7) * 1e-7 + dlon,
                ));
                let (bx, by) = b.to_screen(w);
                let (vx, vy) = view.to_screen(w);
                assert!(
                    (bx - vx).abs() < 1e-9 && (by - vy).abs() < 1e-9,
                    "{shape:?} {dlat}/{dlon}: blend ({bx}, {by}) view ({vx}, {vy})"
                );
            }
        }
    }
}

#[test]
fn the_inverse_closes_on_the_forward() {
    for shape in SHAPES {
        for centre in centres() {
            for z in [2.0, 3.0, 3.5, 4.5, 6.0] {
                let b = Blend::new(shape, centre, z, 480, 320);
                let at = unproject(centre);
                for dlat in [-40.0, -10.0, 0.0, 10.0, 40.0] {
                    for dlon in [-60.0, -15.0, 0.0, 15.0, 60.0] {
                        let lat = (f64::from(at.lat_e7) * 1e-7 + dlat).clamp(-80.0, 80.0);
                        let lon = f64::from(at.lon_e7) * 1e-7 + dlon;
                        let w = project(LatLon::from_degrees(lat, lon));
                        if !b.facing(w) {
                            continue;
                        }
                        let (x, y) = b.to_screen(w);
                        let Some(back) = b.to_world(x, y) else {
                            panic!("{shape:?} z{z} {lat}/{lon}: no inverse at ({x}, {y})");
                        };
                        let (rx, ry) = b.to_screen(back);
                        assert!(
                            (rx - x).abs() < 0.0625 && (ry - y).abs() < 0.0625,
                            "{shape:?} z{z} {lat}/{lon}: ({x}, {y}) returned ({rx}, {ry})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn off_the_world_there_is_no_inverse() {
    // At t = 0 the screen above Equal Earth's top edge, and outside the
    // globe's limb, maps to no ground.
    let centre = project(LatLon::from_degrees(0.0, 0.0));
    for shape in SHAPES {
        let b = Blend::new(shape, centre, 2.0, 1200, 1200);
        assert_eq!(b.t(), 0.0, "z2 is the world");
        assert!(
            b.to_world(600.0, -2000.0).is_none(),
            "{shape:?}: far above the world"
        );
    }
}

/// The shoelace area of a ring in Lambert cylindrical equal-area terms
/// `(λ, sin φ)`, which is the true spherical area in steradians (over 2),
/// and in blended screen pixels: the ratio of ratios is the test.
fn ring_areas(b: &Blend, ring: &outline::Ring<'_>, bits: u32) -> (f64, f64) {
    let shift = 32 - bits;
    let pts: Vec<World> = ring
        .points()
        .map(|(x, y)| World {
            x: x << shift,
            y: y << shift,
        })
        .collect();
    let mut sphere = 0.0;
    let mut screen = 0.0;
    for i in 0..pts.len() {
        let (a, c) = (pts[i], pts[(i + 1) % pts.len()]);
        let (la, pa) = degrees(a);
        let (lc, pc) = degrees(c);
        // Longitudes around a ring never span the antimeridian here
        // (Greenland and Africa do not), so plain differences hold.
        sphere += (lc.to_radians() - la.to_radians())
            * (pa.to_radians().sin() + pc.to_radians().sin())
            / 2.0;
        let (ax, ay) = b.to_screen(a);
        let (cx, cy) = b.to_screen(c);
        screen += (cx - ax) * (ay + cy) / 2.0;
    }
    (sphere.abs(), screen.abs())
}

fn degrees(w: World) -> (f64, f64) {
    let at = unproject(w);
    (f64::from(at.lon_e7) * 1e-7, f64::from(at.lat_e7) * 1e-7)
}

/// The largest ring of the land table whose box holds `at`.
fn ring_holding(at: World, bits: u32) -> Option<usize> {
    let shift = 32 - bits;
    let (tx, ty) = (at.x >> shift, at.y >> shift);
    outline::rings(WORLD_50M)
        .enumerate()
        .filter(|(_, r)| {
            !r.hole && r.bbox[0] <= tx && tx <= r.bbox[2] && r.bbox[1] <= ty && ty <= r.bbox[3]
        })
        .max_by_key(|(_, r)| r.len())
        .map(|(i, _)| i)
}

#[test]
fn areas_hold_at_world_zoom() {
    let bits = outline::bits(WORLD_50M).expect("land table");
    let greenland =
        ring_holding(project(LatLon::from_degrees(72.0, -40.0)), bits).expect("Greenland's ring");
    let africa =
        ring_holding(project(LatLon::from_degrees(2.0, 17.0)), bits).expect("Africa's ring");
    assert_ne!(greenland, africa);
    let centre = project(LatLon::from_degrees(0.0, 0.0));
    let b = Blend::new(Shape::EqualEarth, centre, 2.0, 1024, 700);
    assert_eq!(b.t(), 0.0);
    let rings: Vec<outline::Ring> = outline::rings(WORLD_50M).collect();
    let (g_true, g_px) = ring_areas(&b, &rings[greenland], bits);
    let (a_true, a_px) = ring_areas(&b, &rings[africa], bits);
    let want = g_true / a_true;
    let got = g_px / a_px;
    let off = (got / want - 1.0).abs();
    assert!(
        off < 0.02,
        "Greenland/Africa: true {want:.4}, drawn {got:.4}, off by {:.2} %",
        off * 100.0
    );
    // And against Mercator today, where Greenland looks about seven
    // times its share: the same ratio is off by whole multiples.
    let m = ExactView::new(centre, 2 * MILLI, 1024, 700);
    let shoelace = |ring: &outline::Ring| {
        let shift = 32 - bits;
        let pts: Vec<(f64, f64)> = ring
            .points()
            .map(|(x, y)| {
                m.to_screen(World {
                    x: x << shift,
                    y: y << shift,
                })
            })
            .collect();
        let mut area = 0.0;
        for i in 0..pts.len() {
            let (a, c) = (pts[i], pts[(i + 1) % pts.len()]);
            area += (c.0 - a.0) * (a.1 + c.1) / 2.0;
        }
        area.abs()
    };
    let mercator = shoelace(&rings[greenland]) / shoelace(&rings[africa]);
    assert!(
        mercator > want * 4.0,
        "Mercator's Greenland is wildly large: {mercator:.4} against {want:.4}"
    );
}

/// The largest residual between two zooms of one ramp step, raw (beyond
/// the uniform scale the zoom explains) and with a best-fit per-axis
/// scale taken out (what remains of shape change alone).
fn step_residual(shape: Shape, centre: World, z: f64, dz: f64) -> (f64, f64) {
    let (w, h) = (480u32, 320u32);
    let a = Blend::new(shape, centre, z, w, h);
    let b = Blend::new(shape, centre, z + dz, w, h);
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    // Fixed ground points: what sits in the middle third at this zoom.
    let mut from = Vec::new();
    let mut to = Vec::new();
    for gx in -4i32..=4 {
        for gy in -3i32..=3 {
            let sx = cx + f64::from(gx) * (f64::from(w) / 6.0) / 4.5;
            let sy = cy + f64::from(gy) * (f64::from(h) / 6.0) / 3.5;
            let Some(ground) = a.to_world(sx, sy) else {
                continue;
            };
            if !b.facing(ground) {
                continue;
            }
            let (ax, ay) = a.to_screen(ground);
            let (bx, by) = b.to_screen(ground);
            from.push((ax - cx, ay - cy));
            to.push((bx - cx, by - cy));
        }
    }
    let scale = libm::exp2(dz);
    let raw = from
        .iter()
        .zip(&to)
        .map(|(f, t)| libm::hypot(t.0 - f.0 * scale, t.1 - f.1 * scale))
        .fold(0.0, f64::max);
    // The best per-axis scales, least squares: what a stretch about the
    // centre explains on top of the zoom.
    let fit = |sel: fn(&(f64, f64)) -> f64| -> f64 {
        let num: f64 = from.iter().zip(&to).map(|(f, t)| sel(f) * sel(t)).sum();
        let den: f64 = from.iter().map(|f| sel(f) * sel(f)).sum();
        if den < 1e-12 {
            scale
        } else {
            num / den
        }
    };
    let (ax, ay) = (fit(|p| p.0), fit(|p| p.1));
    let shaped = from
        .iter()
        .zip(&to)
        .map(|(f, t)| libm::hypot(t.0 - f.0 * ax, t.1 - f.1 * ay))
        .fold(0.0, f64::max);
    (raw, shaped)
}

#[test]
fn the_zoom_ramp_moves_the_middle_third_gently() {
    for shape in SHAPES {
        let mut worst_raw: (f64, f64, usize) = (0.0, 0.0, 9);
        let mut worst_shaped = 0.0f64;
        for (i, centre) in centres().into_iter().enumerate() {
            let mut z = 1.0;
            while z < 7.0 - 1e-9 {
                let (raw, shaped) = step_residual(shape, centre, z, 0.1);
                if raw > worst_raw.0 {
                    worst_raw = (raw, z, i);
                }
                worst_shaped = worst_shaped.max(shaped);
                z += 0.1;
            }
        }
        println!(
            "{shape:?}: raw {:.2} px at z{:.1} centre {}, axis-corrected {:.3} px",
            worst_raw.0, worst_raw.1, worst_raw.2, worst_shaped
        );
        // The axis-corrected residual is what no scale about the centre
        // explains: the actual shape morph per tenth of a zoom, under a
        // pixel for Equal Earth (measured 0.47). The raw residual also
        // carries the axis scales sliding from the world projection's to
        // Mercator's: Equal Earth's anisotropy pays up to 2.82 at the
        // 65 N centre.
        //
        // The globe's radius at the world is the zoom's alone since 1762,
        // wherever it is turned to, so at 65 N it still has Mercator's
        // 1/cos 65° = 2.37 to grow through the handover on top of the
        // zoom: a tenth of a zoom there is up to 0.18 of one in scale.
        // That is the raw 5.52 px measured at z3.4, and the shape morph
        // per step grows with the step to 1.29 px (0.59 before, when the
        // globe was 2.37 times too large at the world instead).
        let (shaped_bound, raw_bound) = match shape {
            Shape::EqualEarth => (1.0, 3.5),
            Shape::Globe => (1.5, 6.0),
        };
        assert!(
            worst_shaped < shaped_bound,
            "{shape:?}: {worst_shaped} px of shape morph in one step"
        );
        assert!(
            worst_raw.0 < raw_bound,
            "{shape:?}: {} px raw in one step",
            worst_raw.0
        );
    }
}

#[test]
fn panning_crosses_the_antimeridian_smoothly() {
    // Fiji's corner: the centre walks east across the antimeridian; a
    // fixed point near it stays put on screen relative to the centre,
    // moving only by the pan itself.
    for shape in SHAPES {
        let fixed = project(LatLon::from_degrees(-17.8, 178.0));
        let mut last: Option<(f64, f64)> = None;
        for step in -20i64..=20 {
            let centre = World {
                x: (step * (1 << 24)).rem_euclid(1 << 32) as u32,
                y: project(LatLon::from_degrees(-17.8, 0.0)).y,
            };
            let b = Blend::new(shape, centre, 2.0, 480, 320);
            if !b.facing(fixed) {
                last = None;
                continue;
            }
            let (x, y) = b.to_screen(fixed);
            if let Some((lx, ly)) = last {
                // One step of the centre is one world pixel at z2 times
                // 2^(32-24-...): 2^24 world units = 2^24 / 2^(24-2) = 4
                // screen pixels; the fixed point moves by about that and
                // never jumps.
                let (dx, dy) = (x - lx, y - ly);
                assert!(
                    (dx + 4.0).abs() < 1.0 && dy.abs() < 1.0,
                    "{shape:?} step {step}: moved by ({dx}, {dy})"
                );
            }
            last = Some((x, y));
        }
    }
}
