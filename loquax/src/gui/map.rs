//! The window's map (`docs/src/concepts/loquax-map.md`, slice (d)): the
//! frame of a view as RGBA for an iced image, and the pins over it with
//! their labels set as text. The frame, the pins' places and the names
//! are [`loquax::mapframe`]'s, which the Android app draws through too
//! (1144); what is the window's own is its font measuring a label
//! ([`measure`]), as iced sets the label on the canvas.

pub use loquax::screenmap::{
    area, battery_at, hit, pin_hue, Placed, BATTERY, DRAG, LABEL, LINE, MARKER, OWN_DOT,
};

/// What the model's map functions are told in the window
/// ([`loquax::screenmap::setup`]), with the radar's zooms (1386): deeper
/// than the outline reaches where no region is, so the radar is there to
/// zoom into.
pub fn setup(painter: &loquax::picture::Painter) -> loquax::ui::map::MapSetup {
    let mut setup = loquax::screenmap::setup(painter);
    setup.zooms = loquax::ui::radar::zooms(setup.zooms);
    setup
}

/// The width of `text` at `size` pixels as the window sets it: iced's own
/// paragraph over its font system, with nothing drawn, so it works without
/// a display.
pub fn measure(text: &str, size: f32) -> u32 {
    use iced::advanced::graphics::text::Paragraph;
    use iced::advanced::text::{self, Paragraph as _};
    let paragraph = Paragraph::with_text(text::Text {
        content: text,
        bounds: iced::Size::INFINITE,
        size: iced::Pixels(size),
        line_height: text::LineHeight::default(),
        font: iced::Font::DEFAULT,
        align_x: text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::default(),
        wrapping: text::Wrapping::None,
    });
    paragraph.min_bounds().width.ceil() as u32
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::{ExactView, MILLI};
    use leviculum_mapfill::style::Style;
    use loquax::mapframe::{hold, Ask, Frames, Shot};
    use loquax::picture::Maps;
    use loquax::screenmap::metrics;
    use loquax::ui::map::{fit, zoom_floor, MapPin};
    use std::path::PathBuf;

    /// The committed Florence region alone.
    fn florence_maps() -> Maps {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let maps = Maps::new(Some(dir));
        let (regions, problems) = maps.load();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(regions.len(), 1);
        maps
    }

    /// The frames over the committed Florence region alone.
    pub(crate) fn florence() -> Frames {
        Frames::new(florence_maps())
    }

    pub(crate) fn pin(peer: u8, label: &str, lat: f64, lon: f64) -> MapPin {
        MapPin {
            peer: [peer; 16],
            at: project(LatLon::from_degrees(lat, lon)),
            label: label.to_string(),
            age: Some("5m".to_string()),
            stale: peer == 3,
            nearby: false,
            own: false,
            fix_at: None,
            battery: None,
        }
    }

    /// A frame of `view` at rest, without margins, the pins measured by
    /// the window's font and left for its canvas: what the window asks.
    fn rest(
        frames: &mut Frames,
        view: ExactView,
        pins: &[MapPin],
        selected: Option<[u8; 16]>,
        scale: f32,
    ) -> Shot {
        frames.frame(&Ask {
            view,
            margin: (0, 0),
            moving: false,
            style: Style::PC,
            scale,
            pins,
            selected,
            zooms: (0, 20),
            draw_pins: false,
            tracks: &[],
            radar: None,
            measure: Some(measure),
            now_ms: None,
            scaled_while_moving: false,
        })
    }

    /// The map view's frame over the fixture region at three zooms through
    /// the window's own pipeline: the region's credit when the view is
    /// inside it, the outline's when zoomed out past it, every pixel set,
    /// and the pins that land counted. The window draws exactly this:
    /// [`Frames::frame`] for the picture and the pins' places, `measure`
    /// for the labels.
    #[test]
    fn the_map_frame_over_the_fixture_at_three_zooms() {
        let mut frames = florence();
        let setup = setup(frames.painter());
        // Three inside the region's box (11.2505,43.7728 to 11.2700,43.7785,
        // the fixture's README): the Duomo, San Lorenzo's market, east of
        // Piazza d'Azeglio's corner; and Pisa, far to the west.
        let pins = [
            pin(1, "alice", 43.7731, 11.2560),
            pin(2, "bob", 43.7760, 11.2540),
            pin(3, "carol", 43.7770, 11.2650),
            pin(4, "dave", 43.7228, 10.4017),
        ];
        let size = (800u32, 600u32);
        let scale = 1.0;
        let (centre, fitted) = fit(&pins[..3], area(size), &setup);
        let floor = zoom_floor(area(size), &setup);
        let cases = [
            // (zoom, credit, pins in view)
            (fitted, "© OpenStreetMap contributors", 3),
            (11, "© OpenStreetMap contributors", 3),
            (8, "Natural Earth", 4),
        ];
        for (zoom, credit, landed) in cases {
            let view = ExactView::new(centre, u32::from(zoom) * MILLI, size.0, size.1);
            let view = hold(view, floor, setup.zooms.1);
            let picture = rest(&mut frames, view, &pins, Some([2; 16]), scale);
            assert_eq!(picture.view.size(), size);
            assert_eq!(picture.rgba.len(), (size.0 * size.1 * 4) as usize);
            assert_eq!(picture.attribution, credit, "zoom {zoom}");
            assert!(
                picture.rgba.chunks(4).all(|px| px[3] == 0xff),
                "zoom {zoom}: every pixel opaque"
            );
            let placed = frames.placed().to_vec();
            assert_eq!(placed.len(), landed, "zoom {zoom}: {placed:?}");
            let last = placed.last().expect("a pin");
            assert!(last.selected && last.peer == [2; 16], "selected on top");
            assert!(last.label.is_some(), "the selected keeps its label");
            assert!(placed.iter().any(|p| p.stale), "carol is stale");
            // A click on a marker finds its pin, or one drawn over it.
            for (i, p) in placed.iter().enumerate() {
                let found = hit(&placed, p.x, p.y, scale).expect("a pin under the marker");
                let at = placed.iter().position(|q| q.peer == found).expect("placed");
                assert!(at >= i, "zoom {zoom}: {:?} under {:?}", p.peer, found);
            }
            assert_eq!(hit(&placed, last.x, last.y, scale), Some(last.peer));
        }
        assert!(fitted > 11, "three pins in the old town fit deep: {fitted}");
    }

    /// The smallest shift by which every pixel column of `rgba`, `w`
    /// wide, repeats: the copies of the world side by side.
    fn period(rgba: &[u8], w: usize) -> Option<usize> {
        let h = rgba.len() / 4 / w;
        let px = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
        (1..w).find(|&p| (0..w - p).all(|x| (0..h).all(|y| px(x, y) == px(x + p, y))))
    }

    /// The window shows one world at most (1131): a window of 1280 x 800
    /// asked for z0 is held at z3, the world 2048 pixels wide, and no
    /// column of its picture repeats. The view as asked, the control,
    /// repeats every 256.
    ///
    /// The names are off: since 1683 the world outline brings its own
    /// names, and those are placed per copy of the world, so a copy with
    /// several cities in it does not repeat the one beside it. What must
    /// not repeat is the picture under them.
    #[test]
    fn the_window_never_shows_the_world_twice() {
        let mut frames = Frames::new(Maps::new(None));
        frames.set_names_off(true);
        let setup = setup(frames.painter());
        let size = (1280u32, 800u32);
        let centre = project(LatLon::from_degrees(30.0, 10.0));
        let asked = ExactView::new(centre, 0, size.0, size.1);
        let control = rest(&mut frames, asked, &[], None, 1.0);
        assert_eq!(period(&control.rgba, 1280), Some(256), "the control");
        let floor = zoom_floor(area(size), &setup);
        let view = hold(asked, floor, setup.zooms.1);
        assert_eq!(view.zoom_milli(), 3 * MILLI);
        let picture = rest(&mut frames, view, &[], None, 1.0);
        assert_eq!(period(&picture.rgba, 1280), None);
    }

    /// The same view on a screen of twice the density: twice the pixels,
    /// the pins at twice the coordinates, and the lines drawn wider rather
    /// than the picture scaled up.
    #[test]
    fn a_dense_screen_draws_the_same_view_at_its_own_pixels() {
        let mut frames = florence();
        let at = project(LatLon::from_degrees(43.7731, 11.2560));
        let one = ExactView::new(at, 14 * MILLI, 400, 300);
        let two = ExactView::new(at, 15 * MILLI, 800, 600);
        let pins = [pin(1, "alice", 43.7700, 11.2500)];
        rest(&mut frames, one, &pins, None, 1.0);
        let a = frames.placed().to_vec();
        let picture = rest(&mut frames, two, &pins, None, 2.0);
        let b = frames.placed().to_vec();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert!((b[0].x - 2.0 * a[0].x).abs() <= 2.0, "{a:?} {b:?}");
        assert_eq!(picture.rgba.len(), 800 * 600 * 4);
        assert_eq!(metrics(2.0).marker, 2 * metrics(1.0).marker);
    }

    /// The map's names set into the window's frame (1100, 1144): the
    /// Florence fixture at z16 with one pin, the parks and the streets the
    /// zoom shows, each name with its halo, none under the pin's name; the
    /// frame the same on a second one and pixel for pixel the golden. A
    /// frame drawn while the view zooms in to z16 from a rest at z15
    /// places its names as the frame at rest at z16 does (1225), the same
    /// names. The control: a zooming frame over a region with no labels,
    /// Pisa outside the fixture, sets none.
    #[test]
    fn the_map_names_are_set_into_the_picture() {
        let at = project(LatLon::from_degrees(43.7752, 11.2660));
        let view = ExactView::new(at, 16 * MILLI, 480, 320);
        let pins = [pin(1, "alice", 43.7752, 11.2640)];
        let moving = |view| Ask {
            view,
            margin: (0, 0),
            moving: true,
            style: Style::PC,
            scale: 1.0,
            pins: &pins,
            selected: None,
            zooms: (0, 20),
            draw_pins: false,
            tracks: &[],
            radar: None,
            measure: Some(measure),
            now_ms: None,
            scaled_while_moving: false,
        };
        let mut ground = florence();
        ground.set_names_off(true);
        let plain = rest(&mut ground, view, &pins, None, 1.0);
        assert!(ground.names().is_empty());
        let mut frames = florence();
        let named = rest(&mut frames, view, &pins, None, 1.0);
        let names = frames.names();
        assert_ne!(named.rgba, plain.rgba, "the names changed the picture");
        assert!(names.iter().any(|n| n == "Piazza D'Azeglio"), "{names:?}");
        assert!(
            names.iter().filter(|n| n.starts_with("Via ")).count() >= 2,
            "{names:?}"
        );
        let again = rest(&mut frames, view, &pins, None, 1.0);
        assert_eq!(again.rgba, named.rgba, "a second frame is the same");
        assert_eq!(frames.names(), names);
        let rgb: Vec<u8> = named
            .rgba
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        same_as("gui-names-florence-z16.png", &rgb, 480, 320);
        // Zooming in: at rest at z15, then a moving frame at z16.
        let mut zooming = florence();
        rest(
            &mut zooming,
            ExactView::new(at, 15 * MILLI, 480, 320),
            &pins,
            None,
            1.0,
        );
        let at_15 = zooming.names();
        assert_ne!(at_15, names, "z15 and z16 show other names");
        zooming.frame(&moving(view));
        assert_eq!(
            zooming.names(),
            names,
            "the zooming frame's are the rest frame's"
        );
        // The control: no labels where the view zooms, no names.
        let pisa = project(LatLon::from_degrees(43.7228, 10.4017));
        let mut empty = florence();
        empty.frame(&moving(ExactView::new(pisa, 16 * MILLI, 480, 320)));
        assert!(empty.names().is_empty(), "{:?}", empty.names());
    }

    /// Hold `rgb` to the PNG golden `name` under `loquax/tests/golden`,
    /// pixel for pixel; a picture that moved is left in the temp dir.
    pub(in crate::gui) fn same_as(name: &str, rgb: &[u8], w: u32, h: u32) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        let encode = || {
            let mut out = Vec::new();
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            let mut writer = e.write_header().expect("header");
            writer.write_image_data(rgb).expect("data");
            writer.finish().expect("finish");
            out
        };
        let keep = || {
            let dir = std::env::temp_dir().join("loquax-golden");
            std::fs::create_dir_all(&dir).expect("temp dir");
            let kept = dir.join(name);
            std::fs::write(&kept, encode()).expect("write");
            kept
        };
        let Ok(bytes) = std::fs::read(&path) else {
            panic!("no golden {name}; this run's is {}", keep().display());
        };
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .expect("png");
        let mut want = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut want).expect("frame");
        want.truncate(info.buffer_size());
        if (info.width, info.height) != (w, h) || want != rgb {
            let differ = want
                .chunks(3)
                .zip(rgb.chunks(3))
                .filter(|(a, b)| a != b)
                .count();
            panic!(
                "{name} moved: {differ} pixels differ; this run's is {}",
                keep().display()
            );
        }
    }

    /// The label width comes from the font: a longer name is wider.
    #[test]
    fn a_label_is_measured_by_the_windows_font() {
        let short = measure("bob", LABEL);
        let long = measure("bob · 5m and a long name", LABEL);
        assert!(long > short, "{short} {long}");
    }

    /// The window draws the buildings from z16 (1192): over the Florence
    /// cut with its building sections, its frames at rest at z16 and z17,
    /// light and dark, in the window's finish ([`Style::SCREEN`], the one
    /// the phone asks for too) are their goldens pixel for pixel; the
    /// names it places are the ones and in the places a frame without
    /// buildings places, and a tenth of the frame is footprint.
    #[test]
    fn the_window_draws_the_buildings_and_they_move_no_name() {
        use leviculum_mapfill::frame::rgb888;
        use leviculum_mapfill::style::{Class, MapTheme};
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/buildings");
        let at = project(LatLon::from_degrees(43.7752, 11.2660));
        for zoom in [16u32, 17] {
            for (theme, t) in [(MapTheme::Light, "light"), (MapTheme::Dark, "dark")] {
                let shot = |style: Style| {
                    let mut frames = Frames::new(Maps::new(Some(dir.clone())));
                    let shot = frames.frame(&Ask {
                        view: ExactView::new(at, zoom * MILLI, 480, 320),
                        margin: (0, 0),
                        moving: false,
                        style: style.in_theme(theme),
                        scale: 1.0,
                        pins: &[],
                        selected: None,
                        zooms: (0, 20),
                        draw_pins: false,
                        tracks: &[],
                        radar: None,
                        measure: Some(measure),
                        now_ms: None,
                        scaled_while_moving: false,
                    });
                    (shot, frames.drawn_names())
                };
                let (with, names) = shot(Style::SCREEN);
                let (without, names_without) = shot(Style::PC);
                assert!(!names.is_empty(), "z{zoom} {t}");
                assert_eq!(names, names_without, "z{zoom} {t}: a building moved a name");
                let fill = rgb888(theme.palette().area(Class::Building));
                let footprints = with
                    .rgba
                    .chunks(4)
                    .zip(without.rgba.chunks(4))
                    .filter(|(a, b)| a != b && a[..3] == fill)
                    .count();
                assert!(footprints * 10 > 480 * 320, "z{zoom} {t}: {footprints}");
                let rgb: Vec<u8> = with
                    .rgba
                    .chunks(4)
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                same_as(&format!("gui-buildings-{t}-z{zoom}.png"), &rgb, 480, 320);
            }
        }
    }

    /// The world view of 1683, where no region covers the view: central
    /// Europe at z2 and z6, the outline's lakes, rivers, ice, borders and
    /// dashed disputed lines with the world's own names on them — a
    /// country's, a sea's, and at z6 the names the places carry — set by
    /// the map's one placement rule. The window's picture and the
    /// phone's (2.75 pixels a logical one, its names at that size), each
    /// as its golden; and the controls that the names are the world's
    /// and that a name no table carries never appears.
    #[test]
    fn the_world_view_names_its_countries_and_seas_at_z2_and_z6() {
        use leviculum_map::labels::LabelKind;
        let dir = tempfile::tempdir().expect("tmp");
        let at = project(LatLon::from_degrees(50.0, 10.0));
        for zoom in [2u32, 6] {
            for (scale, who) in [(1.0f32, "window"), (2.75, "phone")] {
                let mut frames = Frames::new(Maps::new(Some(dir.path().to_path_buf())));
                let shot = frames.frame(&Ask {
                    view: ExactView::new(at, zoom * MILLI, 480, 320),
                    margin: (0, 0),
                    moving: false,
                    style: Style::SCREEN,
                    scale,
                    pins: &[],
                    selected: None,
                    zooms: (0, 20),
                    draw_pins: false,
                    tracks: &[],
                    radar: None,
                    measure: Some(measure),
                    now_ms: None,
                    scaled_while_moving: false,
                });
                let names = frames.drawn_names();
                println!("z{zoom} {who}: {names:?}");
                // A country's name is the table's first, so it is placed
                // wherever there is room; at z6 on the phone's letters a
                // place name has grown larger than the country's box.
                if !(zoom == 6 && scale > 1.0) {
                    assert!(
                        names.iter().any(|n| n.kind == LabelKind::Country),
                        "z{zoom} {who}: no country's name in {names:?}"
                    );
                }
                // Beyond the countries the table brings the waters and
                // the ranges, but those are placed after the countries and
                // the places, and at z6 over central Europe are crowded
                // out (as they are on the phone's larger letters at z2):
                // the control for them is the window at z2.
                if zoom == 2 && scale == 1.0 {
                    assert!(
                        names
                            .iter()
                            .any(|n| matches!(n.kind, LabelKind::Sea | LabelKind::Range)),
                        "z{zoom} {who}: no sea or range in {names:?}"
                    );
                }
                if zoom == 6 {
                    assert!(
                        names.iter().any(|n| n.kind.is_place()),
                        "z{zoom} {who}: no place's name in {names:?}"
                    );
                }
                assert!(
                    names.iter().all(|n| !n.text.contains('\u{fffd}')),
                    "z{zoom} {who}: a name that did not read: {names:?}"
                );
                let rgb: Vec<u8> = shot
                    .rgba
                    .chunks(4)
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                same_as(&format!("gui-world-{who}-z{zoom}.png"), &rgb, 480, 320);
            }
        }
        // The control: a view past the outline's deepest zoom has no world
        // names, and neither has one a region fills.
        let mut frames = Frames::new(Maps::new(Some(dir.path().to_path_buf())));
        frames.frame(&Ask {
            view: ExactView::new(at, 12 * MILLI, 480, 320),
            margin: (0, 0),
            moving: false,
            style: Style::SCREEN,
            scale: 1.0,
            pins: &[],
            selected: None,
            zooms: (0, 20),
            draw_pins: false,
            tracks: &[],
            radar: None,
            measure: Some(measure),
            now_ms: None,
            scaled_while_moving: false,
        });
        assert!(
            frames.drawn_names().is_empty(),
            "z12 has the world's names: {:?}",
            frames.drawn_names()
        );
    }

    /// The six track scenes (1385) through the window's own frames over
    /// the Florence fixture, 480 x 320, the pins drawn in: each the
    /// golden `gui-tracks-<name>.png`, the park also in the dark table.
    /// With `LOQUAX_TRACK_PNG_DIR` set, every picture is written there
    /// as well, for a look.
    #[test]
    fn the_track_scenes_are_the_goldens() {
        use leviculum_mapfill::style::MapTheme;
        use loquax::ui::tracks::{scenes, tracks, TrackWindow};
        let out = std::env::var_os("LOQUAX_TRACK_PNG_DIR").map(PathBuf::from);
        let mut frames = florence();
        for scene in scenes::all() {
            let themes: &[(MapTheme, &str)] = match scene.name {
                "park" => &[(MapTheme::Light, ""), (MapTheme::Dark, "-dark")],
                _ => &[(MapTheme::Light, "")],
            };
            let fitted = scenes::view_of(&scene, 480, 320, 28);
            let view = ExactView::new(fitted.centre, u32::from(fitted.zoom()) * MILLI, 480, 320);
            let unit = libm::exp2(24.0 - f64::from(fitted.zoom()));
            let drawn = tracks(
                &scenes::input(&scene),
                TrackWindow::Episode,
                &scene.now,
                unit,
                1.0,
                loquax::ui::strings::Lang::En,
            );
            let pins = scenes::pins(&scene);
            for (theme, suffix) in themes {
                let shot = frames.frame(&Ask {
                    view,
                    margin: (0, 0),
                    moving: false,
                    style: Style::SCREEN.in_theme(*theme),
                    scale: 1.0,
                    pins: &pins,
                    tracks: &drawn,
                    radar: None,
                    selected: None,
                    zooms: (0, 20),
                    draw_pins: true,
                    measure: None,
                    now_ms: None,
                    scaled_while_moving: false,
                });
                let rgb: Vec<u8> = shot
                    .rgba
                    .chunks(4)
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                let name = format!("gui-tracks-{}{suffix}.png", scene.name);
                if let Some(dir) = &out {
                    std::fs::create_dir_all(dir).expect("png dir");
                    let mut bytes = Vec::new();
                    let mut e = png::Encoder::new(&mut bytes, 480, 320);
                    e.set_color(png::ColorType::Rgb);
                    e.set_depth(png::BitDepth::Eight);
                    let mut writer = e.write_header().expect("header");
                    writer.write_image_data(&rgb).expect("data");
                    writer.finish().expect("finish");
                    std::fs::write(dir.join(&name), bytes).expect("write png");
                }
                same_as(&name, &rgb, 480, 320);
            }
        }
    }

    /// 1386's five scenes through the window's own frames over the
    /// Florence fixture, 480 x 320, the pins drawn in, each the golden
    /// `gui-tracks-<name>.png`: the radar where no region touches the view
    /// (decided by [`loquax::ui::radar::shows`] over the fixture's
    /// regions, as the window decides), the way back in grey, the late
    /// stretch dashed, the low battery beside Kai's marker, and "fit all"
    /// at the view its fit gives. `LOQUAX_TRACK_PNG_DIR` writes them there
    /// too.
    #[test]
    fn the_marks_scenes_are_the_goldens() {
        use loquax::ui::radar;
        use loquax::ui::tracks::{scenes, tracks, TrackWindow};
        let out = std::env::var_os("LOQUAX_TRACK_PNG_DIR").map(PathBuf::from);
        let mut frames = florence();
        let setup = setup(frames.painter());
        let (w, h) = (480u32, 320u32);
        for scene in scenes::marks() {
            let fitted = match scene.name {
                "fit-all" => {
                    let (centre, zoom) = fit(&scenes::fitted_pins(&scene), area((w, h)), &setup);
                    leviculum_map::viewport::Viewport::new(centre, zoom, w, h)
                }
                _ => scenes::view_of(&scene, w, h, 28),
            };
            let view = ExactView::new(fitted.centre, u32::from(fitted.zoom()) * MILLI, w, h);
            let unit = libm::exp2(24.0 - f64::from(fitted.zoom()));
            let drawn = tracks(
                &scenes::input(&scene),
                TrackWindow::Episode,
                &scene.now,
                unit,
                1.0,
                loquax::ui::strings::Lang::En,
            );
            let mut pins = scenes::pins(&scene);
            let shown = match radar::shows(&fitted, &setup.regions) {
                true => radar::radar(
                    &pins,
                    view.unit(),
                    view.size(),
                    loquax::ui::strings::Lang::En,
                ),
                false => None,
            };
            assert_eq!(shown.is_some(), scene.name == "radar", "{}", scene.name);
            if let Some(r) = &shown {
                radar::label(&mut pins, r);
            }
            let shot = frames.frame(&Ask {
                view,
                margin: (0, 0),
                moving: false,
                style: Style::SCREEN,
                scale: 1.0,
                pins: &pins,
                tracks: &drawn,
                radar: shown.as_ref(),
                selected: None,
                zooms: (0, 20),
                draw_pins: true,
                measure: None,
                now_ms: None,
                scaled_while_moving: false,
            });
            let rgb: Vec<u8> = shot
                .rgba
                .chunks(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let name = format!("gui-tracks-{}.png", scene.name);
            if let Some(dir) = &out {
                std::fs::create_dir_all(dir).expect("png dir");
                let mut bytes = Vec::new();
                let mut e = png::Encoder::new(&mut bytes, w, h);
                e.set_color(png::ColorType::Rgb);
                e.set_depth(png::BitDepth::Eight);
                let mut writer = e.write_header().expect("header");
                writer.write_image_data(&rgb).expect("data");
                writer.finish().expect("finish");
                std::fs::write(dir.join(&name), bytes).expect("write png");
            }
            same_as(&name, &rgb, w, h);
        }
    }

    /// With `LOQUAX_BUILDINGS_REGION` set to a region file with building
    /// sections: the phone's frame (1080 x 2400 at a density of 2.75) over
    /// Frankfurt at z16 and z17, light and dark, places the same names in
    /// the same places with buildings as without. A check for the reviewer
    /// over a region too large to commit: `LOQUAX_BUILDINGS_REGION=
    /// frankfurt.lmr cargo test -p loquax --features gui --bins
    /// buildings_move_no_frankfurt_name -- --nocapture`. Without it, the
    /// test returns at once.
    #[test]
    fn buildings_move_no_frankfurt_name() {
        use leviculum_mapfill::style::MapTheme;
        let Some(path) = std::env::var_os("LOQUAX_BUILDINGS_REGION") else {
            return;
        };
        let dir = tempfile::tempdir().expect("maps dir");
        let linked = std::fs::canonicalize(PathBuf::from(path)).expect("region");
        std::os::unix::fs::symlink(&linked, dir.path().join("region.lmr")).expect("link");
        let at = project(LatLon::from_degrees(50.1109, 8.6821));
        for zoom in [16u32, 17] {
            for theme in [MapTheme::Light, MapTheme::Dark] {
                let names = |style: Style| {
                    let mut frames = Frames::new(Maps::new(Some(dir.path().to_path_buf())));
                    let shot = frames.frame(&Ask {
                        view: ExactView::new(at, zoom * MILLI, 1080, 2400),
                        margin: (0, 0),
                        moving: false,
                        style: style.in_theme(theme),
                        scale: 2.75,
                        pins: &[],
                        selected: None,
                        zooms: (0, 20),
                        draw_pins: false,
                        tracks: &[],
                        radar: None,
                        measure: Some(measure),
                        now_ms: None,
                        scaled_while_moving: false,
                    });
                    (frames.drawn_names(), shot.rgba)
                };
                let (with, drawn) = names(Style::SCREEN);
                let (without, bare) = names(Style::PC);
                // The control: the buildings were drawn.
                assert!(drawn != bare, "z{zoom} {theme:?}: no building drawn");
                println!("BUILDINGS names z{zoom} {theme:?}: {} placed", with.len());
                assert!(!with.is_empty(), "z{zoom} {theme:?}");
                assert_eq!(with, without, "z{zoom} {theme:?}: a building moved a name");
            }
        }
    }
}
