//! Host cost of the world outline under a frame no cached tile covers
//! (1143): the phone's frame, 1080 x 2400 with a quarter screen of margin
//! on each side (1620 x 3600), composed from an empty cache drawing no
//! tile, so the frame is the outline alone, against the same frame
//! composed from its tiles cached (the outline skipped): the difference is
//! what the outline adds to a frame, the rest is the frame's own (its
//! buffer, the sampling). Frankfurt at z15, z14.6 and z10, Europe at z5. A measuring tool for the reviewer and the commit
//! message, not a gate.
//!
//! ```sh
//! cargo run --release -p loquax --features picture --example outline_cost
//! ```
//!
//! No maps directory: only the outline. The median of 21 frames after one
//! warm-up, every core the cache takes and one.

use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::ExactView;
use leviculum_mapfill::style::Style;
use loquax::picture::Maps;
use loquax::tiles::{Frame, TileCache, BUDGET_MB};

fn median(mut ms: Vec<f64>) -> f64 {
    ms.sort_by(f64::total_cmp);
    ms[ms.len() / 2]
}

fn main() {
    let (w, h) = (1620u32, 3600u32);
    let frankfurt = project(LatLon::from_degrees(50.1109, 8.6821));
    let europe = project(LatLon::from_degrees(50.0, 10.0));
    let views = [
        ("Frankfurt z15", frankfurt, 15_000, 15),
        ("Frankfurt z14.6", frankfurt, 14_600, 15),
        ("Frankfurt z10", frankfurt, 10_000, 10),
        ("Europe z5", europe, 5_000, 5),
    ];
    for workers in [0usize, 1] {
        for &(name, centre, milli, zoom) in &views {
            let mut cache = TileCache::new(Maps::new(None), BUDGET_MB << 20);
            if workers > 0 {
                cache.set_workers(workers);
            }
            let frame = Frame {
                view: ExactView::new(centre, milli, w, h),
                zoom,
                style: Style::PC,
                scale: 2.75,
                render_limit: Some(0),
                fallback: None,
                moving: false,
                asked: None,
                smooth: true,
                resmooth: true,
            };
            let time = |cache: &mut TileCache, frame: &Frame| {
                let mut ms = Vec::new();
                for i in 0..22 {
                    let t = Instant::now();
                    let composed = cache.compose(frame);
                    let dt = t.elapsed().as_secs_f64() * 1000.0;
                    if i > 0 {
                        ms.push(dt);
                    }
                    assert_eq!(composed.missing > 0, frame.render_limit.is_some());
                }
                median(ms)
            };
            let outline = time(&mut cache, &frame);
            let mut warm = TileCache::new(Maps::new(None), BUDGET_MB << 20);
            if workers > 0 {
                warm.set_workers(workers);
            }
            let whole = Frame {
                render_limit: None,
                ..frame
            };
            warm.compose(&whole);
            let tiles = time(&mut warm, &whole);
            println!(
                "{name:16} {w}x{h} workers {:>2}: outline frame {outline:6.2} ms, \
                 cached frame {tiles:6.2} ms, outline adds {:6.2} ms",
                cache.workers(),
                outline - tiles
            );
        }
    }
}
