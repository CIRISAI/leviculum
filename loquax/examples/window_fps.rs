//! Host frame time of the window's map (1144) at 1920 x 1080 and
//! 3840 x 2160, Frankfurt at z15, before and after the window took the
//! phone's frames ([`loquax::mapframe`]).
//!
//! Before: every view painted whole by [`loquax::screenmap::paint`] at an
//! integer zoom, without names while a drag moves it and with them at
//! rest, a wheel notch a whole step painted at rest. After: the frame
//! composed from the tile cache with an eighth of the screen of margin on
//! each side, as the window asks: a pan with the cache warm (40 pixels a
//! frame, the names kept), a pan with the cache dropped before each frame
//! (at most two tiles a worker drawn, the rest owed), a frame at rest warm
//! and cold (every tile drawn and the names placed), and a wheel zoom from
//! z15.0 to z15.9 in tenths while it turns, then at rest at z15.9. Each
//! frame's buffer is handed back ([`Frames::recycle`]), as the phone's
//! facade does since 1154; the window itself gives its frames to iced.
//! While the wheel turns the frame is the tile step one to one, which
//! the window's image scales (1155).
//!
//! ```sh
//! cargo run --release -p loquax --features screenmap --example window_fps -- MAPS
//! ```
//!
//! `MAPS` holds the Frankfurt region with its names (LMR2, cut as
//! `loquax/examples/map_bench.rs` says) and nothing else. The median of
//! the frames after one warm-up, every core the cache takes. A measuring
//! tool for the reviewer and the commit message, not a gate.

use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::{ExactView, Viewport, MILLI};
use leviculum_mapfill::style::Style;
use loquax::mapframe::{Ask, Frames};
use loquax::mapnames::Typesetter;
use loquax::picture::{Maps, Painter};
use loquax::screenmap;

fn median(mut ms: Vec<f64>) -> f64 {
    ms.sort_by(f64::total_cmp);
    ms[ms.len() / 2]
}

fn timed(mut f: impl FnMut()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: window_fps MAPS");
        std::process::exit(2);
    };
    let at = project(LatLon::from_degrees(50.1109, 8.6821));
    let style = Style::PC;
    let mut type_ = Typesetter::new();
    println!("| size | frame | before ms | after ms |");
    println!("|---|---|---|---|");
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let size = format!("{w}x{h}");
        let mut painter = Painter::new(Maps::new(Some(dir.clone())));
        let mut paint = |view: &Viewport, names: bool| {
            let after = names.then(Vec::new);
            let p = screenmap::paint(&mut painter, view, style, 1.0, after, &mut type_);
            std::hint::black_box(p.rgba.len());
        };
        // Before: a drag, a rest frame, a wheel notch.
        let pan_old: Vec<f64> = (0..8)
            .map(|i| {
                let mut v = Viewport::new(at, 15, w, h);
                v.pan(40 * i, 0);
                timed(|| paint(&v, false))
            })
            .skip(1)
            .collect();
        let rest_old: Vec<f64> = (0..4)
            .map(|_| timed(|| paint(&Viewport::new(at, 15, w, h), true)))
            .skip(1)
            .collect();
        let wheel_old: Vec<f64> = (0..4)
            .map(|i| timed(|| paint(&Viewport::new(at, 15 + (i % 2), w, h), true)))
            .skip(1)
            .collect();

        let mut frames = Frames::new(Maps::new(Some(dir.clone())));
        let margin = (w / 8, h / 8);
        let frame = |frames: &mut Frames, view: ExactView, moving: bool| {
            let shot = frames.frame(&Ask {
                view,
                margin,
                moving,
                style,
                scale: 1.0,
                pins: &[],
                selected: None,
                zooms: (3, 18),
                draw_pins: false,
                tracks: &[],
                radar: None,
                measure: None,
                now_ms: None,
                scaled_while_moving: true,
            });
            std::hint::black_box(shot.rgba.len());
            let missing = shot.missing;
            frames.recycle(shot.rgba);
            missing
        };
        let base = ExactView::new(at, 15 * MILLI, w, h);
        let rest_cold: Vec<f64> = (0..4)
            .map(|_| {
                frames.clear_tiles();
                timed(|| {
                    frame(&mut frames, base, false);
                })
            })
            .skip(1)
            .collect();
        let rest_warm: Vec<f64> = (0..4)
            .map(|_| {
                timed(|| {
                    frame(&mut frames, base, false);
                })
            })
            .skip(1)
            .collect();
        let pan_warm: Vec<f64> = (0..8)
            .map(|i| {
                let mut v = base;
                v.pan(40 * i, 0);
                timed(|| {
                    frame(&mut frames, v, true);
                })
            })
            .skip(1)
            .collect();
        let mut owed = 0;
        let pan_cold: Vec<f64> = (0..8)
            .map(|i| {
                frame(&mut frames, base, false);
                frames.clear_tiles();
                let mut v = base;
                v.pan(40 * i, 0);
                timed(|| {
                    owed = frame(&mut frames, v, true);
                })
            })
            .skip(1)
            .collect();
        frame(&mut frames, base, false);
        let wheel_turn: Vec<f64> = (1..10)
            .map(|i| {
                let mut v = base;
                v.zoom_about(15 * MILLI + i * 100, f64::from(w) / 2.0, f64::from(h) / 2.0);
                timed(|| {
                    frame(&mut frames, v, true);
                })
            })
            .skip(1)
            .collect();
        let mut end = base;
        end.zoom_about(15_900, f64::from(w) / 2.0, f64::from(h) / 2.0);
        let wheel_rest = timed(|| {
            frame(&mut frames, end, false);
        });
        let (tiles, bytes) = frames.tile_memory();
        let (pan_old, rest_old, wheel_old) = (median(pan_old), median(rest_old), median(wheel_old));
        let rows = [
            ("pan, warm (40 px a frame)", pan_old, median(pan_warm)),
            (
                "pan, cold (cache dropped, tiles owed)",
                pan_old,
                median(pan_cold),
            ),
            ("at rest, warm", rest_old, median(rest_warm)),
            ("at rest, cold", rest_old, median(rest_cold)),
            (
                "wheel, while it turns (before: a notch painted at rest)",
                wheel_old,
                median(wheel_turn),
            ),
            ("wheel, at rest at z15.9", wheel_old, wheel_rest),
        ];
        for (name, before, after) in rows {
            println!("| {size} | {name} | {before:.1} | {after:.1} |");
        }
        println!(
            "| {size} | cache after the run | | {tiles} tiles, {} MiB; cold pan owed {owed} |",
            bytes >> 20
        );
    }
}
