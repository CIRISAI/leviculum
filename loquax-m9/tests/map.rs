//! The map on the M9, on the host (slice seven, 1375,
//! `handheld-messenger.md`, "Slice seven, host half"). The goldens of its
//! two screens are pinned with the board's in `firmware.rs`; this holds
//! what they rest on: every row of the map's key table runs the terminal's
//! map command through the shared resolver, the pins are the model's from
//! the three ways a position comes, the last key of each walk is in its
//! frame, and one renderer walking into the map and out of it draws what a
//! fresh one draws.

use leviculum_ui::keymap::Ctx;
use loquax::keys::{panes_of, resolve, Action};
use loquax::ui::{update, Pane, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::fixture::{self, Screen, OWN};
use loquax_m9::frame::{Frame, WIDTH};
use loquax_m9::group;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::map::{self, Map, CELL_PX, FIXTURE, PICTURE, ROWS};
use loquax_m9::render::Renderer;
use loquax_m9::screen::Look;

mod support;

fn drawn(model: &UiModel<Field>) -> Frame {
    let mut frame = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    Renderer::new(&mut frame)
        .expect("renderer")
        .show(model, &Look::of(model), &mut map)
        .expect("draw");
    frame
}

/// Each row's key, made the terminal's by `map::key` on the open map,
/// resolves to the row's map command through the shared table. The
/// control: Space and Backspace off the map stay what they were.
#[test]
fn every_map_key_runs_the_terminals_map_command() {
    let model = Screen::Map.model();
    assert!(map::shown(&model), "the walk opens the map");
    for (verb, key, cmd) in ROWS {
        let as_terminal = map::key(&model, event(key));
        let got = resolve(&as_terminal, panes_of(Pane::Map), Ctx::NONE);
        assert_eq!(got, Some(Action::Map(cmd)), "{verb} ({key:?})");
    }
    let list = fixture::model();
    for key in [M9Key::Char(' '), M9Key::Backspace] {
        assert_eq!(
            map::key(&list, event(key)),
            event(key),
            "{key:?} off the map"
        );
    }
}

/// The pins are the model's, from a share, a `lev.position` announce and
/// the board's own fix: you, Jürgen, Mum, Zoë stale, Rifugio nearby. The
/// control: without the GNSS fix the board has no pin of its own.
#[test]
fn the_map_pins_come_from_shares_an_announce_and_the_gnss_fix() {
    let model = Screen::Map.model();
    let pins = model.map_pins();
    let labels: Vec<&str> = pins.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(pins.len(), 5, "{labels:?}");
    let own = pins.iter().find(|p| p.peer == OWN).expect("our pin");
    assert_eq!(
        own.label, "du",
        "the catalogue's, in the board's German (1447)"
    );
    assert!(pins
        .iter()
        .any(|p| p.nearby && p.label.starts_with("Rifugio")));
    assert_eq!(pins.iter().filter(|p| p.stale).count(), 1, "Zoë's is old");
    let (_, placed) = map::view(&model).expect("the open map");
    assert_eq!(placed.len(), 5, "every pin is in the fitted view");
    for pin in &placed {
        assert!(pin.row < PICTURE.bottom(), "{pin:?}");
        assert!(pin.text.contains('*') || pin.text.contains('@'), "{pin:?}");
    }
    let mut without = Screen::Map.model();
    update(&mut without, UiEvent::OwnFix(None));
    assert!(without.map_pins().iter().all(|p| p.peer != OWN));
}

/// The model's view is the picture's pixels: 32 x 11 cells of 10 x 20
/// (1531: no title row, no legend).
#[test]
fn the_view_is_the_picture_in_pixels() {
    let model = Screen::Map.model();
    let (view, _) = map::view(&model).expect("map");
    assert_eq!(
        view.size(),
        (
            u32::from(PICTURE.width * CELL_PX.0),
            u32::from(PICTURE.height * CELL_PX.1)
        )
    );
    assert_eq!(model.map_area(), PICTURE);
}

/// Each map walk's last key changes the frame, and the map is not the
/// list it opened from.
#[test]
fn the_last_key_of_every_map_walk_is_in_its_frame() {
    for shown in [Screen::Map, Screen::Pan] {
        let keys = shown.keys();
        let (_, walk) = keys.split_last().expect("a walk");
        let mut before = fixture::model();
        let mut row = 0;
        for key in walk {
            group::press(&mut before, &mut row, &event(*key));
        }
        assert_ne!(
            drawn(&shown.model()).pixels(),
            drawn(&before).pixels(),
            "{shown:?}"
        );
    }
    // Every key of Pan moved something: the zoom, the chosen pin, the view.
    let map = Screen::Map.model().map.expect("open");
    let pan = Screen::Pan.model().map.expect("open");
    assert_eq!(pan.zoom, map.zoom + 1);
    assert_ne!(pan.selected, map.selected);
    assert_ne!(pan.centre, map.centre);
}

/// The board's loop over the map: one renderer, a key, a draw, a flush
/// of the band. Into the map, around it and out to the list again, what
/// the panel shows after each key is what a fresh draw shows: leaving the
/// map redraws every cell the pixels covered.
#[test]
fn a_map_redraw_is_the_frame_a_fresh_draw_gives() {
    let mut model = fixture::model();
    let mut panel = Frame::new();
    let mut shown = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let flush = |renderer: &Renderer<'_, Frame>, shown: &mut Frame| {
        support::flush_runs(renderer.panel(), shown)
    };
    renderer
        .show(&model, &Look::of(&model), &mut map)
        .expect("draw");
    flush(&renderer, &mut shown);
    let mut row = 0;
    for key in [
        M9Key::Char('M'),
        M9Key::Char('+'),
        M9Key::Char(' '),
        M9Key::Right,
        M9Key::Char('-'),
        M9Key::Back,
        M9Key::Back,
        M9Key::Char('m'),
        M9Key::Back,
    ] {
        group::press(&mut model, &mut row, &event(key));
        renderer
            .show(&model, &Look::of(&model), &mut map)
            .expect("draw");
        flush(&renderer, &mut shown);
        assert_eq!(shown.pixels(), drawn(&model).pixels(), "after {key:?}");
    }
    assert!(!map::shown(&model), "the walk ends off the map");
}

