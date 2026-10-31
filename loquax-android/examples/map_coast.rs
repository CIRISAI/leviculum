//! Host frame rate of the phone's map during a coast (1188): the portrait
//! 1080 x 2400 screen at a density of 2.75 with a quarter screen of margin
//! on each side, a 1620 x 3600 frame, Frankfurt at z15, drawn through the
//! facade's call into a Bitmap-sized buffer as `MapView` asks it.
//!
//! The coast runs on the wall clock: a fling at `SPEED` pixels a second
//! (default 10000, a hard fling on such a phone, whose ceiling is
//! ViewConfiguration's 8000 dp a second, 22000 pixels) slowing down as
//! e^(-t / 0.325 s) until below 50 pixels a second, the shape of the
//! platform's fling, not its curve. As on the phone, each Choreographer
//! frame (60 Hz here) moves the screen by the coast and owes a frame, and
//! the frame thread asks for it, with the pan the coast made since, moving,
//! as soon as it is free: at the next vsync when the last frame landed
//! within one, at once when it took longer. Printed: the
//! frames drawn and their rate, the vsyncs (60 Hz) the coast spanned and
//! how many of them saw a new frame, the frame times, and the farthest the
//! screen ran ahead of the frame it showed against the margin it has: past
//! the margin the phone would show an edge with no map. Then the frame at
//! rest that ends the coast.
//!
//! ```sh
//! cargo run --release -p loquax-android --example map_coast -- HOME [SPEED]
//! ```
//!
//! `HOME` as for `map_fps`: `maps/` with the Frankfurt region and a
//! `config` with `map_autoload = off`. A measuring tool, not a gate.

use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use loquax::when::WallTime;
use loquax_android::map::Canvas;
use loquax_android::schema::{self, MapRequest, MapShown};

/// The app's Bitmap, as in `map_fps`.
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

/// The coast's time constant, seconds.
const TAU: f64 = 0.325;
/// The coast ends below this speed, pixels a second.
const END: f64 = 50.0;
const VSYNC_MS: f64 = 1000.0 / 60.0;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(home) = args.next().map(PathBuf::from) else {
        eprintln!("usage: map_coast HOME [SPEED]");
        std::process::exit(2);
    };
    let speed: f64 = args
        .next()
        .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(10_000.0);
    let (w, h) = (1080u32, 2400u32);
    let at = project(LatLon::from_degrees(50.1109, 8.6821));
    let now = WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    };
    let mut ask = MapRequest {
        dir: home.display().to_string(),
        w,
        h,
        density_milli: 2750,
        x: Some(at.x),
        y: Some(at.y),
        zoom_milli: Some(15_000),
        margin_x: w / 4,
        margin_y: h / 4,
        ..MapRequest::default()
    };
    let mut bitmap = Bitmap::default();
    let mut draw = |ask: &MapRequest| -> MapShown {
        let shown = loquax_android::map::render_into(ask, Some(Vec::new()), now, &mut bitmap);
        std::hint::black_box(schema::encode(&shown));
        if !shown.ok {
            eprintln!("no frame: {:?}", shown.error);
            std::process::exit(1);
        }
        shown
    };
    // Warm: the view at rest, then a coast's worth of the way there and
    // back, so the coast measures the warm cache as a second fling would.
    let rest = draw(&ask);
    println!(
        "frame {}x{}, margin {}x{}",
        rest.w, rest.h, rest.margin_x, rest.margin_y
    );
    let reach = (speed * TAU).round() as i32;
    for pan in [reach, -reach] {
        let shown = draw(&MapRequest {
            pan_x: pan,
            moving: true,
            x: ask.x,
            y: ask.y,
            ..ask.clone()
        });
        ask.x = Some(shown.x);
        ask.y = Some(shown.y);
    }
    let _ = draw(&ask);

    let way = |t: f64| speed * TAU * (1.0 - (-t / TAU).exp());
    let duration = TAU * (speed / END).ln();
    let start = Instant::now();
    let secs = || start.elapsed().as_secs_f64();
    let mut done = 0i64;
    let mut times = Vec::new();
    let mut ahead_max = 0.0f64;
    let mut shown_way = 0.0;
    let mut landed = Vec::new();
    let mut asked_at: Option<u64> = None;
    loop {
        // The frame owed by the vsync after the one the last was asked at.
        let owed = asked_at.map_or(0, |k| k + 1) as f64 * VSYNC_MS / 1e3;
        let wait = owed - secs();
        if wait > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(wait));
        }
        let t0 = secs();
        asked_at = Some((t0 * 1e3 / VSYNC_MS).floor() as u64);
        if t0 >= duration {
            break;
        }
        let to = way(t0).round() as i64;
        let pan = (to - done) as i32;
        done = to;
        let asked = to as f64;
        let shown = draw(&MapRequest {
            pan_x: pan,
            moving: true,
            ..ask.clone()
        });
        let t1 = secs();
        times.push((t1 - t0) * 1e3);
        landed.push(t1);
        // Until this frame landed the screen showed the last one, moved
        // by all the coast went since that one was asked.
        ahead_max = ahead_max.max(way(t1) - shown_way);
        shown_way = asked;
        ask.x = Some(shown.x);
        ask.y = Some(shown.y);
        ask.zoom_milli = Some(shown.zoom_milli);
    }
    let span = secs().min(duration);
    let vsyncs = (span * 1e3 / VSYNC_MS).floor() as usize;
    let fresh = (0..vsyncs)
        .filter(|&k| {
            let (a, b) = (k as f64 * VSYNC_MS / 1e3, (k + 1) as f64 * VSYNC_MS / 1e3);
            landed.iter().any(|&t| t >= a && t < b)
        })
        .count();
    times.sort_by(f64::total_cmp);
    let pick = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    println!(
        "coast at {speed:.0} px/s: {:.0} ms, {:.0} px, {} frames, {:.1} frames/s",
        duration * 1e3,
        way(duration),
        times.len(),
        times.len() as f64 / span
    );
    println!(
        "vsyncs {vsyncs}, with a new frame {fresh}, without {}",
        vsyncs - fresh
    );
    println!(
        "frame ms: median {:.1}, p95 {:.1}, max {:.1}",
        pick(0.5),
        pick(0.95),
        pick(1.0)
    );
    println!(
        "screen ahead of its frame at most {ahead_max:.0} px, margin {} px",
        rest.margin_x
    );
    let t = Instant::now();
    let _ = draw(&MapRequest {
        pan_x: 0,
        moving: false,
        ..ask.clone()
    });
    println!(
        "frame at rest after the coast: {:.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
}
