//! Leviculum's patch (1442): the renderer's work taken away where a
//! rectangle is a rectangle. tiny-skia fills an axis-aligned rectangle
//! without anti-aliasing through its scan converter and a raster
//! pipeline, keeps a clip as a mask the size of the window that every
//! clipped pixel reads, and draws an opaque picture at a whole-pixel
//! offset as a pattern shader sampling it pixel by pixel. Each of these
//! is a row copy or a row fill of the same bytes; this module computes
//! them, and the tests in `leviculum-ui-iced` (`fast_paths`) hold every
//! one of them to tiny-skia's own result, byte for byte.

use crate::core::Rectangle;

/// A rectangle of whole pixels, `x0..x1` by `y0..y1`, never empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Px {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Px {
    /// The pixels both cover; `None` when they do not meet.
    pub fn intersect(self, other: Px) -> Option<Px> {
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        let x1 = self.x1.min(other.x1);
        let y1 = self.y1.min(other.y1);
        (x0 < x1 && y0 < y1).then_some(Px { x0, y0, x1, y1 })
    }
}

/// The edge a non-anti-aliased fill puts at `v`: tiny-skia converts the
/// coordinate to 26.6 fixed point by truncation (`fdot6::from_f32`) and
/// rounds that to a pixel (`fdot6::round`), a pixel is covered when its
/// centre lies inside.
fn edge(v: f32) -> i64 {
    let fixed = (v * 64.0) as i32;
    (i64::from(fixed) + 32) >> 6
}

/// The pixels a fill of `bounds` without anti-aliasing covers on a
/// `width` by `height` target, as tiny-skia's scan converter covers them
/// (`Mask::fill_path` and `PixmapMut::fill_path` of the rectangle's
/// path, `anti_alias: false`); `None` when it covers none.
pub fn covered(bounds: Rectangle, width: u32, height: u32) -> Option<Px> {
    if !(bounds.width > 0.0 && bounds.height > 0.0)
        || !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
    {
        return None;
    }
    covered_ltrb(
        [
            bounds.x,
            bounds.y,
            bounds.x + bounds.width,
            bounds.y + bounds.height,
        ],
        width,
        height,
    )
}

/// [`covered`] for a rectangle given by its edges, left, top, right and
/// bottom, as a path's bounds name them (1473).
fn covered_ltrb(ltrb: [f32; 4], width: u32, height: u32) -> Option<Px> {
    let [l, t, r, b] = ltrb;
    let clamp = |v: i64, max: u32| v.clamp(0, i64::from(max)) as u32;
    let x0 = clamp(edge(l), width);
    let x1 = clamp(edge(r), width);
    let y0 = clamp(edge(t), height);
    let y1 = clamp(edge(b), height);
    (x0 < x1 && y0 < y1).then_some(Px { x0, y0, x1, y1 })
}

/// The pixels an anti-aliased fill of the rectangle `bounds` changes
/// within `clip` on a `width` by `height` target, when `bounds` lies on
/// whole pixels (1468): each of them it covers whole, so an opaque colour
/// there is the colour's bytes, and the clip, tiny-skia's mask of the
/// clip's rectangle filled without anti-aliasing, keeps the pixels whose
/// centres it holds. `None` when `bounds` is not on whole pixels or
/// nothing is left.
pub fn whole_rect(
    bounds: Rectangle,
    clip: Rectangle,
    width: u32,
    height: u32,
) -> Option<Px> {
    let edges = [bounds.x, bounds.y, bounds.width, bounds.height];
    if !edges.iter().all(|v| v.is_finite() && v.fract() == 0.0) {
        return None;
    }
    covered(bounds, width, height)?.intersect(covered(clip, width, height)?)
}

/// Whether every pixel of the RGBA8 `raw` is opaque; read as words where
/// it can be, as [`swap_red_blue`] reads it (1468).
pub fn all_opaque(raw: &[u8]) -> bool {
    match bytemuck::try_cast_slice::<u8, u32>(raw) {
        Ok(words) => {
            let alpha = u32::from_ne_bytes([0, 0, 0, 0xff]);
            words.iter().fold(alpha, |all, &w| all & w) & alpha == alpha
        }
        Err(_) => raw
            .chunks_exact(4)
            .fold(true, |opaque, pixel| opaque & (pixel[3] == 255)),
    }
}