/// The kept picture (1507): the same walk as above with the board's
/// cache on draws every frame a fresh, uncached draw gives, and a switch
/// from the map to the list and back to the same view draws the picture
/// once and copies it back. The control: a zoom draws it again.
#[test]
fn a_kept_map_picture_is_copied_back_after_a_page_switch() {
    let mut model = fixture::model();
    let mut panel = Frame::new();
    let mut shown = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    assert_eq!(map.keep_pictures(), 153_600);
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let mut row = 0;
    let mut press = |key, model: &mut UiModel<Field>, renderer: &mut Renderer<'_, Frame>| {
        group::press(model, &mut row, &event(key));
        renderer
            .show(model, &Look::of(model), &mut map)
            .expect("draw");
        support::flush_runs(renderer.panel(), &mut shown);
        assert_eq!(shown.pixels(), drawn(model).pixels(), "after {key:?}");
        let kept = map.kept.as_ref().expect("kept");
        (kept.drawn, kept.copied, renderer.take_map_stats())
    };
    let (drawn_once, _, stats) = press(M9Key::Char('M'), &mut model, &mut renderer);
    assert_eq!(drawn_once, 1, "the map opened: drawn");
    assert!(!stats.expect("a map draw").kept);
    // Out to the list and into the map again: no picture drawn.
    press(M9Key::Back, &mut model, &mut renderer);
    assert!(!map::shown(&model));
    let (again, copied, stats) = press(M9Key::Char('M'), &mut model, &mut renderer);
    assert!(map::shown(&model));
    assert_eq!(again, 1, "back on the map: copied, not drawn");
    assert_eq!(copied, 1, "the draw's own copy is not counted");
    let stats = stats.expect("a map draw");
    assert!(stats.kept);
    assert_eq!(stats.picture_us, 0);
    // The control: a zoom is a new view.
    let (zoomed, _, stats) = press(M9Key::Char('+'), &mut model, &mut renderer);
    assert_eq!(zoomed, 2, "the zoom draws it");
    assert!(!stats.expect("a map draw").kept);
}

