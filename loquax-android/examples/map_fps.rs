//! Host frame rate of the phone's map frame (1131, 1134): a portrait
//! 1080 x 2400 screen at a density of 2.75, Frankfurt at z15, drawn
//! through the facade's own call straight into a Bitmap-sized buffer
//! ([`loquax_android::map::render_into`], 1155) with a quarter screen of
//! margin on each side, as the app asks.
//!
//! Rows: a pan with the tile cache warm (each frame 40 pixels from the
//! last, the finger down), the frame at rest where that pan started (the
//! names placed), its names drawn again from their sprites with the bytes
//! the sprites hold (1156), a pan that uncovers a new tile column every frame (256
//! pixels a frame), a cold frame at rest (the cache dropped, every tile
//! drawn and the names placed), a cold frame while the finger is down (at
//! most two tiles a worker drawn, the rest owed), and a pinch from z14.3
//! to z15.7 in steps of 0.07 with the finger down (since 1155 the step's
//! tiles one to one, the app's Canvas scaling them), then at rest, then
//! the same pinch again with its tiles held. Each
//! with every core the cache takes and with one. Then the facade's own
//! cost, the frame copied out as before 1155 against drawn into the
//! Bitmap, on a warm pan and at rest. Then the prefetch (1135): a
//! pan north with the finger down, a frame at rest, then for one second
//! the prefetch calls the app makes while idle (tiles drawn, a call's
//! time, which is what a gesture waits at most), and the first frame of a
//! further pan north by a tile row, with and without the prefetch before
//! it. The cache's memory after the run is printed last. The cold rows
//! also say the time the workers spent drawing tiles, summed.
//!
//! ```sh
//! cargo run --release -p loquax-android --example map_fps -- HOME
//! ```
//!
//! `HOME` holds `maps/` with the region, Frankfurt with its names (LMR2,
//! cut as `loquax/examples/map_eye_bench.rs` names), and a `config` with
//! `map_autoload = off`. The median of the frames after one warm-up. A
//! measuring tool for the reviewer and the commit message, not a gate.
//!
//! `MAPFPS_ZOOM` sets the zoom of the view at rest (15 unset; the
//! buildings' rows of 1192 are 16 and 17, over a region cut with and
//! without them), and the pinch goes from 0.7 below it to 0.7 above.
//! `MAPFPS_QUICK=1` stops after the rows of each worker count, before the
//! names pinched out, the facade and the prefetch.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use loquax::when::WallTime;
use loquax_android::map::Canvas;
use loquax_android::schema::{self, MapRequest, MapShown};

/// The app's Bitmap: one of the frame's size, made anew when it changes,
/// as `MapView` keeps its spare.
#[derive(Default)]
struct Bitmap {
    size: (u32, u32),
    rgba: Vec<u8>,
}

impl Canvas for Bitmap {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        if self.size != (w, h) {
            self.size = (w, h);
            self.rgba = vec![0; w as usize * h as usize * 4];
        }
        Ok(&mut self.rgba)
    }
}

