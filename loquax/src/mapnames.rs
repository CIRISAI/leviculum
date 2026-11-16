//! The map's names on a pixel screen (`docs/drafts/map-renderer-roadmap-2026-10.md`,
//! section 1, "Placement" and "Fonts"): set into the picture's raster once,
//! when the view is at rest, so they ride along with it and cost nothing a
//! frame. Placed by the map core's one rule ([`leviculum_map::labels::place`])
//! in screen pixels, after the pins and their names, which nothing covers.
//! The window (1100), the Android app (1128) and since 1565 the M9 set
//! them through this one module.
//!
//! Places, waters and parks are set centred on their anchor; a street's
//! or a river's name on the middle of the longest run of its line (its
//! pieces joined by the map core) that turns less than [`STRAIGHT_DEG`] a
//! vertex, or where that is taken further along it or on the next run, at
//! the run's angle and turned to read left to right, with
//! squares along its baseline as its boxes (the study's first step;
//! bending text along curves is later). Rivers are blue and slanted, the
//! font's own glyphs sheared, since the font has no italic here. Every name
//! carries a halo of the map's ground colour, its glyphs' coverage widened
//! by [`HALO`] pixels and drawn first.
//!
//! The text is shaped by cosmic-text and rasterised by swash in Fira Sans,
//! the face iced embeds, carried here as `fonts/FiraSans-Regular.ttf` (the
//! same file, under the SIL Open Font License, `fonts/OFL.txt`): one font
//! on every host and on the phone, so a picture and its golden agree.
//! The M9 sets the same face through its own [`Shaper`]
//! (`loquax_m9::glyphs`, ab_glyph's rasteriser), since cosmic-text needs
//! `std`; everything but the shaping (the runs, the rule, the masks, the
//! halos and the paint) is this module's, `no_std` with `alloc` under the
//! `mapnames` feature, the cosmic-text [`Typesetter`] under `screenmap`.

use alloc::string::String;
#[cfg(feature = "screenmap")]
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
#[cfg(feature = "screenmap")]
use std::collections::HashMap;

#[cfg(feature = "screenmap")]
use cosmic_text::{self as ct, fontdb, Attrs, Buffer, Family, FontSystem, Shaping, SwashCache};
use embedded_graphics::pixelcolor::{Rgb565, Rgb888};
use leviculum_map::labels::{self, Candidate, LabelKind, Rect, Rule, ViewLabel, SUBPIXEL};
use leviculum_mapfill::coverage::Coverage;
use leviculum_mapfill::style::MapTheme;

#[cfg(not(feature = "std"))]
use float::Float as _;

/// The float methods the pass uses, from `libm` where the crate has no
/// `std` (the M9, 1565). With `std` the inherent methods are used and
/// this trait is not in scope, so the phone's and the window's pictures
/// are the bytes they were.
#[cfg(not(feature = "std"))]
mod float {
    pub trait Float {
        fn ceil(self) -> Self;
        fn floor(self) -> Self;
        fn round(self) -> Self;
        fn atan2(self, x: Self) -> Self;
        fn hypot(self, y: Self) -> Self;
        fn cos(self) -> Self;
        fn sin(self) -> Self;
        fn rem_euclid(self, rhs: Self) -> Self;
    }

    impl Float for f64 {
        fn ceil(self) -> Self {
            libm::ceil(self)
        }
        fn floor(self) -> Self {
            libm::floor(self)
        }
        fn round(self) -> Self {
            libm::round(self)
        }
        fn atan2(self, x: Self) -> Self {
            libm::atan2(self, x)
        }
        fn hypot(self, y: Self) -> Self {
            libm::hypot(self, y)
        }
        fn cos(self) -> Self {
            libm::cos(self)
        }
        fn sin(self) -> Self {
            libm::sin(self)
        }
        // As std defines it.
        fn rem_euclid(self, rhs: Self) -> Self {
            let r = self % rhs;
            if r < 0.0 {
                r + rhs.abs()
            } else {
                r
            }
        }
    }

    impl Float for f32 {
        fn ceil(self) -> Self {
            libm::ceilf(self)
        }
        fn floor(self) -> Self {
            libm::floorf(self)
        }
        fn round(self) -> Self {
            libm::roundf(self)
        }
        fn atan2(self, x: Self) -> Self {
            libm::atan2f(self, x)
        }
        fn hypot(self, y: Self) -> Self {
            libm::hypotf(self, y)
        }
        fn cos(self) -> Self {
            libm::cosf(self)
        }
        fn sin(self) -> Self {
            libm::sinf(self)
        }
        fn rem_euclid(self, rhs: Self) -> Self {
            let r = self % rhs;
            if r < 0.0 {
                r + rhs.abs()
            } else {
                r
            }
        }
    }
}

/// Fira Sans Regular 4.203, the file iced 0.14 embeds with its `fira-sans`
/// feature, byte for byte.
pub const FIRA_SANS_REGULAR: &[u8] = include_bytes!("../fonts/FiraSans-Regular.ttf");

/// The most a street's straight run may turn at a vertex, in degrees.
pub const STRAIGHT_DEG: f64 = 12.0;
/// The halo's width in logical pixels.
pub const HALO: f32 = 1.5;
/// The space around a name no other may enter, in logical pixels.
pub const MARGIN: f32 = 3.0;
/// How far apart two names of one street stand, in logical pixels.
pub const REPEAT: f32 = 300.0;

/// A name's text size in logical pixels, by what it names.
pub fn size_of(kind: LabelKind) -> f32 {
    match kind {
        LabelKind::Country => 17.0,
        LabelKind::City => 16.0,
        LabelKind::Sea => 15.0,
        LabelKind::Town => 14.0,
        LabelKind::District | LabelKind::Village => 13.0,
        LabelKind::MajorRoad | LabelKind::SecondaryRoad | LabelKind::MinorRoad => 11.0,
        _ => 12.0,
    }
}

/// A name's text colour and its halo's, RGB, in `theme`: the ground's
/// colour as the halo, waters blue, parks green, ranges and deserts in
/// the earth's own tones (1683).
pub fn colours(kind: LabelKind, theme: MapTheme) -> ([u8; 3], [u8; 3]) {
    let dark = theme == MapTheme::Dark;
    let ink = match (kind, dark) {
        (LabelKind::Water | LabelKind::River | LabelKind::Sea, false) => [0x1f, 0x4e, 0x8c],
        (LabelKind::Water | LabelKind::River | LabelKind::Sea, true) => [0x8c, 0xb4, 0xe6],
        (LabelKind::Park, false) => [0x2e, 0x6b, 0x2e],
        (LabelKind::Park, true) => [0x94, 0xc8, 0x8c],
        (LabelKind::Range, false) => [0x6e, 0x5a, 0x44],
        (LabelKind::Range, true) => [0xb8, 0xa4, 0x8c],
        (LabelKind::Desert, false) => [0x8a, 0x6a, 0x38],
        (LabelKind::Desert, true) => [0xd0, 0xb0, 0x78],
        (_, false) => [0x26, 0x26, 0x26],
        (_, true) => [0xe6, 0xe6, 0xe6],
    };
    let halo = match dark {
        false => [0xff, 0xff, 0xff],
        true => [0x18, 0x18, 0x18],
    };
    (ink, halo)
}

/// One name's glyph coverage, unturned: `w` by `h`, a byte a pixel, the
/// baseline `base` rows from the top, `pad` pixels of room on every side
/// for the halo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub pad: usize,
    pub alpha: Vec<u8>,
}