/// The kept picture's key is the view and the pins (1554): the key hint
/// and the corner credit are words over it, so their going leaves the
/// picture kept. Window 13 drew it three times on one open (at the open
/// and each time a word over it went) and again on Map after Back. The
/// board's walk with its chrome's clock ([`map::Chrome`]): open with the
/// hint and the credit, the hint's second page, the hint and the credit
/// gone, Back, Map (the hint again, the credit once a boot): one picture
/// drawn, three copies, every frame the one a fresh, uncached draw with the same words
/// gives. The control: a zoom draws it again.
#[test]
fn the_words_over_the_map_never_draw_its_picture_again() {
    let mut model = fixture::model();
    let mut panel = Frame::new();
    let mut shown = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    map.keep_pictures();
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let mut chrome = map::Chrome::default();
    let mut row = 0;
    let fresh = |model: &UiModel<Field>, look: &Look| {
        let mut frame = Frame::new();
        let mut map = Map::new(FIXTURE).expect("region");
        Renderer::new(&mut frame)
            .expect("renderer")
            .show(model, look, &mut map)
            .expect("draw");
        frame
    };
    let mut step = |key: Option<M9Key>,
                    now: u64,
                    model: &mut UiModel<Field>,
                    renderer: &mut Renderer<'_, Frame>,
                    chrome: &mut map::Chrome| {
        if let Some(key) = key {
            group::press(model, &mut row, &event(key));
        }
        chrome.seen(map::shown(model), now);
        let overlay = chrome.overlay(now, || !model.map_pins().is_empty());
        let look = Look {
            map: overlay,
            ..Look::of(model)
        };
        renderer.show(model, &look, &mut map).expect("draw");
        support::flush_runs(renderer.panel(), &mut shown);
        assert_eq!(
            shown.pixels(),
            fresh(model, &look).pixels(),
            "at {now} ms after {key:?} under {overlay:?}"
        );
        let kept = map.kept.as_ref().expect("kept");
        (overlay, kept.drawn, kept.copied)
    };
    let (open, ..) = step(
        Some(M9Key::Char('M')),
        0,
        &mut model,
        &mut renderer,
        &mut chrome,
    );
    assert!(open.hint.is_some() && open.credit, "{open:?}");
    let (turned, ..) = step(None, 3_500, &mut model, &mut renderer, &mut chrome);
    assert_ne!(turned.hint, open.hint, "the hint's second page: {turned:?}");
    assert!(turned.credit);
    let (bare, ..) = step(None, 6_500, &mut model, &mut renderer, &mut chrome);
    assert_eq!(
        bare,
        map::Overlay::default(),
        "the hint and the credit went"
    );
    step(
        Some(M9Key::Back),
        13_500,
        &mut model,
        &mut renderer,
        &mut chrome,
    );
    assert!(!map::shown(&model));
    let (again, drawn, copied) = step(
        Some(M9Key::Char('M')),
        14_000,
        &mut model,
        &mut renderer,
        &mut chrome,
    );
    assert!(again.hint.is_some() && !again.credit, "{again:?}");
    assert_eq!((drawn, copied), (1, 3), "draws 1, copies 3");
    let stats = renderer.take_map_stats().expect("a map draw");
    assert!(stats.kept);
    // The control: a zoom is a new view, drawn.
    let (_, zoomed, _) = step(
        Some(M9Key::Char('+')),
        14_500,
        &mut model,
        &mut renderer,
        &mut chrome,
    );
    assert_eq!(zoomed, 2, "the zoom draws it");
}

