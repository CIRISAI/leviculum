//! The browser's core: the model, [`update()`], the [`Effect`]s, and the
//! browser's own input and geometry types.
//!
//! The rule this module exists for (`docs/src/concepts/loquax-architecture.md`,
//! "the core must not know it has a terminal", Codeberg #219): nothing in here
//! imports `crossterm`, `ratatui` or any other terminal crate. Events come in
//! as [`AppEvent`], effects go out as [`Effect`], and a frontend is a
//! translator and a renderer over that pair. The terminal frontend is
//! [`crate::tui`]; a GUI is a second one, with no change here. `just
//! lnomad-core-no-tui` builds this module without the terminal dependency tree
//! and fails if any of it creeps back in.
//!
//! Nor does it need `std`: without lnomad's `std` feature the crate is
//! `no_std` plus `alloc` and this module is most of what is left (slice 4a of
//! `docs/src/concepts/lnomad-everywhere.md`; `just lnomad-core-no-std` builds
//! it for the firmware triples). Files, the clock and the text editor are the
//! platform's: the paths of the stores are `std` fields of the model, the
//! wall clock is [`Model::clock`] when no system clock exists, and a text
//! field is an [`input::Field`].
//!
//! The one thing still shaped by the terminal is the unit: positions and
//! rectangles are character cells, and the click and hover hit-tests run here
//! against the cell layout of the chrome ([`chrome`]). A pixel frontend will
//! want semantic events (follow this link, press this control) beside the
//! cell-addressed mouse ones.

pub mod chrome;
pub mod event;
pub mod geometry;
pub mod input;
pub mod intent;
pub mod keys;
pub mod model;
pub mod scene;
pub mod strings;

pub use chrome::*;
pub use event::*;
pub use geometry::Rect;
pub use input::{
    field_with, Field, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
pub use intent::Intent;
/// A form field's kind, for a host that hands fields to its platform (1650).
pub use leviculum_micron::FieldKind;
pub use model::*;