impl Mask {
    fn at(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return 0;
        }
        self.alpha[y as usize * self.w + x as usize]
    }

    /// The coverage widened by `r` pixels: each pixel the strongest within
    /// a disc of radius `r`, the halo's shape, a pixel off the mask
    /// counting as nothing.
    ///
    /// The disc is a stack of row spans: on the row `dy` away it holds the
    /// offsets `dx` with `dx * dx + dy * dy <= r * r + 0.5`, all of
    /// `-half..=half` for one `half`. So each distinct half width gets one
    /// plane of horizontal maxes, and a pixel is the max over the disc's
    /// rows of the plane of that row's half width: the same bytes as the
    /// max over the disc's pixels ([`widened_by_disc`](Self::widened_by_disc)),
    /// at a few reads a pixel and a row instead of `(2r + 1)^2` (1154).
    fn widened(&self, r: f32) -> Mask {
        let ri = r.ceil() as i64;
        let (w, h) = (self.w, self.h);
        // The disc's half width on each row from `-ri` to `ri`, none where
        // even `dx = 0` is outside it.
        let half: Vec<Option<usize>> = (-ri..=ri)
            .map(|dy| {
                (0..=ri)
                    .rev()
                    .find(|&dx| ((dx * dx + dy * dy) as f32) <= r * r + 0.5)
                    .map(|dx| dx as usize)
            })
            .collect();
        let mut widths: Vec<usize> = half.iter().flatten().copied().collect();
        widths.sort_unstable();
        widths.dedup();
        let planes: Vec<Vec<u8>> = widths
            .iter()
            .map(|&hx| {
                let mut plane = vec![0u8; self.alpha.len()];
                for (row, out) in self.alpha.chunks_exact(w).zip(plane.chunks_exact_mut(w)) {
                    for (x, o) in out.iter_mut().enumerate() {
                        let span = &row[x.saturating_sub(hx)..(x + hx + 1).min(w)];
                        *o = span.iter().copied().max().unwrap_or(0);
                    }
                }
                plane
            })
            .collect();
        let mut out = vec![0u8; self.alpha.len()];
        for (k, hx) in half.iter().enumerate() {
            let Some(hx) = hx else {
                continue;
            };
            let Ok(p) = widths.binary_search(hx) else {
                continue;
            };
            let dy = k as i64 - ri;
            for y in 0..h as i64 {
                let sy = y + dy;
                if sy < 0 || sy >= h as i64 {
                    continue;
                }
                let src = &planes[p][sy as usize * w..(sy as usize + 1) * w];
                let dst = &mut out[y as usize * w..(y as usize + 1) * w];
                for (d, &s) in dst.iter_mut().zip(src) {
                    *d = (*d).max(s);
                }
            }
        }
        Mask {
            alpha: out,
            ..self.clone()
        }
    }

    /// [`widened`](Self::widened) as it was written first, the max over
    /// every pixel of the disc: the reference the span version is held to.
    #[cfg(test)]
    fn widened_by_disc(&self, r: f32) -> Mask {
        let ri = r.ceil() as i64;
        let mut out = vec![0u8; self.alpha.len()];
        for y in 0..self.h as i64 {
            for x in 0..self.w as i64 {
                let mut best = 0u8;
                for dy in -ri..=ri {
                    for dx in -ri..=ri {
                        if ((dx * dx + dy * dy) as f32) <= r * r + 0.5 {
                            best = best.max(self.at(x + dx, y + dy));
                        }
                    }
                }
                out[y as usize * self.w + x as usize] = best;
            }
        }
        Mask {
            alpha: out,
            ..self.clone()
        }
    }

    /// Half the size, each pixel the mean of the two by two it covers,
    /// those past the edge counting as nothing.
    fn halved(&self) -> Mask {
        let (w, h) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let mut alpha = vec![0u8; w * h];
        for (y, row) in alpha.chunks_exact_mut(w).enumerate() {
            for (x, a) in row.iter_mut().enumerate() {
                let (sx, sy) = (2 * x as i64, 2 * y as i64);
                let sum = u32::from(self.at(sx, sy))
                    + u32::from(self.at(sx + 1, sy))
                    + u32::from(self.at(sx, sy + 1))
                    + u32::from(self.at(sx + 1, sy + 1));
                *a = ((sum + 2) / 4) as u8;
            }
        }
        Mask {
            w,
            h,
            pad: self.pad / 2,
            alpha,
        }
    }

    /// Bilinear coverage at a fractional pixel.
    fn sample(&self, x: f64, y: f64) -> f64 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let a = |dx: i64, dy: i64| f64::from(self.at(x0 + dx, y0 + dy));
        let top = a(0, 0) * (1.0 - fx) + a(1, 0) * fx;
        let bottom = a(0, 1) * (1.0 - fx) + a(1, 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

#[cfg(feature = "screenmap")]
/// A name's text set one way: the text, its size's bits, the pad, slanted.
type SetKey = (String, u32, usize, bool);

/// What the label pass asks of a font (1565): a name's size shaped and
/// its coverage drawn, at `size` pixels with `pad` pixels around, slanted
/// with `italic`. The window and the phone shape with cosmic-text
/// ([`Typesetter`]); the M9, which has no `std`, lays its glyphs out
/// itself (`loquax_m9::glyphs`). Both set Fira Sans
/// ([`FIRA_SANS_REGULAR`]), so the picture is one picture.
pub trait Shaper {
    /// The size of the mask [`mask`](Self::mask) makes for the same
    /// arguments: `(width, height)` with the pad on every side.
    fn extent(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> (usize, usize);

    /// `text`'s coverage at `size` pixels, with `pad` pixels around;
    /// slanted with `italic`.
    fn mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Mask;

    /// [`mask`](Self::mask), kept where the shaper keeps masks.
    fn kept_mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Arc<Mask> {
        Arc::new(self.mask(text, size, pad, italic))
    }

    /// The halo of `mask`, made by [`kept_mask`](Self::kept_mask) for the
    /// same text: its coverage widened by `r` pixels, kept where the
    /// shaper keeps halos.
    #[allow(clippy::too_many_arguments)]
    fn kept_halo(
        &mut self,
        text: &str,
        size: f32,
        pad: usize,
        italic: bool,
        r: f32,
        mask: &Mask,
    ) -> Arc<Mask> {
        let _ = (text, size, pad, italic);
        Arc::new(mask.widened(r))
    }

    /// Set every name anew, the way before 1154: the reference a test
    /// holds the kept path to.
    #[cfg(test)]
    fn unkept(&self) -> bool {
        false
    }
}

#[cfg(feature = "screenmap")]
/// The most masks, and the most halos, a [`Typesetter`] keeps.
pub const MASKS_KEPT: usize = 8192;
#[cfg(feature = "screenmap")]
/// The most bytes of masks and halos together a [`Typesetter`] keeps:
/// at the phone's density a frame over a city at z13 asks about 7 MB of
/// them, so a bound below that drops them all every frame and shapes and
/// rasterises them again (1225: 32 ms of a placement at 4 MB).
pub const MASK_BYTES_KEPT: usize = 16 << 20;
#[cfg(feature = "screenmap")]
/// The most text sizes a [`Typesetter`] keeps.
pub const EXTENTS_KEPT: usize = 32768;

#[cfg(feature = "screenmap")]
/// Shapes and rasterises names in the embedded font.
///
/// It keeps what a frame at rest sets again in the next one (1154): every
/// name in view is shaped for its size, but only one with a spot wholly in
/// the picture is rasterised, and only one the rule placed gets a halo.
/// The sizes, the masks and the halos are kept by text, size, pad and
/// slant, and a frame that sets the same names looks them up. Each store
/// is dropped whole when it passes its bound ([`EXTENTS_KEPT`],
/// [`MASKS_KEPT`], [`MASK_BYTES_KEPT`]), which a pan across a city takes
/// many frames to reach. What is kept is what would be made again, so a
/// picture is the same bytes either way.
pub struct Typesetter {
    fonts: FontSystem,
    cache: SwashCache,
    extents: HashMap<SetKey, (usize, usize)>,
    masks: HashMap<SetKey, Arc<Mask>>,
    /// By the mask's key and the halo's width's bits.
    halos: HashMap<(SetKey, u32), Arc<Mask>>,
    /// The bytes of `masks` and `halos`.
    kept: usize,
    /// Set every name anew, the way before 1154: the reference a test
    /// holds the kept path to.
    #[cfg(test)]
    pub(crate) unkept: bool,
}

#[cfg(feature = "screenmap")]
impl Default for Typesetter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "screenmap")]
impl Typesetter {
    /// A font system holding Fira Sans alone, so no host font changes a
    /// picture.
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_font_data(FIRA_SANS_REGULAR.to_vec());
        Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".to_string(), db),
            cache: SwashCache::new(),
            extents: HashMap::new(),
            masks: HashMap::new(),
            halos: HashMap::new(),
            kept: 0,
            #[cfg(test)]
            unkept: false,
        }
    }

    /// Masks and halos kept, and their bytes.
    pub fn kept(&self) -> (usize, usize) {
        (self.masks.len() + self.halos.len(), self.kept)
    }

    /// Drop every size, mask and halo kept: a measuring tool's first
    /// frame.
    pub fn forget(&mut self) {
        self.extents.clear();
        self.masks.clear();
        self.halos.clear();
        self.kept = 0;
    }

    /// The size [`mask`](Self::mask) makes for the same arguments, shaped
    /// and not drawn, kept.
    pub fn extent(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> (usize, usize) {
        let key = (text.to_string(), size.to_bits(), pad, italic);
        if let Some(&e) = self.extents.get(&key) {
            return e;
        }
        let buffer = self.shaped(text, size, italic);
        let e = extent_of(&buffer, size, pad, italic);
        if self.extents.len() >= EXTENTS_KEPT {
            self.extents.clear();
        }
        self.extents.insert(key, e);
        e
    }

    /// [`mask`](Self::mask), kept.
    pub fn kept_mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Arc<Mask> {
        let key = (text.to_string(), size.to_bits(), pad, italic);
        if let Some(m) = self.masks.get(&key) {
            return m.clone();
        }
        let m = Arc::new(self.mask(text, size, pad, italic));
        self.keep(m.alpha.len());
        self.masks.insert(key, m.clone());
        m
    }

    /// The halo of [`kept_mask`](Self::kept_mask)'s mask, its coverage
    /// widened by `r` pixels, kept.
    pub fn kept_halo(
        &mut self,
        text: &str,
        size: f32,
        pad: usize,
        italic: bool,
        r: f32,
    ) -> Arc<Mask> {
        let key = ((text.to_string(), size.to_bits(), pad, italic), r.to_bits());
        if let Some(m) = self.halos.get(&key) {
            return m.clone();
        }
        let halo = Arc::new(self.kept_mask(text, size, pad, italic).widened(r));
        self.keep(halo.alpha.len());
        self.halos.insert(key, halo.clone());
        halo
    }

    /// Room for `bytes` more of masks and halos: all dropped when they
    /// would pass a bound.
    fn keep(&mut self, bytes: usize) {
        let full = self.masks.len().max(self.halos.len()) >= MASKS_KEPT;
        if full || self.kept + bytes > MASK_BYTES_KEPT {
            self.masks.clear();
            self.halos.clear();
            self.kept = 0;
        }
        self.kept += bytes;
    }

    /// `text`'s width at `size` pixels, shaped and not drawn: what a
    /// screen without iced measures a pin's label with.
    pub fn width(&mut self, text: &str, size: f32) -> u32 {
        let buffer = self.shaped(text, size, false);
        let width = buffer
            .layout_runs()
            .fold(0f32, |width, run| width.max(run.line_w));
        width.ceil() as u32
    }

    fn shaped(&mut self, text: &str, size: f32, italic: bool) -> Buffer {
        let metrics = ct::Metrics::new(size, (size * 1.25).ceil());
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(&mut self.fonts, None, None);
        let mut attrs = Attrs::new().family(Family::Name("Fira Sans"));
        if italic {
            attrs = attrs.cache_key_flags(ct::CacheKeyFlags::FAKE_ITALIC);
        }
        buffer.set_text(&mut self.fonts, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }

    /// `text`'s coverage at `size` pixels, with `pad` pixels around;
    /// slanted with `italic`.
    pub fn mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Mask {
        let buffer = self.shaped(text, size, italic);
        let (w, h) = extent_of(&buffer, size, pad, italic);
        let mut alpha = vec![0u8; w * h];
        let white = ct::Color::rgb(0xff, 0xff, 0xff);
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            white,
            |x, y, cw, ch, c| {
                for yy in y..y + ch as i32 {
                    for xx in x..x + cw as i32 {
                        let (px, py) = (xx + pad as i32, yy + pad as i32);
                        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                            let i = py as usize * w + px as usize;
                            alpha[i] = alpha[i].max(c.a());
                        }
                    }
                }
            },
        );
        Mask { w, h, pad, alpha }
    }
}

