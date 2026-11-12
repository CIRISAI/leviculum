//! Golden images per profile and the counts that gate their cost (1099,
//! `docs/drafts/map-renderer-roadmap-2026-10.md`, section 8).
//!
//! The Florence region drawn the way each device draws it: the M9 at
//! 320x240 (light, square caps, no casings), the PC at 1x in both tables
//! and at 2x (casings, round joins, dashed paths), and the terminal's text
//! techniques as cell dumps. The integer path has no anti-aliasing, so the
//! pictures are bit-exact: a PNG golden is compared pixel by pixel, zero
//! differing. The cost gate is counts, never a clock (the forge runner is
//! shared, 672, 689): the edges, spans and pixels the filler handed out
//! for fills, strokes and casings, per profile. A change that draws more
//! shows as a number here and states its delta in its commit.
//!
//! The RGBA path with its span ends blended by their coverage (1422) is
//! integer too, so its goldens are as exact.
//!
//! A picture that moved is written to `$TMPDIR/leviculum-golden/` under
//! its golden's name; look at it, and copy it over the golden when the
//! change is the one intended.

use std::path::PathBuf;

use embedded_graphics::prelude::*;
use leviculum_map::cells::{self, Encoding, Sets};
use leviculum_map::draw::{draw_in_scaled, draws_buildings, Ink, BUILDING_CAP};
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::Region;
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, FillStats, Scratch};
use leviculum_mapfill::frame::{rgb888, Frame, RgbaFrame};
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{MapTheme, Profile, Style};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");
/// The same Florence cut again with its building sections (version 3,
/// 1192): its tile blobs are `FLORENCE`'s byte for byte.
const BUILDINGS: &[u8] = include_bytes!("../fixtures/buildings/firenze.lmr");

/// The centre of the region's box.
fn centre() -> LatLon {
    let b = Region::from_bytes(FLORENCE).expect("open").header().bounds;
    LatLon {
        lat_e7: (b.north + b.south) / 2,
        lon_e7: (b.west + b.east) / 2,
    }
}

/// `view` drawn in `ink` and `style`, lines `percent` per cent wide, on a
/// frame cleared to the table's background: its pixels as RGB and what
/// the filler counted.
fn draw(view: &Viewport, ink: Ink, style: Style, percent: u32) -> (Vec<u8>, RenderStats) {
    draw_from(FLORENCE, view, ink, style, percent)
}

/// [`draw`] from the region `bytes`, with its building sections where
/// the view draws them, as the painter reads them.
fn draw_from(
    bytes: &[u8],
    view: &Viewport,
    ink: Ink,
    style: Style,
    percent: u32,
) -> (Vec<u8>, RenderStats) {
    let r = Region::from_bytes(bytes).expect("open");
    let mut tiles = r.view(view).expect("tiles");
    if draws_buildings(style, ink, view.zoom()) {
        r.with_buildings(&mut tiles).expect("buildings");
    }
    let (w, h) = view.size();
    let mut fb = vec![0u16; (w * h) as usize];
    let mut edges = vec![Edge::default(); 8192];
    let mut active = vec![0u16; 8192];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 8192];
    let mut ring_ends = vec![0u16; 2048];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let Ok(()) = frame.clear(style.theme.palette().background);
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(st) = draw_in_scaled(
        &tiles,
        r.area(view),
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
    let frame = Frame::new(&mut fb, w, h).expect("frame");
    frame.rgb888_into(&mut rgb);
    (rgb, st)
}

/// [`draw_from`] into an RGBA frame with the span ends blended by their
/// coverage (1422): its pixels as RGB.
fn draw_smooth(bytes: &[u8], view: &Viewport, style: Style, percent: u32) -> Vec<u8> {
    let r = Region::from_bytes(bytes).expect("open");
    let mut tiles = r.view(view).expect("tiles");
    if draws_buildings(style, Ink::Colour, view.zoom()) {
        r.with_buildings(&mut tiles).expect("buildings");
    }
    let (w, h) = view.size();
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut edges = vec![Edge::default(); 8192];
    let mut active = vec![0u16; 8192];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 8192];
    let mut ring_ends = vec![0u16; 2048];
    let mut frame = RgbaFrame::new(&mut px, w, h).expect("frame").smoothed(true);
    let Ok(()) = frame.clear(style.theme.palette().background);
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
        r.area(view),
        Profile::Full,
        Ink::Colour,
        style,
        percent,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    px.chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// 1422: the edges smoothed, at z13, z15 and z17 (with the buildings),