/// A redraw that changes nothing flushes nothing (1455): the board
/// redraws on every pulled entry, ended send and word of its home, and
/// the map's picture and the cells are drawn whole each time; the panel
/// is sent only the rows whose pixels moved, which for the same model
/// is none, on the list and on the map. The control: `+` zooms the map,
/// and that redraw has rows to send.
#[test]
fn an_unchanged_redraw_flushes_nothing() {
    let mut model = fixture::model();
    let mut panel = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    let mut renderer = Renderer::new(&mut panel).expect("renderer");
    let mut row = 0;
    for (key, name) in [(None, "list"), (Some(M9Key::Char('M')), "map")] {
        if let Some(key) = key {
            group::press(&mut model, &mut row, &event(key));
        }
        renderer
            .show(&model, &Look::of(&model), &mut map)
            .expect("draw");
        assert!(renderer.panel().take_dirty().is_some(), "{name}: drawn");
        renderer
            .show(&model, &Look::of(&model), &mut map)
            .expect("draw");
        assert_eq!(renderer.panel().take_dirty(), None, "{name}: drawn again");
    }
    assert!(map::shown(&model));
    group::press(&mut model, &mut row, &event(M9Key::Char('+')));
    renderer
        .show(&model, &Look::of(&model), &mut map)
        .expect("draw");
    assert!(renderer.panel().take_dirty().is_some(), "the zoom");
}

