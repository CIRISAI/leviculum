//! What the M9 shows from power-on until the UI's first frame (1485): the
//! logo on the panel's ground with "Leviculum Loquax" under it, and the
//! rule that the backlight never lights a panel nothing has been written
//! to.
//!
//! The ST7789's memory holds noise after its reset, so a backlight lit at
//! the end of the panel's init showed that noise for the boot's two
//! seconds (Lew, 2026-11-07). [`power_on`] is the order the board keeps:
//! the splash drawn into the frame, the whole frame written, and only then
//! the backlight on. The splash stays on the panel while the board runs
//! its census and restores its store (1476): nothing flushes until the
//! UI's first redraw, which replaces it.
//!
//! The logo is one asset, [`LOGO`]: `loquax-m9/assets/splash-logo.rgba`,
//! written by `loquax-android/scripts/render-icon.py` from the same
//! geometry as every other picture of the jackdaw, so the next logo
//! replaces this one at the script's next run. Raw RGBA, 8 bits a channel,
//! not premultiplied, [`LOGO_PX`] square, row by row: the board has no PNG
//! decoder, and the alpha lets the picture lie on whatever the ground is.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::{Rgb565, Rgb888};
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use mousefood::ColorTheme;

use crate::font::CELL_PX;
use crate::frame::{Frame, HEIGHT, WIDTH};

/// The logo's side in panel pixels.
pub const LOGO_PX: u32 = 112;

/// The logo, raw RGBA (see the module's head).
pub const LOGO: &[u8] = include_bytes!("../assets/splash-logo.rgba");

const _: () = assert!(LOGO.len() == (LOGO_PX * LOGO_PX * 4) as usize);

/// The words under the logo, one line: the suite's name and the app's
/// (Lew, 2026-11-07). Sixteen cells of the atlas are 160 of the panel's
/// 320 pixels, so the two stay on one line at the UI's size. No version,
/// no hash: the splash names the app.
pub const WORD: &str = "Leviculum Loquax";

/// Pixels between the logo and the word.
const GAP: u32 = 12;

/// The logo's top-left corner: logo, gap and one cell of text, centred.
pub fn logo_at() -> Point {
    let block = LOGO_PX + GAP + u32::from(CELL_PX.1);
    Point::new(
        ((WIDTH - LOGO_PX) / 2) as i32,
        ((HEIGHT - block) / 2) as i32,
    )
}

/// The panel's ground and ink: the theme the cells are drawn in, so the
/// splash is the colour the UI then draws over it.
fn theme() -> (Rgb565, Rgb565) {
    let theme = ColorTheme::default();
    (theme.background.into(), theme.foreground.into())
}

/// `over` laid on `ground` at coverage `alpha` out of 255.
fn blend(over: u8, ground: u8, alpha: u8) -> u8 {
    let (o, g, a) = (u32::from(over), u32::from(ground), u32::from(alpha));
    ((o * a + g * (255 - a) + 127) / 255) as u8
}

/// The splash, drawn into the whole of `frame`.
pub fn draw(frame: &mut Frame) {
    let (ground, ink) = theme();
    let ground888 = Rgb888::from(ground);
    let _ = frame.clear(ground);
    let at = logo_at();
    let pixels = LOGO.chunks_exact(4).enumerate().map(|(i, px)| {
        let colour = Rgb888::new(
            blend(px[0], ground888.r(), px[3]),
            blend(px[1], ground888.g(), px[3]),
            blend(px[2], ground888.b(), px[3]),
        );
        let (x, y) = (i as u32 % LOGO_PX, i as u32 / LOGO_PX);
        Pixel(at + Point::new(x as i32, y as i32), Rgb565::from(colour))
    });
    let _ = frame.draw_iter(pixels);
    let font = crate::font::atlas();
    let style = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    let _ = Text::with_text_style(
        WORD,
        Point::new((WIDTH / 2) as i32, at.y + (LOGO_PX + GAP) as i32),
        MonoTextStyle::new(&font, ink),
        style,
    )
    .draw(frame);
}

/// The panel as the boot order sees it: rows written, the backlight
/// switched. The board's ST7789 is one; the host's test panel records.
pub trait Panel {
    type Error;
    /// Write rows `first..=last` of `frame`, full width.
    fn write(&mut self, frame: &Frame, first: u32, last: u32) -> Result<(), Self::Error>;
    /// Switch the backlight.
    fn light(&mut self, on: bool);
}

/// Power-on (1485): the splash drawn into `frame`, all of it written to
/// `panel`, then the backlight on. A write that fails leaves the backlight
/// off and the frame whole to write again, so the first redraw that does
/// reach the panel sends every row; the board lights the backlight after
/// that one ([`Panel::light`] only ever follows a written frame).
pub fn power_on<P: Panel>(panel: &mut P, frame: &mut Frame) -> Result<(), P::Error> {
    draw(frame);
    let (first, last) = frame.take_dirty().unwrap_or((0, HEIGHT - 1));
    if let Err(e) = panel.write(frame, first, last) {
        frame.forget_flushed();
        return Err(e);
    }
    panel.light(true);
    Ok(())
}

/// What a panel saw, in order: the record the boot rule is checked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Write(u32, u32),
    Light(bool),
}

/// Whether `events` keep the rule: the backlight is never on while a row
/// of the panel has not been written since power-on. `Err` names the
/// first event that breaks it.
pub fn lights_only_written(events: &[Event]) -> Result<(), usize> {
    let mut written = [false; HEIGHT as usize];
    for (i, event) in events.iter().enumerate() {
        match *event {
            Event::Write(first, last) => {
                for row in first..=last.min(HEIGHT - 1) {
                    written[row as usize] = true;
                }
            }
            Event::Light(true) if written.iter().any(|w| !w) => return Err(i),
            Event::Light(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_and_the_word_fit_the_panel() {
        let at = logo_at();
        assert!(at.x >= 0 && at.y >= 0);
        let bottom = at.y as u32 + LOGO_PX + GAP + u32::from(CELL_PX.1);
        assert!(bottom <= HEIGHT, "{bottom}");
        assert!(WORD.len() as u32 * u32::from(CELL_PX.0) <= WIDTH);
    }

    #[test]
    fn a_transparent_pixel_is_the_ground_and_an_opaque_one_itself() {
        assert_eq!(blend(200, 10, 0), 10);
        assert_eq!(blend(200, 10, 255), 200);
        assert_eq!(blend(255, 0, 128), 128);
    }
}
