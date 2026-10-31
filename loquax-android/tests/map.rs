//! The map screen's call through the facade (1128): the frame Java gets
//! is loquax's own map on a pixel screen, the same pixels the window's
//! renderer draws for the view, the pins drawn in, the dark table when the
//! phone is dark or the map switch says so (1133), and a position in the store is a pin. A test binary of
//! its own: the map takes a running session's pins over the store's, and
//! the facade tests run sessions in their process.

use std::path::{Path, PathBuf};

use leviculum_core::Identity;
use leviculum_lxmf::telemetry::Location;
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::viewport::{ExactView, Viewport};
use leviculum_mapfill::frame::rgb888;
use leviculum_mapfill::style::{MapTheme, Style};
use leviculum_ui::theme::ThemeFlag;
use loquax::mapnames::Typesetter;
use loquax::picture::{Maps, Painter};
use loquax::store::Store;
use loquax::telemetry::Fix;
use loquax::tiles;
use loquax::ui::history::StoreWrite;
use loquax::ui::map::MapPin;
use loquax::when::WallTime;
use loquax_android::schema::{self, MapRequest, MapShown};

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

/// The Florence view of the window's golden of the names: z16, 480 x 320.
const LAT: f64 = 43.7752;
const LON: f64 = 11.2660;
const W: u32 = 480;
const H: u32 = 320;
const NOW: i64 = 1_790_000_000;

/// A home with the committed Florence region in its maps, the autoload
/// off: no test reaches for the internet.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("home");
    let maps = home.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
    std::fs::copy(fixture, maps.join("firenze.lmr")).expect("the region");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    home
}

/// The request for the Florence view.
fn request(home: &Path, dark: bool) -> MapRequest {
    let at = project(LatLon::from_degrees(LAT, LON));
    MapRequest {
        dir: home.display().to_string(),
        w: W,
        h: H,
        density_milli: 1000,
        dark,
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(16),
        ..MapRequest::default()
    }
}

fn frame(bytes: &[u8]) -> (MapShown, Vec<u8>) {
    let (shown, rgba) = schema::unframe(bytes).expect("a frame");
    assert!(shown.ok, "{:?}", shown.error);
    assert_eq!(rgba.len(), (shown.w * shown.h * 4) as usize);
    (shown, rgba.to_vec())
}

fn pin(peer: u8, label: &str, lat: f64, lon: f64) -> MapPin {
    MapPin {
        peer: [peer; 16],
        at: project(LatLon::from_degrees(lat, lon)),
        label: label.to_string(),
        age: Some("5m".to_string()),
        stale: false,
    }
}

fn now() -> WallTime {
    WallTime {
        unix_secs: NOW,
        offset_secs: 0,
    }
}

/// The view's picture as loquax's painter draws it, no names, no pins,
/// as RGBA.
fn picture(home: &Path, theme: MapTheme) -> Vec<u8> {
    let mut painter = Painter::new(Maps::new(Some(home.join("maps"))));
    let view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let drawn = painter.draw(&view, Ink::Colour, Style::PC.in_theme(theme), 100);
    drawn
        .fb
        .iter()
        .flat_map(|&px| {
            use embedded_graphics::pixelcolor::raw::RawU16;
            use embedded_graphics::pixelcolor::Rgb565;
            let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
            [r, g, b, 0xff]
        })
        .collect()
}

/// How many of the pixels of `a` and `b` are the same, per cent.
fn same_share(a: &[u8], b: &[u8]) -> usize {
    let same = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x == y).count();
    same * 100 / (a.len() / 4)
}

