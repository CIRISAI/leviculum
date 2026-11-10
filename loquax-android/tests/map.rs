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
use loquax::ui::strings::Lang;
use loquax::when::WallTime;
use loquax_android::schema::{self, MapRegions, MapRequest, MapShown, MapThemeChoice};

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

#[path = "../../loquax/tests/support/first_tile.rs"]
mod first_tile;
use first_tile::{
    arrived, first_tile_archive, first_tile_blob, region_files, start_region, FIRST_TILE_AT,
    FIRST_TILE_RATE,
};

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
    // Every frame at rest the full picture and every moving frame at the
    // full rung, however fast the host draws (1422): a test reads what
    // one frame shows. A test of the ladder lets go.
    loquax_android::map::hold_rung(home.path(), Some(0));
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
        nearby: false,
        own: false,
        fix_at: None,
        battery: None,
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
    // On a whole pixel of its step, as the frame sits (1134): smoothed
    // edges show a sub-pixel shift that integer ones hide (1422).
    let at = ExactView::new(project(LatLon::from_degrees(LAT, LON)), 16_000, W, H).on_whole_pixel();
    let view = Viewport::new(at.centre, 16, W, H);
    // Smoothed, as a frame at rest draws its tiles (1422).
    painter
        .draw_rgba(&view, Ink::Colour, Style::PC.in_theme(theme), 100, true)
        .rgba
}

/// How many of the pixels of `a` and `b` are the same, per cent.
fn same_share(a: &[u8], b: &[u8]) -> usize {
    let same = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x == y).count();
    same * 100 / (a.len() / 4)
}

/// The frame through the facade with one pin: the window's renderer's
/// pixels for the view, its names and its pin, but for a fifth of a per
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
        &[],
        None,
        true,
    );
    // Composed from tiles, the frame is the view drawn whole but where a
    // dashed path crosses a seam: its dashes start again at each tile's
    // edge, as they do at a view's. A turned name's sprite rounds its
    // coverage to a level (1156), so its pixels may be a step or two of
    // a channel off the whole view's float paint: not counted. With the
    // edges smoothed (1422) a dash's end covers two pixels where it set
    // one, so the restarted dashes at the view's left edge (x 0 to 14
    // here, one dash period) count about twice: a fifth of a per cent.
    let differ = rgba
        .chunks(4)
        .zip(direct.picture.rgba.chunks(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
        .count();
    assert!(
        differ * 500 < (W * H) as usize,
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

/// The map menu's time window (1419) as the frame says it: without a
/// running session the default, the title "Map" alone, and the sheet's six
/// entries with the current movement first and chosen; a word Java sends
/// back is one of the sheet's.
#[test]
fn the_frame_carries_the_title_and_the_time_window_sheet() {
    let home = home();
    let mut ask = request(home.path(), false);
    ask.track_window = Some("week".to_string());
    let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert_eq!(shown.track_window, "now", "no session keeps no window");
    assert_eq!(shown.title, "Map");
    let words: Vec<&str> = shown
        .track_windows
        .iter()
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(words, ["now", "today", "yesterday", "week", "month", "all"]);
    let chosen: Vec<&str> = shown
        .track_windows
        .iter()
        .filter(|c| c.chosen)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(chosen, ["now"]);
    assert_eq!(shown.track_windows[0].label, "Current movement");
}

/// The sheet and the title in the phone's language for a window that is
/// not the default (1419): German, "Diese Woche" chosen and after the
/// title; every entry's word names its window again.
#[test]
fn a_window_not_the_default_is_chosen_in_the_sheet_and_named_in_the_title() {
    use loquax::ui::tracks::TrackWindow;
    let sheet = loquax_android::map::track_choices(TrackWindow::Week, Lang::De);
    let labels: Vec<&str> = sheet.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Aktuelle Bewegung",
            "Heute",
            "Gestern",
            "Diese Woche",
            "Dieser Monat",
            "Alles"
        ]
    );
    let chosen: Vec<&str> = sheet
        .iter()
        .filter(|c| c.chosen)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(chosen, ["week"]);
    for (entry, window) in sheet.iter().zip(TrackWindow::CHOICES) {
        assert_eq!(TrackWindow::of_word(&entry.word), Some(window));
    }
    assert_eq!(TrackWindow::Week.title(Lang::De), "Karte · Diese Woche");
    assert_eq!(TrackWindow::Episode.title(Lang::De), "Karte");
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

/// The phone draws the buildings (1192), and in the window's pixels:
/// over the Florence cut with its building sections, its frame at z16 and
/// z17, light and dark, once its names faded in, is the window's golden
/// of the same view pixel for pixel (`loquax/tests/golden/gui-buildings-*.png`, drawn by
/// `loquax::mapframe` as the window draws): one library, one picture,
/// buildings and names. Over the cut without them the frame differs, by
/// footprints.
#[test]
fn the_phone_draws_the_buildings_as_the_window_does() {
    let home = home();
    let maps = home.path().join("maps");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../leviculum-map/fixtures/buildings/firenze.lmr");
    std::fs::copy(fixture, maps.join("firenze.lmr")).expect("the region");
    let bare = tempfile::tempdir().expect("bare");
    loquax_android::map::hold_rung(bare.path(), Some(0));
    std::fs::create_dir_all(bare.path().join("maps")).expect("maps");
    std::fs::copy(
        home.path().join(loquax::config::CONFIG_FILE),
        bare.path().join(loquax::config::CONFIG_FILE),
    )
    .expect("config");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr"),
        bare.path().join("maps/firenze.lmr"),
    )
    .expect("the region");
    // The frame once its names faded in (1205): the renderer's clock held
    // at `at_ms` for every frame, then five seconds further. Never the
    // running clock: a renderer serves all four views, and a name new to
    // a later one fades in from the clock of its first frame, which on
    // the running clock is however long the views before it took. Held
    // back below that, the name was still nothing at "five seconds
    // later" on a host slow enough, 2502 pixels of the z17 names on the
    // forge (1405). A renderer's first frame shows its names at once, so
    // the clock it is made with does not matter.
    let settled = |ask: &MapRequest, at_ms: u64| {
        let dir = Path::new(&ask.dir);
        let draw = || loquax_android::map::render(ask, Some(Vec::new()), now());
        loquax_android::map::tune_clock(dir, Some(at_ms));
        let _ = draw();
        loquax_android::map::tune_clock(dir, Some(at_ms));
        let _ = draw();
        loquax_android::map::tune_clock(dir, Some(at_ms + 5_000));
        let settled = frame(&draw());
        loquax_android::map::tune_clock(dir, None);
        settled
    };
    let mut at_ms = 0;
    for zoom in [16u8, 17] {
        for (dark, t) in [(false, "light"), (true, "dark")] {
            let ask = |dir: &Path| MapRequest {
                zoom: Some(zoom),
                ..request(dir, dark)
            };
            at_ms += 10_000;
            let (shown, rgba) = settled(&ask(home.path()), at_ms);
            assert_eq!((shown.w, shown.h), (W, H));
            let (_, without) = settled(&ask(bare.path()), at_ms);
            let share = same_share(&rgba, &without);
            assert!(
                share < 95,
                "z{zoom} {t}: buildings drawn, {share} % the same"
            );
            let window = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../loquax/tests/golden/gui-buildings-{t}-z{zoom}.png"
            ));
            let bytes = std::fs::read(&window).expect("the window's golden");
            let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
                .read_info()
                .expect("png");
            let mut want = vec![0; reader.output_buffer_size().expect("size")];
            let info = reader.next_frame(&mut want).expect("frame");
            want.truncate(info.buffer_size());
            let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            if want != rgb {
                let name = format!("phone-buildings-{t}-z{zoom}.png");
                panic!(
                    "z{zoom} {t}: the phone is not the window's picture; {}",
                    tell_the_difference(&name, &want, &rgb, W, H)
                );
            }
        }
    }
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
    // Its ground (1441), what the view shows where the frame it moves
    // does not reach: the dark table's land, opaque; the light one's in
    // the light table.
    let argb = |theme: MapTheme| {
        let [r, g, b] = rgb888(theme.palette().background);
        0xff00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
    };
    assert_eq!(shown.ground, argb(MapTheme::Dark));
    let (shown, _) = draw(&request(home.path(), false));
    assert_eq!(shown.ground, argb(MapTheme::Light));
    assert_ne!(argb(MapTheme::Light), argb(MapTheme::Dark));
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