/// 1476, the grey map of Lew's photo, and 1477: a home's board opens the
/// map where its own fix is, far from the detailed region the image
/// carries. The picture is the world overview there (1477 §3): drawn, not
/// grey, with "Nur Übersicht" over it and how a region gets onto the
/// board. Past the overview's zooms the picture says "Keine Karte für
/// diese Gegend" above the own pin and how a region gets onto the board in
/// its last row, no row has a credit for data it does not show, and
/// nothing of the map is drawn. Both pinned as goldens. The control: the
/// fixture's own view over Florence draws the detailed region and says
/// none of it.
#[test]
fn the_map_says_so_where_no_region_lies_under_the_view() {
    use leviculum_lxmf::telemetry::Location;
    use loquax::ui::strings::{StrId, Text};
    use loquax_m9::frame::HEIGHT;
    use loquax_m9::home;
    use loquax_m9::screen::{COLS, ROWS as GRID};

    let mut model = home::model([0x24; 16]);
    // Frankfurt, a minute ago: the M9 on Lew's desk.
    let fix = Location::saturating(50_110_900, 8_682_100, 5_000, 0, 0, 500, 1_762_000_000);
    update(&mut model, UiEvent::OwnFix(Some(fix)));
    let mut row = 0;
    group::press(&mut model, &mut row, &event(M9Key::Char('M')));
    assert!(map::shown(&model), "M opens the home's map");
    let board = Map::board(FIXTURE, &[]).expect("regions");
    let (view, _) = map::view(&model).expect("map");
    assert_eq!(
        board.choose(&view),
        map::Drawn::Overview,
        "z{}",
        view.zoom()
    );

    let top = (PICTURE.y * CELL_PX.1) as u32;
    let bottom = (PICTURE.bottom() * CELL_PX.1) as u32;
    let inks_of = |frame: &Frame| {
        let mut inks = std::collections::BTreeSet::new();
        for y in top..bottom.min(HEIGHT) {
            for x in 0..WIDTH {
                inks.insert(frame.pixel(x, y));
            }
        }
        inks.len()
    };
    let overview = drawn_on_board(&model);
    support::check("m9-map-overview.png", &overview);
    assert!(inks_of(&overview) > 8, "the world is drawn");
    // The names the board sets on that picture (1733), from the renderer's
    // own inputs: the painted view, the own pin's cell and the overview's
    // two rows of words. Three countries, and why no more:
    // - Deutschland's and Frankreich's boxes touch the own pin at Frankfurt
    //   (the cell below), which is placed first and never covered.
    // - Vereinigtes Königreich is 22 cells wide and its label point lies 9
    //   cells from the left edge: the name does not fit the picture whole.
    // - London gives way to Irland and Paris and Rom to Italien, which are
    //   drawn: no name gives way to one that is not.
    // - Spanien, Portugal, Griechenland, Madrid, Istanbul and the
    //   Mittelmeer lie in the bottom row the words keep, Estland, Dänemark
    //   and Stockholm in or beside the top one; Ukraine, Türkei and the
    //   Schwarzes Meer do not fit the picture whole.
    // - Belarus and Berlin give way to Polen, Rumänien, Österreich and the
    //   Alpen to Italien (Österreich and Berlin to the pin as well).
    let (view, pins) = map::view(&model).expect("map");
    let painted = map::with_bottom(&view);
    let mut taken = map::pin_cells(&pins);
    assert!(taken.contains(&(16, 5, 1)), "the own pin: {taken:?}");
    taken.extend(map::overview_rows());
    let rows = (painted.size().1 / CELL_PX.1 as u32) as u16;
    let (names, stats) = map::names(&board.region, &painted, rows, &taken, true);
    let texts: Vec<&str> = names.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(texts, ["Italien", "Irland", "Polen"], "{names:?}");
    assert!(stats.found > texts.len(), "{stats:?}");
    // The control: without the own pin, Deutschland is set, and Frankreich
    // still gives way, now to Deutschland's box beside it.
    let free = map::overview_rows().to_vec();
    let (names, _) = map::names(&board.region, &painted, rows, &free, true);
    let texts: Vec<&str> = names.iter().map(|n| n.text.as_str()).collect();
    assert!(texts.contains(&"Deutschland"), "{texts:?}");
    assert!(!texts.contains(&"Frankreich"), "{texts:?}");

    // Past the overview's zooms: no region at all.
    if let Some(state) = model.map.as_mut() {
        state.zoom = 14;
    }
    let (view, _) = map::view(&model).expect("map");
    assert_eq!(board.choose(&view), map::Drawn::Bare);
    assert!(!map::covers(board.region.header(), &view));

    let look = Look::of(&model);
    let lines = map::lines(&look, true, COLS, GRID);
    let text: Vec<String> = lines
        .iter()
        .map(|l| l.cells.iter().map(|c| c.ch).collect())
        .collect();
    let lang = model.lang;
    let none = Text::key(StrId::MapNoRegion).resolve(lang);
    let how = Text::key(StrId::MapRegionHow).resolve(lang);
    assert_eq!(none, "Keine Karte für diese Gegend", "the 1447 §5 words");
    let picture = usize::from(PICTURE.y)..usize::from(PICTURE.bottom());
    let second = usize::from(PICTURE.y) + 1;
    assert_eq!(text[second].trim(), none, "{text:#?}");
    assert_eq!(text[picture.end - 1].trim(), how, "{text:#?}");
    assert!(how.contains("lnflash"));
    assert!(
        !text[0].contains("OSM"),
        "no credit over no data: {:?}",
        text[0]
    );
    for line in &text {
        assert_eq!(line.chars().count(), COLS, "{line:?}");
    }

    let frame = drawn_on_board(&model);
    support::check("m9-map-no-region.png", &frame);
    // The picture's rows hold the ground and the words' two inks only: no
    // map was drawn.
    assert!(
        inks_of(&frame) <= 8,
        "{} inks in a picture of words",
        inks_of(&frame)
    );

    // The control: the fixture's view over Florence draws the region.
    let florence = Screen::Map.model();
    let (view, _) = map::view(&florence).expect("map");
    assert_eq!(board.choose(&view), map::Drawn::Detail(0));
    let text: Vec<String> = map::lines(&Look::of(&florence), false, COLS, GRID)
        .iter()
        .map(|l| l.cells.iter().map(|c| c.ch).collect())
        .collect();
    assert!(text.iter().all(|l| !l.contains(&none)));
    assert!(inks_of(&drawn(&florence)) > 8, "the region's picture");
}

/// 1477 §3: the world is on the board. A z2 view over Europe draws the
/// world overview, coasts and land in the map's inks, with the overview's
/// words; pinned as a golden. The control: at the same place at z2 with
/// the fixture alone, as the board drew before 1477, the picture is the
/// ground and nothing else.
#[test]
fn the_world_overview_draws_europe_at_z2() {
    let mut model = Screen::Map.model();
    if let Some(state) = model.map.as_mut() {
        state.zoom = 2;
        state.selected = None;
        state.centre = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_e6(
            50_000_000, 10_000_000,
        ));
    }
    let board = Map::board(FIXTURE, &[]).expect("regions");
    let (view, _) = map::view(&model).expect("map");
    assert_eq!(board.choose(&view), map::Drawn::Overview);
    let frame = drawn_on_board(&model);
    support::check("m9-map-world-z2.png", &frame);
    let colours = |frame: &Frame| {
        let mut inks = std::collections::BTreeSet::new();
        for y in 0..HEIGHT_PICTURE {
            for x in 0..WIDTH {
                inks.insert(frame.pixel(x, y));
            }
        }
        inks.len()
    };
    assert!(colours(&frame) > 8, "{} inks", colours(&frame));
    assert!(
        colours(&drawn(&model)) <= 8,
        "control: no world, no picture"
    );
}

