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
//! field ([`chrome`]). A program keeps its own nodes, words and layout;
//! these take them as arguments, coloured by a [`Palette`] each program
//! fills from its own tokens.
//!
//! Generic over the program's message: a press is the `M` it was given.

pub mod avatar;
pub mod chrome;
pub mod control;
pub mod icon;
pub mod paint;
pub mod wave;

pub use icon::{drawing, Icon};
pub use paint::{colour, rgb, Palette};
