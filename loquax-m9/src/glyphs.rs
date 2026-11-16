//! The map's names in Fira Sans on the M9 (1565): the [`Shaper`] the
//! phone's label pass (`loquax::mapnames`) asks for, without `std`.
//!
//! The phone and the window shape with cosmic-text and rasterise with
//! swash; both need `std`. The board lays the glyphs out itself, left to
//! right, each advanced by its width and the font's kerning pairs, and
//! rasterises them with ab_glyph (`ab_glyph_rasterizer`, `no_std` with
//! `libm`), from the same file the phone embeds
//! ([`loquax::mapnames::FIRA_SANS_REGULAR`], 440,984 B in the image). A
//! map name is a word or three of Latin, Greek or Cyrillic text, which
//! needs no shaping past that; a script that does (Arabic, the Indic
//! ones) is set glyph by glyph, as the board's atlas sets it everywhere
//! else.
//!
//! The box is the one cosmic-text gives the same text: the advance's
//! width and a line of `ceil(1.25 size)`, the baseline where cosmic-text
//! puts it in that line, and room for the slant (a quarter of the size)
//! when the name is a river's. The slant is the glyphs' own coverage
//! sheared row by row, as swash's fake italic leans them.
//!
//! Nothing is kept between draws: a mask lives as long as the picture's
//! label pass, so the board's census sees the draw hold nothing
//! ([`crate::census::MAP`]). [`Glyphs::made`] counts the glyphs
//! rasterised, the `[MAP]` line's `glyphs=`.

use alloc::vec;
use alloc::vec::Vec;

use ab_glyph::{point, Font, FontRef, Glyph, GlyphId, PxScale, ScaleFont};
use loquax::mapnames::{Mask, Shaper, FIRA_SANS_REGULAR};

/// How far a river's name leans: its glyphs' tops move right by this
/// much of their height above the baseline (swash's fake italic, about
/// 14 degrees).
const SLANT: f32 = 0.25;

/// The font and a count of the glyphs rasterised.
pub struct Glyphs {
    font: FontRef<'static>,
    /// Glyphs rasterised since [`Glyphs::new`].
    pub made: u32,
}

impl Glyphs {
    /// Fira Sans from the image; `None` should the file not read, which
    /// the board's own test rules out.
    pub fn new() -> Option<Self> {
        let font = FontRef::try_from_slice(FIRA_SANS_REGULAR).ok()?;
        Some(Self { font, made: 0 })
    }

    /// The scale of a text `size` pixels to the em, as cosmic-text sizes
    /// it: ab_glyph's scale is the font's height, ascent to descent.
    fn scale(&self, size: f32) -> PxScale {
        let em = self.font.units_per_em().unwrap_or(1000.0);
        PxScale::from(size * self.font.height_unscaled() / em)
    }

    /// `text`'s glyphs at `size`, their pen positions on a baseline at
    /// y 0, and the advance's width.
    fn laid(&self, text: &str, size: f32) -> (Vec<Glyph>, f32) {
        let scaled = self.font.as_scaled(self.scale(size));
        let mut caret = 0.0f32;
        let mut last: Option<GlyphId> = None;
        let mut out = Vec::with_capacity(text.len());
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = last {
                caret += scaled.kern(prev, id);
            }
            out.push(id.with_scale_and_position(scaled.scale(), point(caret, 0.0)));
            caret += scaled.h_advance(id);
            last = Some(id);
        }
        (out, caret)
    }

    /// The line's height and the baseline's row in it, as cosmic-text
    /// sets one line of `size` pixels: the line `ceil(1.25 size)` high,
    /// the font's ascent and descent centred in it.
    fn line(&self, size: f32) -> (usize, f32) {
        let scaled = self.font.as_scaled(self.scale(size));
        let line = libm::ceilf(size * 1.25);
        let glyphs = scaled.ascent() - scaled.descent();
        (line as usize, (line - glyphs) / 2.0 + scaled.ascent())
    }
}

/// The room a slanted name needs past its advance.
fn lean(size: f32, italic: bool) -> usize {
    match italic {
        true => libm::ceilf(size * SLANT) as usize,
        false => 0,
    }
}

impl Shaper for Glyphs {
    fn extent(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> (usize, usize) {
        let (_, width) = self.laid(text, size);
        let (line, _) = self.line(size);
        (
            libm::ceilf(width) as usize + lean(size, italic) + 2 * pad,
            line + 2 * pad,
        )
    }

    fn mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Mask {
        let (w, h) = self.extent(text, size, pad, italic);
        let (_, base) = self.line(size);
        let base = base + pad as f32;
        let mut alpha = vec![0u8; w * h];
        let (glyphs, _) = self.laid(text, size);
        for mut glyph in glyphs {
            glyph.position.x += pad as f32;
            glyph.position.y = base;
            let Some(outline) = self.font.outline_glyph(glyph) else {
                continue;
            };
            self.made = self.made.saturating_add(1);
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, c| {
                let x = bounds.min.x + gx as f32;
                let y = bounds.min.y + gy as f32;
                // The slant moves a row right by its height over the
                // baseline, split between the two pixels it lands on.
                let shift = match italic {
                    true => (base - (y + 0.5)) * SLANT,
                    false => 0.0,
                };
                let at = x + shift;
                let left = libm::floorf(at);
                let part = at - left;
                let cover = c.clamp(0.0, 1.0);
                for (px, share) in [(left, 1.0 - part), (left + 1.0, part)] {
                    if px < 0.0 || y < 0.0 || share <= 0.0 {
                        continue;
                    }
                    let (px, py) = (px as usize, y as usize);
                    if px < w && py < h {
                        let i = py * w + px;
                        let v = libm::roundf(cover * share * 255.0) as u16;
                        alpha[i] = (u16::from(alpha[i]) + v).min(255) as u8;
                    }
                }
            });
        }
        Mask { w, h, pad, alpha }
    }
}