/// The frame through the facade with one pin: the window's renderer's
/// pixels for the view, its names and its pin, but for a tenth of a per
/// cent at the tile seams
/// (`loquax::screenmap::render` called here as the window's code calls
/// it); the map under them `loquax::picture`'s; the pin's marker at its
/// place; pixel for pixel the golden.
#[test]
fn the_frame_is_the_shared_renderers_with_names_and_a_pin() {
    let home = home();
    let pins = vec![pin(1, "alice", 43.7752, 11.2640)];
    let bytes =
        loquax_android::map::render(&request(home.path(), false), Some(pins.clone()), now());
    let (shown, rgba) = frame(&bytes);
    assert_eq!((shown.w, shown.h, shown.zoom), (W, H, 16));
    assert_eq!(shown.pins, 1);
    assert_eq!(shown.attribution, "© OpenStreetMap contributors");

    let mut painter = Painter::new(Maps::new(Some(home.path().join("maps"))));
    let mut type_ = Typesetter::new();
    // The frame sits on a whole pixel of its step (1134): drawn whole
    // there, the view is the frame's.
    let at = ExactView::new(
        World {
            x: shown.x,
            y: shown.y,
        },
        16_000,
        W,
        H,
    )
    .on_whole_pixel();
    let view = Viewport::new(at.centre, 16, W, H);
    let direct = loquax::screenmap::render(
        &mut painter,
        &mut type_,
        &view,
        Style::PC,
        1.0,
        &pins,
        None,
        true,
    );
    // Composed from tiles, the frame is the view drawn whole but where a
    // dashed path crosses a seam: its dashes start again at each tile's
    // edge, as they do at a view's. A turned name's sprite rounds its
    // coverage to a level (1156), so its pixels may be a step or two of
    // a channel off the whole view's float paint: not counted.
    let differ = rgba
        .chunks(4)
        .zip(direct.picture.rgba.chunks(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
        .count();
    assert!(
        differ * 1000 < (W * H) as usize,
        "the shared renderer's pixels: {differ} differ"
    );
    assert!(
        direct.picture.names.iter().any(|n| n.starts_with("Via ")),
        "{:?}",
        direct.picture.names
    );

    let plain = picture(home.path(), MapTheme::Light);
    let share = same_share(&rgba, &plain);
    assert!(
        (90..100).contains(&share),
        "the map under the names and the pin is the picture's: {share} %"
    );

    let placed = &direct.placed[0];
    let (x, y) = (placed.x as usize, placed.y as usize);
    let at = (y * W as usize + x) * 4;
    assert_eq!(&rgba[at..at + 3], &[0x20, 0x50, 0xc0], "the marker's blue");

    same_as("map-florence-z16.png", &rgba, W, H);
}

/// The phone in its dark setting draws the map's dark table: the frame
/// is the dark picture under its names, and not the light one.
#[test]
fn a_dark_phone_draws_the_dark_table() {
    let home = home();
    let (_, light) = frame(&loquax_android::map::render(
        &request(home.path(), false),
        Some(Vec::new()),
        now(),
    ));
    let (_, dark) = frame(&loquax_android::map::render(
        &request(home.path(), true),
        Some(Vec::new()),
        now(),
    ));
    let dark_picture = picture(home.path(), MapTheme::Dark);
    let share = same_share(&dark, &dark_picture);
    assert!(share >= 90, "dark under its names: {share} %");
    let share = same_share(&dark, &light);
    assert!(share < 50, "not the light table: {share} %");
}

/// The map's own switch (1133): with the phone dark, a toggle draws the
/// light table and stores it as the config's `map_theme`, the other keys
/// kept; the choice wins over the phone from then on, for a renderer that
/// reads the config afresh too; "follow the phone" stores `auto` and the
/// phone decides again.
#[test]
fn the_map_switch_wins_over_the_phone_until_it_follows_again() {
    let home = home();
    let draw = |request: &MapRequest| {
        frame(&loquax_android::map::render(
            request,
            Some(Vec::new()),
            now(),
        ))
    };
    let dark_picture = picture(home.path(), MapTheme::Dark);
    let light_picture = picture(home.path(), MapTheme::Light);

    let (shown, rgba) = draw(&request(home.path(), true));
    assert!(shown.dark && !shown.theme_chosen, "{shown:?}");
    assert!(same_share(&rgba, &dark_picture) >= 90);

    let toggle = MapRequest {
        toggle_theme: true,
        ..request(home.path(), true)
    };
    let (shown, rgba) = draw(&toggle);
    assert!(!shown.dark && shown.theme_chosen, "{shown:?}");
    assert_eq!(shown.notice, None);
    assert!(same_share(&rgba, &light_picture) >= 90, "light at once");
    let config = loquax::config::load(home.path()).expect("the config");
    assert_eq!(config.map_theme, Some(ThemeFlag::Light));
    assert_eq!(config.map_autoload, Some(false), "the other keys kept");

    for phone_dark in [true, false] {
        let (shown, rgba) = draw(&request(home.path(), phone_dark));
        assert!(!shown.dark && shown.theme_chosen, "phone dark {phone_dark}");
        assert!(same_share(&rgba, &light_picture) >= 90);
    }

    let (shown, _) = draw(&MapRequest {
        toggle_theme: true,
        ..request(home.path(), false)
    });
    assert!(shown.dark && shown.theme_chosen, "toggled back to dark");
    assert_eq!(
        loquax::config::load(home.path())
            .expect("the config")
            .map_theme,
        Some(ThemeFlag::Dark)
    );

    let (shown, rgba) = draw(&MapRequest {
        follow_phone: true,
        ..request(home.path(), false)
    });
    assert!(
        !shown.dark && !shown.theme_chosen,
        "follows the light phone"
    );
    assert!(same_share(&rgba, &light_picture) >= 90);
    assert_eq!(
        loquax::config::load(home.path())
            .expect("the config")
            .map_theme,
        Some(ThemeFlag::Auto)
    );
    let (shown, _) = draw(&request(home.path(), true));
    assert!(shown.dark && !shown.theme_chosen, "and the dark one");
}

/// A choice stored before the app draws its first map wins over the
/// phone: phone dark, `map_theme = light`, the light table.
#[test]
fn a_stored_map_theme_wins_over_the_phone() {
    let home = home();
    loquax::config::save(home.path(), "map_theme", "light").expect("stored");
    let (shown, rgba) = frame(&loquax_android::map::render(
        &request(home.path(), true),
        Some(Vec::new()),
        now(),
    ));
    assert!(!shown.dark && shown.theme_chosen, "{shown:?}");
    assert!(same_share(&rgba, &picture(home.path(), MapTheme::Light)) >= 90);
}

/// A tap on a pin's marker selects it and names it; the selected pin is
/// drawn red. A pan moves the centre by the pixels at the zoom.
#[test]
fn a_tap_names_the_pin_and_a_pan_moves_the_view() {
    let home = home();
    let pins = vec![pin(7, "bob", 43.7752, 11.2640)];
    let first = request(home.path(), false);
    let (shown, _) = frame(&loquax_android::map::render(
        &first,
        Some(pins.clone()),
        now(),
    ));
    assert_eq!(shown.tapped, None);
    let mut view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let marker = leviculum_map::pins::place(
        &view,
        &[leviculum_map::pins::Pin {
            at: pins[0].at,
            label: "bob",
            stale: false,
        }],
    )[0];
    let tap = MapRequest {
        tap_x: Some(marker.x as i32),
        tap_y: Some(marker.y as i32),
        ..first.clone()
    };
    let (shown, rgba) = frame(&loquax_android::map::render(
        &tap,
        Some(pins.clone()),
        now(),
    ));
    assert_eq!(shown.tapped.as_deref(), Some("07".repeat(16).as_str()));
    assert_eq!(shown.tapped_name.as_deref(), Some("bob · 5m"));
    assert_eq!(shown.selected, shown.tapped);
    let at = (marker.y as usize * W as usize + marker.x as usize) * 4;
    assert_eq!(&rgba[at..at + 3], &[0xd0, 0x30, 0x30], "the selected red");

    let pan = MapRequest {
        pan_x: 40,
        pan_y: -20,
        ..first
    };
    let (shown, _) = frame(&loquax_android::map::render(&pan, Some(pins), now()));
    view.pan(40, -20);
    assert_eq!((shown.x, shown.y), (view.centre.x, view.centre.y));
}

/// A position in the store is a pin on the map, with no session running:
/// the first frame fits it and selects it, named by the display rule.
#[test]
fn a_position_in_the_store_makes_a_pin() {
    let home = home();
    let identity = Identity::generate(&mut rand_core::OsRng);
    let key: [u8; 64] = identity.public_key_bytes();
    loquax::home::client::save_client(home.path(), &key).expect("client");
    let address = loquax::home::lxmf_address(&key).expect("address");
    let peer = [0x42; 16];
    {
        let mut store = Store::open_in(home.path(), address).expect("store");
        let fix = Fix {
            peer,
            via: peer,
            message: None,
            received_at: Some(NOW - 60),
            location: Location {
                latitude_e6: 43_775_200,
                longitude_e6: 11_264_000,
                altitude_e2: 0,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 500,
                last_update: NOW - 90,
            },
            telemetry: Vec::new(),
        };
        store
            .write(&[StoreWrite::Position(fix)])
            .expect("the position");
    }
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..request(home.path(), false)
    };
    let bytes = loquax_android::render_map(&schema::encode(&first));
    let (shown, _) = frame(&bytes);
    assert_eq!(shown.pins, 1, "{shown:?}");
    assert_eq!(shown.selected.as_deref(), Some("42".repeat(16).as_str()));
    assert!(shown.zoom > 11, "one pin fits deep: {}", shown.zoom);
}

/// The ugly store (1173) under the phone's client address, with no
/// session running: the frame comes, and every peer whose received
/// position the store keeps is a pin but the blocked one, whose pin the
/// model's pins leave out. The imports keep their positions in their
/// rows' fields only, so their peers have no pin.
#[test]
fn the_ugly_store_makes_a_pin_of_each_position() {
    let home = home();
    let identity = Identity::generate(&mut rand_core::OsRng);
    loquax::home::client::save_client(home.path(), &identity.public_key_bytes()).expect("client");
    let filled = ugly_store::fill_store_of(home.path(), &identity);
    assert_eq!(
        filled.messages,
        ugly_store::CONVERSATIONS * ugly_store::PER_CONVERSATION
    );
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::render_map(&schema::encode(&first)));
    let live = ugly_store::CONVERSATIONS - 2;
    assert_eq!(shown.pins as usize, live - 1, "{shown:?}");
}