/// The opaque RGBA8 pixels `raw` as tiny-skia's premultiplied BGRA8
/// words, `u32::from_ne_bytes([b, g, r, 255])` each (1442's conversion
/// of an opaque picture). On a little-endian target a word's red and
/// blue bytes swapped in place (1468): the byte-wise form compiled to
/// about 18 instructions a pixel, 29 % of a map-wheel frame's present
/// in callgrind, since every frame of the zoom is a new picture.
pub fn swap_red_blue(raw: &[u8]) -> Vec<u32> {
    let swap = |v: u32| {
        (v & 0x0000_ff00)
            | ((v >> 16) & 0xff)
            | ((v & 0xff) << 16)
            | 0xff00_0000
    };
    match bytemuck::try_cast_slice::<u8, u32>(raw) {
        // Words read as words: the map over them vectorises.
        Ok(words) if cfg!(target_endian = "little") => {
            words.iter().map(|&v| swap(v)).collect()
        }
        _ => raw
            .chunks_exact(4)
            .map(|p| u32::from_ne_bytes([p[2], p[1], p[0], 255]))
            .collect(),
    }
}

/// The four bytes tiny-skia stores for `color` filled with the `Source`
/// blend and no anti-aliasing: the same pipeline run on one pixel.
pub fn solid_pixel(color: tiny_skia::Color) -> [u8; 4] {
    let Some(mut one) = tiny_skia::Pixmap::new(1, 1) else {
        return [0; 4];
    };
    let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0) else {
        return [0; 4];
    };
    one.fill_path(
        &tiny_skia::PathBuilder::from_rect(rect),
        &tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(color),
            anti_alias: false,
            blend_mode: tiny_skia::BlendMode::Source,
            ..Default::default()
        },
        tiny_skia::FillRule::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    let d = one.data();
    [d[0], d[1], d[2], d[3]]
}

/// `px` written over `rect` of the `width` pixels wide RGBA8 `pixels`.
pub fn fill(pixels: &mut [u8], width: u32, rect: Px, px: [u8; 4]) {
    let stride = width as usize * 4;
    let (x0, x1) = (rect.x0 as usize * 4, rect.x1 as usize * 4);
    for row in pixels
        .chunks_exact_mut(stride)
        .skip(rect.y0 as usize)
        .take((rect.y1 - rect.y0) as usize)
    {
        for p in row[x0..x1].chunks_exact_mut(4) {
            p.copy_from_slice(&px);
        }
    }
}

/// `value` written over `rect` of the `width` wide one-byte `mask`.
pub fn fill_mask(mask: &mut [u8], width: u32, rect: Px, value: u8) {
    let stride = width as usize;
    for row in mask
        .chunks_exact_mut(stride)
        .skip(rect.y0 as usize)
        .take((rect.y1 - rect.y0) as usize)
    {
        row[rect.x0 as usize..rect.x1 as usize].fill(value);
    }
}

/// The clip as tiny-skia's mask holds it, kept lazily (1442): a layer
/// names its clip rectangle, the mask is written only when a draw that
/// crosses it asks for it, and then only the rows of the rectangle it
/// held before and of the one it holds now, where tiny-skia's
/// `adjust_clip_mask` cleared all of the window's bytes and filled the
/// rectangle's path through the scan converter, once per layer and
/// damage rectangle. The mask a draw is handed is unknown to it: the
/// first time it is asked within a draw, it clears all of it.
#[derive(Debug, Default)]
pub struct Clip {
    want: Option<Rectangle>,
    have: Option<Option<Px>>,
}

impl Clip {
    /// A new draw over a mask whose content is not known.
    pub fn reset(&mut self) {
        self.want = None;
        self.have = None;
    }

    /// The clip is `bounds` from here on.
    pub fn set(&mut self, bounds: Rectangle) {
        self.want = Some(bounds);
    }

    /// The pixels the clip covers on a `width` by `height` target.
    pub fn rect(&self, width: u32, height: u32) -> Option<Px> {
        self.want.and_then(|b| covered(b, width, height))
    }