fn main() {
    let Some(home) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: map_fps HOME");
        std::process::exit(2);
    };
    let (w, h) = (1080u32, 2400u32);
    let at = project(LatLon::from_degrees(50.1109, 8.6821));
    let zoom: u32 = std::env::var("MAPFPS_ZOOM")
        .ok()
        .and_then(|z| z.parse().ok())
        .unwrap_or(15);
    let z0 = zoom * 1000;
    let quick = std::env::var_os("MAPFPS_QUICK").is_some();
    let now = WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    };
    let base = MapRequest {
        dir: home.display().to_string(),
        w,
        h,
        density_milli: 2750,
        x: Some(at.x),
        y: Some(at.y),
        zoom_milli: Some(z0),
        margin_x: w / 4,
        margin_y: h / 4,
        ..MapRequest::default()
    };
    // As the app draws since 1155: straight into its Bitmap, the
    // description encoded for Java.
    let bitmap = RefCell::new(Bitmap::default());
    // The renderer's clock held a second further each frame: every name's
    // fade (1205) is done by the next, so a row measures what it did
    // before the fades, a frame at rest asked again placing its names.
    // The prefetch's rows run on the real clock, which orders the
    // prefetch by the pan's velocity.
    let clock = std::cell::Cell::new(Some(0u64));
    let draw = |ask: &MapRequest| -> MapShown {
        clock.set(clock.get().map(|ms| ms + 1000));
        loquax_android::map::tune_clock(&home, clock.get());
        let mut bitmap = bitmap.borrow_mut();
        let shown = loquax_android::map::render_into(ask, Some(Vec::new()), now, &mut *bitmap);
        std::hint::black_box(schema::encode(&shown));
        if !shown.ok {
            eprintln!("no frame: {:?}", shown.error);
            std::process::exit(1);
        }
        shown
    };
    let timed = |ask: &MapRequest| -> (f64, MapShown) {
        let t = Instant::now();
        let shown = draw(ask);
        (t.elapsed().as_secs_f64() * 1e3, shown)
    };
    let median = |mut ms: Vec<f64>| {
        ms.sort_by(f64::total_cmp);
        ms[ms.len() / 2]
    };
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    println!("| frame | workers | frame ms | fps | tiles drawn a frame | owed |");
    println!("|---|---|---|---|---|---|");
    for threads in [workers, 1] {
        let _ = draw(&base);
        loquax_android::map::tune_tiles(&home, Some(threads), false);
        let stats = || loquax_android::map::tile_stats(&home).unwrap_or_default();
        let row = |name: &str, ms: f64, drawn: f64, owed: u32| {
            println!(
                "| {name} | {threads} | {ms:.1} | {:.0} | {drawn:.1} | {owed} |",
                1e3 / ms
            );
        };
        // A warm pan: back and forth over cached tiles.
        let mut ask = MapRequest {
            moving: true,
            ..base.clone()
        };
        let _ = draw(&ask);
        let before = stats().rendered;
        let mut ms = Vec::new();
        for i in 0..21 {
            ask.pan_x = if i % 2 == 0 { 40 } else { -40 };
            let (t, shown) = timed(&ask);
            (ask.x, ask.y) = (Some(shown.x), Some(shown.y));
            ms.push(t);
        }
        let drawn = (stats().rendered - before) as f64 / 21.0;
        row("pan, cache warm", median(ms), drawn, 0);
        // At rest where the pan started, the tiles cached: the names
        // placed.
        let rest = MapRequest {
            moving: false,
            pan_x: 0,
            ..ask.clone()
        };
        let ms: Vec<f64> = (0..5).map(|_| timed(&rest).0).collect();
        row("at rest, cache warm, names placed", median(ms), 0.0, 0);
        // Its names drawn again from their sprites, as a moving frame
        // draws them, and the bytes the sprites hold.
        let (fw, fh) = (w + 2 * base.margin_x, h + 2 * base.margin_y);
        let mut draws = Vec::new();
        let mut bytes = 0;
        for _ in 0..11 {
            if let Some((b, ms)) = loquax_android::map::sprite_cost(&home, fw, fh) {
                bytes = b;
                draws.push(ms);
            }
        }
        if !draws.is_empty() {
            println!(
                "| names drawn from their sprites, {:.2} MB of sprites | {threads} | {:.2} | | | |",
                bytes as f64 / 1e6,
                median(draws)
            );
        }
        // A pan that uncovers a tile column a frame.
        let before = stats().rendered;
        let (mut ms, mut owed) = (Vec::new(), 0);
        ask.pan_x = 256;
        for _ in 0..21 {
            let (t, shown) = timed(&ask);
            (ask.x, ask.y) = (Some(shown.x), Some(shown.y));
            owed = owed.max(u32::from(shown.pending));
            ms.push(t);
        }
        let drawn = (stats().rendered - before) as f64 / 21.0;
        row("pan, a new tile column a frame", median(ms), drawn, owed);
        // Cold: the cache dropped before each frame, at rest and moving.
        for moving in [false, true] {
            let cold = MapRequest {
                moving,
                ..base.clone()
            };
            let before = stats();
            let (mut ms, mut owed) = (Vec::new(), 0);
            for _ in 0..7 {
                loquax_android::map::tune_tiles(&home, None, true);
                let (t, shown) = timed(&cold);
                owed = owed.max(u32::from(shown.pending));
                ms.push(t);
            }
            let drawn = (stats().rendered - before.rendered) as f64 / 7.0;
            let cpu = (stats().render_us - before.render_us) as f64 / 7e3;
            let name = match moving {
                false => "cold, at rest, names placed",
                true => "cold, finger down",
            };
            row(name, median(ms), drawn, owed);
            println!("| {name}: tile time, workers summed | {threads} | {cpu:.1} | | | |");
        }
        // A pinch from z14.3 to z15.7 (at z15), the finger down, then at
        // rest.
        let start = MapRequest {
            zoom_milli: Some(z0 - 700),
            ..base.clone()
        };
        let _ = draw(&start);
        let before = stats().rendered;
        let mut ms = Vec::new();
        let mut owed = 0;
        let mut pinch = MapRequest {
            moving: true,
            ..start.clone()
        };
        for z in (z0 - 630..=z0 + 700).step_by(70) {
            pinch.zoom_milli = Some(z);
            let (t, shown) = timed(&pinch);
            owed += u32::from(shown.pending);
            ms.push(t);
        }
        let n = ms.len() as f64;
        let drawn = (stats().rendered - before) as f64 / n;
        let mean = ms.iter().sum::<f64>() / n;
        let name = format!(
            "pinch z{:.1} to z{:.1} (median)",
            f64::from(z0 - 700) / 1e3,
            f64::from(z0 + 700) / 1e3
        );
        row(&name, median(ms), drawn, owed);
        println!(
            "| pinch, mean | {threads} | {mean:.1} | {:.0} | | |",
            1e3 / mean
        );
        let rest = MapRequest {
            moving: false,
            ..pinch.clone()
        };
        let (t, shown) = timed(&rest);
        row(
            &format!(
                "at rest after the pinch, z{:.2}",
                f64::from(shown.zoom_milli) / 1e3
            ),
            t,
            0.0,
            u32::from(shown.pending),
        );
        // The same pinch again, every tile it shows held (the frames at
        // rest at z14 and z15 draw them all): what a frame of the pinch
        // costs itself, no tile drawn or owed.
        for z in [z0 - 1000, z0, z0 - 700] {
            let _ = draw(&MapRequest {
                zoom_milli: Some(z),
                ..start.clone()
            });
        }
        let mut ms = Vec::new();
        let mut owed = 0;
        for z in (z0 - 630..=z0 + 700).step_by(70) {
            pinch.zoom_milli = Some(z);
            let (t, shown) = timed(&pinch);
            owed += u32::from(shown.pending);
            ms.push(t);
        }
        row("pinch again, tiles held (median)", median(ms), 0.0, owed);
        if !quick {
            carried(&home, &base, &draw, &timed, &median, threads);
        }
    }
    if quick {
        return;
    }
    facade(&home, &base, &draw, now, median, workers);
    clock.set(None);
    prefetch(&home, &base, draw, timed, median, workers);
    if let Some((tiles, bytes)) = loquax_android::map::tile_memory(&home) {
        println!(
            "\ncache: {tiles} tiles, {:.1} MB of the budget's {} MB",
            bytes as f64 / f64::from(1u32 << 20),
            loquax::tiles::BUDGET_MB
        );
    }
}