/// How long a phone's whole screen takes on this host, for the report:
/// 1080 x 2400 at 2.75, the Florence view, one pin.
#[test]
fn a_phone_screen_frame_takes() {
    let home = home();
    let at = project(LatLon::from_degrees(LAT, LON));
    let phone = MapRequest {
        w: 1080,
        h: 2400,
        density_milli: 2750,
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(17),
        ..request(home.path(), false)
    };
    let pins = vec![pin(1, "alice", 43.7752, 11.2640)];
    for round in 0..3 {
        let started = std::time::Instant::now();
        let (shown, _) = frame(&loquax_android::map::render(
            &phone,
            Some(pins.clone()),
            now(),
        ));
        eprintln!(
            "MAPFRAME 1080x2400 round={round} ms={} total_ms={}",
            shown.ms,
            started.elapsed().as_millis()
        );
    }
}

/// Hold `rgba` to the PNG golden `name` under `tests/golden`, pixel for
/// pixel; a picture that moved is left in the temp dir.
fn same_as(name: &str, rgba: &[u8], w: u32, h: u32) {
    let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let encode = || {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header().expect("header");
        writer.write_image_data(&rgb).expect("data");
        writer.finish().expect("finish");
        out
    };
    let keep = || {
        let dir = std::env::temp_dir().join("loquax-android-golden");
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

/// The smallest shift by which every pixel column of `rgba`, `w` wide,
/// repeats: the copies of the world side by side.
fn period(rgba: &[u8], w: usize) -> Option<usize> {
    let h = rgba.len() / 4 / w;
    let px = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
    (1..w).find(|&p| (0..w - p).all(|x| (0..h).all(|y| px(x, y) == px(x + p, y))))
}

/// The phone shows one world at most (Lew 2026-10-29, 1131): a portrait
/// screen asked for z0, where the world was drawn five times across a
/// thin band, gets z3, the world 2048 pixels wide over its 1080, no
/// column repeated; zooming out stops there; the first view with no pin
/// and no region is that world, centred.
#[test]
fn the_phone_never_shows_the_world_twice() {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    let ask = MapRequest {
        dir: home.path().display().to_string(),
        w: 1080,
        h: 2400,
        density_milli: 1000,
        x: Some(1 << 31),
        y: Some(1 << 31),
        zoom: Some(0),
        moving: true,
        ..MapRequest::default()
    };
    let (shown, rgba) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert_eq!((shown.zoom, shown.min_zoom), (3, 3));
    assert_eq!(period(&rgba, 1080), None);
    let out = MapRequest {
        zoom_by: -2,
        about_x: 540,
        about_y: 1200,
        ..ask.clone()
    };
    let (shown, _) = frame(&loquax_android::map::render(&out, Some(Vec::new()), now()));
    assert_eq!(shown.zoom, 3, "zooming out stops at one world");
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..ask
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &first,
        Some(Vec::new()),
        now(),
    ));
    assert_eq!((shown.zoom, shown.y), (3, 1 << 31));
}