    /// The clip for a draw that crosses it: its rectangle where
    /// [`blit_over`] blends as tiny-skia does, else `mask` written.
    pub fn clipping<'a>(
        &mut self,
        mask: &'a mut tiny_skia::Mask,
    ) -> Clipping<'a> {
        match BLEND_EXACT {
            true => Clipping::Rect(self.rect(mask.width(), mask.height())),
            false => Clipping::Mask(self.mask(mask)),
        }
    }

    /// `mask` holding the clip, as `adjust_clip_mask` left it.
    pub fn mask<'a>(
        &mut self,
        mask: &'a mut tiny_skia::Mask,
    ) -> &'a tiny_skia::Mask {
        let (w, h) = (mask.width(), mask.height());
        let want = self.rect(w, h);
        match self.have {
            Some(have) if have == want => {}
            Some(have) => {
                if let Some(old) = have {
                    fill_mask(mask.data_mut(), w, old, 0);
                }
                if let Some(new) = want {
                    fill_mask(mask.data_mut(), w, new, 255);
                }
            }
            None => {
                mask.data_mut().fill(0);
                if let Some(new) = want {
                    fill_mask(mask.data_mut(), w, new, 255);
                }
            }
        }
        self.have = Some(want);
        mask
    }
}

/// How far past its box a text's glyphs may put ink, logical pixels
/// (1468): an accent over a capital, a descender, an italic's overhang.
/// A text whose box is further than this from a clip draws nothing in it.
pub const TEXT_INK: f32 = 8.0;

/// tiny-skia's rounding of a whole-pixel coordinate (`Rect::round`,
/// `saturate_round`: a half added, then truncated toward zero): a
/// negative one moves a pixel toward zero.
fn round(v: i32) -> i64 {
    (v as f32 + 0.5) as i64
}

/// How a draw is clipped (1442): not at all, to the pixels of a
/// rectangle (`None`: to none), or by tiny-skia's mask; a draw that
/// reaches the rectangle form never needs the mask written.
#[derive(Debug, Clone, Copy)]
pub enum Clipping<'a> {
    Unclipped,
    Rect(Option<Px>),
    Mask(&'a tiny_skia::Mask),
}

impl<'a> Clipping<'a> {
    /// The mask, for a draw through tiny-skia.
    pub fn mask(self) -> Option<&'a tiny_skia::Mask> {
        match self {
            Clipping::Mask(mask) => Some(mask),
            Clipping::Unclipped | Clipping::Rect(_) => None,
        }
    }
}

/// The opaque `src` drawn at whole pixels `(x, y)` onto `dst` within
/// `clip`, where tiny-skia's `draw_pixmap` with an identity transform and
/// full opacity samples the pattern at the pixel itself (nearest, as an
/// integer translation is) and blends source-over, which for an opaque
/// source is the source: a row copy. As tiny-skia draws it: its rectangle
/// rounds a negative corner a pixel toward zero and keeps its size, the
/// pattern pads, so a picture hanging off the left or top edge gets its
/// last column or row once more on the far side.
pub fn blit_opaque(
    dst: &mut tiny_skia::PixmapMut<'_>,
    src: tiny_skia::PixmapRef<'_>,
    x: i32,
    y: i32,
    clip: Option<Px>,
) {
    let (dw, dh) = (dst.width(), dst.height());
    let (sw, sh) = (src.width(), src.height());
    let (rx, ry) = (round(x), round(y));
    let x0 = rx.clamp(0, i64::from(dw)) as u32;
    let y0 = ry.clamp(0, i64::from(dh)) as u32;
    let x1 = (rx + i64::from(sw)).clamp(0, i64::from(dw)) as u32;
    let y1 = (ry + i64::from(sh)).clamp(0, i64::from(dh)) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut area = Px { x0, y0, x1, y1 };
    if let Some(clip) = clip {
        match area.intersect(clip) {
            Some(a) => area = a,
            None => return,
        }
    }
    // The source column of a target column, padded at the far edge.
    let column =
        |d: u32| (i64::from(d) - i64::from(x)).min(i64::from(sw) - 1) as usize;
    let row_of =
        |d: u32| (i64::from(d) - i64::from(y)).min(i64::from(sh) - 1) as usize;
    // The columns that map one to one, and the padded one after them.
    let straight_end = (i64::from(x) + i64::from(sw))
        .clamp(i64::from(area.x0), i64::from(area.x1))
        as u32;
    let sstride = sw as usize * 4;
    let dstride = dw as usize * 4;
    let s = src.data();
    let d = dst.data_mut();
    for row in area.y0..area.y1 {
        let from = row_of(row) * sstride;
        let to = row as usize * dstride;
        if area.x0 < straight_end {
            let a = from + column(area.x0) * 4;
            let len = (straight_end - area.x0) as usize * 4;
            let b = to + area.x0 as usize * 4;
            d[b..b + len].copy_from_slice(&s[a..a + len]);
        }
        for col in straight_end.max(area.x0)..area.x1 {
            let a = from + column(col) * 4;
            let b = to + col as usize * 4;
            d[b..b + 4].copy_from_slice(&s[a..a + 4]);
        }
    }
}