/// The picture's pixel rows.
const HEIGHT_PICTURE: u32 = (PICTURE.height as u32) * (CELL_PX.1 as u32);

/// 1712: the world at z6 on the board, as the board draws it (1683's
/// z6 golden came from the map core's own world drawer, which the board
/// never calls). Central Germany at z6 is past the overview's z3 and
/// inside its overzoom, so the board draws the overview region, strokes
/// the world outline's borders over it (`outline::bounds`) and sets the
/// world table's names with the region's. Deutschland is served first,
/// but its label lies under the overview's words ("Nur Übersicht"), so
/// the picture names Frankfurt am Main alone. Pinned as a golden; the
/// border ink is on the picture, and the control: the same view at z12,
/// past the overzoom, draws no border.
#[test]
fn the_world_overview_draws_germany_at_z6() {
    use embedded_graphics::pixelcolor::IntoStorage;
    let at = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_e6(
        50_000_000, 10_000_000,
    ));
    let mut model = Screen::Map.model();
    if let Some(state) = model.map.as_mut() {
        state.zoom = 6;
        state.selected = None;
        state.centre = at;
    }
    let board = Map::board(FIXTURE, &[]).expect("regions");
    let (view, _) = map::view(&model).expect("map");
    assert_eq!(board.choose(&view), map::Drawn::Overview, "z6");
    let frame = drawn_on_board(&model);
    support::check("m9-map-world-z6.png", &frame);
    let border = map::style().theme.palette().border.into_storage();
    let top = u32::from(PICTURE.y) * u32::from(CELL_PX.1);
    let inked = |frame: &Frame| {
        (top..top + HEIGHT_PICTURE)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| frame.pixel(x, y) == border)
            .count()
    };
    assert!(inked(&frame) > 50, "{} border pixels", inked(&frame));
    // The names as the board sets them: on the picture with its bottom
    // row, the overview's two rows of words taken (`render.rs`). The view
    // holds no pin.
    let world = board.of(map::Drawn::Overview).expect("the overview");
    let painted = map::with_bottom(&view);
    let rows = (painted.size().1 / u32::from(CELL_PX.1)) as u16;
    let words = map::overview_rows();
    let (names, _) = map::names(world, &painted, rows, &words, true);
    let texts: Vec<&str> = names.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(texts, ["Frankfurt am Main"], "{names:?}");
    // Without the words, Deutschland is served first: its label point
    // lies on row 2, which the words over the picture take on the board.
    let (free, _) = map::names(world, &painted, rows, &[], true);
    assert_eq!(
        free.first().map(|n| (n.text.as_str(), n.kind, n.row)),
        Some(("Deutschland", leviculum_map::labels::LabelKind::Country, 2)),
        "{free:?}"
    );
    if let Some(state) = model.map.as_mut() {
        state.zoom = 12;
    }
    let (deep, _) = map::view(&model).expect("map");
    assert_ne!(board.choose(&deep), map::Drawn::Overview, "z12");
    assert_eq!(inked(&drawn_on_board(&model)), 0, "control: z12");
}

/// What `model` shows on the board's map: the fixture and the world.
fn drawn_on_board(model: &UiModel<Field>) -> Frame {
    let mut frame = Frame::new();
    let mut map = Map::board(FIXTURE, &[]).expect("regions");
    Renderer::new(&mut frame)
        .expect("renderer")
        .show(model, &Look::of(model), &mut map)
        .expect("draw");
    frame
}