/// The first view with no pin is the newest region, the Florence fixture
/// here, fitted whole, and not the world.
#[test]
fn the_first_view_without_pins_is_the_installed_region() {
    let home = home();
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &first,
        Some(Vec::new()),
        now(),
    ));
    assert!(shown.zoom >= 14, "z{}", shown.zoom);
    let at = leviculum_map::mercator::unproject(leviculum_map::World {
        x: shown.x,
        y: shown.y,
    });
    assert!((at.lat_e7 - 437_756_000).abs() < 100_000, "{at:?}");
}

/// A frame with margins covers them (1131): the Florence view drawn with
/// half a screen past each edge is twice the screen each way, and each
/// margin holds exactly the pixels a frame panned half a screen that way
/// shows, so a finger that pans reveals drawn map and no dark strip. The
/// margins are echoed for Java to place the frame by, and a tap is still
/// in the screen's pixels.
#[test]
fn a_frame_with_margins_covers_them() {
    let home = home();
    let (mx, my) = (W / 2, H / 2);
    let wide = MapRequest {
        margin_x: mx,
        margin_y: my,
        moving: true,
        ..request(home.path(), false)
    };
    // A frame while the finger moves draws a few missing tiles a worker
    // and owes the rest (`MOVING_TILES_PER_WORKER`), so with few cores the
    // first one shows stand-ins: ask again while it owes any, as Java does
    // (1167). Each answer draws more, so this ends.
    let (mut shown, mut big) = frame(&loquax_android::map::render(&wide, Some(Vec::new()), now()));
    for _ in 0..64 {
        if !shown.pending {
            break;
        }
        (shown, big) = frame(&loquax_android::map::render(&wide, Some(Vec::new()), now()));
    }
    assert!(!shown.pending, "the frame still owes tiles after 64 asks");
    assert_eq!((shown.w, shown.h), (2 * W, 2 * H));
    assert_eq!((shown.margin_x, shown.margin_y), (mx, my));
    assert!(big.chunks(4).all(|px| px[3] == 0xff), "every pixel set");
    let bw = (2 * W) as usize;
    for (dx, dy) in [
        (-(mx as i32), 0),
        (mx as i32, 0),
        (0, -(my as i32)),
        (0, my as i32),
        (mx as i32, my as i32),
    ] {
        let panned = MapRequest {
            pan_x: dx,
            pan_y: dy,
            moving: true,
            ..request(home.path(), false)
        };
        let (panned_shown, small) = frame(&loquax_android::map::render(
            &panned,
            Some(Vec::new()),
            now(),
        ));
        assert!(!panned_shown.pending, "the margins held its tiles");
        // The panned screen is the big frame's window at the margin plus
        // the pan.
        let (x0, y0) = ((mx as i32 + dx) as usize, (my as i32 + dy) as usize);
        let window: Vec<u8> = (0..H as usize)
            .flat_map(|y| {
                let at = ((y0 + y) * bw + x0) * 4;
                big[at..at + W as usize * 4].to_vec()
            })
            .collect();
        assert_eq!(same_share(&window, &small), 100, "pan {dx},{dy}");
    }
    // A tap on a pin's marker, in the screen's pixels.
    let pins = vec![pin(7, "bob", 43.7752, 11.2640)];
    let view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let marker = leviculum_map::pins::place(
        &view,
        &[leviculum_map::pins::Pin {
            at: pins[0].at,
            label: "bob",
            stale: false,
        }],
    )[0];
    let _ = loquax_android::map::render(&wide, Some(pins.clone()), now());
    let tap = MapRequest {
        tap_x: Some(marker.x as i32),
        tap_y: Some(marker.y as i32),
        ..wide
    };
    let (shown, _) = frame(&loquax_android::map::render(&tap, Some(pins), now()));
    assert_eq!(shown.tapped.as_deref(), Some("07".repeat(16).as_str()));
}

