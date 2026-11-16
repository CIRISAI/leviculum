//! The browser's key and mouse events: the shared, terminal-free ones of
//! `leviculum-ui` (moved there for loquax, 984), re-exported so the core and
//! its tests keep naming them here.

pub use leviculum_ui::input::*;

/// The text field the core holds: the address bar, the in-page search and
/// each text field of a form, read and written through
/// [`InputBuffer`] only. With `std` it is
/// the terminal's editor, `tui-input`, edited by the shared table
/// ([`leviculum_ui::input::line_request`]); without it the platform brings
/// its own text field and hands the text in, as the scene's address field
/// already assumes, so the field is a plain
/// [`Line`].
#[cfg(feature = "std")]
pub type Field = tui_input::Input;
/// The text field the core holds; see the `std` build's.
#[cfg(not(feature = "std"))]
pub type Field = leviculum_ui::input::Line;

/// A field holding `text`, the cursor at its end.
pub fn field_with(text: &str) -> Field {
    let mut field = Field::default();
    field.set_value(text);
    field
}