/// in the screens' finish at 480x320, each its golden; and against the
/// integer picture of the same view only pixels at an edge differ, a
/// few per cent of them, never the whole picture.
#[test]
fn smoothed_edges_draw_their_goldens_at_three_zooms() {
    let at = project(centre());
    let (w, h) = (480, 320);
    for z in [13u8, 15, 17] {
        let view = Viewport::new(at, z, w, h);
        let smooth = draw_smooth(BUILDINGS, &view, Style::SCREEN, 100);
        let (hard, _) = draw_from(BUILDINGS, &view, Ink::Colour, Style::SCREEN, 100);
        let differ = smooth
            .chunks(3)
            .zip(hard.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        let share = differ as f64 / f64::from(w * h);
        assert!(
            share > 0.005 && share < 0.25,
            "z{z}: {differ} pixels differ"
        );
        same_as(&format!("screen-smooth-480x320-z{z}.png"), &smooth, w, h);
    }
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Where a picture that moved is left for a look.
fn kept(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join("leviculum-golden");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    path
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

fn unpng(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .expect("png");
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buf).expect("frame");
    assert_eq!(
        (info.color_type, info.bit_depth),
        (png::ColorType::Rgb, png::BitDepth::Eight)
    );
    buf.truncate(info.buffer_size());
    (buf, info.width, info.height)
}

/// `LEVICULUM_WRITE_GOLDEN=1` writes the goldens anew, for a change that
/// means to move them.
fn writing() -> bool {
    std::env::var_os("LEVICULUM_WRITE_GOLDEN").is_some()
}

/// Hold `rgb` to the PNG golden `name`, pixel for pixel.
fn same_as(name: &str, rgb: &[u8], w: u32, h: u32) {
    if writing() {
        std::fs::write(golden(name), png(rgb, w, h)).expect("write the golden");
        return;
    }
    let Ok(bytes) = std::fs::read(golden(name)) else {
        let path = kept(name, &png(rgb, w, h));
        panic!("no golden {name}; this run's is {}", path.display());
    };
    let (want, gw, gh) = unpng(&bytes);
    if (gw, gh) != (w, h) || want != rgb {
        let differ = want
            .chunks(3)
            .zip(rgb.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        let path = kept(name, &png(rgb, w, h));
        panic!(
            "{name} moved: {differ} pixels differ ({gw}x{gh} against {w}x{h}); this run's is {}",
            path.display()
        );
    }
}

fn counts(s: FillStats) -> (u32, u32, u32) {
    (s.edges, s.spans, s.pixels)
}

/// One profile's view: its golden, style, zoom, size and line width, and
/// the fill, stroke and casing counts (edges, spans, pixels) it costs.
type Case = (
    &'static str,
    Style,
    u8,
    (u32, u32),
    u32,
    [(u32, u32, u32); 3],
);

/// Each profile's picture, and its fill, stroke and casing counts.
#[test]
fn each_profile_draws_its_golden_at_its_counts() {
    let at = project(centre());
    let cases: [Case; 4] = [
        (
            "m9-light-320x240-z15.png",
            Style::M9,
            15,
            (320, 240),
            100,
            [(831, 2384, 138_593), (2105, 6738, 12_544), (0, 0, 0)],
        ),
        (
            "pc-light-480x240-z15.png",
            Style::PC,
            15,
            (480, 240),
            100,
            [
                (1038, 2725, 191_173),
                (10_435, 7998, 16_419),
                (282, 279, 1613),
            ],
        ),
        (
            "pc-dark-480x240-z15.png",
            Style::PC.in_theme(MapTheme::Dark),
            15,
            (480, 240),
            100,
            [
                (1038, 2725, 191_173),
                (10_435, 7998, 16_419),
                (282, 279, 1613),
            ],
        ),
        (
            "pc-light-2x-640x320-z16.png",
            Style::PC,
            16,
            (640, 320),
            200,
            [(722, 3969, 479_497), (7037, 9310, 34_552), (1, 0, 0)],
        ),
    ];
    let mut moved = Vec::new();
    for (name, style, zoom, (w, h), percent, want) in cases {
        let view = Viewport::new(at, zoom, w, h);
        let (rgb, st) = draw(&view, Ink::Colour, style, percent);
        assert_eq!(st.overflows, 0, "{name}");
        let got = [counts(st.fill), counts(st.stroke), counts(st.casing)];
        if got != want {
            moved.push(format!("{name}: {got:?}"));
        }
        same_as(name, &rgb, w, h);
    }
    assert!(moved.is_empty(), "the counts moved:\n{}", moved.join("\n"));
}

/// The M9's finish strokes no casing, and the PC's finish draws the same
/// fills: casings and joints cost strokes, never areas.
#[test]
fn casings_cost_strokes_only_and_never_on_the_m9() {
    let view = Viewport::new(project(centre()), 15, 480, 240);
    let (_, m9) = draw(&view, Ink::Colour, Style::M9, 100);
    let (_, pc) = draw(&view, Ink::Colour, Style::PC, 100);
    assert_eq!(m9.casing, FillStats::default());
    assert!(pc.casing.pixels > 0);
    assert_eq!(m9.fill, pc.fill);
    assert_eq!((m9.features, m9.culled), (pc.features, pc.culled));
}

/// The terminal's techniques as the frames a terminal shows: each cell's
/// glyph, and its two colours as the sixteen-colour index of the table
/// (`.` for none, `*` for a mean of two table colours).
fn dump(cells: &[cells::Cell], cols: usize, theme: MapTheme) -> String {
    let code = |c: Option<cells::Rgb>| match c {
        None => '.',
        Some(rgb) => match cells::ansi16(rgb, theme) {
            Some(i) => char::from_digit(u32::from(i), 16).unwrap_or('?'),
            None => '*',
        },
    };
    let mut out = String::new();
    for row in cells.chunks(cols) {
        out.extend(row.iter().map(|c| c.ch));
        out.push('\n');
        out.extend(row.iter().map(|c| code(c.fg)));
        out.push('\n');
        out.extend(row.iter().map(|c| code(c.bg)));
        out.push('\n');
    }
    out
}

/// The text techniques' frames, drawn the way the terminal draws them: at
/// four by eight pixels a cell, one zoom deeper and twice as wide.
#[test]
fn the_terminal_techniques_dump_their_golden_frames() {
    let (cols, rows) = (60usize, 16usize);
    let (w, h) = ((cols * cells::SX) as u32, (rows * cells::SY) as u32);
    let view = Viewport::new(project(centre()), 16, w, h);
    let cases = [
        ("octant-pc-light.cells", Encoding::Octant, Style::PC),
        (
            "octant-pc-dark.cells",
            Encoding::Octant,
            Style::PC.in_theme(MapTheme::Dark),
        ),
        (
            "braille-thin-m9-light.cells",
            Encoding::BrailleThin,
            Style::M9,
        ),
    ];
    let sets = Sets::default();
    let mut moved = Vec::new();
    for (name, encoding, style) in cases {
        let to_rgb = |v: Vec<u8>| -> Vec<cells::Rgb> {
            v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
        };
        let ground = cells::rgb_of(style.theme.palette().background);
        let mask = |percent| -> Vec<bool> {
            let ink = Ink::Lines(embedded_graphics::pixelcolor::Rgb565::new(0, 0, 0));
            to_rgb(draw(&view, ink, style, percent).0)
                .into_iter()
                .map(|c| c != ground)
                .collect()
        };
        let source = cells::Source {
            cols,
            rows,
            rgb: to_rgb(draw(&view, Ink::Colour, style, 200).0),
            line: mask(200),
            thin: mask(100),
            theme: style.theme,
        };
        let text = dump(&cells::encode(&source, encoding, &sets), cols, style.theme);
        let want = std::fs::read_to_string(golden(name)).unwrap_or_default();
        if want != text {
            moved.push(kept(name, text.as_bytes()).display().to_string());
        }
    }
    assert!(moved.is_empty(), "frames moved, this run's: {moved:?}");
}

/// The buildings' views (1192): z16 and z17, light and dark, at the
/// window's line width and at the phone's (density 2.75), each its golden
/// pixel for pixel, every footprint in view drawn and outlined, none an
/// overflow. The phone and the window draw these through the same tiles
/// (`loquax::tiles`), so a picture here is what both show.
#[test]
fn the_buildings_draw_their_goldens_from_z16() {
    let at = project(centre());
    let mut moved = Vec::new();
    for zoom in [16u8, 17] {
        for (theme, t) in [(MapTheme::Light, "light"), (MapTheme::Dark, "dark")] {
            for (percent, who) in [(100u32, "window"), (275, "phone")] {
                let name = format!("screen-{t}-{who}-480x320-z{zoom}.png");
                let view = Viewport::new(at, zoom, 480, 320);
                let style = Style::SCREEN.in_theme(theme);
                let (rgb, st) = draw_from(BUILDINGS, &view, Ink::Colour, style, percent);
                assert_eq!(st.overflows, 0, "{name}");
                assert_eq!(st.blocks, 0, "{name}");
                assert!(
                    st.buildings > 100 && st.outline > st.buildings,
                    "{name}: {st:?}"
                );
                // Without buildings the same view differs only where they are.
                let (plain, _) = draw_from(
                    BUILDINGS,
                    &view,
                    Ink::Colour,
                    Style::PC.in_theme(theme),
                    percent,
                );
                let fill = rgb888(
                    theme
                        .palette()
                        .area(leviculum_mapfill::style::Class::Building),
                );
                let edge = rgb888(theme.palette().building_edge);
                for (a, b) in rgb.chunks(3).zip(plain.chunks(3)) {
                    assert!(a == b || a == fill || a == edge, "{name}: {a:?} over {b:?}");
                }
                if let Err(e) = std::panic::catch_unwind(|| same_as(&name, &rgb, 480, 320)) {
                    moved.push(format!("{name}: {:?}", e.downcast_ref::<String>()));
                }
            }
        }
    }
    assert!(moved.is_empty(), "moved:\n{}", moved.join("\n"));
}

/// The blended world views (1710): central Europe out of Equal Earth
/// and out of the orthographic globe, at the world (`t = 0`) and in the
/// middle of the handover (z3.5), pinned pixel for pixel. The linear
/// goldens at z6 below pin the other end: from `t = 1` the blend is
/// today's Mercator exactly, which `tests/blend.rs` holds to a
/// nanopixel.
#[test]
fn the_blended_world_views_draw_their_goldens() {
    use leviculum_map::blend::{Blend, Shape};
    let at = project(LatLon::from_degrees(50.0, 10.0));
    for (sname, shape) in [("ee", Shape::EqualEarth), ("globe", Shape::Globe)] {
        for (zname, zoom) in [("z2", 2.0), ("z35", 3.5)] {
            let (w, h) = (480u32, 320u32);
            let blend = Blend::new(shape, at, zoom, w, h);
            let mut fb = vec![0u16; (w * h) as usize];
            let mut frame = Frame::new(&mut fb, w, h).expect("frame");
            let Ok(_) = leviculum_map::outline::draw_world_view(
                &leviculum_map::outline::WORLD,
                &blend,
                Ink::Colour,
                Style::SCREEN.theme,
                &mut frame,
            );
            let mut rgb = Vec::with_capacity(fb.len() * 3);
            let frame = Frame::new(&mut fb, w, h).expect("frame");
            frame.rgb888_into(&mut rgb);
            same_as(&format!("world-blend-{sname}-480x320-{zname}.png"), &rgb, w, h);
        }
    }
}

/// The world view before any region covers it (1683): `view` drawn from
/// the outline's own tables — land, lakes, rivers, ice, the shelves, the
/// country borders and the disputed lines dashed — in `style` and `ink`,
/// on the table's background, as RGB.
fn draw_world(view: &Viewport, ink: Ink, style: Style) -> Vec<u8> {
    let (w, h) = view.size();
    let mut fb = vec![0u16; (w * h) as usize];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let Ok(_) = leviculum_map::outline::draw_world(
        &leviculum_map::outline::WORLD,
        view,
        ink,
        style.theme,
        &mut frame,
    );
    let mut rgb = Vec::with_capacity(fb.len() * 3);
    let frame = Frame::new(&mut fb, w, h).expect("frame");
    frame.rgb888_into(&mut rgb);
    rgb
}

/// The world view at z2 and z6, where no region of the app's own is
/// loaded (1683): central Europe, drawn the way the M9 and the window
/// draw it, and the terminal's cells, each pinned pixel for pixel
/// (`.cells` byte for byte). The phone draws the window's bytes: the
/// outline's widths are in sub-pixels of the view, not of the density,
/// and a world view has no tiles to gain the density's width from; its
/// names at 2.75 are pinned by the window's map tests. The layers the outline gained in 1683
/// are in these: the lakes and rivers of Europe, the ice of the Alps, the
/// coasts, and the borders, some of them a disputed line dashed.
#[test]
fn the_world_view_draws_its_goldens_at_z2_and_z6() {
    let at = project(LatLon::from_degrees(50.0, 10.0));
    for zoom in [2u8, 6] {
        for (name, style, (w, h)) in [
            (
                format!("world-m9-320x240-z{zoom}.png"),
                Style::M9,
                (320, 240),
            ),
            (
                format!("world-window-480x320-z{zoom}.png"),
                Style::SCREEN,
                (480, 320),
            ),
        ] {
            let view = Viewport::new(at, zoom, w, h);
            let rgb = draw_world(&view, Ink::Colour, style);
            same_as(&name, &rgb, w, h);
        }
        // The terminal's technique: the view a text cell level deeper,
        // twice as wide, in the window's finish, as cells of glyphs.
        let (cols, rows) = (60usize, 16usize);
        let (w, h) = ((cols * cells::SX) as u32, (rows * cells::SY) as u32);
        let view = Viewport::new(at, zoom + 1, w, h);
        let style = Style::PC;
        let to_rgb = |v: Vec<u8>| -> Vec<cells::Rgb> {
            v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
        };
        let ground = cells::rgb_of(style.theme.palette().background);
        let ink = Ink::Lines(embedded_graphics::pixelcolor::Rgb565::new(0, 0, 0));
        let inked: Vec<bool> = to_rgb(draw_world(&view, ink, style))
            .into_iter()
            .map(|c| c != ground)
            .collect();
        let source = cells::Source {
            cols,
            rows,
            rgb: to_rgb(draw_world(&view, Ink::Colour, style)),
            line: inked.clone(),
            thin: inked,
            theme: style.theme,
        };
        let sets = Sets::default();
        let text = dump(
            &cells::encode(&source, Encoding::Octant, &sets),
            cols,
            style.theme,
        );
        let name = format!("world-octant-pc-light-z{zoom}.cells");
        if writing() {
            std::fs::write(golden(&name), &text).expect("write the golden");
            continue;
        }
        let want = std::fs::read_to_string(golden(&name)).unwrap_or_default();
        assert_eq!(
            want,
            text,
            "the frame moved; this run's is {}",
            kept(&name, text.as_bytes()).display()
        );
    }
}

/// The world view's data is small enough for a phone to carry it whole
/// (1683): every table the outline draws, its names included, under
/// 400 KB — the pin the concept sets, and a budget the firmware's flash
/// and a package both feel.
#[test]
fn the_world_tables_stay_under_four_hundred_kilobytes() {
    use leviculum_map::outline::{
        DISPUTED_50M, ICE_50M, LAKES_50M, NAMES_50M, NAMES_M9, RIVERS_50M, SHELVES_50M, WORLD_50M,
    };
    let tables: [(&str, &[u8]); 8] = [
        ("land", WORLD_50M),
        ("lakes", LAKES_50M),
        ("rivers", RIVERS_50M),
        ("ice", ICE_50M),
        ("shelves", SHELVES_50M),
        ("disputed", DISPUTED_50M),
        ("names", NAMES_50M),
        ("names for the M9", NAMES_M9),
    ];
    let mut total = 0usize;
    let mut lines = Vec::new();
    for (name, bytes) in tables {
        total += bytes.len();
        lines.push(format!("{name}: {} B", bytes.len()));
    }
    let borders = leviculum_map::outline::BORDERS_50M;
    total += borders.len();
    lines.push(format!("borders: {} B", borders.len()));
    assert!(
        total < 400 * 1024,
        "the world view: {total} B\n{}",
        lines.join("\n")
    );
}

/// the region with building sections, drawn in the window's and the
/// phone's finish, gives the committed goldens byte for byte, and z11 to
/// z14 the frames of the region without them. A view the terminal draws
/// (the PC's finish) never draws them, at z16 neither.
#[test]
fn the_frames_at_z15_and_shallower_are_todays() {
    let at = project(centre());
    for (name, style, zoom, (w, h)) in [
        ("pc-light-480x240-z15.png", Style::SCREEN, 15, (480, 240)),
        (
            "pc-dark-480x240-z15.png",
            Style::SCREEN.in_theme(MapTheme::Dark),
            15,
            (480, 240),
        ),
        ("pc-light-2x-640x320-z16.png", Style::PC, 16, (640, 320)),
    ] {
        let view = Viewport::new(at, zoom, w, h);
        let percent = if zoom == 16 { 200 } else { 100 };
        let (rgb, st) = draw_from(BUILDINGS, &view, Ink::Colour, style, percent);
        assert_eq!(st.buildings, 0, "{name}");
        same_as(name, &rgb, w, h);
    }
    for zoom in 11..=15 {
        for theme in [MapTheme::Light, MapTheme::Dark] {
            let view = Viewport::new(at, zoom, 480, 320);
            let with = draw_from(
                BUILDINGS,
                &view,
                Ink::Colour,
                Style::SCREEN.in_theme(theme),
                100,
            );
            let without = draw_from(FLORENCE, &view, Ink::Colour, Style::PC.in_theme(theme), 100);
            assert!(with.0 == without.0, "z{zoom} {theme:?} moved");
            assert_eq!(with.1, without.1, "z{zoom} {theme:?}");
        }
    }
}

/// The cap (1192): a z15 tile holding [`BUILDING_CAP`] footprints is drawn
/// outlined, one more and the tile is drawn as blocks, every footprint
/// filled, no outline, and the fills the same as the outlined ones'.
#[test]
fn a_tile_over_the_cap_is_drawn_as_blocks() {
    use leviculum_map::buildings::{encode_section, Buildings};
    let r = Region::from_bytes(FLORENCE).expect("open");
    let view = Viewport::new(project(centre()), 16, 480, 320);
    let squares = |n: u32| -> Vec<Vec<Vec<(i16, i16)>>> {
        (0..n)
            .map(|i| {
                let (x, y) = ((i % 46) as i16 * 88, (i / 46) as i16 * 88);
                vec![vec![(x, y), (x + 40, y), (x + 40, y + 40), (x, y + 40)]]
            })
            .collect()
    };
    let mut seen = Vec::new();
    for n in [BUILDING_CAP, BUILDING_CAP + 1] {
        let section = encode_section(&squares(n)).expect("section");
        let b = Buildings::parse(&section).expect("parse");
        assert_eq!(b.len(), n);
        let mut tiles = r.view(&view).expect("tiles");
        for t in tiles.iter_mut().filter(|t| t.tile.id.z == 15) {
            t.buildings = Some(b);
        }
        let (w, h) = view.size();
        let mut fb = vec![0u16; (w * h) as usize];
        let mut edges = vec![Edge::default(); 8192];
        let mut active = vec![0u16; 8192];
        let mut heads = vec![0u16; h as usize];
        let mut points = vec![Point::zero(); 8192];
        let mut ring_ends = vec![0u16; 2048];
        let mut frame = Frame::new(&mut fb, w, h).expect("frame");
        let mut scratch = Scratch {
            edges: &mut edges,
            heads: &mut heads,
            active: &mut active,
        };
        let mut work = Work {
            points: &mut points,
            ring_ends: &mut ring_ends,
        };
        let Ok(st) = draw_in_scaled(
            &tiles,
            r.area(&view),
            Profile::Full,
            Ink::Colour,
            Style::SCREEN,
            100,
            &mut frame,
            &mut scratch,
            &mut work,
            &mut NoClock,
        );
        assert!(st.buildings > 0, "{n}");
        seen.push(st);
    }
    let (under, over) = (seen[0], seen[1]);
    assert_eq!((under.blocks, over.blocks), (0, 2));
    assert!(under.outline >= 4 * under.buildings, "{under:?}");
    assert_eq!(over.outline, 0);
    assert!(over.buildings >= under.buildings);
}
