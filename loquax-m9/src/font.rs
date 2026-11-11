//! The atlas the M9 sets its cells in, and what one cell measures on the
//! panel (1428).
//!
//! The panel is a 2.4-inch ST7789 at 320 x 240 (`thinknode-m9.md`): 400
//! pixels on the diagonal over 60.96 mm, 0.1524 mm a pixel, about 167
//! dpi. Mousefood's first atlas, the Unicode 6 x 10, set a capital in
//! 7 pixels (1.07 mm) and an x in 5 (0.76 mm) on a 53 x 24 grid: too small
//! to read at arm's length (Lew at the board, 2026-11-04). The atlases
//! `embedded-graphics-unicodefonts` 0.2.0 ships `no_std`, monospaced and
//! with the umlauts, measured on the host (x height, capital height):
//!
//! | atlas | grid | x | capital | cell height |
//! |---|---|---|---|---|
//! | 6 x 10 | 53 x 24 | 5 px, 0.76 mm | 7 px, 1.07 mm | 1.52 mm |
//! | 7 x 14 | 45 x 17 | 7 px, 1.07 mm | 10 px, 1.52 mm | 2.13 mm |
//! | 8 x 13 | 40 x 18 | 6 px, 0.91 mm | 9 px, 1.37 mm | 1.98 mm |
//! | 9 x 15 | 35 x 16 | 7 px, 1.07 mm | 10 px, 1.52 mm | 2.29 mm |
//! | 9 x 18 | 35 x 13 | 7 px, 1.07 mm | 10 px, 1.52 mm | 2.74 mm |
//! | 10 x 20 | 32 x 12 | 8 px, 1.22 mm | 13 px, 1.98 mm | 3.05 mm |
//!
//! No atlas there has an x of 2.4 mm (it would need one near 16 pixels,
//! a 30-pixel cell); the 10 x 20 is the largest, the only one whose
//! capital is near twice the old one's, and the only one that tiles the
//! panel without a pixel over. It has no 8 x 16 and no 12 x 24.
//! The cost: 130,400 B of glyphs in flash against the 6 x 10's 12,000 B.

//!
//! Two sizes since 1532 ([`Size`]): Lew at the board on 2026-11-07 found
//! the 10 x 20 too large to hold what matters on a screen, and asked for
//! a smaller default and an easy switch. The smaller atlas is measured
//! the same way, against every glyph the board draws (the delivery
//! ladder's ✓ and ◷, the selection bar ▌, the ellipsis, the umlauts):
//!
//! | atlas | grid | x | capital | Ä | lacks |
//! |---|---|---|---|---|---|
//! | 6 x 10 | 53 x 24 | 5 px | 7 px | 8 px | ✓ ◷ |
//! | 6 x 12 | 53 x 20 | 5 px | 7 px | 9 px | |
//! | 6 x 13 | 53 x 18 | 6 px | 9 px | 10 px | |
//! | 7 x 13 | 45 x 18 | 6 px | 9 px | 10 px | ✓ |
//! | 7 x 14 | 45 x 17 | 7 px | 10 px | 11 px | ✓ ◷ |
//! | 8 x 13 | 40 x 18 | 6 px | 9 px | 10 px | ✓ |
//! | 9 x 15 | 35 x 16 | 7 px, 1.07 mm | 10 px, 1.52 mm | 11 px | |
//! | 10 x 20 | 32 x 12 | 8 px, 1.22 mm | 13 px, 1.98 mm | 15 px | |
//!
//! The 6 x 10 cannot draw a delivered message's tick, and the 6 x 12 sets
//! the same 5-pixel x Lew could not read at arm's length on 2026-11-04;
//! no 7 x 12 ships. The 9 x 15 is the small size: an x one pixel below the
//! large one's, every glyph, and a third more rows (16 against 12) on a
//! panel it tiles from top to bottom (5 pixels stay blank at the right).

//!
//! Three sizes since 1561: Lew at the board on 2026-11-08 asked for a
//! third, smaller one, "sehr klein", the 6 x 12 above: every glyph the
//! board draws, an x of 5 px. He found 5 px hard at arm's length on
//! 2026-11-04 (the 6 x 10's); offering it is his choice, the default
//! stays the 9 x 15. The 6 x 12 tiles the panel's height (20 rows of 12
//! px) and leaves 2 pixels at the right (53 cells of 6 px).

use embedded_graphics::mono_font::MonoFont;

/// The three sizes the M9's cells come in (1532, 1561): the small one the
/// default, the large one the 10 x 20 of 1428, the very small one the
/// 6 x 12.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    /// The 6 x 12: 53 x 20 cells ("sehr klein", 1561).
    Tiny,
    /// The 9 x 15: 35 x 16 cells ("klein").
    #[default]
    Small,
    /// The 10 x 20: 32 x 12 cells ("groß").
    Large,
}

impl Size {
    /// What a board shows before the reader chose.
    pub const DEFAULT: Size = Size::Small;
    /// All three, smallest first: the order the settings row offers them.
    pub const ALL: [Size; 3] = [Size::Tiny, Size::Small, Size::Large];