/// A text that came with a position (Sideband's telemetry beside a body,
/// Lew's device test of 1132): its bubble carries the text and the
/// position line's words, and the map request a tap on it makes, the fix
/// and the sender's pin, opens the map centred there at the region's
/// street zoom with that pin selected. The same text without the field is
/// a plain bubble with nothing to open.
#[test]
fn a_tap_on_a_text_with_a_position_opens_the_map_at_it() {
    use leviculum_lxmf::telemetry::Telemetry;
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent, UiModel};

    let home = home();
    let peer = [0x42; 16];
    let fix = Location {
        latitude_e6: 43_775_200,
        longitude_e6: 11_264_000,
        altitude_e2: 0,
        speed_e2: 0,
        bearing_e2: 0,
        accuracy_e2: 500,
        last_update: NOW - 90,
    };
    let telemetry = Telemetry {
        time: Some(NOW - 90),
        location: Some(fix),
        ..Telemetry::default()
    };
    let mut model = UiModel::<loquax_android::session::Line>::new(&[]);
    update(&mut model, UiEvent::Clock(now()));
    for (id, fields) in [
        (
            1u8,
            vec![(
                leviculum_lxmf::constants::FIELD_TELEMETRY,
                telemetry.encode_field_value(),
            )],
        ),
        (2, Vec::new()),
    ] {
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Received {
                message_id: [id; 32],
                source: peer,
                title: Vec::new(),
                body: b"Brandenburger Tor, Testposition".to_vec(),
                fields,
                verified: true,
            }),
        );
    }
    let rows = loquax_android::session::rows(&model, 0);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let with = &rows[0];
    assert_eq!(with.body, "Brandenburger Tor, Testposition");
    assert_eq!(
        (with.lat_e6, with.lon_e6),
        (Some(43_775_200), Some(11_264_000))
    );
    assert_eq!(with.place.as_deref(), Some("43.77520 N 11.26400 E"));
    assert_eq!(with.accuracy.as_deref(), Some("±5 m"));
    assert_eq!(with.fix_at, Some(NOW - 90));
    assert_eq!(
        with.stale_at,
        Some(NOW - 90 + loquax::telemetry::STALE_AFTER_SECS)
    );
    let without = &rows[1];
    assert_eq!(without.body, "Brandenburger Tor, Testposition");
    assert_eq!((without.lat_e6, without.place.as_ref()), (None, None));

    // The tap: what ChatActivity hands MapActivity, as MapView asks.
    let tap = MapRequest {
        x: None,
        y: None,
        zoom: None,
        at_lat_e6: with.lat_e6,
        at_lon_e6: with.lon_e6,
        selected: Some(loquax::address::to_hex(&peer)),
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &tap,
        Some(model.map_pins()),
        now(),
    ));
    let at = project(LatLon::from_e6(43_775_200, 11_264_000));
    assert_eq!((shown.x, shown.y), (at.x, at.y), "centred on the fix");
    // The Florence fixture's deepest zoom, where its streets are named.
    assert!(shown.zoom >= 15, "a street zoom: {}", shown.zoom);
    assert_eq!(
        shown.selected.as_deref(),
        Some("42".repeat(16).as_str()),
        "the sender's pin selected"
    );
    assert_eq!(shown.pins, 1, "{shown:?}");
}

/// The zoom a pinch leaves is the zoom the view keeps (Lew's device test
/// of dd23d987, 1134): a pinch from 14.3 that ends at 15.37 is drawn at
/// rest at 15.37, and in every frame after, from the z15 tiles scaled; a
/// double tap goes one step on exactly. While the fingers are down the
/// frame is the z15 step one to one, and Java's Canvas scales it the 0.37
/// on (1155), so the next request asks from z15 by what is left.
#[test]
fn a_pinch_ending_between_steps_stays_there() {
    let home = home();
    let start = MapRequest {
        zoom: None,
        zoom_milli: Some(14_300),
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &start,
        Some(Vec::new()),
        now(),
    ));
    assert_eq!(shown.zoom_milli, 14_300);
    let pinch = MapRequest {
        zoom_by_milli: 1_070,
        about_x: (W / 2) as i32,
        about_y: (H / 2) as i32,
        moving: true,
        ..start.clone()
    };
    let (moving, _) = frame(&loquax_android::map::render(
        &pinch,
        Some(Vec::new()),
        now(),
    ));
    assert_eq!(
        (moving.zoom_milli, moving.tile_zoom),
        (15_000, 15),
        "while the fingers are down, the step one to one"
    );
    let rest = MapRequest {
        x: Some(moving.x),
        y: Some(moving.y),
        zoom_milli: Some(moving.zoom_milli),
        zoom_by_milli: 370,
        about_x: (W / 2) as i32,
        about_y: (H / 2) as i32,
        ..start.clone()
    };
    let (at_rest, _) = frame(&loquax_android::map::render(&rest, Some(Vec::new()), now()));
    assert_eq!(at_rest.zoom_milli, 15_370, "no snap at rest");
    assert_eq!((at_rest.zoom, at_rest.tile_zoom), (15, 15));
    assert_eq!((at_rest.x, at_rest.y), (moving.x, moving.y));
    let rest = MapRequest {
        zoom_milli: Some(at_rest.zoom_milli),
        zoom_by_milli: 0,
        ..rest
    };
    let (again, _) = frame(&loquax_android::map::render(&rest, Some(Vec::new()), now()));
    assert_eq!(again.zoom_milli, 15_370, "nor in a frame after");
    let double_tap = MapRequest {
        zoom_by_milli: 1_000,
        ..rest
    };
    let (tapped, _) = frame(&loquax_android::map::render(
        &double_tap,
        Some(Vec::new()),
        now(),
    ));
    assert_eq!(tapped.zoom_milli, 16_370, "a double tap is one step");
}