/// Whether [`blit_over`] computes what tiny-skia computes on this target:
/// its high-precision pipeline rounds to the nearest even integer on
/// x86-64 (SSE2 `cvtps2dq`) and on aarch64 (NEON `vcvtnq`); elsewhere
/// its fallback rounds otherwise and the renderer keeps tiny-skia's draw.
pub const BLEND_EXACT: bool = cfg!(any(
    all(target_arch = "x86_64", target_feature = "sse2"),
    all(target_arch = "aarch64", target_feature = "neon")
));

/// tiny-skia's high-precision store of one channel (`unnorm`).
#[inline(always)]
pub fn unnorm(v: f32) -> u8 {
    // In 0..=255 already: through i32, which converts four at once,
    // where `as u8` saturates lane by lane.
    ties_even(v.max(0.0).min(1.0) * 255.0) as i32 as u8
}

/// `v` in `0.0..=255.0` rounded to the nearest integer, a tie to the
/// even one (1468): the sum with 2^23 has no bits below the units, so
/// the addition rounds in the FPU's mode, which is to nearest even, and
/// the subtraction is exact. `f32::round_ties_even` is the same number,
/// but without SSE4.1's `roundss` (x86-64's baseline lacks it) a call
/// to libm's `rintf` per channel: 5 to 9 % of a chat frame's present in
/// callgrind. `fast_paths` holds it to `round_ties_even` over every
/// value a channel can take.
#[inline(always)]
pub fn ties_even(v: f32) -> f32 {
    const SHIFT: f32 = 8_388_608.0;
    (v + SHIFT) - SHIFT
}

/// The premultiplied `src` (a glyph) drawn source-over at whole pixels
/// `(x, y)` onto `dst` at `opacity`, within `clip`: what tiny-skia's
/// `draw_pixmap` with an identity transform computes, without building a
/// pipeline for it (1442; a chat frame built one per glyph, and ran it
/// through a pattern sampler). The same f32 arithmetic in the same
/// order, lane for lane: the source loaded as `c * (1/255)`, scaled by
/// the opacity when it is not one, the destination loaded alike,
/// `d * (1 - sa) + s`, clamped and rounded to the nearest even; a pixel
/// the source leaves transparent at full opacity keeps the destination
/// and an opaque one is the source, which that arithmetic gives too
/// (`fast_paths` holds both over every byte). The rectangle and the pad
/// are `blit_opaque`'s. Only where [`BLEND_EXACT`].
pub fn blit_over(
    dst: &mut tiny_skia::PixmapMut<'_>,
    src: tiny_skia::PixmapRef<'_>,
    x: i32,
    y: i32,
    opacity: f32,
    clip: Option<Px>,
) {
    let opacity = if opacity.is_finite() {
        opacity.max(0.0).min(1.0)
    } else {
        0.0
    };
    let scaled = opacity != 1.0;
    let (dw, dh) = (dst.width(), dst.height());
    let (sw, sh) = (src.width(), src.height());
    let (rx, ry) = (round(x), round(y));
    let x0 = rx.clamp(0, i64::from(dw)) as u32;
    let y0 = ry.clamp(0, i64::from(dh)) as u32;
    let x1 = (rx + i64::from(sw)).clamp(0, i64::from(dw)) as u32;
    let y1 = (ry + i64::from(sh)).clamp(0, i64::from(dh)) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut area = Px { x0, y0, x1, y1 };
    if let Some(clip) = clip {
        match area.intersect(clip) {
            Some(a) => area = a,
            None => return,
        }
    }
    const FACTOR: f32 = 1.0 / 255.0;
    let column =
        |d: u32| (i64::from(d) - i64::from(x)).min(i64::from(sw) - 1) as usize;
    let row_of =
        |d: u32| (i64::from(d) - i64::from(y)).min(i64::from(sh) - 1) as usize;
    let sstride = sw as usize * 4;
    let dstride = dw as usize * 4;
    let s = src.data();
    let d = dst.data_mut();
    for row in area.y0..area.y1 {
        let from = row_of(row) * sstride;
        let to = row as usize * dstride;
        for col in area.x0..area.x1 {
            let a = from + column(col) * 4;
            let sp = [s[a], s[a + 1], s[a + 2], s[a + 3]];
            let b = to + col as usize * 4;
            if !scaled {
                match sp[3] {
                    0 if sp == [0; 4] => continue,
                    255 => {
                        d[b..b + 4].copy_from_slice(&sp);
                        continue;
                    }
                    _ => {}
                }
            }
            let mut sf = [
                f32::from(sp[0]) * FACTOR,
                f32::from(sp[1]) * FACTOR,
                f32::from(sp[2]) * FACTOR,
                f32::from(sp[3]) * FACTOR,
            ];
            if scaled {
                for c in &mut sf {
                    *c *= opacity;
                }
            }
            let inv = 1.0 - sf[3];
            for c in 0..4 {
                let df = f32::from(d[b + c]) * FACTOR;
                d[b + c] = unnorm(df * inv + sf[c]);
            }
        }
    }
}