/// What `model` shows under `overlay`, drawn by a fresh renderer.
fn drawn_with(model: &UiModel<Field>, overlay: map::Overlay) -> Frame {
    let mut frame = Frame::new();
    let mut map = Map::new(FIXTURE).expect("region");
    Renderer::new(&mut frame)
        .expect("renderer")
        .show(
            model,
            &Look {
                map: overlay,
                ..Look::of(model)
            },
            &mut map,
        )
        .expect("draw");
    frame
}

/// The fixture's map at `zoom`, its view and pins as the model has them.
fn at_zoom(zoom: u8) -> UiModel<Field> {
    let mut model = Screen::Map.model();
    if let Some(state) = model.map.as_mut() {
        state.zoom = zoom;
    }
    model
}

/// 1531: the M9's map carries names. Over Florence at z16 the label pass
/// sets street names, never on a pin's cells, and they are in the
/// frame; at z13 the fixture holds no name its zoom shows (its place
/// points lie outside the cut, its streets start at z14), and the frame
/// is the picture alone. Both pinned as goldens. The control: the same
/// z16 frame without the names differs.
#[test]
fn the_map_carries_names_at_z13_and_z16() {
    let region = Map::new(FIXTURE).expect("region");
    for (zoom, golden) in [(13, "m9-map-z13.png"), (16, "m9-map-z16.png")] {
        let model = at_zoom(zoom);
        let (view, pins) = map::view(&model).expect("map");
        let tall = map::with_bottom(&view);
        let taken = map::pin_cells(&pins);
        let (names, stats) = map::names(&region.region, &tall, map::BOTTOM + 1, &taken, false);
        println!("z{zoom}: {stats:?} {names:?}");
        assert_eq!(stats.drawn, names.len());
        assert!(names.len() <= map::NAMES_MAX);
        match zoom {
            13 => assert_eq!(stats.found, 0, "{names:?}"),
            _ => assert!(names.len() >= 2, "z{zoom}: {stats:?}"),
        }
        for name in &names {
            let width = name.text.chars().count() as u16;
            assert!(name.col + width <= PICTURE.width, "{name:?}");
            for &(col, row, w) in &taken {
                let apart = row != name.row || col + w <= name.col || name.col + width <= col;
                assert!(apart, "{name:?} over a pin at {col},{row}");
            }
        }
        let frame = drawn_with(&model, map::Overlay::default());
        support::check(golden, &frame);
    }
}

/// 1531: places are set first, then the streets and rivers, then from
/// z15 the parks: over Berlin's Mitte (the map core's second fixture,
/// which holds the city's point) "Berlin" is the first name at every
/// zoom, and a park comes only from z15.
#[test]
fn places_come_first_and_parks_from_z15() {
    use leviculum_map::labels::LabelKind;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::region::Region;
    use leviculum_map::viewport::Viewport;

    let bytes: &[u8] = include_bytes!("../../leviculum-map/fixtures/berlin/mitte.lmr");
    let region = Region::from_bytes(bytes).expect("region");
    let at = project(LatLon::from_e6(52_517_500, 13_395_000));
    for zoom in 12..=16 {
        let view = Viewport::new(at, zoom, 320, 240);
        let (names, stats) = map::names(&region, &view, map::BOTTOM + 1, &[], false);
        println!("z{zoom}: {stats:?} {names:?}");
        let first = names.first().expect("a name");
        assert_eq!(
            (first.kind, first.text.as_str()),
            (LabelKind::City, "Berlin")
        );
        let kinds: Vec<LabelKind> = names.iter().map(|n| n.kind).collect();
        let parks = kinds.iter().any(|k| matches!(k, LabelKind::Park));
        assert!(!(parks && zoom < map::POI_ZOOM), "z{zoom}: {kinds:?}");
        // Rounds in order: no place after a street, no street after a park.
        let round = |k: &LabelKind| match k {
            LabelKind::Park | LabelKind::Water => 2,
            k if k.is_line() => 1,
            _ => 0,
        };
        assert!(
            kinds.windows(2).all(|w| round(&w[0]) <= round(&w[1])),
            "{kinds:?}"
        );
    }
    let view = Viewport::new(at, 15, 320, 240);
    let (names, _) = map::names(&region, &view, map::BOTTOM + 1, &[], false);
    assert!(
        names.iter().any(|n| n.kind == LabelKind::Park),
        "the control: z15 sets a park"
    );
}