    /// One cell's pixels, across and down.
    pub const fn cell_px(self) -> (u16, u16) {
        match self {
            Size::Tiny => (6, 12),
            Size::Small => (9, 15),
            Size::Large => CELL_PX,
        }
    }

    /// Cells across and rows on the 320 x 240 panel.
    pub const fn grid(self) -> (usize, usize) {
        let (w, h) = self.cell_px();
        (
            crate::frame::WIDTH as usize / w as usize,
            crate::frame::HEIGHT as usize / h as usize,
        )
    }

    /// The atlas, built (and leaked, as [`atlas`] says) on each call.
    pub fn atlas(self) -> MonoFont<'static> {
        match self {
            Size::Tiny => mousefood::fonts::mono_6x12_atlas(),
            Size::Small => mousefood::fonts::mono_9x15_atlas(),
            Size::Large => atlas(),
        }
    }

    /// Its place in [`Size::ALL`]: the renderer's grids and atlases are
    /// kept in that order.
    pub const fn index(self) -> usize {
        match self {
            Size::Tiny => 0,
            Size::Small => 1,
            Size::Large => 2,
        }
    }

    /// The size as the client record keeps it (`loquax::home::record`,
    /// byte 6 bits 0-1): 0 the default, 1 the large one (the 1532 flag,
    /// so a record written before 1561 reads as it was), 2 the very small
    /// one.
    pub const fn record(self) -> u8 {
        match self {
            Size::Small => 0,
            Size::Large => 1,
            Size::Tiny => 2,
        }
    }

    /// The size a record's two bits name; a value no build wrote (3) is
    /// the default.
    pub const fn of_record(bits: u8) -> Size {
        match bits & 0b11 {
            1 => Size::Large,
            2 => Size::Tiny,
            _ => Size::DEFAULT,
        }
    }

    /// Its name in a log line.
    pub const fn name(self) -> &'static str {
        match self {
            Size::Tiny => "tiny",
            Size::Small => "small",
            Size::Large => "large",
        }
    }
}

/// The large size's cell, across and down: the map's cells always.
pub const CELL_PX: (u16, u16) = (10, 20);

/// The large size's cells across: 320 px of a 10 px wide atlas, none left
/// over. The map's grid always.
pub const COLS: usize = 32;
/// The large size's cell rows: 240 px of a 20 px high atlas, none left
/// over. The map's grid always.
pub const ROWS: usize = 12;

/// The millionths of a millimetre one panel pixel measures.
pub const NM_PER_PX: u32 = 152_400;

/// The large atlas. Each call builds its lookup table and leaks it (the
/// crate's own warning); the renderer makes each size once, for as long
/// as the panel.
pub fn atlas() -> MonoFont<'static> {
    mousefood::fonts::mono_10x20_atlas()
}

#[cfg(test)]
mod tests {
    use super::Size;
    use super::*;
    use crate::frame::{HEIGHT, WIDTH};
    use alloc::string::String;
    use embedded_graphics::mock_display::MockDisplay;
    use embedded_graphics::mono_font::MonoTextStyle;
    use embedded_graphics::pixelcolor::BinaryColor;
    use embedded_graphics::prelude::*;
    use embedded_graphics::text::{Baseline, Text};

    /// The rows `text` inks, top and bottom, set at the cell's top.
    fn inked(font: &MonoFont<'_>, text: &str) -> (i32, i32) {
        let mut display: MockDisplay<BinaryColor> = MockDisplay::new();
        let style = MonoTextStyle::new(font, BinaryColor::On);
        Text::with_baseline(text, Point::zero(), style, Baseline::Top)
            .draw(&mut display)
            .expect("draws");
        let lit = display.affected_area();
        (lit.top_left.y, lit.top_left.y + lit.size.height as i32 - 1)
    }

    fn mm_x1000(px: i32) -> u32 {
        px as u32 * NM_PER_PX / 1000
    }

    /// The cell tiles the panel, and the grid is the constants'.
    #[test]
    fn the_atlas_tiles_the_panel_as_the_grid_says() {
        let font = atlas();
        let size = font.character_size;
        assert_eq!((size.width as u16, size.height as u16), CELL_PX);
        assert_eq!(WIDTH % size.width, 0);
        assert_eq!(HEIGHT % size.height, 0);
        assert_eq!((WIDTH / size.width) as usize, COLS);
        assert_eq!((HEIGHT / size.height) as usize, ROWS);
    }

