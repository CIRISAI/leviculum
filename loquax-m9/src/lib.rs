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
//! - [`font`]: the 10 x 20 atlas the cells are set in, 32 x 12 of them,
//!   and what a cell measures on the panel (1428).
//! - [`screen`]: the scene as styled lines, 32 x 12 cells, the chats list
//!   and the delivery marks on their ASCII ladder (the atlas has no tick),
//!   and which scene and cursor a model shows.
//! - [`chat`]: the open chat under its bar, the bubbles and the compose
//!   line (slice two, 1356).
//! - [`field`]: the compose field the M9's keys edit.
//! - [`render`]: the lines through ratatui into any `DrawTarget` of
//!   `Rgb565`, with mousefood as the backend.
//! - [`frame`]: the panel's frame on the heap, what the host draws into.
//! - [`keyboard`]: the controller's key codes to key events, and the
//!   `KEY` line that names each press (slice three, 1359; the measured
//!   protocol, 1427).
//! - [`keys`]: which M9 key is which key event, the list's rows and the
//!   chat's.
//! - [`kbraw`]: the keyboard controller's I2C answer, logged raw and read
//!   as idle or one key code (slice four, 1361; measured, 1427).
//! - [`dump`]: the frame as text lines over the debug link, and read back,
//!   for the rig to compare with the goldens (slice four).
//! - [`fixture`]: the model the board shows before it has a network.
//! - [`pair`]: the home code typed on the keys, and the join it makes
//!   (slice five, 1363).
//! - [`group`]: a group's screens, the member sheet and "New group", the
//!   keys that run their verbs, and what the board asks its home for each
//!   (slice six, 1366).
//! - [`map`]: the model's map drawn in pixels under the bar, its pins and
//!   legend in the atlas, and the keys the M9 has none for (slice seven,
//!   1375).
//! - [`splash`]: the logo from power-on until the UI's first frame, and
//!   the order that lights the backlight only over a written panel (1485).
//! - [`census`]: what a screen costs where it is drawn, and its budget.

#![no_std]

extern crate alloc;

pub mod census;
pub mod chat;
pub mod dump;
pub mod field;
pub mod fixture;
pub mod font;
pub mod frame;
pub mod group;
pub mod home;
pub mod kbraw;
pub mod keyboard;
pub mod keys;
pub mod map;
pub mod page;
pub mod pair;
pub mod render;
pub mod screen;
pub mod splash;

/// The language the board speaks (1447, walk 1 finding 23): German unless
/// the build says otherwise with `LOQUAX_M9_LANG=en`. The board has no
/// locale to read, and an English "You:" over a German message is what a
/// German reader saw.
pub const LANG: loquax::ui::strings::Lang = match option_env!("LOQUAX_M9_LANG") {
    Some(tag) if tag.len() >= 2 && tag.as_bytes()[0] == b'e' && tag.as_bytes()[1] == b'n' => {
        loquax::ui::strings::Lang::En
    }
    _ => loquax::ui::strings::Lang::De,
};
