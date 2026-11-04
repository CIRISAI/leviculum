//! What placing the names costs beside drawing the ground (1225), over
//! Berlin Mitte at z10, z13 and z16, for the phone's frame (1080 x 2400
//! with a quarter screen of margin each side, density 2.75) and the
//! window's (1920 x 1080 with an eighth, density 1).
//!
//! Rows per zoom: the ground of a frame that zooms (moving, between two
//! steps, the step's tiles one to one as the phone and the window ask,
//! warm cache); the names of a full placement in its parts, as a frame at
//! rest runs it: looked up (`Painter::labels`), placed by the rule with
//! their masks and halos kept by the typesetter (`mapnames::place`), and
//! rasterised into sprites (`Sprite::of_all`, every worker and one); and
//! the whole of a frame that zooms since 1225, which places its names
//! anew ([`Frames::frame`] at a zoom between steps). The median of the
//! frames after one warm-up.
//!
//! ```sh
//! cargo run --release -p loquax --features screenmap --example names_cost -- MAPS
//! ```
//!
//! `MAPS` holds Berlin Mitte with its names (LMR2, cut as the man page
//! says: `--bbox 13.33,52.49,13.45,52.55 --locale de`) and, for z10, a
//! wider cut to z12 under it. A measuring tool for the reviewer and the
//! commit message, not a gate.

use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::labels::SUBPIXEL;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::Style;
use loquax::mapframe::{frame_size, Ask, Frames};
use loquax::mapnames::{self, Sprite, Typesetter};
use loquax::picture::{Maps, Painter};

fn median(mut ms: Vec<f64>) -> f64 {
    ms.sort_by(f64::total_cmp);
    ms[ms.len() / 2]
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let out = f();
    (out, t.elapsed().as_secs_f64() * 1e3)
}

fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: names_cost MAPS");
        std::process::exit(2);
    };
    let at = project(LatLon::from_degrees(52.5200, 13.4050));
    let style = Style::PC;
    let targets = [
        (
            "phone 1080x2400 d2.75",
            (1080u32, 2400u32),
            (270u32, 600u32),
            2.75f32,
        ),
        ("window 1920x1080 d1", (1920, 1080), (240, 135), 1.0),
    ];
    println!("| target | zoom | names | ground ms | labels ms | place ms | sprites ms (all) | sprites ms (1) | placement ms | zooming frame ms | its names: kept or looked up / placed / sampled ms |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for (name, (w, h), margin, scale) in targets {
        let mut frames = Frames::new(Maps::new(Some(dir.clone())));
        let workers = frames.workers();
        let mut painter = Painter::new(Maps::new(Some(dir.clone())));
        let mut type_ = Typesetter::new();
        let ask = |view: ExactView, moving: bool| Ask {
            view,
            margin,
            moving,
            style,
            scale,
            pins: &[],
            selected: None,
            zooms: (3, 18),
            draw_pins: false,
            tracks: &[],
            measure: None,
            now_ms: None,
            scaled_while_moving: true,
        };
        for z in [10u32, 13, 16] {
            let rest = ExactView::new(at, z * MILLI, w, h);
            // Warm the tiles of the step and the one below it.
            for _ in 0..2 {
                let shot = frames.frame(&ask(rest, false));
                frames.recycle(shot.rgba);
            }
            // The frames of a pinch out from the zoom, 0.05 a frame.
            let mut parts = (vec![], vec![], vec![], 0);
            let zooming: Vec<f64> = (1..12)
                .map(|i| {
                    let mut v = rest;
                    v.zoom_about(z * MILLI - i * 50, f64::from(w) / 2.0, f64::from(h) / 2.0);
                    let (shot, ms) = timed(|| frames.frame(&ask(v, true)));
                    frames.recycle(shot.rgba);
                    let cost = frames.place_cost();
                    parts.0.push(cost.labels_ns as f64 / 1e6);
                    parts.1.push(cost.place_ns as f64 / 1e6);
                    parts.2.push(cost.sprites_ns as f64 / 1e6);
                    parts.3 += usize::from(cost.looked_up);
                    ms
                })
                .skip(1)
                .collect();
            let zooming = median(zooming);
            let parts = format!(
                "{:.2} / {:.2} / {:.2} ({} looked up)",
                median(parts.0),
                median(parts.1),
                median(parts.2),
                parts.3
            );
            // The ground alone: the same frames with no names, at rest
            // elsewhere so none is carried.
            let mut ground_frames = Frames::new(Maps::new(Some(dir.clone())));
            ground_frames.set_names_off(true);
            for _ in 0..40 {
                let mut v = rest;
                v.zoom_about(z * MILLI - 50, f64::from(w) / 2.0, f64::from(h) / 2.0);
                let shot = ground_frames.frame(&ask(v, true));
                ground_frames.recycle(shot.rgba);
            }
            let ground: Vec<f64> = (1..12)
                .map(|i| {
                    let mut v = rest;
                    v.zoom_about(z * MILLI - i * 50, f64::from(w) / 2.0, f64::from(h) / 2.0);
                    let (shot, ms) = timed(|| ground_frames.frame(&ask(v, true)));
                    ground_frames.recycle(shot.rgba);
                    ms
                })
                .skip(1)
                .collect();
            let ground = median(ground);
            // A full placement at a zoom between steps, as a frame at rest
            // there runs it.
            let (mut labels_ms, mut place_ms, mut all_ms, mut one_ms) =
                (vec![], vec![], vec![], vec![]);
            let mut count = 0;
            for i in 0..6u32 {
                let mut v = rest;
                v.zoom_about(
                    z * MILLI - 300 - i * 50,
                    f64::from(w) / 2.0,
                    f64::from(h) / 2.0,
                );
                let mut drawn = v.on_whole_pixel();
                let (fw, fh) = frame_size(&ask(v, false));
                drawn.resize(fw, fh);
                let step = v.tile_zoom();
                let base = drawn.at_zoom(step);
                let (labels, ms) = timed(|| painter.labels(&base, true));
                labels_ms.push(ms);
                let (bw, bh) = base.size();
                let s = drawn.scale_at(step);
                let sub = SUBPIXEL as f64;
                let px = |p: (i64, i64)| {
                    (
                        (p.0 as f64 / sub - f64::from(bw) / 2.0) * s + f64::from(fw) / 2.0,
                        (p.1 as f64 / sub - f64::from(bh) / 2.0) * s + f64::from(fh) / 2.0,
                    )
                };
                let (placed, ms) =
                    timed(|| mapnames::place(fw, fh, &labels, px, &[], scale, &mut type_));
                place_ms.push(ms);
                count = placed.len();
                let (sprites, ms) = timed(|| Sprite::of_all(&placed, workers));
                std::hint::black_box(sprites.len());
                all_ms.push(ms);
                let (sprites, ms) = timed(|| Sprite::of_all(&placed, 1));
                std::hint::black_box(sprites.len());
                one_ms.push(ms);
            }
            let skip = |v: Vec<f64>| median(v.into_iter().skip(1).collect());
            let (l, p, a, o) = (skip(labels_ms), skip(place_ms), skip(all_ms), skip(one_ms));
            println!(
                "| {name} | {z} | {count} | {ground:.2} | {l:.2} | {p:.2} | {a:.2} | {o:.2} | {:.2} | {zooming:.2} | {parts} |",
                l + p + a
            );
        }
    }
}