/// tiny-skia's tiler takes over above this many pixels a side
/// (`DrawTiler::MAX_DIMENSIONS`); [`draw_scaled`] leaves such a target to
/// it.
const UNTILED: u32 = 8192 - 1;

/// How far from the target a scaled picture's edges may lie for
/// [`draw_scaled`]: within it, an edge in 26.6 fixed point fits an `i32`
/// and a sample's coordinate truncates as tiny-skia's `cvttps` does.
const REACH: f32 = 1_048_576.0;

/// A channel byte as tiny-skia's high-precision pipeline loads it
/// (`load_8888`: the byte times `1/255`), for every byte.
fn loads() -> [f32; 256] {
    const FACTOR: f32 = 1.0 / 255.0;
    let mut table = [0.0; 256];
    for (byte, value) in table.iter_mut().enumerate() {
        *value = byte as f32 * FACTOR;
    }
    table
}

/// The column, or row, of a picture `limit` pixels across a sample at
/// `v` reads, as `gather_ix` reads it: clamped to the picture, its last
/// pixel's edge one ulp short, and truncated.
#[inline(always)]
fn texel(v: f32, limit: u32) -> usize {
    let last = f32::from_bits((limit as f32).to_bits() - 1);
    v.max(0.0).min(last) as i32 as usize
}

/// `v` less its floor, the floor as tiny-skia's `f32x8::floor` takes it:
/// truncated, and one less where truncation went up.
#[inline(always)]
fn fract(v: f32) -> f32 {
    let t = (v as i32) as f32;
    v - (t - if t > v { 1.0 } else { 0.0 })
}

/// Where a bilinear sample at `v` reads, on a picture `limit` across:
/// the two texels and their weights, as tiny-skia's `bilinear` and
/// `sampler_2x2` take them (the fraction of `v + 0.5`, the first texel
/// at `v - 0.5`, the second one further).
#[inline(always)]
fn taps(v: f32, limit: u32) -> ([usize; 2], [f32; 2]) {
    let f = fract(v + 0.5);
    let first = v + -0.5;
    let second = first + 1.0;
    ([texel(first, limit), texel(second, limit)], [1.0 - f, f])
}

