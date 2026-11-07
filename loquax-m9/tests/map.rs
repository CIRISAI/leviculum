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
        assert!(
            PICTURE.y <= pin.row && pin.row < PICTURE.bottom(),
            "{pin:?}"
        );
        assert!(pin.text.contains('*') || pin.text.contains('@'), "{pin:?}");
    }
    let mut without = Screen::Map.model();
    update(&mut without, UiEvent::OwnFix(None));
    assert!(without.map_pins().iter().all(|p| p.peer != OWN));
}

/// The model's view is the picture's pixels: 32 x 9 cells of 10 x 20.
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
        let panel = renderer.panel();
        if let Some((first, last)) = panel.take_dirty() {
            let mut pixels = shown.pixels().to_vec();
            let rows = (first * WIDTH) as usize..((last + 1) * WIDTH) as usize;
            pixels[rows.clone()].copy_from_slice(&panel.pixels()[rows]);
            *shown = Frame::from_pixels(pixels).expect("whole");
        }
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
