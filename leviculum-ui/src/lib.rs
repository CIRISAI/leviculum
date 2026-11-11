//! `leviculum-ui`: the terminal-free half of the UI that lnomad and loquax
//! share (Codeberg #475, `docs/src/concepts/handheld-messenger.md`, "One
//! interaction model").
//!
//! Code is shared where it is the same problem: a styled text model, one
//! palette and its colour maths, the colour tokens with their tier ladder,
//! the programs' own key and mouse events with the text-editing table over
//! them, the keymap table every reader of a
//! key takes its chords from, the chrome models (footer strip, help
//! overlay, toast), the loading spinner, the delivery glyph table of
//! `docs/src/concepts/loquax-ui.md`, the OSC 52 clipboard sequence, and the
//! rule for following a link, with the finder for links in a message's
//! text. The scene's shared vocabulary is here too: its nodes, their
//! accessibility and actions, and the keyed catalogue of words they say
//! (`scene`, `strings`, the `catalogue!` macro), generic over a program's
//! intents and keys. No terminal crate is named here, which is what lets a
//! program's core take these types and stay terminal-free;
//! `just ui-no-terminal` checks it. The ratatui widgets over this model are
//! `leviculum-ui-ratatui`.
//!
//! What is NOT here: a program's own catalogue, intents and node ids, its
//! own labels and actions (lnomad's footer buttons and help text stay in
//! lnomad), its layout of the screen, and its state.
//!
//! Without the default `std` feature the crate is `no_std` plus `alloc`: a
//! messenger core built for a board takes these types too (loquax's `ui`
//! model, checked by `just loquax-core-no-std`).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod clipboard;
pub mod color;
pub mod commands;
pub mod delivery;
pub mod footer;
pub mod geometry;
pub mod glyph;
pub mod help;
pub mod input;
pub mod keymap;
pub mod link;
pub mod pixel;
pub mod read;
pub mod scene;
pub mod spinner;
pub mod strings;
pub mod style;
pub mod text;
pub mod theme;
pub mod toast;
pub mod tokens;

pub use color::{resolve_tier, rgb_to_ansi256, ColorDepth, ColorFlag, Ink, Tier, TierEnv};
pub use geometry::Rect;
pub use style::{RLine, RStyle, StyledChar};
pub use theme::{resolve_theme, Bg, Theme, ThemeFlag};
pub use tokens::{tokens, Paint, Tokens};
