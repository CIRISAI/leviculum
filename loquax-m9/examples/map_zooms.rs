//! The M9's map drawn as the phone draws it (1565), on the host, zoom by
//! zoom: what the board's `[MAP]` line says for each, so the reviewer's
//! window has a host column beside the board's.
//!
//! ```sh
//! cargo run --release -p loquax-m9 --example map_zooms
//! ```
//!
//! Over the board's own map (the Florence fixture and the world) at the
//! fixture's walk centre, zooms 11 to 17, through the board's renderer
//! (`Renderer::show`) with its clock set, as the firmware runs it:
//!
//! - `cold`: the picture drawn, the view moved an eighth of the screen on
//!   every run so the kept picture never holds it; a pan step is this.
//! - `zoom`: the step from the zoom above or below into this one.
//! - `cache`: a page switch back to the map, the kept picture copied.
//!
//! Each is the median of [`RUNS`] after one warm-up. The heap a draw
//! takes is the census's (`loquax-android/tests/m9_fixture_census.rs`,
//! which counts the allocator and holds the map's draw to
//! `loquax_m9::census::MAP`); this crate forbids the `unsafe` a counting
//! allocator here would need. Host times rank, they do not time the
//! board, which is about twenty times slower
//! (`docs/drafts/map-measurement-2026-10.md`).
//!
//! A second table: the names the phone's rule sets at each zoom with the
//! type at other scales than the board's [`map::SCALE`], the trade-off
//! between the size of a name and how many fit the 320 x 240 picture.
//! A measuring tool, not a gate.

use std::sync::OnceLock;
use std::time::Instant;

use loquax_m9::fixture::Screen;
use loquax_m9::frame::Frame;
use loquax_m9::glyphs::Glyphs;
use loquax_m9::map::{self, Map, FIXTURE};
use loquax_m9::render::{MapStats, Renderer};
use loquax_m9::screen::Look;

const RUNS: usize = 21;

fn clock() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

fn median(mut v: Vec<u64>) -> f64 {
    v.sort_unstable();
    v[v.len() / 2] as f64 / 1e3
}

/// The fixture's map screen at `zoom`, its centre moved east by `east`
/// pixels.
fn at(zoom: u8, east: i32) -> loquax::ui::UiModel<loquax_m9::field::Field> {
    let mut model = Screen::Map.model();
    if let Some(state) = model.map.as_mut() {
        state.zoom = zoom;
    }
    if east != 0 {
        if let Some((view, _)) = map::view(&model) {
            let mut moved = view;
            moved.pan(east, 0);
            if let Some(state) = model.map.as_mut() {
                state.centre = moved.centre;
            }
        }
    }
    model
}

fn show(renderer: &mut Renderer<'_, Frame>, map: &mut Map<'_>, zoom: u8, east: i32) -> MapStats {
    let model = at(zoom, east);
    renderer.show(&model, &Look::of(&model), map).expect("show");
    renderer.take_map_stats().expect("map stats")
}

fn main() {
    let mut panel = Frame::new();
    let mut map = Map::board(FIXTURE, &[]).expect("map");
    map.keep_pictures();
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    renderer.set_clock(clock);
    println!(
        "zoom names found glyphs | cold: picture_ms names_ms total_ms | zoom_step_ms | cache_ms"
    );
    for zoom in 11u8..=17 {
        // Warm-up, then cold draws a pan step apart.
        let _ = show(&mut renderer, &mut map, zoom, 0);
        let (mut picture, mut names, mut total) = (Vec::new(), Vec::new(), Vec::new());
        let mut last = MapStats::default();
        for i in 0..RUNS {
            let east = if i % 2 == 0 { 40 } else { 0 };
            let t = clock();
            last = show(&mut renderer, &mut map, zoom, east);
            total.push(clock() - t);
            assert!(!last.kept, "a cold draw draws");
            picture.push(last.picture_us);
            names.push(last.names_us);
        }
        // The zoom step: from the zoom beside it into this one.
        let mut step = Vec::new();
        let from = if zoom > 11 { zoom - 1 } else { zoom + 1 };
        for _ in 0..RUNS {
            let _ = show(&mut renderer, &mut map, from, 0);
            let t = clock();
            let _ = show(&mut renderer, &mut map, zoom, 0);
            step.push(clock() - t);
        }
        // The page switch: the same view again, copied from the kept one.
        let mut cache = Vec::new();
        for _ in 0..RUNS {
            let t = clock();
            let st = show(&mut renderer, &mut map, zoom, 0);
            cache.push(clock() - t);
            assert!(st.kept, "copied back");
        }
        println!(
            "z{zoom} {} {} {} | {:.3} {:.3} {:.3} | {:.3} | {:.3}",
            last.names.drawn,
            last.names.found,
            last.names.glyphs,
            median(picture),
            median(names),
            median(total),
            median(step),
            median(cache),
        );
    }
    drop(renderer);
    println!();
    println!(
        "names set / found by the type's scale (the board's is {})",
        map::SCALE
    );
    let mut glyphs = Glyphs::new().expect("font");
    for scale in [1.0f32, 1.2, 1.4, 1.6] {
        let mut row = format!("scale={scale}");
        for zoom in 11u8..=17 {
            let model = at(zoom, 0);
            let Some((view, pins)) = map::view(&model) else {
                continue;
            };
            let tall = map::with_bottom(&view);
            let (_, st) = map::names_scaled(
                &map.region,
                &tall,
                &map::pin_cells(&pins),
                &mut glyphs,
                false,
                scale,
            );
            row.push_str(&format!(" z{zoom}={}/{}", st.drawn, st.found));
        }
        println!("{row}");
    }
}