/// The names a pinch out places (1205, 1225): a frame at rest at z15,
/// then a pinch out to z14.0 in tenths, the finger down; the frames'
/// median, and what a frame spent placing its names, the median of the
/// frames of the pinch: looked up or kept, placed, sampled.
fn carried(
    home: &std::path::Path,
    base: &MapRequest,
    draw: &impl Fn(&MapRequest) -> MapShown,
    timed: &impl Fn(&MapRequest) -> (f64, MapShown),
    median: &impl Fn(Vec<f64>) -> f64,
    threads: usize,
) {
    let (mut labels, mut place, mut sprites) = (Vec::new(), Vec::new(), Vec::new());
    let mut ms = Vec::new();
    let (mut names, mut looked_up) = (0, 0);
    for _ in 0..5 {
        let _ = draw(base);
        let mut pinch = MapRequest {
            moving: true,
            ..base.clone()
        };
        let z0 = base.zoom_milli.unwrap_or(15_000);
        for z in (z0 - 1000..z0).rev().step_by(100) {
            pinch.zoom_milli = Some(z);
            ms.push(timed(&pinch).0);
            if let Some(cost) = loquax_android::map::place_cost(home) {
                names = cost.names;
                looked_up += usize::from(cost.looked_up);
                labels.push(cost.labels_ns as f64 / 1e6);
                place.push(cost.place_ns as f64 / 1e6);
                sprites.push(cost.sprites_ns as f64 / 1e6);
            }
        }
    }
    // Back at rest at z15, so the rows after start where they did
    // before this one: a frame that moves keeps the step of the last.
    let _ = draw(base);
    let ms = median(ms);
    println!(
        "| pinch out z15 to z14, names placed (median) | {threads} | {ms:.1} | {:.0} | | |",
        1e3 / ms
    );
    for (pass, ms) in [
        ("looked up or kept", median(labels)),
        ("placed", median(place)),
        ("sampled", median(sprites)),
    ] {
        println!(
            "| names placed: {names}, {pass} ms a frame ({looked_up} of 50 looked up) | {threads} | {ms:.2} | | | |"
        );
    }
}

