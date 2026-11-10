//! `leviculum-ui-iced`: the iced half of the UI the Leviculum window
//! programs share (Codeberg #475), the twin of `leviculum-ui-ratatui`,
//! extracted from Loquax's window (`loquax/src/gui/chat.rs`, 1145, 1218,
//! 1270, 1276) so that lnomad's window draws with the same controls
//! (`docs/src/concepts/lnomad-everywhere.md`, slice 2).
//!
//! The widgets every window here draws the same way: the drawn icons
//! ([`icon`]), the waveform ([`wave`]), the bare and the link buttons and a
//! bar's icon action ([`control`]), an avatar ([`avatar`]), the top bar,
//! a count badge, the tabs' rail or bar, the list item and the search
//! field ([`chrome`]), the help overlay ([`help`]). A program keeps its own nodes, words and layout;
//! these take them as arguments, coloured by a [`Palette`] each program
//! fills from its own tokens. A key as the keymap's ([`key`]). And a view without a window ([`headless`]):
//! drawn into pixels for the goldens, clicked for the tests. The desktop's
//! light or dark, which iced alone does not read on Linux ([`desktop`]).
//! The suite's window icon ([`window_icon`]). A list whose wheel scrolls
//! softly ([`smooth`]). A list that lays out only the rows on screen
//! ([`rows`]). The window's frame meter ([`fps`]). Text that can be
//! selected ([`select`]).
//!
//! Generic over the program's message: a press is the `M` it was given.
//!
//! Empty on a musl target, where the window's crates do not link (see the
//! manifest).

// On musl the crate is empty and the links above name nothing; rustdoc
// still reads the crate's doc there (`just doc-touched`).
#![cfg_attr(target_env = "musl", allow(rustdoc::broken_intra_doc_links))]
#![cfg(not(target_env = "musl"))]

pub mod avatar;
pub mod chrome;
pub mod control;
pub mod desktop;
pub mod fps;
pub mod headless;
pub mod help;
pub mod icon;
pub mod key;
pub mod paint;
pub mod rows;
pub mod select;
pub mod smooth;
pub mod wave;
pub mod window_icon;

#[cfg(test)]
mod fast_paths;

pub use icon::{drawing, Icon};
pub use paint::{colour, rgb, Palette};
pub use window_icon::window_icon;