/// The settings page's map theme (1199): asking draws nothing and says
/// `auto` while unset; each choice stored is the config's `map_theme`,
/// the other keys kept, and the next frame draws in it whatever the
/// phone says; `auto` hands the choice back to the phone; a word the
/// config does not know is refused and stores nothing.
#[test]
fn the_settings_page_sets_the_map_theme() {
    let home = home();
    let theme = |set: Option<&str>| {
        loquax_android::map::theme(home.path(), set).unwrap_or_else(|error| MapThemeChoice {
            ok: false,
            theme: error,
            ..MapThemeChoice::default()
        })
    };
    let shown = theme(None);
    assert!(
        shown.ok && shown.theme == "auto" && !shown.theme_chosen,
        "{shown:?}"
    );
    let draw = |phone_dark: bool| {
        frame(&loquax_android::map::render(
            &request(home.path(), phone_dark),
            Some(Vec::new()),
            now(),
        ))
        .0
    };
    for (word, flag, dark) in [
        ("dark", ThemeFlag::Dark, true),
        ("light", ThemeFlag::Light, false),
    ] {
        let shown = theme(Some(word));
        assert!(
            shown.ok && shown.theme == word && shown.theme_chosen,
            "{shown:?}"
        );
        let config = loquax::config::load(home.path()).expect("the config");
        assert_eq!(config.map_theme, Some(flag));
        assert_eq!(config.map_autoload, Some(false), "the other keys kept");
        for phone_dark in [true, false] {
            let shown = draw(phone_dark);
            assert_eq!(shown.dark, dark, "{word}, phone dark {phone_dark}");
            assert_eq!(shown.theme, word);
        }
        assert_eq!(theme(None).theme, word);
    }
    let shown = theme(Some("auto"));
    assert!(shown.ok && !shown.theme_chosen, "{shown:?}");
    assert!(draw(true).dark && !draw(false).dark, "the phone decides");
    let shown = theme(Some("sepia"));
    assert!(!shown.ok, "{shown:?}");
    assert_eq!(
        loquax::config::load(home.path())
            .expect("the config")
            .map_theme,
        Some(ThemeFlag::Auto)
    );
}

/// The Map tab's loading chip (1199) reads the frame's `loading_here`
/// (1292, `loading` before): a map
/// opened on a place no region holds asks the autoload for it, and while
/// the download runs (a fake source that takes the connection and never
/// answers) every frame says `loading`, so Java asks again and the chip
/// stays. A source that refuses ends the load with a notice, the chip's
/// line for a load that could not start, and once the loads the screen
/// asked for found no internet too, `loading` drops and the frame says
/// `offline`.
#[test]
fn a_missing_region_loads_by_itself_and_the_frame_says_so() {
    let stalls = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let refuses = std::net::TcpListener::bind("127.0.0.1:0").expect("another port");
    let refused_port = refuses.local_addr().expect("its address").port();
    drop(refuses);
    let berlin = |home: &Path| MapRequest {
        at_lat_e6: Some(52_520_000),
        at_lon_e6: Some(13_405_000),
        ..request(home, false)
    };
    let source = |home: &Path, port: u16| {
        std::fs::write(
            home.join(loquax::config::CONFIG_FILE),
            format!("map_source = \"http://127.0.0.1:{port}/planet.pmtiles\"\n"),
        )
        .expect("config");
    };

    let home = home();
    source(
        home.path(),
        stalls.local_addr().expect("its address").port(),
    );
    let (shown, _) = frame(&loquax_android::map::render(
        &berlin(home.path()),
        Some(Vec::new()),
        now(),
    ));
    assert!(
        shown.loading && shown.loading_here,
        "the autoload runs: {shown:?}"
    );
    let (shown, _) = frame(&loquax_android::map::render(
        &request(home.path(), false),
        Some(Vec::new()),
        now(),
    ));
    assert!(
        shown.loading && shown.notice.is_none(),
        "still loading: {shown:?}"
    );

    let home = self::home();
    source(home.path(), refused_port);
    let (shown, _) = frame(&loquax_android::map::render(
        &berlin(home.path()),
        Some(Vec::new()),
        now(),
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut last = shown;
    while last.notice.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "no notice for a refused source: {last:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        last = frame(&loquax_android::map::render(
            &request(home.path(), false),
            Some(Vec::new()),
            now(),
        ))
        .0;
    }
    // The view asks for the cells its screen lacks too (1292); each finds
    // no internet in turn, said once, and then nothing loads and the chip
    // says "Offline".
    while last.loading {
        assert!(
            std::time::Instant::now() < deadline,
            "the loads never ended: {last:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        last = frame(&loquax_android::map::render(
            &request(home.path(), false),
            Some(Vec::new()),
            now(),
        ))
        .0;
        assert!(last.notice.is_none(), "no internet said once: {last:?}");
    }
    assert!(last.offline && !last.loading_here, "{last:?}");
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
            source: loquax::telemetry::Source::Telemetry,
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

/// What differs between the RGB picture `want` and `got`, `w` x `h`: the
/// count, the box around it and the count per band of 32 rows, with `got`
/// kept under the target's temp dir as `name`, so a run on another host
/// (the forge, whose container goes with its picture) shows which pixels.
fn tell_the_difference(name: &str, want: &[u8], got: &[u8], w: u32, h: u32) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("loquax-android-golden");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let kept = dir.join(name);
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    let mut writer = e.write_header().expect("header");
    writer.write_image_data(got).expect("data");
    writer.finish().expect("finish");
    std::fs::write(&kept, out).expect("write");
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut bands = vec![0usize; h.div_ceil(32) as usize];
    let mut differ = 0;
    for (i, (a, b)) in want.chunks(3).zip(got.chunks(3)).enumerate() {
        if a != b {
            let (x, y) = (i as u32 % w, i as u32 / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            bands[(y / 32) as usize] += 1;
            differ += 1;
        }
    }
    let bands: Vec<String> = bands
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, n)| format!("rows {}..{}: {n}", i * 32, i * 32 + 32))
        .collect();
    format!(
        "{differ} pixels differ in x {x0}..={x1}, y {y0}..={y1} ({}); this run's is {}",
        bands.join(", "),
        kept.display()
    )
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
                sent_at: 0,
                message_id: [id; 32],
                source: peer,
                title: Vec::new(),
                body: b"Brandenburger Tor, Testposition".to_vec(),
                fields,
                verified: true,
            }),
        );
    }
    let rows = loquax_android::session::rows(&model, 0, Lang::En);
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
    loquax_android::map::tune_clock(home.path(), Some(0));
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
    assert_eq!((rest.zoom_milli, rest.tile_zoom), (15_700, 16));
    // The names new to the view fade in (1205): pending until they have.
    loquax_android::map::tune_clock(home.path(), Some(loquax::mapframe::FADE_MS));
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
    pan_every(ask, dx, dy, at_ms, FRAME_MS)
}