/// The prefetch rows (1135): five runs each with and without the
/// prefetch, from a cold cache, the medians.
fn prefetch(
    home: &std::path::Path,
    base: &MapRequest,
    draw: impl Fn(&MapRequest) -> MapShown,
    timed: impl Fn(&MapRequest) -> (f64, MapShown),
    median: impl Fn(Vec<f64>) -> f64,
    workers: usize,
) {
    loquax_android::map::tune_tiles(home, Some(workers), true);
    let stats = || loquax_android::map::tile_stats(home).unwrap_or_default();
    println!("\n| prefetch | runs | value |");
    println!("|---|---|---|");
    let (mut tiles_1s, mut calls_1s, mut call_ms, mut done_tiles, mut done_ms) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut first = [Vec::new(), Vec::new()];
    let mut drawn_first = [Vec::new(), Vec::new()];
    let mut hits_first = [Vec::new(), Vec::new()];
    for run in 0..10 {
        let ahead = run % 2 == 0;
        loquax_android::map::tune_tiles(home, None, true);
        // A pan north with the finger down, then the frame at rest.
        let mut ask = MapRequest {
            moving: true,
            pan_y: -40,
            ..base.clone()
        };
        for _ in 0..8 {
            let shown = draw(&ask);
            (ask.x, ask.y) = (Some(shown.x), Some(shown.y));
        }
        ask.moving = false;
        ask.pan_y = 0;
        let rest = draw(&ask);
        assert!(rest.prefetching || !ahead);
        if ahead {
            let call = MapRequest {
                prefetch: true,
                ..ask.clone()
            };
            let from = stats().prefetched;
            let started = std::time::Instant::now();
            let (mut tiles, mut calls, mut more) = (0u32, 0u32, true);
            while more {
                let (t, shown) = timed(&call);
                if started.elapsed().as_secs_f64() <= 1.0 {
                    tiles += shown.prefetched;
                    calls += 1;
                }
                call_ms.push(t);
                more = shown.prefetching;
            }
            done_ms.push(started.elapsed().as_secs_f64() * 1e3);
            done_tiles.push((stats().prefetched - from) as f64);
            tiles_1s.push(f64::from(tiles));
            calls_1s.push(f64::from(calls));
        }
        let before = stats();
        let further = MapRequest {
            moving: true,
            pan_y: -(loquax::tiles::TILE as i32),
            ..ask.clone()
        };
        let (t, _) = timed(&further);
        let after = stats();
        let i = usize::from(!ahead);
        first[i].push(t);
        drawn_first[i].push((after.rendered - before.rendered) as f64);
        hits_first[i].push((after.prefetch_hits - before.prefetch_hits) as f64);
    }
    let n = tiles_1s.len();
    println!(
        "| tiles drawn in the first second | {n} | {:.0} |",
        median(tiles_1s)
    );
    println!(
        "| calls in the first second | {n} | {:.0} |",
        median(calls_1s)
    );
    let max = call_ms.iter().copied().fold(0.0, f64::max);
    println!("| a call, median ms | {n} | {:.1} |", median(call_ms));
    println!("| a call, longest ms | {n} | {max:.1} |");
    println!("| plan done: tiles | {n} | {:.0} |", median(done_tiles));
    println!("| plan done: ms | {n} | {:.0} |", median(done_ms));
    for (i, name) in ["with prefetch", "without"].iter().enumerate() {
        println!(
            "| first pan frame {name}: ms | {n} | {:.1} |",
            median(first[i].clone())
        );
        println!(
            "| first pan frame {name}: tiles drawn | {n} | {:.0} |",
            median(drawn_first[i].clone())
        );
        println!(
            "| first pan frame {name}: prefetched tiles hit | {n} | {:.0} |",
            median(hits_first[i].clone())
        );
    }
}

