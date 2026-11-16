//! Colours: a token as iced's colour, and the roles the shared widgets
//! paint with.

use iced::Color;

/// A token, `0xaarrggbb`, as iced's colour.
pub fn colour(argb: u32) -> Color {
    let [a, r, g, b] = argb.to_be_bytes();
    Color::from_rgba8(r, g, b, f32::from(a) / 255.0)
}

/// An `0xrrggbb` as an opaque colour.
pub fn rgb(value: u32) -> Color {
    colour(0xff00_0000 | value)
}

/// The roles the shared widgets paint with, filled by each program from
/// its tokens (Loquax's `loquax::ui::chat::Colours`, whose chrome is the
/// suite's; lnomad's window takes the suite's chrome itself, [`Palette::of`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The screen's ground.
    pub background: Color,
    /// The bars' ground, a hovered row, a field's ground.
    pub bar: Color,
    /// A tooltip's and a toast's ground.
    pub panel: Color,
    pub text: Color,
    pub text_secondary: Color,
    /// Links, icon actions, the active tab.
    pub accent: Color,
    /// The selected row's and the active tab's ground, a selection.
    pub selected_row: Color,
    pub badge: Color,
    pub badge_text: Color,
}

impl Palette {
    /// The roles from the suite's chrome ([`leviculum_ui::pixel`]), as a
    /// program without tokens of its own fills them (lnomad's window).
    pub fn of(c: &leviculum_ui::pixel::Chrome) -> Palette {
        Palette {
            background: colour(c.background),
            bar: colour(c.bar),
            panel: colour(c.panel),
            text: colour(c.text),
            text_secondary: colour(c.text_secondary),
            accent: colour(c.accent),
            selected_row: colour(c.selected_row),
            badge: colour(c.badge),
            badge_text: colour(c.badge_text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_its_bytes() {
        assert_eq!(
            colour(0x80ff_2000),
            Color::from_rgba8(0xff, 0x20, 0x00, 128.0 / 255.0)
        );
        assert_eq!(rgb(0x00_10_20), Color::from_rgb8(0x00, 0x10, 0x20));
    }
}