#[cfg(feature = "screenmap")]
impl Shaper for Typesetter {
    fn extent(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> (usize, usize) {
        Typesetter::extent(self, text, size, pad, italic)
    }

    fn mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Mask {
        Typesetter::mask(self, text, size, pad, italic)
    }

    fn kept_mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Arc<Mask> {
        Typesetter::kept_mask(self, text, size, pad, italic)
    }

    fn kept_halo(
        &mut self,
        text: &str,
        size: f32,
        pad: usize,
        italic: bool,
        r: f32,
        _mask: &Mask,
    ) -> Arc<Mask> {
        Typesetter::kept_halo(self, text, size, pad, italic, r)
    }

    #[cfg(test)]
    fn unkept(&self) -> bool {
        self.unkept
    }
}

#[cfg(feature = "screenmap")]
/// The size of the mask of the shaped `buffer`, set at `size` pixels with
/// `pad` pixels around, slanted with `italic`.
fn extent_of(buffer: &Buffer, size: f32, pad: usize, italic: bool) -> (usize, usize) {
    let (mut width, mut height) = (0f32, 0f32);
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        height = height.max(run.line_top + run.line_height);
    }
    // The slant leans the glyphs' tops right by about a quarter of
    // the size: room for it past the advance.
    let lean = match italic {
        true => (size * 0.25).ceil() as usize,
        false => 0,
    };
    (
        width.ceil() as usize + lean + 2 * pad,
        height.ceil() as usize + 2 * pad,
    )
}

