//! What the map costs on the M9's numbers, on the host (slice seven,
//! 1375, `handheld-messenger.md`, "Slice seven, host half"): the tile set
//! of a view, its bytes and its decode, and the view drawn two ways into
//! the 320 x 240 RGB565 frame: the map core's filler straight into the
//! panel ([`loquax_m9::map::Painter`], what the M9 draws), and the
//! terminal's way, the view as cells of glyphs (`leviculum_map::cells`)
//! through ratatui and mousefood, in the two glyph sets mousefood's atlas
//! has, braille and quadrants.
//!
//! ```sh
//! cargo run --release -p loquax-m9 --example map_measure -- REGION...
//! cargo run --release -p loquax-m9 --example map_measure -- --world REGION...
//! ```
//!
//! `--world` measures the world overview instead (1683): each region is
//! drawn at its centre at zooms 0 to 3 as the board draws it, borders and
//! all ([`loquax_m9::map::Painter::picture`] with `overview`), and the
//! board's own name pass over it ([`loquax_m9::map::names`]), each a line
//! shaped like the board's `[MAP]` one. That line is the board's to say
//! with the board's clock; here it is the host's, so it ranks the zooms
//! and bounds nothing.
//!
//! Each region is viewed at its centre at zooms 13 to 15 over the M9's
//! picture, 320 x 180 pixels (32 x 9 cells of the 10 x 20 atlas; 318 x 210
//! and 53 x 21 when 1375 measured, in the 6 x 10). A row is the median of 50
//! runs after one warm-up; a pan is two views an eighth apart drawn in
//! turn, so ratatui's diff sees what a key that pans gives it. Host times
//! are upper bounds for nothing: the board is about twenty times slower
//! (`docs/drafts/map-measurement-2026-10.md`), so they rank the two ways,
//! they do not time the board. Heap is the census's
//! (`loquax-android/tests/m9_fixture_census.rs`); here the bytes each way
//! holds by construction. A measuring tool, not a gate.

use std::time::Instant;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_map::cells::{self, Encoding, Sets, Source, SX, SY};
use leviculum_map::draw::{draw_in_scaled, Ink};
use leviculum_map::region::Region;
use leviculum_map::viewport::Viewport;
use leviculum_map::World;
use leviculum_mapfill::fill::{Edge, Filler, Scratch};
use leviculum_mapfill::frame::Frame as FillFrame;
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::Profile;
use loquax::ui::map::MapState;
use loquax_m9::fixture;
use loquax_m9::frame::Frame;
use loquax_m9::map::{self, Map, Painter, CELL_PX, PICTURE};
use loquax_m9::render::Renderer;
use loquax_m9::screen::Look;
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;
use ratatui::Terminal;

const RUNS: usize = 50;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// The median milliseconds of `f` over [`RUNS`] after one warm-up, `f`
/// told the run's number.
fn time(mut f: impl FnMut(usize)) -> f64 {
    f(0);
    median(
        (1..=RUNS)
            .map(|i| {
                let t = Instant::now();
                f(i);
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect(),
    )
}

/// The picture's two views a pan apart, `i` choosing.
fn pan(view: &Viewport, i: usize) -> Viewport {
    let mut v = *view;
    if i % 2 == 1 {
        v.pan(i32::try_from(v.size().0 / 8).unwrap_or(0), 0);
    }
    v
}

/// The cells of a view, as a ratatui widget.
struct Cells<'a>(&'a [cells::Cell], usize);

impl Widget for Cells<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rgb = |c: Option<cells::Rgb>| c.map_or(Color::Reset, |[r, g, b]| Color::Rgb(r, g, b));
        for (i, cell) in self.0.iter().enumerate() {
            let (x, y) = ((i % self.1) as u16, (i / self.1) as u16);
            if x < area.width && y < area.height {
                let at = &mut buf[(area.x + x, area.y + y)];
                at.set_char(cell.ch)
                    .set_fg(rgb(cell.fg))
                    .set_bg(rgb(cell.bg));
            }
        }
    }
}

/// One view drawn at the cells' source density, its colour and, for
/// braille, which pixels the lines cover.
fn source(region: &Region<&[u8]>, view: &Viewport, lines: bool, fill: &mut FillScratch) -> Source {
    let (w, h) = view.size();
    let style = map::style();
    let tiles = region.view(view).expect("tiles");
    let mut draw = |ink: Ink, percent: u32| -> Vec<cells::Rgb> {
        let mut fb = vec![0u16; (w * h) as usize];
        let mut frame = FillFrame::new(&mut fb, w, h).expect("frame");
        let Ok(()) = frame.clear(style.theme.palette().background);
        let (mut scratch, mut work) = fill.parts(h);
        let _ = draw_in_scaled(
            &tiles,
            region.area(view),
            Profile::Full,
            ink,
            style,
            percent,
            &mut frame,
            &mut scratch,
            &mut work,
            &mut NoClock,
        );
        let mut rgb = Vec::with_capacity(fb.len() * 3);
        FillFrame::new(&mut fb, w, h)
            .expect("frame")
            .rgb888_into(&mut rgb);
        rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
    };
    let rgb = draw(Ink::Colour, 100);
    let (line, thin) = match lines {
        true => {
            let ground = cells::rgb_of(style.theme.palette().background);
            let mask = |px: Vec<cells::Rgb>| px.into_iter().map(|c| c != ground).collect();
            (
                mask(draw(Ink::Lines(Rgb565::BLACK), 200)),
                mask(draw(Ink::Lines(Rgb565::BLACK), 100)),
            )
        }
        false => (Vec::new(), Vec::new()),
    };
    Source {
        cols: (w as usize) / SX,
        rows: (h as usize) / SY,
        rgb,
        line,
        thin,
        theme: style.theme,
    }
}