/// [`pan_and_rest`] with a frame every `frame_ms`.
fn pan_every(
    ask: &MapRequest,
    dx: i32,
    dy: i32,
    at_ms: u64,
    frame_ms: u64,
) -> (MapShown, MapRequest) {
    let dir = PathBuf::from(&ask.dir);
    let mut ask = ask.clone();
    for i in 0..5 {
        loquax_android::map::tune_clock(&dir, Some(at_ms + i * frame_ms));
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
    loquax_android::map::tune_clock(&dir, Some(at_ms + 5 * frame_ms));
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

/// The prefetch plans by the heading when the frame at rest was asked,
/// not what is left of it once a slow phone has drawn that frame (1189):
/// a pan north at 600 pixels a second, 12 every 20 ms, a frame at rest
/// that takes 1.2 s, and the first tile prefetched is still the one a
/// further pan north uncovers. Measured from the rest frame's ask the
/// smoothed velocity, about 190 pixels a second, falls to about 17 in
/// those 1.2 s, under [`tiles::MOTION_FLOOR`]. A phone held upright,
/// so the tiles left and right are the nearest and a plan without a
/// heading starts with one of them.
#[test]
fn the_prefetch_keeps_the_heading_a_slow_rest_frame_was_asked_with() {
    let home = home();
    let ask = MapRequest {
        w: H,
        h: W,
        ..request(home.path(), false)
    };
    let _ = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    loquax_android::map::tune_tiles(home.path(), Some(1), false);
    let (rest, ask) = pan_every(&ask, 0, -12, 1000, 20);
    assert!(rest.prefetching, "a frame at rest owing nothing prefetches");
    loquax_android::map::tune_clock(home.path(), Some(1000 + 5 * 20 + 1200));
    let one = prefetch(&ask);
    assert_eq!(one.prefetched, 1);
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
        1,
        "the tile ahead of the pan north first"
    );
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
        // Both on one clock: a pinch's frame places its names and those
        // it sets anew fade in by it (1225).
        for dir in [one.path(), two.path()] {
            loquax_android::map::tune_clock(dir, Some(10_000 + 16 * i as u64));
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

/// The map's people (1200): asked with `avatars`, the frame draws no pin
/// of its own and says where the app draws its avatar circles. Over
/// Florence at z13 two friends 150 m apart are one cluster of two, a third
/// a kilometre east a pin of its own; at the zoom the cluster names they
/// lie a circle apart and are two pins. Two in one place never part: their
/// cluster has no zoom, and its tap opens the filtered sheet.
#[test]
fn avatar_pins_cluster_and_split_at_the_zoom_they_name() {
    let home = home();
    let pair_lat: f64 = 43.7752;
    let (west, east) = (
        11.2650,
        11.2650 + 150.0 / (111_320.0 * pair_lat.to_radians().cos()),
    );
    let far = 11.2650 + 1_000.0 / (111_320.0 * pair_lat.to_radians().cos());
    let pins = vec![
        pin(1, "anna", pair_lat, west),
        pin(2, "bob", pair_lat, east),
        pin(3, "carla", pair_lat, far),
    ];
    let centre = project(LatLon::from_degrees(pair_lat, (west + far) / 2.0));
    let ask = MapRequest {
        x: Some(centre.x),
        y: Some(centre.y),
        zoom: Some(13),
        avatars: true,
        ..request(home.path(), false)
    };
    let (shown, rgba) = frame(&loquax_android::map::render(
        &ask,
        Some(pins.clone()),
        now(),
    ));
    let hex = |b: u8| format!("{b:02x}").repeat(16);
    let peers: Vec<Vec<String>> = shown.marks.iter().map(|m| m.peers.clone()).collect();
    assert_eq!(peers, vec![vec![hex(1), hex(2)], vec![hex(3)]], "{shown:?}");
    // The cluster sits on its first pin, where the view puts anna.
    let view = ExactView::new(centre, 13_000, W, H);
    let (ax, ay) = view.to_screen(pins[0].at);
    assert_eq!(
        (shown.marks[0].x, shown.marks[0].y),
        (ax.round() as i32, ay.round() as i32)
    );
    // No marker drawn there: neither pin colour under anna.
    let at = (ay as usize * W as usize + ax as usize) * 4;
    assert_ne!(&rgba[at..at + 3], &[0xd0, 0x30, 0x30]);
    let split = shown.marks[0].split_milli.expect("the regions part them");
    assert!(
        split > 13_000 && split <= u32::from(shown.max_zoom) * 1000,
        "{split}"
    );
    assert_eq!(shown.marks[1].split_milli, Some(13_000), "a pin is split");

    // At that zoom, about the pair's middle, anna and bob are two pins.
    let middle = project(LatLon::from_degrees(pair_lat, (west + east) / 2.0));
    let zoomed = MapRequest {
        x: Some(middle.x),
        y: Some(middle.y),
        zoom: None,
        zoom_milli: Some(split),
        ..ask.clone()
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &zoomed,
        Some(pins.clone()),
        now(),
    ));
    let alone: Vec<usize> = shown.marks.iter().map(|m| m.peers.len()).collect();
    assert!(alone.iter().all(|&n| n == 1), "{shown:?}");
    assert!(shown.marks.iter().any(|m| m.peers == [hex(1)]));
    assert!(shown.marks.iter().any(|m| m.peers == [hex(2)]));
    // A fifth of a step short of it they are one again: the zoom named is
    // where they part, not far past it.
    let short = MapRequest {
        zoom_milli: Some(split - 200),
        ..zoomed.clone()
    };
    let (shown, _) = frame(&loquax_android::map::render(&short, Some(pins), now()));
    assert!(shown.marks.iter().any(|m| m.peers.len() == 2), "{shown:?}");

    // Two in one place: a cluster no zoom parts.
    let same = vec![
        pin(4, "dora", pair_lat, west),
        pin(5, "emil", pair_lat, west),
    ];
    let (shown, _) = frame(&loquax_android::map::render(&ask, Some(same), now()));
    assert_eq!(shown.marks.len(), 1);
    assert_eq!(shown.marks[0].split_milli, None);

    // Without `avatars` the frame draws its pins and says no marks.
    let drawn = MapRequest {
        avatars: false,
        ..ask
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &drawn,
        Some(vec![pin(6, "fritz", pair_lat, west)]),
        now(),
    ));
    assert!(shown.marks.is_empty());
    assert_eq!(shown.pins, 1);
}

/// "No map here yet" (1200): a view no region holds, with the autoload
/// off, says so; the Florence view does not.
#[test]
fn a_view_without_a_region_says_no_map_here_yet() {
    let home = home();
    let (shown, _) = frame(&loquax_android::map::render(
        &request(home.path(), false),
        Some(Vec::new()),
        now(),
    ));
    assert!(!shown.no_map && !shown.offline, "{shown:?}");
    let berlin = project(LatLon::from_degrees(52.52, 13.405));
    let away = MapRequest {
        x: Some(berlin.x),
        y: Some(berlin.y),
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(&away, Some(Vec::new()), now()));
    assert!(shown.no_map && !shown.loading, "{shown:?}");
}

/// The Maps page's call (1201, 1207) for `home`: no frame, the regions;
/// a refusal as `ok: false`.
fn regions(home: &Path, change: impl FnOnce(&mut (Option<String>, Option<bool>))) -> MapRegions {
    let mut ask = (None, None);
    change(&mut ask);
    loquax_android::map::regions(home, ask.0.as_deref(), ask.1).unwrap_or_else(|_| MapRegions {
        ok: false,
        ..MapRegions::default()
    })
}

/// 1201: the Maps page lists the regions newest first with their sizes,
/// their sum and the autoload; it switches the autoload in the config and
/// deletes a region, which the next frame no longer draws; a name that is
/// no region file of the directory deletes nothing.
#[test]
fn the_maps_page_lists_sizes_and_deletes_regions() {
    let home = home();
    let maps = home.path().join("maps");
    let len = std::fs::metadata(maps.join("firenze.lmr"))
        .expect("the region")
        .len();
    std::fs::copy(maps.join("firenze.lmr"), maps.join("auto-old.lmr")).expect("a second");
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(maps.join("auto-old.lmr"))
        .and_then(|f| f.set_modified(old))
        .expect("an older time");
    std::fs::write(maps.join("notes.txt"), "not a region").expect("a stray file");

    let shown = regions(home.path(), |_| {});
    assert!(shown.ok, "{shown:?}");
    let names: Vec<&str> = shown.regions.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["firenze.lmr", "auto-old.lmr"], "newest first");
    assert!(shown.regions.iter().all(|r| r.bytes == len));
    assert_eq!(shown.region_bytes, 2 * len);
    assert!(!shown.autoload, "the test home's config says off");
    let florence = &shown.regions[0];
    assert!(!florence.auto && shown.regions[1].auto);
    assert_eq!(shown.regions[1].modified, 1_700_000_000);
    assert!(
        (florence.lat_e6 - 43_770_000).abs() < 50_000
            && (florence.lon_e6 - 11_250_000).abs() < 50_000,
        "named by its middle: {florence:?}"
    );
    assert!(florence.min_zoom <= florence.max_zoom && florence.max_zoom >= 14);

    // The autoload, on and off, in the config.
    let on = regions(home.path(), |ask| ask.1 = Some(true));
    assert!(on.ok && on.autoload, "{on:?}");
    let config = loquax::config::load(home.path()).expect("the config");
    assert_eq!(config.map_autoload, Some(true));
    let off = regions(home.path(), |ask| ask.1 = Some(false));
    assert!(off.ok && !off.autoload);
    assert_eq!(
        loquax::config::load(home.path())
            .expect("the config")
            .map_autoload,
        Some(false)
    );

    // Names that are no region file delete nothing.
    for bad in ["../config", "notes.txt", "gone.lmr", "", "maps/firenze.lmr"] {
        let refused = regions(home.path(), |ask| ask.0 = Some(bad.to_string()));
        assert!(!refused.ok, "{bad:?}: {refused:?}");
    }
    assert!(home.path().join(loquax::config::CONFIG_FILE).is_file());
    assert!(maps.join("notes.txt").is_file());

    let left = regions(home.path(), |ask| ask.0 = Some("auto-old.lmr".to_string()));
    assert!(left.ok, "{left:?}");
    assert_eq!(left.regions.len(), 1);
    assert_eq!(left.region_bytes, len);
    assert!(!maps.join("auto-old.lmr").exists());

    // Florence still draws; once its region is deleted the frame says
    // there is no map here.
    let before = frame(&loquax_android::map::render(
        &request(home.path(), false),
        Some(Vec::new()),
        now(),
    ))
    .0;
    assert!(!before.no_map, "{before:?}");
    let none = regions(home.path(), |ask| ask.0 = Some("firenze.lmr".to_string()));
    assert!(none.ok && none.regions.is_empty() && none.region_bytes == 0);
    let after = frame(&loquax_android::map::render(
        &request(home.path(), false),
        Some(Vec::new()),
        now(),
    ))
    .0;
    assert!(after.no_map, "the deleted region is not drawn: {after:?}");
}