/// The runs of `line` whose vertices turn less than [`STRAIGHT_DEG`], in
/// pixels, longest first (in line order where two are as long): each its
/// middle, its angle in radians turned to read left to right, and its
/// length. The places a name may go along its line, so a name whose best
/// run is taken tries the next; none for a line without a segment.
pub fn straight_runs(line: &[Vec<(f64, f64)>]) -> Vec<((f64, f64), f64, f64)> {
    let mut runs: Vec<((f64, f64), f64, f64)> = Vec::new();
    let limit = STRAIGHT_DEG.to_radians();
    for piece in line {
        let mut start = 0;
        while start + 1 < piece.len() {
            // Grow the run while each next segment turns little against the
            // run's first one.
            let dir = |i: usize| {
                let (a, b) = (piece[i], piece[i + 1]);
                (b.1 - a.1).atan2(b.0 - a.0)
            };
            let first = dir(start);
            let mut end = start + 1;
            while end + 1 < piece.len() {
                let turn = (dir(end) - first + core::f64::consts::PI)
                    .rem_euclid(2.0 * core::f64::consts::PI)
                    - core::f64::consts::PI;
                if turn.abs() >= limit {
                    break;
                }
                end += 1;
            }
            let (a, b) = (piece[start], piece[end]);
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            if len > 0.0 {
                let mut angle = (b.1 - a.1).atan2(b.0 - a.0);
                if angle > core::f64::consts::FRAC_PI_2 {
                    angle -= core::f64::consts::PI;
                } else if angle <= -core::f64::consts::FRAC_PI_2 {
                    angle += core::f64::consts::PI;
                }
                runs.push((((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), angle, len));
            }
            start = end;
        }
    }
    runs.sort_by(|a, b| b.2.total_cmp(&a.2));
    runs
}

/// One name the rule placed (1134): where its middle went, in the
/// picture's pixels, at which angle, its glyphs and its halo. Kept as a
/// [`Sprite`], a placed name is drawn again where the view moved it, so a
/// frame that moves shows the names its rest frame set and does not place
/// them again.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedName {
    pub text: String,
    pub kind: LabelKind,
    /// The shallowest view zoom that shows it ([`ViewLabel::min_zoom`]).
    pub min_zoom: u8,
    /// The boxes the rule placed it by, in the picture's pixels.
    pub boxes: Vec<Rect>,
    pub centre: (f64, f64),
    pub angle: f64,
    /// Shared with the typesetter's store: a frame that places its names
    /// again copies none (1225).
    pub mask: Arc<Mask>,
    pub halo: Arc<Mask>,
}

/// One name to set: where, at which angle, its text and size, and its
/// mask once a spot made it a candidate.
struct Setting {
    centre: (f64, f64),
    angle: f64,
    mask: Arc<Mask>,
    size: f32,
    kind: LabelKind,
    min_zoom: u8,
}

/// Set the names of `names` that the rule places into `rgba`, `w` by `h`
/// pixels, at `scale` screen pixels a logical one, after `taken` (the pins
/// and their names, in screen pixels). Returns the names set, in the
/// order the rule placed them.
#[allow(clippy::too_many_arguments)]
pub fn set(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    names: &[ViewLabel],
    taken: &[Rect],
    scale: f32,
    theme: MapTheme,
    type_: &mut (impl Shaper + ?Sized),
) -> Vec<String> {
    let px = |v: (i64, i64)| (v.0 as f64 / SUBPIXEL as f64, v.1 as f64 / SUBPIXEL as f64);
    let placed = place(w, h, names, px, taken, scale, type_);
    paint_placed(rgba, w, h, &placed, theme);
    placed.into_iter().map(|p| p.text).collect()
}

/// The names of `names` that the rule places in a picture of `w` by `h`
/// pixels, at `scale` screen pixels a logical one, after `taken`; `px`
/// takes a name's sub-pixel position in the view it was looked up in to
/// the picture's pixels, which for a view between two zoom steps scales
/// it (1134). In the order the rule placed them.
#[allow(clippy::too_many_arguments)]
pub fn place<'a>(
    w: u32,
    h: u32,
    names: impl IntoIterator<Item = &'a ViewLabel>,
    px: impl Fn((i64, i64)) -> (f64, f64),
    taken: &[Rect],
    scale: f32,
    type_: &mut (impl Shaper + ?Sized),
) -> Vec<PlacedName> {
    let pad = (HALO * scale).ceil() as usize + 1;
    let (wf, hf) = (f64::from(w), f64::from(h));
    let mut settings: Vec<Setting> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for l in names {
        let river = l.kind == LabelKind::River;
        let size = size_of(l.kind) * scale;
        // Its spots need only its size; its glyphs are drawn once a spot
        // lies wholly in the picture.
        #[cfg(test)]
        let mut mask: Option<Arc<Mask>> = type_
            .unkept()
            .then(|| Arc::new(type_.mask(&l.text, size, pad, river)));
        #[cfg(not(test))]
        let mut mask: Option<Arc<Mask>> = None;
        let (mw, mh) = match &mask {
            Some(m) => (m.w, m.h),
            None => type_.extent(&l.text, size, pad, river),
        };
        let (tw, th) = ((mw - 2 * pad) as f64, (mh - 2 * pad) as f64);
        // Where the name may go, best first: a point's anchor, or each
        // straight run of a line long enough to hold it.
        let spots: Vec<((f64, f64), f64)> = match l.kind.is_line() {
            false => vec![(px(l.anchor), 0.0)],
            true => {
                let line: Vec<Vec<(f64, f64)>> = l
                    .line
                    .iter()
                    .map(|p| p.iter().map(|&v| px(v)).collect())
                    .collect();
                straight_runs(&line)
                    .into_iter()
                    .take_while(|&(_, _, len)| len >= tw + th)
                    .flat_map(|(mid, angle, len)| along_run(mid, angle, len, tw + th))
                    .collect()
            }
        };
        for (centre, angle) in spots {
            let boxes = boxes_of(centre, angle, tw, th);
            let whole = boxes
                .iter()
                .all(|b| b.x0 >= 0 && b.y0 >= 0 && f64::from(b.x1) <= wf && f64::from(b.y1) <= hf);
            if !whole {
                continue;
            }
            candidates.push(Candidate {
                kind: l.kind,
                rank: l.rank,
                text: l.text.clone(),
                at: l.at,
                boxes,
            });
            let mask = mask
                .get_or_insert_with(|| type_.kept_mask(&l.text, size, pad, river))
                .clone();
            settings.push(Setting {
                centre,
                angle,
                mask,
                size,
                kind: l.kind,
                min_zoom: l.min_zoom,
            });
        }
    }
    let rule = Rule {
        margin: (MARGIN * scale).round() as i32,
        block: (0, 0),
        repeat: (REPEAT * scale).round() as i32,
    };
    let placed = labels::place(&candidates, taken, rule);
    placed
        .iter()
        .map(|&i| {
            let s = &settings[i];
            let text = &candidates[i].text;
            let river = s.kind == LabelKind::River;
            #[cfg(test)]
            let halo = match type_.unkept() {
                true => Arc::new(s.mask.widened_by_disc(HALO * scale)),
                false => type_.kept_halo(text, s.size, pad, river, HALO * scale, &s.mask),
            };
            #[cfg(not(test))]
            let halo = type_.kept_halo(text, s.size, pad, river, HALO * scale, &s.mask);
            PlacedName {
                text: text.clone(),
                kind: s.kind,
                min_zoom: s.min_zoom,
                boxes: candidates[i].boxes.clone(),
                centre: s.centre,
                angle: s.angle,
                halo,
                mask: s.mask.clone(),
            }
        })
        .collect()
}

/// Draw `placed` into `rgba`, `w` by `h` pixels, in `theme`'s colours,
/// where the rule placed them: the halos of all first would cover a
/// neighbour's glyphs, so each name's halo then its glyphs, in the order
/// placed.
pub fn paint_placed(rgba: &mut [u8], w: u32, h: u32, placed: &[PlacedName], theme: MapTheme) {
    for p in placed {
        let (ink, halo) = colours(p.kind, theme);
        paint(rgba, w, h, &p.halo, p.centre, p.angle, halo);
        paint(rgba, w, h, &p.mask, p.centre, p.angle, ink);
    }
}

