//! The M9's power-on (1485): the splash the panel shows until the UI's
//! first frame, pinned as `tests/golden/m9-fw-splash.png` through the
//! board's dump, and the order the board keeps: backlight off, the whole
//! panel written, backlight on. A panel lit before its first write shows
//! the ST7789's noise, which is what Lew saw for two seconds at every
//! power-on.
//!
//! `LOQUAX_WRITE_GOLDEN=1` writes the golden instead.

mod support;

use support::check;

use loquax_m9::fixture::Screen;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::map::{Map, FIXTURE};
use loquax_m9::render::Renderer;
use loquax_m9::screen::Look;
use loquax_m9::splash::{self, lights_only_written, Event, Panel, LOGO, LOGO_PX};

/// A panel that records what reaches it, and refuses writes on request.
#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
    refuse: bool,
}

impl Panel for Recorder {
    type Error = ();

    fn write(&mut self, _frame: &Frame, first: u32, last: u32) -> Result<(), ()> {
        if self.refuse {
            return Err(());
        }
        self.events.push(Event::Write(first, last));
        Ok(())
    }

    fn light(&mut self, on: bool) {
        self.events.push(Event::Light(on));
    }
}

#[test]
fn the_splash_through_the_dump_is_its_golden() {
    let mut frame = Frame::new();
    splash::draw(&mut frame);
    check("m9-fw-splash.png", &frame);
    // The logo is there: its centre is the jackdaw's ground, not the
    // panel's, and a corner of the panel is the ground.
    let at = splash::logo_at();
    let centre = (at.x as u32 + LOGO_PX / 2, at.y as u32 + LOGO_PX / 2);
    assert_ne!(frame.pixel(centre.0, centre.1), frame.pixel(0, 0));
    assert_eq!(frame.pixel(0, 0), frame.pixel(WIDTH - 1, HEIGHT - 1));
    assert_eq!(LOGO.len(), (LOGO_PX * LOGO_PX * 4) as usize);
}

/// Off, the whole panel written, on: the backlight follows the write.
#[test]
fn power_on_writes_every_row_before_the_backlight() {
    let mut panel = Recorder::default();
    let mut frame = Frame::new();
    splash::power_on(&mut panel, &mut frame).expect("power on");
    assert_eq!(
        panel.events,
        [Event::Write(0, HEIGHT - 1), Event::Light(true)]
    );
    assert_eq!(lights_only_written(&panel.events), Ok(()));
    // What was written is the splash.
    let mut want = Frame::new();
    splash::draw(&mut want);
    assert_eq!(frame.pixels(), want.pixels());
}

/// The positive control: the order the image had before 1485, the
/// backlight on before any row, and a light after half a panel, are both
/// caught; a light after the whole panel is not.
#[test]
fn a_backlight_lit_before_the_write_goes_red() {
    assert_eq!(
        lights_only_written(&[Event::Light(true), Event::Write(0, HEIGHT - 1)]),
        Err(0)
    );
    assert_eq!(
        lights_only_written(&[Event::Write(0, 119), Event::Light(true)]),
        Err(1)
    );
    assert_eq!(
        lights_only_written(&[
            Event::Light(false),
            Event::Write(0, 119),
            Event::Write(120, HEIGHT - 1),
            Event::Light(true),
        ]),
        Ok(())
    );
}

/// A write that fails leaves the backlight off and the frame whole to
/// write again: the first redraw that reaches the panel sends every row.
#[test]
fn a_failed_write_keeps_the_backlight_off() {
    let mut panel = Recorder {
        refuse: true,
        ..Recorder::default()
    };
    let mut frame = Frame::new();
    assert!(splash::power_on(&mut panel, &mut frame).is_err());
    assert_eq!(panel.events, []);
    assert_eq!(frame.take_dirty(), Some((0, HEIGHT - 1)));
}

/// The splash stays on the panel until the UI's first frame, and that
/// frame is the UI whole: the renderer made over the splash starts from
/// the ground, so no blank cell keeps a piece of the logo.
#[test]
fn the_first_frame_over_the_splash_is_the_ui_whole() {
    let model = Screen::Chats.model();
    let mut map = Map::new(FIXTURE).expect("the fixture region");
    let mut panel = Recorder::default();
    let mut frame = Frame::new();
    splash::power_on(&mut panel, &mut frame).expect("power on");
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    renderer
        .show(&model, &Look::of(&model), &mut map)
        .expect("draw");
    let band = renderer.panel().take_dirty();
    drop(renderer);
    let mut fresh = Frame::new();
    Renderer::new(&mut fresh)
        .expect("renderer")
        .show(&model, &Look::of(&model), &mut map)
        .expect("draw");
    assert_eq!(frame.pixels(), fresh.pixels());
    // Every row the splash inked is sent with the first frame.
    let at = splash::logo_at();
    let (first, last) = band.expect("the first frame flushes");
    assert!(first <= at.y as u32, "{first}");
    assert!(last >= at.y as u32 + LOGO_PX + 12 + 19, "{last}");
}