/// The facade's own cost (1155) on a warm pan and at rest, every worker:
/// the frame as Java got it before, copied out behind its description
/// into the answer's bytes, against the frame drawn into the Bitmap with
/// the description encoded. Java's side, the `byte[]` and the Bitmap
/// copy it no longer makes, is not in either.
fn facade(
    home: &std::path::Path,
    base: &MapRequest,
    into: &impl Fn(&MapRequest) -> MapShown,
    now: WallTime,
    median: impl Fn(Vec<f64>) -> f64,
    workers: usize,
) {
    loquax_android::map::tune_tiles(home, Some(workers), false);
    let copied = |ask: &MapRequest| -> MapShown {
        let bytes = loquax_android::map::render(ask, Some(Vec::new()), now);
        let Ok((shown, rgba)) = schema::unframe(&bytes) else {
            eprintln!("no frame");
            std::process::exit(1);
        };
        std::hint::black_box(rgba.len());
        shown
    };
    println!("\n| facade, {workers} workers | copied out, ms | into the Bitmap, ms |");
    println!("|---|---|---|");
    for moving in [true, false] {
        let mut ask = MapRequest {
            moving,
            ..base.clone()
        };
        let _ = into(&ask);
        let (mut old, mut new) = (Vec::new(), Vec::new());
        for i in 0..21 {
            ask.pan_x = match moving {
                true if i % 2 == 0 => 40,
                true => -40,
                false => 0,
            };
            let t = Instant::now();
            let shown = match i % 2 == 0 {
                true => copied(&ask),
                false => into(&ask),
            };
            let ms = t.elapsed().as_secs_f64() * 1e3;
            match i % 2 == 0 {
                true => old.push(ms),
                false => new.push(ms),
            }
            (ask.x, ask.y) = (Some(shown.x), Some(shown.y));
        }
        let name = match moving {
            true => "pan, cache warm",
            false => "at rest, cache warm",
        };
        println!("| {name} | {:.2} | {:.2} |", median(old), median(new));
    }
}