/// [`paint_placed`] into a target of the panel's colours (1565, the
/// M9): each name's halo then its glyphs, blended by the target
/// ([`Coverage::blend`]) at the coverage the RGBA paint blends with, in
/// the colours [`colours`] gives, made Rgb565. A target that does not
/// blend takes a pixel the glyphs cover at least half as the ink, and
/// the halo likewise.
pub fn paint_placed_on<C>(target: &mut C, placed: &[PlacedName], theme: MapTheme)
where
    C: Coverage<Color = Rgb565> + ?Sized,
{
    for p in placed {
        let (ink, halo) = colours(p.kind, theme);
        paint_on(target, &p.halo, p.centre, p.angle, halo);
        paint_on(target, &p.mask, p.centre, p.angle, ink);
    }
}

/// [`paint`] into a [`Coverage`] target.
fn paint_on<C>(target: &mut C, mask: &Mask, centre: (f64, f64), angle: f64, colour: [u8; 3])
where
    C: Coverage<Color = Rgb565> + ?Sized,
{
    let size = target.bounding_box().size;
    let (w, h) = (i64::from(size.width), i64::from(size.height));
    let colour: Rgb565 = Rgb888::new(colour[0], colour[1], colour[2]).into();
    let smooth = target.smooth();
    let turn = (angle.cos(), angle.sin());
    let (x0, x1, y0, y1) = reach(mask, centre);
    let (x0, x1) = (x0.max(0), x1.min(w));
    let (y0, y1) = (y0.max(0), y1.min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let a = level(cover_turned(mask, centre, angle, turn, x, y));
            let (Ok(xi), Ok(yi)) = (i32::try_from(x), i32::try_from(y)) else {
                continue;
            };
            let _ = match (a, smooth) {
                (0, _) => Ok(()),
                (255, _) => target.draw_iter(core::iter::once(embedded_graphics::Pixel(
                    embedded_graphics::prelude::Point::new(xi, yi),
                    colour,
                ))),
                (a, true) => target.blend(xi, yi, colour, a),
                (a, false) if a >= 128 => {
                    target.draw_iter(core::iter::once(embedded_graphics::Pixel(
                        embedded_graphics::prelude::Point::new(xi, yi),
                        colour,
                    )))
                }
                _ => Ok(()),
            };
        }
    }
}

/// The middles a name `need` pixels long may take on a straight run of
/// `len` pixels about `mid` at `angle`: the run's middle first, then half
/// a name further each way in turn while the name stays on the run.
fn along_run(mid: (f64, f64), angle: f64, len: f64, need: f64) -> Vec<((f64, f64), f64)> {
    let step = need / 2.0;
    let room = (len - need) / 2.0;
    let mut out = vec![(mid, angle)];
    let mut d = step;
    while d <= room {
        for along in [d, -d] {
            out.push((
                (mid.0 + along * angle.cos(), mid.1 + along * angle.sin()),
                angle,
            ));
        }
        d += step;
    }
    out
}

/// The boxes a name of `tw` by `th` pixels covers, its middle at `centre`,
/// turned by `angle`: one for a level name, squares of its height along
/// the baseline for a turned one.
fn boxes_of(centre: (f64, f64), angle: f64, tw: f64, th: f64) -> Vec<Rect> {
    match angle == 0.0 {
        true => vec![Rect {
            x0: (centre.0 - tw / 2.0).floor() as i32,
            y0: (centre.1 - th / 2.0).floor() as i32,
            x1: (centre.0 + tw / 2.0).ceil() as i32,
            y1: (centre.1 + th / 2.0).ceil() as i32,
        }],
        // Squares of the text's height along the baseline.
        false => {
            let n = (tw / th).ceil().max(1.0) as i32;
            (0..n)
                .map(|i| {
                    let along = -tw / 2.0 + th * (f64::from(i) + 0.5);
                    let (cx, cy) = (
                        centre.0 + along * angle.cos(),
                        centre.1 + along * angle.sin(),
                    );
                    Rect {
                        x0: (cx - th / 2.0).floor() as i32,
                        y0: (cy - th / 2.0).floor() as i32,
                        x1: (cx + th / 2.0).ceil() as i32,
                        y1: (cy + th / 2.0).ceil() as i32,
                    }
                })
                .collect()
        }
    }
}

/// Blend `mask` in `colour` into `rgba`, its middle at `centre`, turned by
/// `angle`: each pixel of the turned mask's box samples the mask through
/// the inverse turn.
fn paint(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    mask: &Mask,
    centre: (f64, f64),
    angle: f64,
    colour: [u8; 3],
) {
    paint_faded(rgba, w, h, mask, centre, angle, colour, 1.0);
}

/// [`paint`] at `opacity`: a pin's dimmed label.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_faded(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    mask: &Mask,
    centre: (f64, f64),
    angle: f64,
    colour: [u8; 3],
    opacity: f64,
) {
    let (x0, x1, y0, y1) = reach(mask, centre);
    let (x0, x1) = (x0.max(0), x1.min(i64::from(w)));
    let (y0, y1) = (y0.max(0), y1.min(i64::from(h)));
    for y in y0..y1 {
        for x in x0..x1 {
            let a = cover(mask, centre, angle, x, y) * opacity;
            blend_at(rgba, w, x, y, colour, a);
        }
    }
}

/// The pixels a mask turned about `centre` may cover, `x0..x1` and
/// `y0..y1`, the frame not counted.
fn reach(mask: &Mask, centre: (f64, f64)) -> (i64, i64, i64, i64) {
    let r = ((mask.w as f64).hypot(mask.h as f64) / 2.0).ceil();
    (
        (centre.0 - r).floor() as i64,
        (centre.0 + r).ceil() as i64,
        (centre.1 - r).floor() as i64,
        (centre.1 + r).ceil() as i64,
    )
}

/// How much of pixel `(x, y)` `mask` covers, its middle at `centre`,
/// turned by `angle`: the mask sampled through the inverse turn, from 0
/// to 1.
fn cover(mask: &Mask, centre: (f64, f64), angle: f64, x: i64, y: i64) -> f64 {
    cover_turned(mask, centre, angle, (angle.cos(), angle.sin()), x, y)
}

/// [`cover`] with the angle's cosine and sine `(c, s)` taken once by the
/// caller.
fn cover_turned(
    mask: &Mask,
    centre: (f64, f64),
    angle: f64,
    (c, s): (f64, f64),
    x: i64,
    y: i64,
) -> f64 {
    let (mw, mh) = (mask.w as f64, mask.h as f64);
    let (dx, dy) = (x as f64 + 0.5 - centre.0, y as f64 + 0.5 - centre.1);
    // Into the mask's frame: turn back by the angle.
    let (mx, my) = (
        dx * c + dy * s + mw / 2.0 - 0.5,
        -dx * s + dy * c + mh / 2.0 - 0.5,
    );
    let a = match angle == 0.0 {
        true => f64::from(mask.at(mx.round() as i64, my.round() as i64)),
        false => mask.sample(mx, my),
    };
    a / 255.0
}

/// Blend `colour` at `a` into the RGBA pixel `(x, y)` of a frame `w`
/// wide, when `a` is more than nothing.
fn blend_at(rgba: &mut [u8], w: u32, x: i64, y: i64, colour: [u8; 3], a: f64) {
    if a <= 0.0 {
        return;
    }
    let i = ((y as usize) * (w as usize) + x as usize) * 4;
    if let Some(px) = rgba.get_mut(i..i + 3) {
        for (p, &k) in px.iter_mut().zip(colour.iter()) {
            *p = (f64::from(*p) * (1.0 - a) + f64::from(k) * a).round() as u8;
        }
    }
}

