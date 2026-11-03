//! Loquax on the ThinkNode M9 (#232, #473): the scene the phone, the window
//! and the terminal draw, drawn in cells through mousefood into the M9's
//! 320 x 240 RGB565 panel, and the M9's key table over the terminal's
//! resolver.
//!
//! Slice one of `docs/src/concepts/handheld-messenger.md`, "The M9
//! frontend: the plan, measured": the renderer as a crate the firmware and
//! the host both build, `no_std` plus `alloc`. The host draws the same
//! frames into [`frame::Frame`] and pins them as PNG goldens
//! (`tests/golden.rs`); the board will draw them into its panel.
//!
//! - [`screen`]: the chats list scene as styled lines, 53 x 24 cells, the
//!   delivery marks on their ASCII ladder (the atlas has no tick).
//! - [`render`]: the lines through ratatui into any `DrawTarget` of
//!   `Rgb565`, with mousefood as the backend.
//! - [`frame`]: the panel's frame on the heap, what the host draws into.
//! - [`keys`]: which M9 key is which key event, for the list's rows.

#![no_std]

extern crate alloc;

pub mod frame;
pub mod keys;
pub mod render;
pub mod screen;