/// 1201: the crosshair centres on the phone's fix at the zoom it asks,
/// held to the regions' deepest, and the frame says where the fix lies in
/// the view; without a fix it says nothing.
#[test]
fn the_crosshair_centres_on_the_own_fix_and_the_frame_places_it() {
    let home = home();
    let (lat, lon) = (43_772_000, 11_255_000);
    let ask = |zoom: u32, own: bool| {
        let mut ask = request(home.path(), false);
        ask.at_lat_e6 = Some(lat);
        ask.at_lon_e6 = Some(lon);
        ask.at_zoom_milli = Some(zoom);
        if own {
            ask.own_lat_e6 = Some(lat);
            ask.own_lon_e6 = Some(lon);
        }
        frame(&loquax_android::map::render(&ask, Some(Vec::new()), now())).0
    };
    let shown = ask(15_000, true);
    assert_eq!(shown.zoom_milli, 15_000, "{shown:?}");
    let (x, y) = (shown.own_x.expect("x"), shown.own_y.expect("y"));
    assert!(
        (x - W as i32 / 2).abs() <= 1 && (y - H as i32 / 2).abs() <= 1,
        "the fix in the middle: {x}, {y}"
    );
    let deep = ask(30_000, true);
    assert_eq!(deep.zoom_milli, u32::from(deep.max_zoom) * 1000, "held");
    let none = ask(15_000, false);
    assert_eq!((none.own_x, none.own_y), (None, None));
    // A fix off the view lies outside it, not clamped onto its edge.
    let mut away = request(home.path(), false);
    away.own_lat_e6 = Some(lat + 50_000);
    away.own_lon_e6 = Some(lon);
    let away = frame(&loquax_android::map::render(&away, Some(Vec::new()), now())).0;
    assert!(away.own_y.expect("y") < 0, "{away:?}");
}

/// How many threads [`names_fade_out_and_in_by_the_clock`] draws on: the
/// painters' cap before their count became the host's cores (3f0f236b).
const FADE_WORKERS: usize = 8;

/// 1205: a name a pinch drops fades out by the renderer's clock (1167),
/// from 255 to nothing over 150 ms; the names the frame at rest after the
/// pinch places anew fade in over as long; while one fades the frame is
/// pending, so Java asks again; and with the fades done the frame at rest
/// is the one a fresh renderer draws there, byte for byte.
///
/// The renderer draws on [`FADE_WORKERS`] threads whatever the host has
/// (1610): a moving frame draws two tiles a thread, and on one CPU, or
/// under a CPU quota, which `available_parallelism` reads as one, the
/// pinch's frames still owed tiles at 150 ms and said `pending` for them,
/// which this test read as a fade not over.
#[test]
fn names_fade_out_and_in_by_the_clock() {
    let first_home = home();
    let dir = first_home.path().to_path_buf();
    let rest = MapRequest {
        w: 1280,
        h: 960,
        zoom: None,
        zoom_milli: Some(15_000),
        ..request(first_home.path(), false)
    };
    let draw = |ask: &MapRequest, at_ms: u64| {
        loquax_android::map::tune_clock(&dir, Some(at_ms));
        let (shown, rgba) = frame(&loquax_android::map::render(ask, Some(Vec::new()), now()));
        let names = loquax_android::map::drawn_names(&dir).expect("a renderer");
        (shown, rgba, names)
    };
    let _ = frame(&loquax_android::map::render(&rest, Some(Vec::new()), now()));
    loquax_android::map::tune_tiles(&dir, Some(FADE_WORKERS), false);
    let (shown, _, names) = draw(&rest, 1000);
    assert!(
        !shown.pending,
        "the first frame at rest shows its names at once"
    );
    assert!(names.len() > 3 && names.iter().all(|n| n.alpha == 255));
    // Pinched out to z14.4: the streets of z15 drop out and fade.
    let pinch = MapRequest {
        moving: true,
        zoom_milli: Some(14_400),
        ..rest.clone()
    };
    let (shown, _, names) = draw(&pinch, 2000);
    let dropped: Vec<String> = names
        .iter()
        .filter(|n| !n.shown)
        .map(|n| n.text.clone())
        .collect();
    assert!(dropped.len() > 2, "{names:?}");
    assert!(
        names.iter().all(|n| n.alpha == 255),
        "the fade starts whole"
    );
    assert!(shown.pending, "fading: ask again");
    let alpha_at = |at_ms: u64| {
        let (shown, _, names) = draw(&pinch, at_ms);
        let alphas: Vec<u8> = names
            .iter()
            .filter(|n| dropped.contains(&n.text))
            .map(|n| n.alpha)
            .collect();
        (shown.pending, alphas)
    };
    assert_eq!(alpha_at(2050), (true, vec![170; dropped.len()]));
    assert_eq!(alpha_at(2075), (true, vec![128; dropped.len()]));
    assert_eq!(alpha_at(2149), (true, vec![2; dropped.len()]));
    assert_eq!(alpha_at(2150), (false, Vec::new()), "gone at 150 ms");
    // At rest at z15.6 the placement runs: what the pinch kept shows at
    // once, the names new to the view fade in from nothing.
    let after = MapRequest {
        zoom_milli: Some(15_600),
        ..rest.clone()
    };
    let (shown, _, first) = draw(&after, 3000);
    assert!(shown.pending);
    assert!(first.iter().all(|n| n.alpha == 255), "kept: {first:?}");
    let (shown, _, half) = draw(&after, 3075);
    assert!(shown.pending);
    let (shown, settled, names) = draw(&after, 3150);
    assert!(!shown.pending, "the fades are done");
    assert!(names.iter().all(|n| n.alpha == 255));
    assert!(first.len() < names.len(), "{first:?} {names:?}");
    assert_eq!(half.len(), names.len());
    let halfway = half.iter().filter(|n| n.alpha == 127).count();
    assert_eq!(halfway, names.len() - first.len(), "{half:?}");
    let fresh = home();
    let (_, want) = frame(&loquax_android::map::render(
        &MapRequest {
            dir: fresh.path().display().to_string(),
            ..after.clone()
        },
        Some(Vec::new()),
        now(),
    ));
    assert!(settled == want, "the frame at rest after the fades");
}