/// 1531: the map's chrome. No title row and no legend: the picture
/// covers the panel. The first open after a boot shows the key hint in
/// the bottom row and "© OpenStreetMap" in the top right corner; a later
/// open the hint alone; once the hint is gone the bottom row is the
/// chosen pin's card with the credit. Each pinned as a golden, and each
/// differs from the others.
#[test]
fn the_hint_the_credit_and_the_card_have_their_goldens() {
    let model = Screen::Map.model();
    let keys = map::Overlay {
        hint: Some(loquax::ui::strings::StrId::MapKeys),
        credit: false,
        ..map::Overlay::default()
    };
    let open = map::Overlay {
        credit: true,
        ..keys
    };
    let frames = [
        ("m9-map-open.png", drawn_with(&model, open)),
        ("m9-map-hint.png", drawn_with(&model, keys)),
        (
            "m9-map-card.png",
            drawn_with(&model, map::Overlay::default()),
        ),
    ];
    for (name, frame) in &frames {
        support::check(name, frame);
    }
    for i in 0..frames.len() {
        for j in i + 1..frames.len() {
            assert_ne!(
                frames[i].1.pixels(),
                frames[j].1.pixels(),
                "{} {}",
                frames[i].0,
                frames[j].0
            );
        }
    }
    let lang = model.lang;
    let card = map::bottom(
        &model,
        &map::Overlay::default(),
        Some("© OpenStreetMap"),
        lang,
    )
    .expect("a pin is chosen");
    assert!(card.ends_with("© OpenStreetMap "), "{card:?}");
    let hint = map::bottom(&model, &keys, Some("© OpenStreetMap"), lang).expect("the hint");
    assert_eq!(hint, "T näher Y weiter Pfeile schieben");
}

/// 1531: a key the open map does not know brings the hint back; every
/// key of the table is known. The control: off the map every key is.
#[test]
fn an_unknown_key_on_the_map_is_told_apart() {
    let model = Screen::Map.model();
    for (verb, key, _) in ROWS {
        assert!(map::bound(&model, &event(key)), "{verb} ({key:?})");
    }
    // `z` is the time zone view's since 1684.
    for c in ['q', 'x', 'v'] {
        assert!(!map::bound(&model, &event(M9Key::Char(c))), "{c}");
    }
    let list = fixture::model();
    assert!(map::bound(&list, &event(M9Key::Char('q'))));
}

/// 1684: the time zone view on the M9: `z` turns it on in the shared
/// model, and once the board knows the time the names over the world are
/// the zones' clocks by the board's own table, Paris's at its summer
/// offset among them; without a clock the map stays as it was.
#[test]
fn the_time_zone_view_writes_the_boards_clocks() {
    let mut model = Screen::Map.model();
    assert_eq!(map::zones(&model), None, "off by default");
    model.time_zones = true;
    let fixture_now = model.now.take();
    assert_eq!(map::zones(&model), None, "no clock, no clocks");
    assert!(fixture_now.is_some(), "the fixture's board knows the time");
    // 2026-07-01T12:00:30Z: the minute is what the clocks show.
    model.now = Some(loquax::when::WallTime {
        unix_secs: 1_782_907_230,
        offset_secs: 0,
    });
    let unix = map::zones(&model).expect("on, with a clock");
    assert_eq!(unix, 1_782_907_200);
    let centre = leviculum_map::mercator::project(leviculum_map::LatLon::from_degrees(45.0, 10.0));
    let view = leviculum_map::Viewport::new(centre, 3, 320, 240);
    let (names, _) = map::zone_names(&view, map::BOTTOM + 1, &[], unix);
    assert!(!names.is_empty());
    assert!(
        names
            .iter()
            .all(|n| n.text.len() == 5 && &n.text[2..3] == ":"),
        "{names:?}"
    );
    assert!(names.iter().any(|n| n.text == "14:00"), "{names:?}");
}