/// The picture `src` drawn as tiny-skia's `PixmapMut::draw_pixmap` draws
/// it at `(x, y)` with `quality`, `opacity` and a scaling `transform`
/// (1473): the map's frame on every frame of a zoom, which went through
/// tiny-skia's high-precision pipeline at 10 to 13 ms a present, its
/// bilinear sampler a third of that and the clip's mask read per pixel.
///
/// The same f32 arithmetic in the same order, lane for lane: the pixel
/// centre `x + 0.5` (`seed_shader`), moved by the inverse of the
/// pattern's transform (`transform`, `mad` as a product and a sum), read
/// at the nearest texel (`gather`: clamped to the last pixel's edge less
/// an ulp, truncated) or between four (`bilinear`: the weights from the
/// fraction of `v + 0.5`, summed in `sampler_2x2`'s order), loaded as
/// `byte * 1/255`, scaled by the opacity when it is not one, blended
/// source-over (`d * (1 - sa) + s`) and stored rounding to the nearest
/// even. A transform without skew keeps a column's coordinate the same
/// on every row, so each column and row is sampled once. An opaque
/// picture read at its nearest texel at full opacity is its bytes, which
/// that arithmetic gives too (`fast_paths` holds it over every byte); a
/// bilinear sample whose alpha sums to one does not read the target.
///
/// Covered are the pixels the rectangle's path, moved by `transform`,
/// covers without anti-aliasing ([`covered`]), within `clip`: the mask
/// tiny-skia multiplies by is one inside the clip, so inside it the draw
/// is the unclipped one, and the mask is never written.
///
/// `false` where it does not draw and the caller keeps tiny-skia's draw:
/// off [`BLEND_EXACT`] targets, a skewed or rotated transform, bicubic
/// quality, a target tiny-skia would tile, edges or samples further than
/// [`REACH`], or a transform without an inverse.
#[allow(clippy::too_many_arguments)]
pub fn draw_scaled(
    dst: &mut tiny_skia::PixmapMut<'_>,
    src: tiny_skia::PixmapRef<'_>,
    opaque: bool,
    x: i32,
    y: i32,
    quality: tiny_skia::FilterQuality,
    opacity: f32,
    transform: tiny_skia::Transform,
    clip: Option<Px>,
) -> bool {
    use tiny_skia::{FilterQuality, Transform};
    if !BLEND_EXACT
        || transform.has_skew()
        || !transform.is_finite()
        || quality == FilterQuality::Bicubic
    {
        return false;
    }
    let (dw, dh) = (dst.width(), dst.height());
    let (sw, sh) = (src.width(), src.height());
    if dw > UNTILED || dh > UNTILED {
        return false;
    }
    let Some(rect) = tiny_skia::IntRect::from_xywh(x, y, sw, sh) else {
        return false;
    };
    // The rectangle's path, moved as `fill_path` moves it.
    let Some(path) =
        tiny_skia::PathBuilder::from_rect(rect.to_rect()).transform(transform)
    else {
        return false;
    };
    let b = path.bounds();
    let ltrb = [b.left(), b.top(), b.right(), b.bottom()];
    // tiny-skia refuses a path this thin; let it say so.
    let thin = 1.0 / 4096.0;
    if b.width() <= thin
        || b.height() <= thin
        || ltrb.iter().any(|v| v.abs() > REACH)
    {
        return false;
    }
    // The pattern's transform (`Shader::transform`), and its inverse as
    // `push_stages` takes it.
    let Some(inverse) = Transform::from_translate(x as f32, y as f32)
        .post_concat(transform)
        .invert()
    else {
        return false;
    };
    if !inverse.is_finite() || inverse.has_skew() {
        return false;
    }
    let quality = match inverse.is_identity() || inverse.is_translate() {
        true => FilterQuality::Nearest,
        false => quality,
    };
    let moved = !inverse.is_identity();
    // The sample coordinate of a column and of a row: `r * sx + (g * kx
    // + tx)` with `kx` a zero, `g * kx` that zero with `kx`'s sign for
    // every row; alike for rows.
    let column = |c: u32| {
        let r = c as f32 + 0.5;
        match moved {
            true => r * inverse.sx + (0.5 * inverse.kx + inverse.tx),
            false => r,
        }
    };
    let row = |c: u32| {
        let g = c as f32 + 0.5;
        match moved {
            true => 0.5 * inverse.ky + (g * inverse.sy + inverse.ty),
            false => g,
        }
    };
    let Some(mut area) = covered_ltrb(ltrb, dw, dh) else {
        return true;
    };
    if let Some(clip) = clip {
        match area.intersect(clip) {
            Some(a) => area = a,
            None => return true,
        }
    }
    let reach = |v: f32| v.is_finite() && v.abs() <= REACH;
    if ![column(area.x0), column(area.x1), row(area.y0), row(area.y1)]
        .into_iter()
        .all(reach)
    {
        return false;
    }
    let opacity = if opacity.is_finite() {
        1.0f32.min(opacity).max(0.0)
    } else {
        0.0
    };
    let (x0, x1) = (area.x0 as usize, area.x1 as usize);
    let dstride = dw as usize;
    let sstride = sw as usize;
    match quality {
        FilterQuality::Nearest => {
            let columns: Vec<usize> =
                (area.x0..area.x1).map(|c| texel(column(c), sw)).collect();
            if opaque && opacity == 1.0 {
                let s = src.pixels();
                let d = dst.pixels_mut();
                for r in area.y0..area.y1 {
                    let from = texel(row(r), sh) * sstride;
                    let to = r as usize * dstride;
                    let out = &mut d[to + x0..to + x1];
                    for (p, &c) in out.iter_mut().zip(&columns) {
                        *p = s[from + c];
                    }
                }
                return true;
            }
            let load = loads();
            let s = src.data();
            let d = dst.data_mut();
            for r in area.y0..area.y1 {
                let from = texel(row(r), sh) * sstride;
                let to = r as usize * dstride;
                for (i, &c) in columns.iter().enumerate() {
                    let a = (from + c) * 4;
                    let mut sf = [
                        load[usize::from(s[a])],
                        load[usize::from(s[a + 1])],
                        load[usize::from(s[a + 2])],
                        load[usize::from(s[a + 3])],
                    ];
                    if opacity != 1.0 {
                        for v in &mut sf {
                            *v *= opacity;
                        }
                    }
                    over(&mut d[(to + x0 + i) * 4..][..4], sf, &load);
                }
            }
        }
        FilterQuality::Bilinear | FilterQuality::Bicubic => {
            let columns: Vec<([usize; 2], [f32; 2])> =
                (area.x0..area.x1).map(|c| taps(column(c), sw)).collect();
            let load = loads();
            let s = src.data();
            let d = dst.data_mut();
            for r in area.y0..area.y1 {
                let (iy, wy) = taps(row(r), sh);
                let rows = [iy[0] * sstride, iy[1] * sstride];
                let to = r as usize * dstride;
                let out = &mut d[(to + x0) * 4..(to + x1) * 4];
                for (p, (ix, wx)) in out.chunks_exact_mut(4).zip(&columns) {
                    let mut sf = [0.0f32; 4];
                    for (j, row) in rows.iter().enumerate() {
                        for (k, col) in ix.iter().enumerate() {
                            let w = wx[k] * wy[j];
                            let t = texel4(s, (row + col) * 4);
                            for (v, t) in sf.iter_mut().zip(t) {
                                *v = w * t + *v;
                            }
                        }
                    }
                    if opacity != 1.0 {
                        for v in &mut sf {
                            *v *= opacity;
                        }
                    }
                    over(p, sf, &load);
                }
            }
        }
    }
    true
}

/// The four channels of the pixel at byte `a` as `load_8888` loads them:
/// each byte times `1/255`, the same products [`loads`] holds.
#[inline(always)]
fn texel4(s: &[u8], a: usize) -> [f32; 4] {
    const FACTOR: f32 = 1.0 / 255.0;
    let mut t = [0.0f32; 4];
    for (v, &c) in t.iter_mut().zip(&s[a..a + 4]) {
        *v = f32::from(c) * FACTOR;
    }
    t
}

/// The premultiplied `s` blended source-over onto the pixel `d` and
/// stored, as `source_over_rgba` does; an `s` whose alpha is one keeps
/// none of `d` (`d * 0 + s` is `s`), so `d` is not read.
#[inline(always)]
fn over(d: &mut [u8], s: [f32; 4], load: &[f32; 256]) {
    if s[3] == 1.0 {
        for (out, v) in d.iter_mut().zip(s) {
            *out = unnorm(v);
        }
        return;
    }
    let inv = 1.0 - s[3];
    for (out, v) in d.iter_mut().zip(s) {
        *out = unnorm(load[usize::from(*out)] * inv + v);
    }
}