/// A coast toward the pole (1188), as Java asks it: a moving frame at
/// each step of the fling, the steps shrinking. Each frame moves the view
/// by its step until the world's edge, says how far it may still go
/// (`room_north`), and holds the view there once that is spent; a frame
/// at rest at the end stays at the edge. North and south room together
/// are the world's height less the screen's at every frame.
#[test]
fn a_coast_moves_less_each_frame_and_stops_at_the_worlds_edge() {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    let at = project(LatLon::from_degrees(60.0, 11.0));
    let mut ask = MapRequest {
        dir: home.path().display().to_string(),
        w: W,
        h: H,
        density_milli: 1000,
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(4),
        ..MapRequest::default()
    };
    let (rest, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert!(rest.fling, "map_fling is on when the config does not say");
    let world = 256u32 << 4;
    let span = |s: &MapShown| s.room_north + s.room_south;
    assert!(span(&rest).abs_diff(world - H) <= 1, "{rest:?}");
    assert!(rest.room_north > 500, "{}", rest.room_north);
    let unit = f64::from(1u32 << (24 - 4));
    let mut room = rest.room_north;
    let mut y = rest.y;
    let mut moved = Vec::new();
    let mut steps = 0;
    for i in 0..40 {
        let step = (400.0 * 0.85f64.powi(i)).round() as i32;
        ask.x = Some(rest.x);
        ask.y = Some(y);
        ask.zoom = None;
        ask.zoom_milli = Some(rest.zoom_milli);
        ask.pan_y = -step;
        ask.moving = true;
        let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        let went = ((f64::from(y) - f64::from(shown.y)) / unit).round() as u32;
        assert_eq!(
            went,
            room.min(step as u32),
            "frame {i}: a step of {step} with {room} to the edge"
        );
        assert_eq!(shown.room_north, room - went, "frame {i}");
        assert!(
            span(&shown).abs_diff(world - H) <= 1,
            "frame {i}: {shown:?}"
        );
        moved.push(went);
        room = shown.room_north;
        y = shown.y;
        steps += step as u32;
    }
    assert!(steps > rest.room_north, "the coast reaches the edge");
    assert!(
        moved.windows(2).all(|w| w[1] <= w[0]),
        "each frame moves less: {moved:?}"
    );
    assert_eq!(room, 0, "stopped at the edge");
    assert_eq!(*moved.last().unwrap_or(&1), 0, "held there: {moved:?}");
    ask.y = Some(y);
    ask.pan_y = 0;
    ask.moving = false;
    let (end, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert_eq!((end.y, end.room_north), (y, 0), "at rest at the edge");
}

/// A coast ends with a frame at rest (1188): the moving frames of the
/// coast keep the names of the frame before it, moved, and the frame at
/// rest after the last step sets the names of its view, the ones a fresh
/// renderer sets there.
#[test]
fn a_coast_ends_with_a_frame_at_rest_that_sets_its_names() {
    let first_home = home();
    let dir = first_home.path().to_path_buf();
    let rest = MapRequest {
        w: 1280,
        h: 960,
        zoom: None,
        zoom_milli: Some(15_000),
        ..request(first_home.path(), false)
    };
    let draw = |ask: &MapRequest, at_ms: u64| {
        loquax_android::map::tune_clock(&dir, Some(at_ms));
        let (shown, _) = frame(&loquax_android::map::render(ask, Some(Vec::new()), now()));
        let mut names: Vec<String> = loquax_android::map::drawn_names(&dir)
            .expect("a renderer")
            .into_iter()
            .filter(|n| n.shown)
            .map(|n| n.text)
            .collect();
        names.sort();
        (shown, names)
    };
    let (start, before) = draw(&rest, 1000);
    let mut ask = rest.clone();
    let (mut x, mut y) = (start.x, start.y);
    let mut at_ms = 1000;
    for i in 0..30 {
        let step = (90.0 * 0.85f64.powi(i)).round() as i32;
        at_ms += 16;
        ask.x = Some(x);
        ask.y = Some(y);
        ask.pan_x = step;
        ask.pan_y = step / 2;
        ask.moving = true;
        let (shown, names) = draw(&ask, at_ms);
        assert_eq!(names, before, "frame {i} of the coast keeps the names");
        (x, y) = (shown.x, shown.y);
    }
    assert!(x > start.x, "the coast moved the view");
    ask.x = Some(x);
    ask.y = Some(y);
    ask.pan_x = 0;
    ask.pan_y = 0;
    ask.moving = false;
    let _ = draw(&ask, at_ms + 16);
    let (end, after) = draw(&ask, at_ms + 400);
    assert!(!end.pending, "the fades are done");
    let fresh = home();
    let fresh_ask = MapRequest {
        dir: fresh.path().display().to_string(),
        ..ask.clone()
    };
    loquax_android::map::tune_clock(fresh.path(), Some(0));
    let _ = frame(&loquax_android::map::render(
        &fresh_ask,
        Some(Vec::new()),
        now(),
    ));
    let mut placed: Vec<String> = loquax_android::map::drawn_names(fresh.path())
        .expect("a renderer")
        .into_iter()
        .filter(|n| n.shown)
        .map(|n| n.text)
        .collect();
    placed.sort();
    assert_ne!(after, before, "the view at rest names other places");
    assert_eq!(after, placed, "the frame at rest sets its view's names");
}

/// The device walk's `buildings` step (1193) on the host: its constants
/// in `scripts/device-walk.py` held to the renderer. Over the walk's
/// Frankfurt region the phone's frame on the walk's place, asked as the
/// exported map screen asks it (`at_zoom_milli`), shows the building fill
/// of either table, through RGB565 as the walk looks for it, on well over
/// the walk's least share of its samples of the middle half at z16, and
/// on next to none at z15, where no building is drawn.
#[test]
fn the_walks_frankfurt_shows_the_building_fill_at_z16_and_not_at_z15() {
    use leviculum_mapfill::style::Class;
    let script = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/device-walk.py"),
    )
    .expect("the walk");
    let value = |name: &str| -> String {
        let line = script
            .lines()
            .find(|l| l.starts_with(&format!("{name} = ")))
            .unwrap_or_else(|| panic!("{name} in the walk"));
        line[name.len() + 3..].trim().to_string()
    };
    let numbers = |name: &str| -> Vec<f64> {
        value(name)
            .trim_matches(|c| c == '(' || c == ')')
            .split(',')
            .map(|n| n.trim().parse().expect("a number"))
            .collect()
    };
    let fill = |name: &str| -> [u8; 3] {
        let n = numbers(name);
        [n[0] as u8, n[1] as u8, n[2] as u8]
    };
    let place = numbers("FRANKFURT_E6");
    let grid = numbers("BUILDINGS_GRID");
    let least = numbers("BUILDINGS_LEAST")[0];
    let z15_most = numbers("BUILDINGS_Z15_MOST")[0];
    let fills = [fill("BUILDING_FILL_LIGHT"), fill("BUILDING_FILL_DARK")];
    assert_eq!(
        fills,
        [
            rgb888(MapTheme::Light.palette().area(Class::Building)),
            rgb888(MapTheme::Dark.palette().area(Class::Building)),
        ],
        "the walk's fills are the tables' through RGB565"
    );

    let home = tempfile::tempdir().expect("home");
    let maps = home.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../leviculum-map/fixtures/frankfurt/roemer.lmr"),
        maps.join("frankfurt-roemer.lmr"),
    )
    .expect("the region");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    // A phone's screen at half its pixels and half its density: the same
    // ground as 1080 x 2400 at 2.75.
    let (w, h) = (540u32, 1200u32);
    let share = |zoom: u32, dark: bool| -> f64 {
        let ask = MapRequest {
            dir: home.path().display().to_string(),
            w,
            h,
            density_milli: 1375,
            dark,
            at_lat_e6: Some(place[0] as i32),
            at_lon_e6: Some(place[1] as i32),
            at_zoom_milli: Some(zoom),
            ..MapRequest::default()
        };
        let (shown, rgba) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        assert_eq!(shown.zoom_milli, zoom, "{shown:?}");
        let (cols, rows) = (grid[0] as u32, grid[1] as u32);
        // The walk's sample_grid over its middle_half.
        let (x1, y1, x2, y2) = (shown.w / 4, shown.h / 4, shown.w * 3 / 4, shown.h * 3 / 4);
        let mut hits = 0;
        for r in 0..rows {
            let y = y1 + (2 * r + 1) * (y2 - y1) / (2 * rows);
            for c in 0..cols {
                let x = x1 + (2 * c + 1) * (x2 - x1) / (2 * cols);
                let i = ((y * shown.w + x) * 4) as usize;
                let p = &rgba[i..i + 3];
                if fills
                    .iter()
                    .any(|f| f.iter().zip(p).all(|(a, b)| a.abs_diff(*b) <= 3))
                {
                    hits += 1;
                }
            }
        }
        f64::from(hits) / f64::from(cols * rows)
    };
    for dark in [false, true] {
        let z16 = share(16_000, dark);
        let z15 = share(15_000, dark);
        assert!(
            z16 >= 2.0 * least,
            "dark={dark}: z16 fill {z16:.3}, the walk wants {least} with room"
        );
        assert!(
            z15 <= z15_most,
            "dark={dark}: z15 fill {z15:.3}, the walk allows {z15_most}"
        );
        eprintln!("dark={dark} fill z16={z16:.3} z15={z15:.3}");
    }
}

/// The walk of 1292: a phone's map driven as `MapView` drives it, the
/// renderer's clock held, a frame every [`FRAME_MS`] while a finger or a
/// coast moves it; at rest a frame again while one is `pending`, every
/// quarter second while one says `filling` or `loading_here` (1527),
/// every 1.5 s while one says `loading`, and the prefetch while one says
/// `prefetching` and none `loading`.
struct Walk {
    home: tempfile::TempDir,
    /// The source that takes the connection and never answers: a load
    /// that starts stays running.
    source: std::net::TcpListener,
    ask: MapRequest,
    t_ms: u64,
    /// When the screen was first more than a tenth uncovered.
    uncovered_ms: Option<u64>,
    /// When a frame first said a region loads for the screen.
    loading_ms: Option<u64>,
    /// The chip of the last frame.
    chip: &'static str,
    /// The chip of the frame that first said a region loads.
    chip_then: &'static str,
    /// The zoom the fingers made, in thousandths: a frame that moves is
    /// drawn at its tile step, and `MapView` asks from the step it got
    /// for what the fingers did since.
    fingers_milli: i32,
}