/// A placed name's coverage, its halo's and its glyphs', worked out once
/// where it was placed (1134): drawing it again where the view moved it
/// costs a blend a covered pixel, so a frame while the finger moves keeps
/// every name of the frame at rest.
///
/// The coverage is kept as a box of `(halo, ink)` levels out of 255 over
/// the pixels it covers, blended in integers row by row (1156). Moved by
/// whole pixels a level name is the same pixels [`paint_placed`] sets
/// there: its levels are the mask's own bytes, and the integer blend
/// rounds as the float one does. A turned name's bilinear coverage is
/// rounded to a level, so a channel may differ from the float paint by a
/// step, and by two where the halo's step carries into the glyphs' blend
/// at an edge both cover partly (4 of 460800 bytes in the phone's golden;
/// levels of 16 bits would hold it to one at twice the memory).
#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub text: String,
    pub kind: LabelKind,
    /// The shallowest view zoom that shows it.
    pub min_zoom: u8,
    /// The boxes the rule placed it by, where it was placed: what a frame
    /// that moves holds it apart from the others by (1205).
    pub boxes: Vec<Rect>,
    /// Where its middle was placed.
    pub centre: (f64, f64),
    /// The box's top left pixel where it was placed.
    at: (i64, i64),
    /// The box's width in pixels.
    w: usize,
    /// How much the halo and the glyphs cover each pixel of the box, row
    /// by row, out of 255.
    cover: Vec<[u8; 2]>,
}

/// Below this scale a sprite drawn smaller samples its name's [`Mip`]
/// ([`Sprite::scaled`]): a pixel then spans more than one and a third of
/// the name's, and a single sample of the whole would miss strokes.
pub const MIP_BELOW: f64 = 0.75;

/// A name's halo and glyphs at half their size, each pixel the mean of
/// two by two (1225): what a pinch's frame at the step below samples, so
/// a pixel that spans two of the name's sees both. Made once a name and
/// kept while it shows.
#[derive(Clone, Debug)]
pub struct Mip {
    halo: Mask,
    ink: Mask,
}

impl Mip {
    pub fn of(name: &PlacedName) -> Self {
        Mip {
            halo: name.halo.halved(),
            ink: name.mask.halved(),
        }
    }
}

/// Where a name's mask, turned about its middle and drawn at a scale, may
/// cover a row of pixels: what a sprite samples, the rest of its box left
/// empty, so a turned street's name samples its own band and not the
/// square around it (1225).
#[derive(Clone, Copy)]
struct Reach {
    centre: (f64, f64),
    /// The turn's cosine and sine over the scale.
    turn: (f64, f64),
    mask: (f64, f64),
}

impl Reach {
    fn of(name: &PlacedName, centre: (f64, f64), k: f64) -> Self {
        let turn = (name.angle.cos() / k, name.angle.sin() / k);
        let mask = (name.mask.w as f64, name.mask.h as f64);
        Reach { centre, turn, mask }
    }

    /// The columns of row `y`, within `x0..x1`, a sample may cover: where
    /// the inverse turn of a pixel's middle lands within a pixel of the
    /// mask, and two pixels more each way for a sample off the middle.
    fn row(&self, y: i64, (x0, x1): (i64, i64)) -> (i64, i64) {
        let (c, s) = self.turn;
        let dy = y as f64 + 0.5 - self.centre.1;
        let x = 0.5 - self.centre.0;
        // The mask's column and row at column x of the row: a x + b.
        let (a_m, b_m) = (c, x * c + dy * s + self.mask.0 / 2.0 - 0.5);
        let (a_n, b_n) = (-s, -x * s + dy * c + self.mask.1 / 2.0 - 0.5);
        let within = |a: f64, b: f64, hi: f64| -> Option<(f64, f64)> {
            match a.abs() < 1e-12 {
                true => (b > -1.0 && b < hi).then_some((f64::NEG_INFINITY, f64::INFINITY)),
                false => {
                    let (p, q) = ((-1.0 - b) / a, (hi - b) / a);
                    Some((p.min(q), p.max(q)))
                }
            }
        };
        match (within(a_m, b_m, self.mask.0), within(a_n, b_n, self.mask.1)) {
            (Some(m), Some(n)) => {
                let (lo, hi) = (m.0.max(n.0), m.1.min(n.1));
                if lo > hi {
                    return (x0, x0);
                }
                let lo = (lo.floor() as i64).saturating_sub(2).max(x0);
                let hi = (hi.ceil() as i64).saturating_add(3).min(x1);
                (lo, hi.max(lo))
            }
            _ => (x0, x0),
        }
    }
}

/// A coverage from 0 to 1 as a level out of 255.
fn level(a: f64) -> u8 {
    (a * 255.0).round().clamp(0.0, 255.0) as u8
}

impl Sprite {
    /// `name` worked out where it was placed.
    ///
    /// Only the pixels of the turned mask's box and two more around it are
    /// sampled (1154): a pixel further out maps outside the mask, more
    /// than the bilinear sample's one pixel of reach past its edge, so it
    /// covers nothing, and the box kept is that of the whole `reach`. A
    /// halo of the mask's size is at least the mask on every pixel
    /// (`Mask::widened` keeps each pixel's own value in its max), and both
    /// are sampled at the same place with the same weights, so where the
    /// halo covers nothing the glyphs cover nothing and are not sampled.
    pub fn of(name: &PlacedName) -> Self {
        let (x0, x1, y0, y1) = reach(&name.mask, name.centre);
        let turn = (name.angle.cos(), name.angle.sin());
        let (c, s) = (turn.0.abs(), turn.1.abs());
        let (mw, mh) = (name.mask.w as f64, name.mask.h as f64);
        let (hx, hy) = ((mw * c + mh * s) / 2.0 + 2.0, (mw * s + mh * c) / 2.0 + 2.0);
        let same = (name.halo.w, name.halo.h) == (name.mask.w, name.mask.h);
        let x0 = x0.max((name.centre.0 - hx).floor() as i64);
        let x1 = x1.min((name.centre.0 + hx).ceil() as i64);
        let y0 = y0.max((name.centre.1 - hy).floor() as i64);
        let y1 = y1.min((name.centre.1 + hy).ceil() as i64);
        let at = (name, name.boxes.clone(), name.centre);
        let reach = Reach::of(name, name.centre, 1.0);
        Self::boxed(at, (x0, x1, y0, y1), Some(reach), |x, y| {
            let halo = cover_turned(&name.halo, name.centre, name.angle, turn, x, y);
            if same && halo <= 0.0 {
                return [0, 0];
            }
            let ink = cover_turned(&name.mask, name.centre, name.angle, turn, x, y);
            [level(halo), level(ink)]
        })
    }

    /// [`of`](Self::of) as it was written first, every pixel of the reach
    /// sampled: the reference the boxed loop is held to.
    #[cfg(test)]
    pub(crate) fn of_whole_reach(name: &PlacedName) -> Self {
        let at = (name, name.boxes.clone(), name.centre);
        Self::boxed(at, reach(&name.mask, name.centre), None, |x, y| {
            let halo = cover(&name.halo, name.centre, name.angle, x, y);
            let ink = cover(&name.mask, name.centre, name.angle, x, y);
            [level(halo), level(ink)]
        })
    }

