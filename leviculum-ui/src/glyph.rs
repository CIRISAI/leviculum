//! A glyph drawn as lines on a square grid: Loquax's delivery marks
//! (`docs/src/concepts/loquax-read-receipts.md`, section 6), as data, so a
//! pixel screen's widgets (leviculum-ui-iced, the phone's `Glyph.java`)
//! draw a shape without knowing which mark it is.

/// The square the marks are drawn on, in units; a screen scales it to the
/// mark's size (16 dp in a bubble and in the list).
pub const GRID: f32 = 24.0;

/// A circle on the [`GRID`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    pub x: f32,
    pub y: f32,
    pub r: f32,
}

/// A mark as lines on the [`GRID`], drawn in this order: the disc filled in
/// the failed ground, the ring and the lines stroked in `width` with round
/// caps and joins, the dot filled. The ring, the lines and the dot are in
/// the mark's colour, the failed mark's sign for the failed mark. The
/// phone's `Glyph.java` draws the same numbers, which a test of the facade
/// reads out of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub disc: Option<Circle>,
    pub ring: Option<Circle>,
    pub lines: &'static [&'static [(f32, f32)]],
    pub width: f32,
    pub dot: Option<Circle>,
}