/// The Florence view held by the committed region, 480 x 320 at z16.
const WALK_LAT: f64 = 43.7756;
const WALK_LON: f64 = 11.2600;

impl Walk {
    fn new() -> Self {
        let home = home();
        let source = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = source.local_addr().expect("its address").port();
        std::fs::write(
            home.path().join(loquax::config::CONFIG_FILE),
            format!("map_source = \"http://127.0.0.1:{port}/planet.pmtiles\"\n"),
        )
        .expect("config");
        let at = project(LatLon::from_degrees(WALK_LAT, WALK_LON));
        let ask = MapRequest {
            x: Some(at.x),
            y: Some(at.y),
            ..request(home.path(), false)
        };
        let mut walk = Self {
            home,
            source,
            ask,
            t_ms: 0,
            uncovered_ms: None,
            loading_ms: None,
            chip: "",
            chip_then: "",
            fingers_milli: 16_000,
        };
        let first = walk.frame(0, 0, 0, false);
        assert!(!first.loading_here, "the start is held: {first:?}");
        walk
    }

    /// The share of the screen of `shown` no region holds.
    fn share(&self, shown: &MapShown) -> f64 {
        let regions = Maps::new(Some(self.home.path().join("maps"))).headers();
        let view = Viewport::new(
            World {
                x: shown.x,
                y: shown.y,
            },
            shown.zoom,
            W,
            H,
        );
        loquax::ui::viewload::uncovered(&view, &regions)
    }

    /// The chip `MapPane` shows over `shown`.
    fn chip_of(shown: &MapShown) -> &'static str {
        if shown.loading_here {
            "Loading the map of this area"
        } else if shown.offline {
            "Offline"
        } else if shown.no_map {
            "No map here yet"
        } else {
            "none"
        }
    }

    fn note(&mut self, shown: &MapShown, at_ms: u64) {
        if self.uncovered_ms.is_none() && self.share(shown) > 0.10 {
            self.uncovered_ms = Some(at_ms);
        }
        if self.loading_ms.is_none() && shown.loading_here {
            self.loading_ms = Some(at_ms);
            self.chip_then = Self::chip_of(shown);
        }
        self.chip = Self::chip_of(shown);
    }

    /// One frame at the walk's clock, panned by `(dx, dy)` and the
    /// fingers zoomed by `zoom_by_milli` since the last; the clock then
    /// moves a frame on.
    fn frame(&mut self, dx: i32, dy: i32, zoom_by_milli: i32, moving: bool) -> MapShown {
        let dir = self.home.path().to_path_buf();
        self.fingers_milli += zoom_by_milli;
        let drawn = self.ask.zoom_milli.unwrap_or(16_000) as i32;
        let zoom_by_milli = self.fingers_milli - drawn;
        loquax_android::map::tune_clock(&dir, Some(self.t_ms));
        let ask = MapRequest {
            pan_x: dx,
            pan_y: dy,
            zoom_by_milli,
            about_x: (W / 2) as i32,
            about_y: (H / 2) as i32,
            moving,
            ..self.ask.clone()
        };
        let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        loquax_android::map::tune_clock(&dir, Some(self.t_ms));
        self.ask.x = Some(shown.x);
        self.ask.y = Some(shown.y);
        self.ask.zoom = None;
        self.ask.zoom_milli = Some(shown.zoom_milli);
        let at = self.t_ms;
        self.note(&shown, at);
        self.t_ms += FRAME_MS;
        shown
    }

    /// At rest until `ms` from now, as `MapView`'s frame thread is.
    fn rest(&mut self, ms: u64) {
        let until = self.t_ms + ms;
        let mut last = self.frame(0, 0, 0, false);
        while self.t_ms < until {
            if last.pending {
                last = self.frame(0, 0, 0, false);
            } else if last.filling || last.loading_here {
                // Four times a second while a region arrives or a load
                // runs for the screen (1291, 1527).
                self.t_ms += 250;
                last = self.frame(0, 0, 0, false);
            } else if last.loading {
                self.t_ms += 1500;
                last = self.frame(0, 0, 0, false);
            } else if last.prefetching {
                loquax_android::map::tune_clock(self.home.path(), Some(self.t_ms));
                let answer = prefetch(&self.ask);
                self.t_ms += FRAME_MS;
                // The prefetch's answer is no frame: the chip waits for
                // the next one, which nothing asks for.
                last.prefetching = answer.prefetching;
                if answer.loading_here && self.loading_ms.is_none() {
                    self.loading_ms = Some(self.t_ms);
                    self.chip_then = "none (no frame asked)";
                }
            } else {
                break;
            }
        }
    }

    /// Whether the load reached the source.
    fn connected(&self) -> bool {
        self.source.set_nonblocking(true).expect("nonblocking");
        self.source.accept().is_ok()
    }

    fn row(&self, name: &str) -> String {
        let ms = |t: Option<u64>| t.map_or("never".to_string(), |t| t.to_string());
        let after = match (self.uncovered_ms, self.loading_ms) {
            (Some(u), Some(l)) => format!("{}", l.saturating_sub(u)),
            _ => "-".to_string(),
        };
        format!(
            "| {name} | {} | {} | {after} | {} | {} | {} |",
            ms(self.uncovered_ms),
            ms(self.loading_ms),
            self.chip_then,
            self.chip,
            self.connected()
        )
    }
}

/// A pan, a fling and a zoom out from the Florence region into the empty
/// (1292): the frame that first leaves more than a tenth of the screen
/// uncovered asks for its region within 300 ms, during the motion too,
/// the chip says it loads here, and the load reaches the source. A table
/// row a gesture: uncovered at, loading at, the difference, the chip
/// then and at the end, whether the source was asked.
#[test]
fn a_pan_into_the_empty_loads_what_is_on_screen_at_once() {
    let mut rows = Vec::new();
    let mut late = Vec::new();
    let mut check = |walk: &Walk, name: &str| {
        rows.push(walk.row(name));
        match (walk.uncovered_ms, walk.loading_ms) {
            (Some(u), Some(l)) if l <= u + 300 => {}
            other => late.push(format!("{name}: {other:?}")),
        }
    };
    // A pan of a screen and a half east in 576 ms, then at rest.
    let mut walk = Walk::new();
    for _ in 0..36 {
        walk.frame(20, 0, 0, true);
    }
    walk.rest(3000);
    check(&walk, "pan, then rest");
    // The same pan, then on slowly for two seconds, never at rest.
    let mut walk = Walk::new();
    for _ in 0..36 {
        walk.frame(20, 0, 0, true);
    }
    for _ in 0..125 {
        walk.frame(2, 0, 0, true);
    }
    check(&walk, "pan, on slowly");
    // A short drag east, the finger lifts, the map coasts on (1209).
    let mut walk = Walk::new();
    for _ in 0..8 {
        walk.frame(30, 0, 0, true);
    }
    let mut v = 40.0f64;
    while v >= 1.0 {
        walk.frame(v.round() as i32, 0, 0, true);
        v *= 0.96;
    }
    walk.rest(3000);
    check(&walk, "fling, coasting");
    // Two steps out about the screen's middle in eight frames.
    let mut walk = Walk::new();
    for _ in 0..8 {
        walk.frame(0, 0, -250, true);
    }
    walk.rest(3000);
    check(&walk, "zoom out two steps");
    eprintln!("| gesture | uncovered at ms | loading at ms | after ms | chip then | chip at end | source asked |");
    eprintln!("|---|---|---|---|---|---|---|");
    for row in &rows {
        eprintln!("{row}");
    }
    assert!(late.is_empty(), "late or never: {late:?}");
}