/// While the fingers pinch, a frame keeps the step it was composed from
/// as long as the pinch is within a step of it, never above the fingers'
/// zoom, and is that step one to one; past a step it takes the nearest
/// step's tiles. At rest the nearest step, scaled to the exact zoom.
#[test]
fn a_pinch_keeps_its_tiles_within_a_step() {
    let home = home();
    let mut ask = MapRequest {
        zoom: None,
        zoom_milli: Some(14_300),
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert_eq!(shown.tile_zoom, 14);
    ask.moving = true;
    let mut steps = Vec::new();
    for z in [14_600, 14_900, 15_000, 15_300, 15_700, 14_800] {
        ask.zoom_milli = Some(z);
        let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        steps.push((shown.zoom_milli, shown.tile_zoom));
    }
    assert_eq!(
        steps,
        [
            (14_000, 14),
            (14_000, 14),
            (14_000, 14),
            (15_000, 15),
            (15_000, 15),
            // Out below the step it kept: the step below, scaled up.
            (14_000, 14),
        ]
    );
    ask.moving = false;
    ask.zoom_milli = Some(15_700);
    let (rest, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert_eq!(
        (rest.zoom_milli, rest.tile_zoom, rest.pending),
        (15_700, 16, false),
        "the nearest step at rest"
    );
}

/// The view's picture with no names and no pins, drawn whole about
/// `centre` at z16, as RGBA.
fn bare(home: &Path, centre: World) -> Vec<u8> {
    let mut painter = Painter::new(Maps::new(Some(home.join("maps"))));
    let view = Viewport::new(centre, 16, W, H);
    let drawn = painter.draw(&view, Ink::Colour, Style::PC, 100);
    drawn
        .fb
        .iter()
        .flat_map(|&px| {
            use embedded_graphics::pixelcolor::raw::RawU16;
            use embedded_graphics::pixelcolor::Rgb565;
            let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
            [r, g, b, 0xff]
        })
        .collect()
}

/// Names stay while the finger moves (Lew's device test of dd23d987,
/// 1134): every pixel the rest frame's names and pin cover, where they
/// differ from the bare map, is the same pixel in a frame panned while
/// the finger is down, moved by the pan, in each direction.
#[test]
fn a_moving_frame_keeps_the_names_of_the_rest_frame() {
    let home = home();
    let pins = vec![pin(1, "alice", 43.7752, 11.2640)];
    let first = request(home.path(), false);
    let (rest, still) = frame(&loquax_android::map::render(
        &first,
        Some(pins.clone()),
        now(),
    ));
    let centre = ExactView::new(
        World {
            x: rest.x,
            y: rest.y,
        },
        16_000,
        W,
        H,
    )
    .on_whole_pixel()
    .centre;
    let plain = bare(home.path(), centre);
    let names: Vec<usize> = (0..(W * H) as usize)
        .filter(|&i| still[i * 4..i * 4 + 3] != plain[i * 4..i * 4 + 3])
        .collect();
    assert!(
        names.len() > 2_000,
        "the names cover {} pixels",
        names.len()
    );
    for (dx, dy) in [(40, -20), (-60, 0), (0, 35), (-25, -25)] {
        let panned = MapRequest {
            x: Some(rest.x),
            y: Some(rest.y),
            pan_x: dx,
            pan_y: dy,
            moving: true,
            ..first.clone()
        };
        let (_, moved) = frame(&loquax_android::map::render(
            &panned,
            Some(pins.clone()),
            now(),
        ));
        let (mut kept, mut seen) = (0, 0);
        for &i in &names {
            let (x, y) = ((i % W as usize) as i32, (i / W as usize) as i32);
            let (mx, my) = (x - dx, y - dy);
            if !(0..W as i32).contains(&mx) || !(0..H as i32).contains(&my) {
                continue;
            }
            seen += 1;
            let j = (my as usize * W as usize + mx as usize) * 4;
            if moved[j..j + 4] == still[i * 4..i * 4 + 4] {
                kept += 1;
            }
        }
        assert!(seen > 1_000, "pan {dx},{dy}: {seen}");
        assert!(
            kept * 1000 >= seen * 998,
            "pan {dx},{dy}: {kept} of {seen} name pixels where they were"
        );
    }
}

/// The pins are drawn over the tiles: a frame with other pins, or with
/// none, over the same view draws no tile, and the tiles a pan uncovers
/// are the only ones a moving frame draws.
#[test]
fn a_pin_costs_no_tile() {
    let home = home();
    let ask = request(home.path(), false);
    let _ = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    let before = loquax_android::map::tile_stats(home.path()).expect("a renderer");
    assert!(before.rendered > 0);
    for pins in [
        vec![pin(1, "alice", 43.7752, 11.2640)],
        vec![
            pin(2, "bob", 43.7760, 11.2650),
            pin(3, "carol", 43.7745, 11.2670),
        ],
        Vec::new(),
    ] {
        let _ = frame(&loquax_android::map::render(&ask, Some(pins), now()));
    }
    let after = loquax_android::map::tile_stats(home.path()).expect("a renderer");
    assert_eq!(after.rendered, before.rendered, "no tile drawn for pins");
    assert_eq!(after.hits, before.hits + 3 * before.misses);
}

/// The milliseconds between the frames [`pan_and_rest`] asks for, on the
/// renderer's held clock: a phone's 60 a second.
const FRAME_MS: u64 = 16;

/// Pan the view of `ask` by `(dx, dy)` five times with the finger down,
/// then a frame at rest: the request for the view it left. The renderer's
/// clock is held, a frame every [`FRAME_MS`] from `at_ms`, so the pan's
/// velocity is the gesture's and not how fast the host draws (1167).
fn pan_and_rest(ask: &MapRequest, dx: i32, dy: i32, at_ms: u64) -> (MapShown, MapRequest) {
    let dir = PathBuf::from(&ask.dir);
    let mut ask = ask.clone();
    for i in 0..5 {
        loquax_android::map::tune_clock(&dir, Some(at_ms + i * FRAME_MS));
        ask.pan_x = dx;
        ask.pan_y = dy;
        ask.moving = true;
        let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        ask.x = Some(shown.x);
        ask.y = Some(shown.y);
        ask.zoom = None;
        ask.zoom_milli = Some(shown.zoom_milli);
    }
    ask.pan_x = 0;
    ask.pan_y = 0;
    ask.moving = false;
    loquax_android::map::tune_clock(&dir, Some(at_ms + 5 * FRAME_MS));
    let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    (shown, ask)
}

/// One prefetch call: its answer, no pixels.
fn prefetch(ask: &MapRequest) -> MapShown {
    let call = MapRequest {
        prefetch: true,
        ..ask.clone()
    };
    let bytes = loquax_android::map::render(&call, Some(Vec::new()), now());
    let (shown, pixels) = schema::unframe(&bytes).expect("an answer");
    assert!(shown.ok && pixels.is_empty());
    shown
}

/// The prefetch at rest draws the tile ahead of the last pan first
/// (1135): one tile after a pan north is the one a further pan north
/// uncovers, after a pan south it is not.
#[test]
fn the_prefetch_draws_the_tile_ahead_of_the_pan_first() {
    for (way, hits) in [(-1, 1), (1, 0)] {
        let home = home();
        let ask = request(home.path(), false);
        let _ = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        loquax_android::map::tune_tiles(home.path(), Some(1), false);
        let (rest, ask) = pan_and_rest(&ask, 0, way * 40, 1000);
        assert!(rest.prefetching, "a frame at rest owing nothing prefetches");
        loquax_android::map::tune_clock(home.path(), Some(1000 + 6 * FRAME_MS));
        let one = prefetch(&ask);
        assert_eq!(one.prefetched, 1);
        assert!(one.prefetching);
        let before = loquax_android::map::tile_stats(home.path()).expect("a renderer");
        let north = MapRequest {
            pan_y: -(tiles::TILE as i32),
            moving: true,
            ..ask.clone()
        };
        let _ = frame(&loquax_android::map::render(
            &north,
            Some(Vec::new()),
            now(),
        ));
        let after = loquax_android::map::tile_stats(home.path()).expect("a renderer");
        assert_eq!(
            after.prefetch_hits - before.prefetch_hits,
            hits,
            "way {way}"
        );
    }
}

/// A prefetch runs to its plan's end within its budget; any frame, a
/// gesture's or a new view's, drops the plan, and a frame while the
/// finger is down leaves none to prefetch.
#[test]
fn a_gesture_cancels_the_prefetch_and_the_budget_bounds_it() {
    let home = home();
    let ask = request(home.path(), false);
    let (rest, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert!(rest.prefetching);
    let mut drawn = 0;
    let mut calls = 0;
    loop {
        let answer = prefetch(&ask);
        drawn += answer.prefetched as usize;
        calls += 1;
        if !answer.prefetching {
            break;
        }
        assert!(calls < 1000);
    }
    assert!(drawn > 0 && drawn <= tiles::PREFETCH_BUDGET, "{drawn}");
    let stats = loquax_android::map::tile_stats(home.path()).expect("a renderer");
    assert_eq!(stats.prefetched as usize, drawn);
    assert_eq!(prefetch(&ask).prefetched, 0, "the plan is done");

    let moving = MapRequest {
        pan_x: 30,
        moving: true,
        ..ask.clone()
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &moving,
        Some(Vec::new()),
        now(),
    ));
    assert!(!shown.prefetching);
    let answer = prefetch(&ask);
    assert_eq!((answer.prefetched, answer.prefetching), (0, false));

    // A new view at rest plans anew; a frame between two calls drops it.
    let (rest, _) = frame(&loquax_android::map::render(
        &moving,
        Some(Vec::new()),
        now(),
    ));
    assert!(!rest.prefetching, "still the finger down");
    // Far enough that its ring is not the tiles drawn already.
    let at_rest = MapRequest {
        pan_x: 4000,
        moving: false,
        ..moving.clone()
    };
    let (rest, _) = frame(&loquax_android::map::render(
        &at_rest,
        Some(Vec::new()),
        now(),
    ));
    assert!(rest.prefetching);
    assert!(prefetch(&at_rest).prefetching);
    let _ = frame(&loquax_android::map::render(
        &moving,
        Some(Vec::new()),
        now(),
    ));
    assert!(!prefetch(&at_rest).prefetching, "the gesture dropped it");
}

/// The app's Bitmap as the tests see it: a buffer that keeps whatever
/// the last frame left, poisoned between frames as a Bitmap the screen
/// showed holds anything.
struct Bitmap {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

impl loquax_android::map::Canvas for Bitmap {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        match (self.w, self.h) == (w, h) {
            true => Ok(&mut self.rgba),
            false => Err(format!("{}x{} asked {w}x{h}", self.w, self.h)),
        }
    }
}

/// 1155: a frame composed straight into the app's Bitmap is the frame
/// the host's path gives, byte for byte and description for description:
/// cold at rest, cold while the finger is down (tiles owed, the stand-in
/// path), a pan, a pinch scaled by the Canvas, at rest between steps; the
/// Bitmap holding garbage before each. A Bitmap of another size gets a
/// line and is left as it was; a prefetch needs none.
#[test]
fn a_frame_into_the_bitmap_is_the_frame_of_the_host_path() {
    let (one, two) = (home(), home());
    let margins = |ask: MapRequest| MapRequest {
        margin_x: W / 4,
        margin_y: H / 4,
        ..ask
    };
    let mut host = margins(request(one.path(), false));
    let mut into = margins(request(two.path(), false));
    let (fw, fh) = (W + 2 * (W / 4), H + 2 * (H / 4));
    let mut bitmap = Bitmap {
        w: fw,
        h: fh,
        rgba: vec![0; (fw * fh * 4) as usize],
    };
    let steps: [(bool, i32, i32); 8] = [
        (true, 0, 0),
        (true, 37, -11),
        (false, 0, 0),
        (true, 40, 23),
        (true, 0, 400),
        (true, 0, 300),
        (false, 0, 0),
        (false, 0, 0),
    ];
    // One worker and no tile held: the first frame with the finger down
    // owes tiles.
    for (ask, dir) in [(&host, one.path()), (&into, two.path())] {
        let _ = loquax_android::map::render(ask, Some(Vec::new()), now());
        loquax_android::map::tune_tiles(dir, Some(1), true);
    }
    let mut owed = 0;
    for (i, &(moving, pan, zoom_by)) in steps.iter().enumerate() {
        for ask in [&mut host, &mut into] {
            ask.moving = moving;
            ask.pan_x = pan;
            ask.zoom_by_milli = zoom_by;
            ask.about_x = (W / 2) as i32;
            ask.about_y = (H / 2) as i32;
        }
        for (k, b) in bitmap.rgba.iter_mut().enumerate() {
            *b = (k * 7 + i) as u8;
        }
        let (want, rgba) = frame(&loquax_android::map::render(&host, Some(Vec::new()), now()));
        let mut got = loquax_android::map::render_into(&into, Some(Vec::new()), now(), &mut bitmap);
        assert!(got.ok, "step {i}: {:?}", got.error);
        got.ms = want.ms;
        assert_eq!(got, want, "step {i}");
        assert!(bitmap.rgba == rgba, "step {i}: the pixels differ");
        owed += u32::from(want.pending);
        for ask in [&mut host, &mut into] {
            ask.x = Some(want.x);
            ask.y = Some(want.y);
            ask.zoom = None;
            ask.zoom_milli = Some(want.zoom_milli);
        }
    }
    assert!(owed > 0, "a frame owed tiles: the stand-in path ran");
    let mut small = Bitmap {
        w: fw - 1,
        h: fh,
        rgba: vec![9; ((fw - 1) * fh * 4) as usize],
    };
    let refused = loquax_android::map::render_into(&into, Some(Vec::new()), now(), &mut small);
    assert!(!refused.ok, "{refused:?}");
    assert!(small.rgba.iter().all(|&b| b == 9), "left as it was");
    let ahead = MapRequest {
        prefetch: true,
        ..into.clone()
    };
    let mut none = Bitmap {
        w: 0,
        h: 0,
        rgba: Vec::new(),
    };
    let answer = loquax_android::map::render_into(&ahead, Some(Vec::new()), now(), &mut none);
    assert!(answer.ok, "{answer:?}");
}