    /// `name` placed in a frame at screen size and drawn into another
    /// whose pixels are `k` of its own, its middle at `centre` there
    /// (1225): the frame of a pinch at its tile step, which the frontend
    /// scales by `1 / k`, so the name shows at the size and the place a
    /// frame at rest gives it. Each pixel samples the name's coverage
    /// bilinearly, below [`MIP_BELOW`] from `mip`, its coverage averaged
    /// two by two, so a pixel that spans two of the name's sees both. At
    /// `k` 1 where it was placed, [`of`](Self::of).
    pub fn scaled(name: &PlacedName, centre: (f64, f64), k: f64, mip: Option<&Mip>) -> Self {
        if k == 1.0 && centre == name.centre {
            return Self::of(name);
        }
        let k = k.max(1e-3);
        let turn = (name.angle.cos(), name.angle.sin());
        let (c, s) = (turn.0.abs(), turn.1.abs());
        let (mw, mh) = (name.mask.w as f64, name.mask.h as f64);
        let (hx, hy) = (
            ((mw * c + mh * s) / 2.0 + 2.0) * k,
            ((mw * s + mh * c) / 2.0 + 2.0) * k,
        );
        let x0 = (centre.0 - hx).floor() as i64;
        let x1 = (centre.0 + hx).ceil() as i64;
        let y0 = (centre.1 - hy).floor() as i64;
        let y1 = (centre.1 + hy).ceil() as i64;
        let same = (name.halo.w, name.halo.h) == (name.mask.w, name.mask.h);
        // The masks sampled, and how a pixel of the name maps into them.
        let (halo, ink, half) = match mip.filter(|_| k < MIP_BELOW) {
            Some(mip) => (&mip.halo, &mip.ink, true),
            None => (&*name.halo, &*name.mask, false),
        };
        let boxes = name
            .boxes
            .iter()
            .map(|b| {
                let at = |v: i32, from: f64, to: f64| (f64::from(v) - from) * k + to;
                Rect {
                    x0: at(b.x0, name.centre.0, centre.0).floor() as i32,
                    y0: at(b.y0, name.centre.1, centre.1).floor() as i32,
                    x1: at(b.x1, name.centre.0, centre.0).ceil() as i32,
                    y1: at(b.y1, name.centre.1, centre.1).ceil() as i32,
                }
            })
            .collect();
        let at = |x: f64, y: f64| {
            // The point in the frame the name was placed in.
            let (dx, dy) = ((x - centre.0) / k, (y - centre.1) / k);
            let (mx, my) = (
                dx * turn.0 + dy * turn.1 + mw / 2.0 - 0.5,
                -dx * turn.1 + dy * turn.0 + mh / 2.0 - 0.5,
            );
            let (mx, my) = match half {
                true => ((mx - 0.5) / 2.0, (my - 0.5) / 2.0),
                false => (mx, my),
            };
            let cover = halo.sample(mx, my);
            match same && cover <= 0.0 {
                true => (0.0, 0.0),
                false => (cover, ink.sample(mx, my)),
            }
        };
        Self::boxed(
            (name, boxes, centre),
            (x0, x1, y0, y1),
            Some(Reach::of(name, centre, k)),
            |x, y| {
                let (halo, ink) = at(x as f64 + 0.5, y as f64 + 0.5);
                [level(halo / 255.0), level(ink / 255.0)]
            },
        )
    }

    /// The sprite of `name`, its boxes and middle where it is drawn, from
    /// the levels `cover` gives the pixels `x0..x1` by `y0..y1`, cut to
    /// the box of those it covers.
    fn boxed(
        (name, boxes, centre): (&PlacedName, Vec<Rect>, (f64, f64)),
        (x0, x1, y0, y1): (i64, i64, i64, i64),
        reach: Option<Reach>,
        cover: impl Fn(i64, i64) -> [u8; 2],
    ) -> Self {
        let (sw, sh) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut sampled = Vec::with_capacity(sw * sh);
        // The covered box within the sampled one: columns, rows.
        let (mut c0, mut c1, mut r0, mut r1) = (sw, 0, sh, 0);
        for (r, y) in (y0..y1).enumerate() {
            let (xa, xb) = reach.map_or((x0, x1), |reach| reach.row(y, (x0, x1)));
            for (c, x) in (x0..x1).enumerate() {
                if x < xa || x >= xb {
                    sampled.push([0, 0]);
                    continue;
                }
                let both = cover(x, y);
                if both != [0, 0] {
                    (c0, c1, r0, r1) = (c0.min(c), c1.max(c + 1), r0.min(r), r1.max(r + 1));
                }
                sampled.push(both);
            }
        }
        let w = c1.saturating_sub(c0);
        let cover = match w {
            0 => Vec::new(),
            _ => (r0..r1)
                .flat_map(|r| sampled[r * sw + c0..r * sw + c1].iter().copied())
                .collect(),
        };
        Self {
            text: name.text.clone(),
            kind: name.kind,
            min_zoom: name.min_zoom,
            boxes,
            centre,
            at: (x0 + c0 as i64, y0 + r0 as i64),
            w,
            cover,
        }
    }

    /// The sprites of `names`, in their order, worked out on up to
    /// `workers` threads: thread `t` takes every `workers`-th name from
    /// the `t`-th, so a run of long street names does not land on one.
    #[cfg(feature = "std")]
    pub fn of_all(names: &[PlacedName], workers: usize) -> Vec<Self> {
        Self::all_of(names, workers, Self::of)
    }