/// A region shows itself while it downloads (1291): the autoload writes it
/// in place, coarse to fine, and every frame on its area shows what has
/// arrived, the z14 tiles standing in for the z15 ones not there yet, and
/// says `filling`, so Java asks again in a quarter second. The source is
/// the loopback server throttled to 8 kB/s, the Grunewald archive (z12 to
/// z15, 80 kB) about ten seconds of it. While it loads the frames go
/// from the outline through at least two pictures of stand-ins, and the
/// whole region ends it.
///
/// The link stops at [`REGION_HOLD`], inside the region's tiles, until a
/// frame has said `filling` (1610): the tiles arrive in a window of under
/// five seconds, and on a shared runner (pipeline 617) the frames came
/// slowly enough that none fell into it, `frames 51 filling 0 while
/// loading [25, 99, 100]`. Held, the half-written region is there for as
/// long as the host takes to draw it, so the test asks what the product
/// promises, that a frame drawn on a region still arriving says so, and
/// not whether the host drew one in time.
#[test]
fn a_region_fills_the_frame_in_while_it_downloads() {
    use leviculum_mapcut::serve::Serve;
    use std::time::{Duration, Instant};
    const ARCHIVE: &[u8] = include_bytes!("../../leviculum-mapcut/fixtures/grunewald.pmtiles");
    let serve = Serve::start(vec![("o.pmtiles".into(), ARCHIVE.to_vec())]).expect("serve");
    serve.throttle(8_000, Duration::from_millis(20));
    serve.hold_at(REGION_HOLD);
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join("maps")).expect("maps");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        format!("map_source = \"{}\"\n", serve.url("o.pmtiles")),
    )
    .expect("config");
    let at = project(LatLon::from_degrees(52.4715, 13.2280));
    let ask = MapRequest {
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(15),
        ..request(home.path(), false)
    };
    // A liveness bound: the throttled link takes about 36 s on the host.
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut frames: Vec<(MapShown, Vec<u8>)> = Vec::new();
    let mut held = true;
    loop {
        let (shown, rgba) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        if held && shown.filling {
            serve.release();
            held = false;
        }
        let done = !shown.loading && !shown.filling && !shown.pending && frames.len() > 1;
        frames.push((shown, rgba));
        if done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the region never landed (held at {} bytes: {held})",
            serve.sent()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let last = &frames[frames.len() - 1].1;
    // The distinct pictures while the region loads, each with how like
    // the last it is, per cent.
    let mut states: Vec<(Vec<u8>, usize)> = Vec::new();
    for (_, rgba) in frames.iter().filter(|(s, _)| s.loading) {
        if states.last().map(|s| &s.0) != Some(rgba) {
            states.push((rgba.clone(), same_share(rgba, last)));
        }
    }
    let near: Vec<usize> = states.iter().map(|s| s.1).collect();
    let filling = frames.iter().filter(|(s, _)| s.filling).count();
    println!(
        "frames {} filling {filling} while loading {near:?}",
        frames.len()
    );
    assert!(filling > 0, "a frame said the region is arriving");
    assert!(
        near.len() >= 3,
        "the outline and two pictures of stand-ins: {near:?}"
    );
    assert!(
        near[0] < near[near.len() - 1],
        "the region came nearer: {near:?}"
    );
}

/// Where the link of [`a_region_fills_the_frame_in_while_it_downloads`]
/// stops until a frame says `filling`: a body byte inside the region's
/// tiles. The plan's directories end, and the first tiles are drawn, by
/// byte 57 003; the last tile of the region arrives by byte 92 364; the
/// same on one CPU and on ten (1610).
const REGION_HOLD: u64 = 75_000;

/// The first-tile walk of 1527: the Grunewald, a synthetic archive of it
/// ([`leviculum_mapcut::pmtiles::padded`]: a real z15 tile in every slot,
/// each padded with [`FIRST_TILE_PAD`] bytes of its own, so a box's
/// bytes grow with its tiles) served at [`FIRST_TILE_RATE`] bytes a
/// second, so a load's size shows in its time. The phone's map is driven
/// on the real clock as `MapView` drives it: a frame every [`FRAME_MS`]
/// while the view moves; at rest a frame at once while one is `pending`,
/// a quarter second after one that says `filling` or `loading_here`
/// (1527), 1.5 s after one that says `loading`.
struct FirstTile {
    home: tempfile::TempDir,
    _serve: leviculum_mapcut::serve::Serve,
    ask: MapRequest,
    start: std::time::Instant,
    fingers_milli: i32,
    last: Option<(MapShown, u64)>,
    /// When the screen was first more than a tenth uncovered.
    uncovered_ms: Option<u64>,
    /// When a frame first said a region loads for the screen.
    loading_ms: Option<u64>,
    /// When a tile of the screen at the zoom viewed was first on disk.
    on_disk_ms: Option<u64>,
    /// The first frame drawn with that tile there: `first_tile_ms`.
    first_tile_ms: Option<u64>,
    /// The start region's header: what "uncovered" is measured against,
    /// the same for every stack under test.
    start_region: leviculum_map::region::Header,
    /// When a region over more than two zooms (a cell, not the screen's
    /// own box) first appeared, whether its load started before the screen
    /// was uncovered (the cells ahead of a pan, 1292) or after.
    cell_ms: Option<u64>,
}

impl FirstTile {
    /// The walk at its start: a region cut at full speed holds the
    /// start's screen at z13 to z15 (named by hand, so it is no cache),
    /// then the link is throttled and the first frame drawn.
    fn new() -> Self {
        use leviculum_mapcut::serve::Serve;
        use std::time::{Duration, Instant};
        let serve = Serve::start(vec![("p.pmtiles".into(), first_tile_archive())]).expect("serve");
        let home = home();
        let maps = home.path().join("maps");
        std::fs::remove_file(maps.join("firenze.lmr")).expect("only the Grunewald");
        std::fs::write(
            home.path().join(loquax::config::CONFIG_FILE),
            format!("map_source = \"{}\"\n", serve.url("p.pmtiles")),
        )
        .expect("config");
        let at = project(LatLon::from_degrees(FIRST_TILE_AT.0, FIRST_TILE_AT.1));
        let start_region = start_region(&maps, &serve.url("p.pmtiles"), at, (W, H));
        serve.throttle(FIRST_TILE_RATE, Duration::from_millis(20));
        let ask = MapRequest {
            x: Some(at.x),
            y: Some(at.y),
            ..request(home.path(), false)
        };
        let mut walk = Self {
            home,
            _serve: serve,
            ask,
            start: Instant::now(),
            fingers_milli: 16_000,
            last: None,
            uncovered_ms: None,
            loading_ms: None,
            on_disk_ms: None,
            first_tile_ms: None,
            start_region,
            cell_ms: None,
        };
        let first = walk.frame(0, 0, 0, false);
        assert!(!first.loading_here, "the start is held: {first:?}");
        walk
    }

