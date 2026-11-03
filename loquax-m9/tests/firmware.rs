//! What the M9's first image shows, proven on the host (slice four, 1361,
//! `handheld-messenger.md`, "Slice four, host half"): the fixture model
//! the board starts with ([`loquax_m9::fixture`]), each of its screens
//! (the two of slice four, the group's seven of slice six, 1366) walked to
//! with the board's keys, drawn by the board's renderer, dumped
//! as the board dumps it ([`loquax_m9::dump`]), read back and compared in
//! bytes with a PNG golden under `tests/golden/`. The rig compares the
//! board's dump with the same files. The census the board prints runs over
//! the same model in `loquax-android/tests/m9_fixture_census.rs`.
//!
//! `LOQUAX_WRITE_GOLDEN=1` writes the goldens instead, for a change that
//! means to move them.

mod support;

use support::{check, through_dump};

use loquax::ui::{update, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::fixture::{Screen, SCREENS};
use loquax_m9::frame::{Frame, WIDTH};
use loquax_m9::keys::event;
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look};

/// What `model` shows, drawn by a fresh renderer into a fresh frame.
fn drawn(model: &UiModel<Field>) -> Frame {
    drawn_at(model, 0)
}

/// What `model` shows with the M9's cursor over a group's rows on `row`.
fn drawn_at(model: &UiModel<Field>, row: usize) -> Frame {
    let mut frame = Frame::new();
    Renderer::new(&mut frame)
        .expect("renderer")
        .draw(
            &screen::scene(model),
            &Look {
                row,
                ..Look::of(model)
            },
        )
        .expect("draw");
    frame
}

/// The golden of `screen`: what the rig compares the board's dump with.
fn golden_name(screen: Screen) -> String {
    format!("m9-fw-{}.png", screen.name())
}

#[test]
fn the_boards_screens_through_the_dump_are_the_golden_frames() {
    for screen in SCREENS {
        let (model, row) = screen.walk();
        let frame = drawn_at(&model, row);
        let (_, bytes) = through_dump(&frame);
        println!("{}: dump {bytes} B", screen.name());
        check(&golden_name(screen), &frame);
    }
    // The positive control: the walk matters, the two screens differ.
    assert_ne!(
        drawn(&Screen::Chats.model()).pixels(),
        drawn(&Screen::Chat.model()).pixels()
    );
}

#[test]
fn a_screen_byte_names_the_screen_and_nothing_else() {
    for screen in SCREENS {
        assert_eq!(Screen::of(screen.byte()), Some(screen));
    }
    assert_eq!(Screen::of(SCREENS.len() as u8), None);
}

/// The board's loop: one renderer over one frame, a key, a draw, a flush
/// of the band the draw touched. What it shows after the walk is what a
/// fresh draw shows, and the band covers every row that changed.
#[test]
fn the_boards_redraw_and_its_band_equal_a_fresh_draw() {
    let mut model = Screen::Chats.model();
    let mut panel = Frame::new();
    let mut shown = Frame::new();
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
        .draw(&screen::scene(&model), &Look::of(&model))
        .expect("draw");
    flush(&renderer, &mut shown);
    for key in [
        loquax_m9::keys::M9Key::Down,
        loquax_m9::keys::M9Key::Down,
        loquax_m9::keys::M9Key::Up,
        loquax_m9::keys::M9Key::Enter,
    ] {
        update(&mut model, UiEvent::Key(event(key)));
        renderer
            .draw(&screen::scene(&model), &Look::of(&model))
            .expect("draw");
        flush(&renderer, &mut shown);
    }
    assert_eq!(shown.pixels(), drawn(&model).pixels());
}