    /// The sprite `make` gives each of `names`, in their order, on up to
    /// `workers` threads as [`of_all`](Self::of_all) deals them.
    #[cfg(feature = "std")]
    pub fn all_of<T: Sync>(
        names: &[T],
        workers: usize,
        make: impl Fn(&T) -> Self + Sync,
    ) -> Vec<Self> {
        let workers = workers.clamp(1, names.len().max(1));
        if workers == 1 {
            return names.iter().map(make).collect();
        }
        let make = &make;
        let dealt: Vec<Vec<Self>> = std::thread::scope(|scope| {
            let hands: Vec<_> = (0..workers)
                .map(|t| {
                    scope.spawn(move || names.iter().skip(t).step_by(workers).map(make).collect())
                })
                .collect();
            hands
                .into_iter()
                .map(|hand| {
                    hand.join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        });
        // Name `i` is the `i / workers`-th of hand `i % workers`.
        let mut hands: Vec<_> = dealt.into_iter().map(Vec::into_iter).collect();
        (0..names.len())
            .filter_map(|i| hands[i % workers].next())
            .collect()
    }

    /// The bytes its coverage holds: the measuring tool's sprite memory.
    pub fn bytes(&self) -> usize {
        core::mem::size_of_val(self.cover.as_slice())
    }

    /// Draw it into `rgba`, `w` by `h` pixels, in `theme`'s colours,
    /// moved by `(dx, dy)` pixels from where it was placed: each pixel the
    /// halo blended, then the glyphs.
    pub fn draw(&self, rgba: &mut [u8], w: u32, h: u32, theme: MapTheme, dx: i64, dy: i64) {
        self.draw_faded(rgba, w, h, theme, (dx, dy), 255);
    }

    /// [`draw`](Self::draw) at `alpha` out of 255, a name fading in or out
    /// (1205): each level scaled by it as the blend reads it, so at 255
    /// the bytes of [`draw`](Self::draw) and nothing drawn at 0.
    pub fn draw_faded(
        &self,
        rgba: &mut [u8],
        w: u32,
        h: u32,
        theme: MapTheme,
        (dx, dy): (i64, i64),
        alpha: u8,
    ) {
        if self.w == 0 || alpha == 0 {
            return;
        }
        let (ink, halo) = colours(self.kind, theme);
        let rows = (self.cover.len() / self.w) as i64;
        let (ox, oy) = (self.at.0 + dx, self.at.1 + dy);
        let (xa, xb) = (ox.max(0), (ox + self.w as i64).min(i64::from(w)));
        let (ya, yb) = (oy.max(0), (oy + rows).min(i64::from(h)));
        if xa >= xb || ya >= yb {
            return;
        }
        let n = (xb - xa) as usize;
        for y in ya..yb {
            let from = (y - oy) as usize * self.w + (xa - ox) as usize;
            let at = (y as usize * w as usize + xa as usize) * 4;
            let (Some(src), Some(row)) =
                (self.cover.get(from..from + n), rgba.get_mut(at..at + n * 4))
            else {
                return;
            };
            match alpha {
                255 => {
                    for (px, &[ha, ia]) in row.chunks_exact_mut(4).zip(src) {
                        blend_level(px, halo, ha);
                        blend_level(px, ink, ia);
                    }
                }
                _ => {
                    for (px, &[ha, ia]) in row.chunks_exact_mut(4).zip(src) {
                        blend_level(px, halo, faded(ha, alpha));
                        blend_level(px, ink, faded(ia, alpha));
                    }
                }
            }
        }
    }
}

/// The level `a` at `alpha`, both out of 255, rounded to the nearest.
fn faded(a: u8, alpha: u8) -> u8 {
    ((u32::from(a) * u32::from(alpha) + 127) / 255) as u8
}

/// Blend `colour` at level `a` out of 255 into the RGBA pixel `px`: the
/// integer form of [`blend_at`]. `(n + 127) / 255` rounds `n / 255` to
/// the nearest, as the float blend's `round` does, since `n / 255` is
/// never a half.
fn blend_level(px: &mut [u8], colour: [u8; 3], a: u8) {
    if a == 0 {
        return;
    }
    let (a, na) = (u32::from(a), 255 - u32::from(a));
    for (p, &k) in px.iter_mut().zip(colour.iter()) {
        *p = ((u32::from(*p) * na + u32::from(k) * a + 127) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1154: the halo widened by row spans is the max over the whole
    /// disc, byte for byte, at the widths the densities give and at the
    /// edges of the mask.
    #[test]
    fn the_span_halo_is_the_disc_halo() {
        let mut type_ = Typesetter::new();
        for scale in [1.0f32, 2.0, 2.75, 3.5] {
            let pad = (HALO * scale).ceil() as usize + 1;
            for (text, italic) in [
                ("Piazza della Signoria", false),
                ("Arno", true),
                ("Il", false),
            ] {
                let mask = type_.mask(text, 12.0 * scale, pad, italic);
                for r in [HALO * scale, 0.4, 1.0, 4.1, 7.0] {
                    assert_eq!(
                        mask.widened(r),
                        mask.widened_by_disc(r),
                        "{text} {scale} {r}"
                    );
                }
            }
        }
        // Ink against every edge, no pad: the disc's rows clipped.
        let (w, h) = (9, 7);
        let alpha = (0..w * h).map(|i| ((i * 37) % 11 * 23) as u8).collect();
        let mask = Mask {
            w,
            h,
            pad: 0,
            alpha,
        };
        for r in [0.0, 1.5, 2.5, 6.0, 12.0] {
            assert_eq!(mask.widened(r), mask.widened_by_disc(r), "{r}");
        }
    }

    /// 1154: a sprite sampled over its turned box is the sprite sampled
    /// over its whole reach, at every angle a street takes.
    #[test]
    fn the_boxed_sprite_is_the_whole_reach_sprite() {
        let mut type_ = Typesetter::new();
        let pad = (HALO * 2.75).ceil() as usize + 1;
        let mask = type_.mask("Via dei Calzaiuoli", 11.0 * 2.75, pad, false);
        let halo = mask.widened(HALO * 2.75);
        for k in -8..=8 {
            let angle = f64::from(k) * core::f64::consts::FRAC_PI_2 / 8.0;
            for centre in [(100.0, 80.0), (100.37, 80.81), (-3.5, 2.25)] {
                let name = PlacedName {
                    text: "Via dei Calzaiuoli".to_string(),
                    kind: LabelKind::MinorRoad,
                    min_zoom: 15,
                    boxes: Vec::new(),
                    centre,
                    angle,
                    mask: Arc::new(mask.clone()),
                    halo: Arc::new(halo.clone()),
                };
                assert_eq!(
                    Sprite::of(&name),
                    Sprite::of_whole_reach(&name),
                    "{angle} {centre:?}"
                );
            }
        }
    }

    /// 1156: a sprite drawn where it was placed is the float paint of
    /// its name, pixel for pixel for a level name and within two steps of
    /// a channel for a turned one (one a blend, see [`Sprite`]), over a
    /// ground of every shade, in both themes and moved by whole pixels.
    #[test]
    fn the_compact_sprite_is_the_painted_name_within_a_step() {
        let mut type_ = Typesetter::new();
        let scale = 2.75f32;
        let pad = (HALO * scale).ceil() as usize + 1;
        let (w, h) = (320u32, 200u32);
        let ground: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 256) as u8).collect();
        let mut turned_differ = 0;
        for (text, kind) in [
            ("Via dei Calzaiuoli", LabelKind::MinorRoad),
            ("Arno", LabelKind::River),
            ("Firenze", LabelKind::City),
        ] {
            let mask = type_.mask(text, 11.0 * scale, pad, kind == LabelKind::River);
            let halo = mask.widened(HALO * scale);
            for k in -8..=8 {
                let angle = f64::from(k) * core::f64::consts::FRAC_PI_2 / 8.0;
                for centre in [(160.0, 100.0), (150.37, 90.81), (2.5, 3.25)] {
                    let name = PlacedName {
                        text: text.to_string(),
                        kind,
                        min_zoom: 0,
                        boxes: Vec::new(),
                        centre,
                        angle,
                        mask: Arc::new(mask.clone()),
                        halo: Arc::new(halo.clone()),
                    };
                    let sprite = Sprite::of(&name);
                    for theme in [MapTheme::Light, MapTheme::Dark] {
                        for (dx, dy) in [(0i64, 0i64), (-7, 12)] {
                            let mut want = ground.clone();
                            let moved = PlacedName {
                                centre: (centre.0 + dx as f64, centre.1 + dy as f64),
                                ..name.clone()
                            };
                            paint_placed(&mut want, w, h, &[moved], theme);
                            let mut got = ground.clone();
                            sprite.draw(&mut got, w, h, theme, dx, dy);
                            let worst = want
                                .iter()
                                .zip(&got)
                                .map(|(a, b)| a.abs_diff(*b))
                                .max()
                                .unwrap_or(0);
                            match angle == 0.0 {
                                true => assert_eq!(worst, 0, "{text} {centre:?} {theme:?}"),
                                false => {
                                    assert!(worst <= 2, "{text} {angle} {centre:?}: {worst}");
                                    turned_differ += usize::from(worst > 0);
                                }
                            }
                        }
                    }
                }
            }
        }
        // The rounding is real: some turned name moved a step.
        assert!(turned_differ > 0);
    }

    /// 1154: what the typesetter keeps is what it makes, and it stays
    /// within its bound.
    #[test]
    fn kept_masks_are_the_masks_made() {
        let mut type_ = Typesetter::new();
        let (text, size, pad) = ("Ponte Vecchio", 13.0 * 2.75, 6);
        let made = type_.mask(text, size, pad, false);
        assert_eq!(type_.extent(text, size, pad, false), (made.w, made.h));
        assert_eq!(*type_.kept_mask(text, size, pad, false), made);
        assert_eq!(
            *type_.kept_halo(text, size, pad, false, HALO * 2.75),
            made.widened_by_disc(HALO * 2.75)
        );
        // Asked again: the same, from the store.
        assert_eq!(type_.kept().0, 2);
        assert_eq!(*type_.kept_mask(text, size, pad, false), made);
        assert_eq!(type_.kept().0, 2);
        // Past the byte bound the store starts over.
        for i in 0..4000 {
            let _ = type_.kept_mask(
                &format!("Via {i} lunghissima del centro storico"),
                40.0,
                6,
                false,
            );
            assert!(type_.kept().1 <= MASK_BYTES_KEPT);
        }
        type_.forget();
        assert_eq!(type_.kept(), (0, 0));
    }
}
