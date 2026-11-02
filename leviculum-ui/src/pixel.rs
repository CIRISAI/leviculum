//! The chrome colours of a pixel screen, `0xaarrggbb`, that every program
//! with a window or a phone screen draws its bars, panels and words in:
//! one table, so the suite's windows look like one suite
//! (`docs/src/concepts/lnomad-everywhere.md`, slice 2).
//!
//! The values are Loquax's (`docs/src/concepts/loquax-ui.md`, "Pixel
//! screens"), whose own table (`loquax::ui::chat::LIGHT`) takes these
//! fields from here and adds its bubbles and marks; its contrast tests
//! hold every pair. A program's content keeps its own colours: a page's
//! inks and NomadNet's link colour stay lnomad's ([`crate::theme`]).

/// The chrome's roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    /// The screen's ground.
    pub background: u32,
    pub text: u32,
    pub text_secondary: u32,
    /// Links and actions of the chrome, an unread time.
    pub accent: u32,
    /// The top bar, a field's ground.
    pub bar: u32,
    /// A panel over the screen: the reactions, the help.
    pub panel: u32,
    /// A destructive or failed word on the panel.
    pub failed_on_panel: u32,
    /// A selection's ground, translucent over what it selects.
    pub selected_row: u32,
    /// The count badge and its words.
    pub badge: u32,
    pub badge_text: u32,
}

/// The light theme's chrome.
pub const LIGHT: Chrome = Chrome {
    background: 0xffffffff,
    text: 0xff1b1b1d,
    text_secondary: 0xff5c5f66,
    accent: 0xff3a7d44,
    bar: 0xfff1f2f4,
    panel: 0xffffffff,
    failed_on_panel: 0xffba1a1a,
    selected_row: 0x403a7d44,
    badge: 0xff2e6b38,
    badge_text: 0xffffffff,
};

/// The dark theme's chrome.
pub const DARK: Chrome = Chrome {
    background: 0xff17181b,
    text: 0xffececec,
    text_secondary: 0xffa9abb0,
    accent: 0xff81c784,
    bar: 0xff26272b,
    panel: 0xff2c2d31,
    failed_on_panel: 0xffffb4ab,
    selected_row: 0x4081c784,
    badge: 0xff3b6b45,
    badge_text: 0xffffffff,
};

/// The chrome of `theme`.
pub fn chrome(theme: crate::theme::Theme) -> Chrome {
    match theme {
        crate::theme::Theme::Light => LIGHT,
        crate::theme::Theme::Dark => DARK,
    }
}
