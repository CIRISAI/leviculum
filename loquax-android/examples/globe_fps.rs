//! Host frame times of the phone's map frame while a finger turns the
//! world view (1762): a portrait 1224 x 2100 view at a density of 2.75,
//! Lew's phone, with a quarter screen of margin each side as the app asks,
//! drawn through the facade's own call into a Bitmap-sized buffer
//! ([`loquax_android::map::render_into`]). No region is installed, so the
//! frame is the world outline through the blend: Equal Earth, or the
//! globe in a build with `loquax/globe-world`.
//!
//! ```sh
//! cargo run --release -p loquax-android --example globe_fps \
//!     [--features loquax/globe-world] -- HOME
//! ```
//!
//! `HOME` is an empty directory the run makes its `maps/` and `config`
//! in. Rows: a drag of 40 pixels a frame, the finger down, at the world
//! (z2.3), at z3.3 in the handover, and at z4.3 near its end, each
//! centred at 40 S 75 E (Lew's second screenshot) and at 50 N 10 E. Per
//! row the median and the 95th percentile of 60 frames after five
//! warm-up frames, against the display's frame budget: 16.7 ms at 60 Hz,
//! 8.3 ms at 120 Hz, and half of each, the order's p95 bound. A measuring
//! tool for the reviewer and the commit message, not a gate.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use loquax::when::WallTime;
use loquax_android::map::Canvas;
use loquax_android::schema::{MapRequest, MapShown};

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
        eprintln!("usage: globe_fps HOME");
        std::process::exit(2);
    };
    std::fs::create_dir_all(home.join("maps")).expect("maps dir");
    std::fs::write(home.join("config"), "map_autoload = off\n").expect("config");
    let (w, h) = (1224u32, 2100u32);
    let now = WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    };
    let bitmap = RefCell::new(Bitmap::default());
    let draw = |ask: &MapRequest| -> MapShown {
        let mut bitmap = bitmap.borrow_mut();
        let shown = loquax_android::map::render_into(ask, Some(Vec::new()), now, &mut *bitmap);
        if !shown.ok {
            eprintln!("no frame: {:?}", shown.error);
            std::process::exit(1);
        }
        shown
    };
    println!("| centre | zoom | frames | p50 ms | p95 ms | max ms |");
    println!("|---|---|---|---|---|---|");
    for (name, lat, lon) in [("40 S 75 E", -40.0, 75.0), ("50 N 10 E", 50.0, 10.0)] {
        let at = project(LatLon::from_degrees(lat, lon));
        for zoom_milli in [2300u32, 3300, 4300] {
            let mut ask = MapRequest {
                dir: home.display().to_string(),
                w,
                h,
                density_milli: 2750,
                x: Some(at.x),
                y: Some(at.y),
                zoom_milli: Some(zoom_milli),
                margin_x: w / 4,
                margin_y: h / 4,
                moving: true,
                ..MapRequest::default()
            };
            let mut ms = Vec::new();
            for i in 0..65 {
                // East and north, back and forth: a finger turning it.
                ask.pan_x = if (i / 8) % 2 == 0 { 40 } else { -40 };
                ask.pan_y = if (i / 16) % 2 == 0 { -24 } else { 24 };
                let t = Instant::now();
                let shown = draw(&ask);
                let e = t.elapsed().as_secs_f64() * 1e3;
                (ask.x, ask.y, ask.zoom_milli) =
                    (Some(shown.x), Some(shown.y), Some(shown.zoom_milli));
                if i >= 5 {
                    ms.push(e);
                }
            }
            ms.sort_by(f64::total_cmp);
            let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
            println!(
                "| {name} | {:.1} | {} | {:.1} | {:.1} | {:.1} |",
                f64::from(zoom_milli) / 1000.0,
                ms.len(),
                at(0.5),
                at(0.95),
                at(1.0)
            );
        }
    }
}
