//! `leviculum-ui-ratatui`: the ratatui and crossterm half of the UI that
//! lnomad and loquax share (Codeberg #475), over the terminal-free model of
//! `leviculum-ui`.
//!
//! The widgets every keyboard program here draws the same way: the chrome
//! styles and the colour-depth post-pass ([`style`]), the bridge from the
//! styled text model to ratatui text, the spinner span, the toast, the help
//! overlay, the status chip, the footer strip, and inline pictures behind
//! the `images` feature. And the shell around them: the terminal lifecycle
//! with its restore guard and panic hook ([`terminal`]), crossterm's events
//! as the programs' own ([`event`]), and the background query behind
//! `--theme auto` ([`background`]).
//!
//! A program keeps its own labels, its screen layout and its state; these
//! take them as arguments.
//!
//! Without the default `terminal` feature only [`style`] is left, `no_std`
//! plus `alloc`: the bridge from styled lines to ratatui text that a board
//! draws through mousefood as the terminal draws through crossterm.

#![cfg_attr(not(feature = "terminal"), no_std)]

extern crate alloc;

#[cfg(feature = "terminal")]
pub mod background;
#[cfg(feature = "terminal")]
pub mod chip;
#[cfg(feature = "terminal")]
pub mod event;
#[cfg(feature = "terminal")]
pub mod footer;
#[cfg(feature = "terminal")]
pub mod geometry;
#[cfg(feature = "terminal")]
pub mod help;
#[cfg(feature = "images")]
pub mod images;
#[cfg(feature = "terminal")]
pub mod spinner;
pub mod style;
#[cfg(feature = "terminal")]
pub mod terminal;
#[cfg(feature = "terminal")]
pub mod toast;

pub use style::to_ratatui_text;
#[cfg(feature = "terminal")]
pub use terminal::{CrosstermTerminal, TerminalGuard, TerminalOps};
