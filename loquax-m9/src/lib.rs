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
//! - [`screen`]: the scene as styled lines, 53 x 24 cells, the chats list
//!   and the delivery marks on their ASCII ladder (the atlas has no tick),
//!   and which scene and cursor a model shows.
//! - [`chat`]: the open chat under its bar, the bubbles and the compose
//!   line (slice two, 1356).
//! - [`field`]: the compose field the M9's keys edit.
//! - [`render`]: the lines through ratatui into any `DrawTarget` of
//!   `Rgb565`, with mousefood as the backend.
//! - [`frame`]: the panel's frame on the heap, what the host draws into.
//! - [`keyboard`]: the matrix's scan frames to key events: debounce,
//!   the Shift and Sym layers, repeat (slice three, 1359).
//! - [`keys`]: which M9 key is which key event, the list's rows and the
//!   chat's.
//! - [`kbraw`]: the keyboard controller's I2C answer, logged raw and read
//!   into a scan by a table (slice four, 1361).
//! - [`dump`]: the frame as text lines over the debug link, and read back,
//!   for the rig to compare with the goldens (slice four).
//! - [`fixture`]: the model the board shows before it has a network.
//! - [`pair`]: the home code typed on the keys, and the join it makes
//!   (slice five, 1363).
//! - [`group`]: a group's screens, the member sheet and "New group", the
//!   keys that run their verbs, and what the board asks its home for each
//!   (slice six, 1366).
//! - [`census`]: what a screen costs where it is drawn, and its budget.

#![no_std]

extern crate alloc;

pub mod census;
pub mod chat;
pub mod dump;
pub mod field;
pub mod fixture;
pub mod frame;
pub mod group;
pub mod kbraw;
pub mod keyboard;
pub mod keys;
pub mod pair;
pub mod render;
pub mod screen;
