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

use embedded_graphics::mono_font::MonoFont;

/// One cell's pixels, across and down.
pub const CELL_PX: (u16, u16) = (10, 20);

/// Cells across: 320 px of a 10 px wide atlas, none left over.
pub const COLS: usize = 32;
/// Cell rows: 240 px of a 20 px high atlas, none left over.
pub const ROWS: usize = 12;

/// The millionths of a millimetre one panel pixel measures.
pub const NM_PER_PX: u32 = 152_400;

/// The atlas. Each call builds its lookup table and leaks it (the crate's
/// own warning); the renderer makes it once, for as long as the panel.
pub fn atlas() -> MonoFont<'static> {
    mousefood::fonts::mono_10x20_atlas()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{HEIGHT, WIDTH};
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
}