    /// The type is the size the table says: an x of 8 px (1.22 mm) and a
    /// capital of 13 px (1.98 mm), against 5 and 7 px in the 6 x 10; and
    /// the umlauts are in the atlas, not its stand-in.
    #[test]
    fn the_type_is_near_twice_the_old_and_sets_umlauts() {
        let font = atlas();
        let (top, bottom) = inked(&font, "x");
        assert_eq!(bottom - top + 1, 8);
        assert_eq!(mm_x1000(bottom - top + 1), 1219);
        let (top, bottom) = inked(&font, "H");
        assert_eq!(bottom - top + 1, 13);
        assert_eq!(mm_x1000(bottom - top + 1), 1981);
        // Control: the old atlas measured the same way is the small one.
        let old = mousefood::fonts::mono_6x10_atlas();
        let (top, bottom) = inked(&old, "x");
        assert_eq!(bottom - top + 1, 5);
        let (top, bottom) = inked(&old, "H");
        assert_eq!(bottom - top + 1, 7);
        // An umlaut inks above the capital's top: its dots, not the
        // atlas's replacement box.
        let (cap, _) = inked(&font, "A");
        // Control for the lookup: a private-use point is not in it.
        let missing = font.glyph_mapping.index('\u{e000}');
        for umlaut in ['Ä', 'Ö', 'Ü', 'ß', 'ä', 'ö', 'ü'] {
            assert_ne!(font.glyph_mapping.index(umlaut), missing, "{umlaut}");
        }
        assert_ne!(font.glyph_mapping.index('A'), missing);
        let (top, _) = inked(&font, "Ä");
        assert!(top < cap, "Ä inks from {top}, A from {cap}");
    }

    /// The small atlas is the size the module's table says, has every
    /// glyph the board draws, and its grid is what [`Size::grid`] says;
    /// the 6 x 10 and the 7 x 14 lack the tick, the control.
    #[test]
    fn the_small_atlas_measures_as_the_table_and_draws_every_mark() {
        let font = Size::Small.atlas();
        let size = font.character_size;
        assert_eq!(
            (size.width as u16, size.height as u16),
            Size::Small.cell_px()
        );
        assert_eq!(Size::Small.grid(), (35, 16));
        assert_eq!(Size::Large.grid(), (COLS, ROWS));
        assert_eq!(HEIGHT % size.height, 0);
        let (top, bottom) = inked(&font, "x");
        assert_eq!(bottom - top + 1, 7);
        assert_eq!(mm_x1000(bottom - top + 1), 1066);
        let (top, bottom) = inked(&font, "H");
        assert_eq!(bottom - top + 1, 10);
        assert_eq!(mm_x1000(bottom - top + 1), 1524);
        let drawn = "ÄÖÜßäöü✓◷▌…·!";
        let lacks = |font: &MonoFont<'_>| -> String {
            let missing = font.glyph_mapping.index('\u{e000}');
            drawn
                .chars()
                .filter(|&c| font.glyph_mapping.index(c) == missing)
                .collect()
        };
        assert_eq!(lacks(&font), "");
        assert_eq!(lacks(&atlas()), "");
        assert_eq!(lacks(&mousefood::fonts::mono_6x10_atlas()), "✓◷");
        assert_eq!(lacks(&mousefood::fonts::mono_7x14_atlas()), "✓◷");
        let (cap, _) = inked(&font, "A");
        let (top, _) = inked(&font, "Ä");
        assert!(top < cap, "Ä inks from {top}, A from {cap}");
    }

    /// The very small atlas (1561) is the 6 x 12 the table says: an x of
    /// 5 px, a capital of 7, every glyph the board draws, a 53 x 20 grid.
    #[test]
    fn the_tiny_atlas_measures_as_the_table_and_draws_every_mark() {
        let font = Size::Tiny.atlas();
        let size = font.character_size;
        assert_eq!((size.width as u16, size.height as u16), (6, 12));
        assert_eq!(Size::Tiny.grid(), (53, 20));
        assert_eq!(HEIGHT % size.height, 0);
        assert_eq!(WIDTH - 53 * size.width, 2, "two pixels stay blank");
        let (top, bottom) = inked(&font, "x");
        assert_eq!(bottom - top + 1, 5);
        assert_eq!(mm_x1000(bottom - top + 1), 762);
        let (top, bottom) = inked(&font, "H");
        assert_eq!(bottom - top + 1, 7);
        let missing = font.glyph_mapping.index('\u{e000}');
        let lacks: String = "ÄÖÜßäöü✓◷▌…·!"
            .chars()
            .filter(|&c| font.glyph_mapping.index(c) == missing)
            .collect();
        assert_eq!(lacks, "");
        let (cap, _) = inked(&font, "A");
        let (top, _) = inked(&font, "Ä");
        assert!(top < cap, "Ä inks from {top}, A from {cap}");
    }

    /// The record's two bits name each size, a record before 1561 (0 or
    /// the 1532 flag 1) reads as it was written, a value no build writes
    /// is the default; the index is the place in [`Size::ALL`].
    #[test]
    fn the_sizes_keep_as_two_bits_and_index_their_order() {
        assert_eq!(Size::DEFAULT, Size::Small);
        for (i, size) in Size::ALL.into_iter().enumerate() {
            assert_eq!(Size::of_record(size.record()), size);
            assert_eq!(size.index(), i);
        }
        assert_eq!(Size::of_record(0), Size::Small);
        assert_eq!(Size::of_record(1), Size::Large);
        assert_eq!(Size::of_record(3), Size::DEFAULT);
    }
}