/// The filler's storage for the source draws, the painter's sizes.
struct FillScratch {
    edges: Vec<Edge>,
    active: Vec<u16>,
    heads: Vec<u16>,
    points: Vec<Point>,
    ring_ends: Vec<u16>,
}

impl FillScratch {
    fn new() -> Self {
        Self {
            edges: vec![Edge::EMPTY; map::EDGES],
            active: vec![0; map::EDGES],
            heads: Vec::new(),
            points: vec![Point::zero(); map::POINTS],
            ring_ends: vec![0; map::RING_ENDS],
        }
    }

    fn parts(&mut self, h: u32) -> (Scratch<'_>, Work<'_>) {
        self.heads.resize(h as usize, 0);
        (
            Scratch {
                edges: &mut self.edges,
                heads: &mut self.heads,
                active: &mut self.active,
            },
            Work {
                points: &mut self.points,
                ring_ends: &mut self.ring_ends,
            },
        )
    }
}

fn main() {
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    paths.retain(|arg| arg != "--world");
    assert!(!paths.is_empty(), "usage: map_measure [--world] REGION...");
    let world = std::env::args().skip(1).any(|arg| arg == "--world");
    let (pw, ph) = (
        u32::from(PICTURE.width * CELL_PX.0),
        u32::from(PICTURE.height * CELL_PX.1),
    );
    println!(
        "picture {pw}x{ph} px, {}x{} cells; painter storage {} B; frame {} B",
        PICTURE.width,
        PICTURE.height,
        Painter::new().bytes(),
        loquax_m9::frame::WIDTH * loquax_m9::frame::HEIGHT * 2
    );
    for path in &paths {
        let bytes = std::fs::read(path).expect("read the region");
        let region = Region::from_bytes(&bytes).expect("a region");
        let h = region.header();
        let b = h.bounds;
        let centre = leviculum_map::mercator::project(leviculum_map::LatLon {
            lat_e7: (b.north + b.south) / 2,
            lon_e7: (b.west + b.east) / 2,
        });
        println!(
            "\n{path}: {} B, zooms {}-{}, {} tiles",
            bytes.len(),
            h.min_zoom,
            h.max_zoom,
            h.tiles
        );
        match world {
            true => {
                for z in 0..=3u8 {
                    world_row(&region, centre, z, pw, ph);
                }
            }
            false => {
                for z in 13..=15u8 {
                    row(&bytes, &region, centre, z, pw, ph);
                }
            }
        }
    }
}

/// The board's overview row (1683): the picture as the board draws it,
/// the borders and disputed lines over it included, and the board's name
/// pass over both tables, said in the board's `[MAP]` line's shape.
fn world_row(region: &Region<&[u8]>, centre: World, z: u8, pw: u32, ph: u32) {
    let view = Viewport::new(centre, z, pw, ph);
    let mut painter = Painter::new();
    let mut frame = Frame::new();
    let mut stats = None;
    let picture = time(|i| {
        stats = Some(
            painter
                .picture(region, &pan(&view, i), &mut frame, true)
                .expect("draw"),
        );
    });
    let rows = PICTURE.height;
    let mut found = None;
    let names = time(|i| {
        found = Some(map::names(region, &pan(&view, i), rows, &[], true).1);
    });
    let st = stats.unwrap_or_default();
    let n = found.unwrap_or_default();
    let (borders, b) = border_row(&view);
    println!(
        "[MAP] zoom={z} picture_ms={picture:.3} names_ms={names:.3} names={} dropped={} \
         features={} overflows={} pixels={}",
        n.drawn,
        n.dropped(),
        st.features,
        st.overflows,
        st.fill.pixels + st.stroke.pixels
    );
    println!(
        "      borders {borders:.3} ms: {} in view, {} edges + {} disputed ({} dashed) on the scratch's {} edge slots",
        b.borders,
        b.border.edges,
        b.disputed,
        b.dash.edges,
        map::EDGES
    );
}