    fn maps(&self) -> PathBuf {
        self.home.path().join("maps")
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// The screen of the last frame.
    fn view(&self) -> Option<Viewport> {
        let (shown, _) = self.last.as_ref()?;
        Some(Viewport::new(
            World {
                x: shown.x,
                y: shown.y,
            },
            shown.zoom,
            W,
            H,
        ))
    }

    /// What the disk says now: whether the screen leaves the start region,
    /// a tile of the screen at the zoom viewed in a region loaded since,
    /// and a cell.
    fn look(&mut self, at_ms: u64) {
        let Some(view) = self.view() else { return };
        if self.uncovered_ms.is_none()
            && loquax::ui::viewload::uncovered(&view, &[self.start_region]) > 0.10
        {
            self.uncovered_ms = Some(at_ms);
        }
        let start = ["start.lmr".to_string()];
        if self.on_disk_ms.is_none() && arrived(&self.maps(), &start, &view, view.zoom().min(15)) {
            self.on_disk_ms = Some(at_ms);
        }
        if self.cell_ms.is_none() {
            let cell = region_files(&self.maps())
                .iter()
                .any(|(name, z, _)| z.1 - z.0 >= 2 && name != "start.lmr");
            if cell {
                self.cell_ms = Some(at_ms);
            }
        }
    }

    /// One frame now, panned by `(dx, dy)` and the fingers zoomed by
    /// `zoom_by_milli` since the last.
    fn frame(&mut self, dx: i32, dy: i32, zoom_by_milli: i32, moving: bool) -> MapShown {
        let dir = self.home.path().to_path_buf();
        let t = self.now_ms();
        self.look(t);
        let drawn_with_tile = self.on_disk_ms.is_some();
        self.fingers_milli += zoom_by_milli;
        let drawn = self.ask.zoom_milli.unwrap_or(16_000) as i32;
        loquax_android::map::tune_clock(&dir, Some(t));
        let ask = MapRequest {
            pan_x: dx,
            pan_y: dy,
            zoom_by_milli: self.fingers_milli - drawn,
            about_x: (W / 2) as i32,
            about_y: (H / 2) as i32,
            moving,
            ..self.ask.clone()
        };
        let (shown, _) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
        self.ask.x = Some(shown.x);
        self.ask.y = Some(shown.y);
        self.ask.zoom = None;
        self.ask.zoom_milli = Some(shown.zoom_milli);
        if drawn_with_tile && self.first_tile_ms.is_none() {
            self.first_tile_ms = Some(t);
        }
        if self.loading_ms.is_none() && shown.loading_here && self.uncovered_ms.is_some() {
            self.loading_ms = Some(t);
        }
        self.last = Some((shown.clone(), t));
        self.look(t);
        shown
    }

    /// A frame of a gesture, then the rest of its frame interval.
    fn moving(&mut self, dx: i32, dy: i32, zoom_by_milli: i32) {
        let next = self.now_ms() + FRAME_MS;
        self.frame(dx, dy, zoom_by_milli, true);
        self.until(next);
    }

    fn until(&self, ms: u64) {
        let now = self.now_ms();
        if ms > now {
            std::thread::sleep(std::time::Duration::from_millis(ms - now));
        }
    }

    /// At rest as `MapView`'s frame thread is, until a frame drew the
    /// screen's tile and `more` is done, at most `limit_ms`.
    fn rest(&mut self, limit_ms: u64, more: impl Fn(&Self) -> bool) {
        let until = self.now_ms() + limit_ms;
        self.frame(0, 0, 0, false);
        while self.now_ms() < until && (self.first_tile_ms.is_none() || more(self)) {
            let t = self.now_ms();
            self.look(t);
            let (last, at) = self.last.clone().expect("a frame");
            let due = if last.pending {
                true
            } else if last.filling || last.loading_here {
                t >= at + 250
            } else if last.loading {
                t >= at + 1500
            } else {
                // Idle: the next frame is the next gesture's.
                false
            };
            if due {
                self.frame(0, 0, 0, false);
            } else {
                self.until(t + FRAME_MS);
            }
        }
    }

    fn row(&self, name: &str) -> String {
        let after = |t: Option<u64>| match (self.uncovered_ms, t) {
            (Some(u), Some(t)) => format!("{}", t as i64 - u as i64),
            _ => "never".to_string(),
        };
        format!(
            "| {name} | {} | {} | {} | {} | {} |",
            self.uncovered_ms
                .map_or("never".to_string(), |u| u.to_string()),
            after(self.loading_ms),
            after(self.on_disk_ms),
            after(self.first_tile_ms),
            after(self.cell_ms),
        )
    }
}

/// The screen at its zoom loads first (1527), measured on the phone's
/// facade: for the four gestures of 1292, from the frame that first
/// leaves a tenth of the screen uncovered, when the screen's load started
/// (its chip), when a tile of the screen at the zoom viewed was on disk,
/// the first frame drawn with it (`first_tile_ms`), and when a cell's
/// first byte showed; each from the uncovered frame, so a load the pan's
/// ahead started before it (1292) can be negative. "Uncovered" is
/// measured against the start region alone, the same for both stacks.
/// The bar is the loader's order, read from its steps on the renderer's
/// clock ([`loquax_android::map::load_events`], 1616): a tile of the
/// screen at its zoom arrives, and a cell started after the uncovered
/// frame starts only once no box of the screen runs and every one asked
/// since that frame has landed, so not a byte of it is on the link before
/// the screen's box is whole. The times on disk and of the first frame
/// drawn with the tile are reported, not asserted: on a host a parallel
/// build starves, a cell the pan's ahead started 80 ms BEFORE the
/// uncovered frame wrote its file after it, and the screen's tile, which
/// that cell brings, drew later than that file appeared (1613). Then the
/// sizes of the screen's own box and of the cell.
#[test]
fn the_screen_at_its_zoom_draws_first_on_the_phone() {
    let mut rows = Vec::new();
    let mut late: Vec<String> = Vec::new();
    let blob = first_tile_blob();
    let mut check = |walk: &FirstTile, name: &str| {
        rows.push(walk.row(name));
        // The archive's bytes of each load: its tiles, each a blob of the
        // tile and its pad.
        for (file, zooms, _) in region_files(&walk.maps()) {
            if file == "start.lmr" {
                continue;
            }
            let opened = std::fs::File::open(walk.maps().join(&file)).expect("open");
            let tiles = leviculum_map::region::Region::open(opened)
                .expect("region")
                .header()
                .tiles;
            eprintln!(
                "{name}: load z{}-{}: {tiles} tiles, {} archive bytes",
                zooms.0,
                zooms.1,
                u64::from(tiles) * blob
            );
        }
        // The loader's steps, from the uncovered frame (1616): the order
        // the bar is read from.
        let steps: Vec<first_tile::Stepped> = loquax_android::map::load_events(walk.home.path())
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.at_ms, e.step, e.want))
            .collect();
        let Some(u) = walk.uncovered_ms else {
            late.push(format!("{name}: the screen was never uncovered"));
            return;
        };
        first_tile::print_steps(name, &steps, u);
        if walk.on_disk_ms.is_none() {
            late.push(format!("{name}: no tile of the screen at its zoom arrived"));
        }
        late.extend(
            first_tile::out_of_order(&steps, u)
                .into_iter()
                .map(|why| format!("{name}: {why}")),
        );
    };
    // A pan of a screen and a half east in 576 ms, then at rest until the
    // loads for the screen are done: the sizes.
    let mut walk = FirstTile::new();
    for _ in 0..36 {
        walk.moving(20, 0, 0);
    }
    walk.rest(30_000, |w| {
        w.last.as_ref().is_some_and(|l| l.0.loading_here)
    });
    check(&walk, "pan, then rest");
    // The same pan, then on slowly for two seconds, never at rest.
    let mut walk = FirstTile::new();
    for _ in 0..36 {
        walk.moving(20, 0, 0);
    }
    for _ in 0..125 {
        walk.moving(2, 0, 0);
    }
    walk.rest(15_000, |w| w.cell_ms.is_none());
    check(&walk, "pan, on slowly");
    // A short drag east, the finger lifts, the map coasts on (1209).
    let mut walk = FirstTile::new();
    for _ in 0..8 {
        walk.moving(30, 0, 0);
    }
    let mut v = 40.0f64;
    while v >= 1.0 {
        walk.moving(v.round() as i32, 0, 0);
        v *= 0.96;
    }
    walk.rest(15_000, |w| w.cell_ms.is_none());
    check(&walk, "fling, coasting");
    // Two steps out about the screen's middle in eight frames.
    let mut walk = FirstTile::new();
    for _ in 0..8 {
        walk.moving(0, 0, -250);
    }
    walk.rest(15_000, |w| w.cell_ms.is_none());
    check(&walk, "zoom out two steps");
    eprintln!("| gesture | uncovered at ms | loading after ms | on disk after ms | first_tile_ms | cell after ms |");
    eprintln!("|---|---|---|---|---|---|");
    for row in &rows {
        eprintln!("{row}");
    }
    assert!(
        late.is_empty(),
        "a cell before the screen's box, or no tile of the screen: {late:?}"
    );
}

/// The position sheet's map (1448, UI walk 1 finding 16): a radar without
/// a map, the own fix under a pin's number over the ring's label, and
/// nothing saying whose dot it was. With `own_only` the frame draws the
/// own fix alone, no one's pin or cluster and never the radar; where no
/// region covers the fix, the world's outline around it at the outline's
/// deepest zoom. Control: the same request without it is the radar at the
/// sheet's z16, with the pin as a mark.
#[test]
fn the_position_sheet_shows_a_map_with_the_own_dot_alone() {
    // Bremen, far from the Florence region the home holds.
    let (lat, lon) = (53_105_960, 9_159_300);
    let sheet = |own_only: bool| {
        let home = home();
        let ask = MapRequest {
            x: None,
            y: None,
            zoom: None,
            at_lat_e6: Some(lat),
            at_lon_e6: Some(lon),
            at_zoom_milli: Some(16_000),
            own_lat_e6: Some(lat),
            own_lon_e6: Some(lon),
            avatars: true,
            own_only,
            ..request(home.path(), false)
        };
        let pins = vec![
            pin(0x21, "Anna", 53.1061, 9.1597),
            pin(0x22, "Ben", 53.1058, 9.1590),
        ];
        frame(&loquax_android::map::render(&ask, Some(pins), now())).0
    };
    let shown = sheet(true);
    assert!(
        shown.zoom <= leviculum_map::outline::MAX_ZOOM,
        "the outline's zoom, not the radar's: {}",
        shown.zoom
    );
    assert!(shown.marks.is_empty(), "nobody else: {:?}", shown.marks);
    assert_eq!(shown.pins, 0);
    let (x, y) = (shown.own_x.expect("own x"), shown.own_y.expect("own y"));
    assert!(
        (x - W as i32 / 2).abs() <= 1 && (y - H as i32 / 2).abs() <= 1,
        "the own dot in the middle: {x},{y}"
    );

    let control = sheet(false);
    assert!(control.zoom >= 15, "the radar's zoom: {}", control.zoom);
    assert!(!control.marks.is_empty(), "the pins as marks");
}