/// The border table over `view` on the board's own scratch sizes: the
/// millisecond median, and what it stroked — the edges are summed over
/// every fill the pass made, so more than [`map::EDGES`] is the scratch
/// filling and going on, no line lost.
fn border_row(view: &Viewport) -> (f64, leviculum_map::outline::OutlineStats) {
    let (w, h) = view.size();
    let mut fb = vec![0u16; (w * h) as usize];
    let mut frame = FillFrame::new(&mut fb, w, h).expect("frame");
    let mut fill = FillScratch::new();
    let (mut scratch, _work) = fill.parts(h);
    let mut filler = Filler::new(&mut scratch, frame.bounding_box());
    let colour = map::style().theme.palette().border;
    let mut stats = None;
    let ms = time(|_| {
        stats = Some(
            leviculum_map::outline::bounds(
                &leviculum_map::outline::BOUNDARIES,
                view,
                Point::zero(),
                colour,
                &mut filler,
                &mut frame,
            )
            .expect("stroke"),
        );
    });
    (ms, stats.unwrap_or_default())
}

fn row(bytes: &[u8], region: &Region<&[u8]>, centre: World, z: u8, pw: u32, ph: u32) {
    let view = Viewport::new(centre, z, pw, ph);
    let (entries, _) = region.view_entries(&view).expect("entries");
    let blob: u64 = entries.iter().map(|(_, e)| u64::from(e.len)).sum();
    // Decode: the index searched and every tile of the view checked, in
    // place (flash); and read out into buffers (a card).
    let decode = time(|i| {
        let r = Region::from_bytes(bytes).expect("region");
        std::hint::black_box(r.view(&pan(&view, i)).expect("view"));
    });
    let mut out = Vec::new();
    let read = time(|i| {
        region.read_view(&pan(&view, i), &mut out).expect("read");
    });
    // The pixel painter, the picture alone and the whole screen.
    let mut painter = Painter::new();
    let mut frame = Frame::new();
    let mut stats = None;
    let picture = time(|i| {
        stats = Some(
            painter
                .picture(region, &pan(&view, i), &mut frame, false)
                .expect("draw"),
        );
    });
    let mut model = fixture::model();
    model.map = Some(MapState {
        centre,
        zoom: z,
        selected: None,
    });
    model.focus = loquax::ui::Pane::Map;
    let mut panel = Frame::new();
    let mut shown = Map::new(bytes).expect("map");
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let screen = time(|i| {
        if let Some(state) = model.map.as_mut() {
            state.centre = pan(&view, i).centre;
        }
        renderer
            .show(&model, &Look::of(&model), &mut shown)
            .expect("show");
    });
    // The cells through mousefood: the view drawn at the source density
    // (4 x 8 a cell), encoded, and the cells drawn by a terminal made once.
    let cview = Viewport::new(
        view.centre,
        z,
        u32::from(PICTURE.width) * SX as u32,
        u32::from(PICTURE.height) * SY as u32,
    );
    let sets = Sets::default();
    let mut fill = FillScratch::new();
    let mut cells_panel = Frame::new();
    let mut terminal = Terminal::new(EmbeddedBackend::new(
        &mut cells_panel,
        EmbeddedBackendConfig::default(),
    ))
    .expect("terminal");
    let cols = usize::from(PICTURE.width);
    let mut by_encoding = Vec::new();
    for (name, encoding, lines) in [
        ("braille", Encoding::BrailleBg, true),
        ("quadrant", Encoding::Quadrant, false),
    ] {
        let src_ms = time(|i| {
            std::hint::black_box(source(region, &pan(&cview, i), lines, &mut fill));
        });
        let srcs = [
            source(region, &pan(&cview, 0), lines, &mut fill),
            source(region, &pan(&cview, 1), lines, &mut fill),
        ];
        let enc_ms = time(|i| {
            std::hint::black_box(cells::encode(&srcs[i % 2], encoding, &sets));
        });
        let encoded = [
            cells::encode(&srcs[0], encoding, &sets),
            cells::encode(&srcs[1], encoding, &sets),
        ];
        let draw_ms = time(|i| {
            terminal
                .draw(|f| {
                    let area = Rect::new(0, 1, PICTURE.width, PICTURE.height);
                    f.render_widget(Cells(&encoded[i % 2], cols), area);
                })
                .expect("draw");
        });
        let held = srcs[0].rgb.len() * 3
            + srcs[0].line.len()
            + srcs[0].thin.len()
            + encoded[0].len() * std::mem::size_of::<cells::Cell>();
        by_encoding.push((name, src_ms, enc_ms, draw_ms, held));
    }
    let st = stats.unwrap_or_default();
    println!(
        "z{z}: view tiles {} ({blob} B of blobs), decode in place {decode:.3} ms, read out {read:.3} ms; \
         pixels: picture {picture:.3} ms, whole screen {screen:.3} ms, fill+stroke pixels {}",
        entries.len(),
        st.fill.pixels + st.stroke.pixels
    );
    for (name, src, enc, draw, held) in by_encoding {
        println!(
            "      cells {name}: source {src:.3} ms + encode {enc:.3} ms + mousefood {draw:.3} ms = {:.3} ms, \
             {held} B per view",
            src + enc + draw
        );
    }
}
