use crate::core::alignment;
use crate::core::text::{Alignment, Shaping};
use crate::core::{Color, Font, Pixels, Point, Rectangle, Transformation};
use crate::graphics::text::cache::{self, Cache};
use crate::graphics::text::editor;
use crate::graphics::text::font_system;
use crate::graphics::text::paragraph;

use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::hash_map;

#[derive(Debug)]
pub struct Pipeline {
    glyph_cache: GlyphCache,
    cache: RefCell<Cache>,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            glyph_cache: GlyphCache::new(),
            cache: RefCell::new(Cache::new()),
        }
    }

    // TODO: Shared engine
    #[allow(dead_code)]
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) {
        font_system()
            .write()
            .expect("Write font system")
            .load_font(bytes);

        self.cache = RefCell::new(Cache::new());
    }

    pub fn draw_paragraph(
        &mut self,
        paragraph: &paragraph::Weak,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: crate::fast::Clipping<'_>,
        transformation: Transformation,
    ) {
        let Some(paragraph) = paragraph.upgrade() else {
            return;
        };

        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            paragraph.buffer(),
            position,
            color,
            pixels,
            clip_mask,
            transformation,
        );
    }

    pub fn draw_editor(
        &mut self,
        editor: &editor::Weak,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: crate::fast::Clipping<'_>,
        transformation: Transformation,
    ) {
        let Some(editor) = editor.upgrade() else {
            return;
        };

        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            editor.buffer(),
            position,
            color,
            pixels,
            clip_mask,
            transformation,
        );
    }

    pub fn draw_cached(
        &mut self,
        content: &str,
        bounds: Rectangle,
        color: Color,
        size: Pixels,
        line_height: Pixels,
        font: Font,
        align_x: Alignment,
        align_y: alignment::Vertical,
        shaping: Shaping,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: crate::fast::Clipping<'_>,
        transformation: Transformation,
    ) {
        let line_height = f32::from(line_height);

        let mut font_system = font_system().write().expect("Write font system");
        let font_system = font_system.raw();

        let key = cache::Key {
            bounds: bounds.size(),
            content,
            font,
            size: size.into(),
            line_height,
            shaping,
            align_x,
        };

        let (_, entry) = self.cache.get_mut().allocate(font_system, key);

        let width = entry.min_bounds.width;
        let height = entry.min_bounds.height;

        let x = match align_x {
            Alignment::Default | Alignment::Left | Alignment::Justified => {
                bounds.x
            }
            Alignment::Center => bounds.x - width / 2.0,
            Alignment::Right => bounds.x - width,
        };

        let y = match align_y {
            alignment::Vertical::Top => bounds.y,
            alignment::Vertical::Center => bounds.y - height / 2.0,
            alignment::Vertical::Bottom => bounds.y - height,
        };

        draw(
            font_system,
            &mut self.glyph_cache,
            &entry.buffer,
            Point::new(x, y),
            color,
            pixels,
            clip_mask,
            transformation,
        );
    }

    pub fn draw_raw(
        &mut self,
        buffer: &cosmic_text::Buffer,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: crate::fast::Clipping<'_>,
        transformation: Transformation,
    ) {
        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            buffer,
            position,
            color,
            pixels,
            clip_mask,
            transformation,
        );
    }

    pub fn trim_cache(&mut self) {
        self.cache.get_mut().trim();
        self.glyph_cache.trim();
    }
}

fn draw(
    font_system: &mut cosmic_text::FontSystem,
    glyph_cache: &mut GlyphCache,
    buffer: &cosmic_text::Buffer,
    position: Point,
    color: Color,
    pixels: &mut tiny_skia::PixmapMut<'_>,
    clip_mask: crate::fast::Clipping<'_>,
    transformation: Transformation,
) {
    // Leviculum's patch (1442): a clip that covers nothing draws nothing.
    if let crate::fast::Clipping::Rect(None) = clip_mask {
        return;
    }

    let position = position * transformation;

    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            let physical_glyph = glyph.physical(
                (position.x, position.y),
                transformation.scale_factor(),
            );

            if let Some((buffer, placement)) = glyph_cache.allocate(
                physical_glyph.cache_key,
                glyph.color_opt.map(from_color).unwrap_or(color),
                font_system,
            ) {
                let pixmap = tiny_skia::PixmapRef::from_bytes(
                    buffer,
                    placement.width,
                    placement.height,
                )
                .expect("Create glyph pixel map");

                let opacity = color.a
                    * glyph
                        .color_opt
                        .map(|c| c.a() as f32 / 255.0)
                        .unwrap_or(1.0);

                let x = physical_glyph.x + placement.left;
                let y = physical_glyph.y - placement.top
                    + (run.line_y * transformation.scale_factor()).round()
                        as i32;

                // Leviculum's patch (1442): the glyph blended in place,
                // the arithmetic tiny-skia's pipeline runs (`fast`).
                match clip_mask {
                    crate::fast::Clipping::Unclipped
                        if crate::fast::BLEND_EXACT =>
                    {
                        crate::fast::blit_over(
                            pixels, pixmap, x, y, opacity, None,
                        );
                    }
                    crate::fast::Clipping::Rect(clip) => {
                        crate::fast::blit_over(
                            pixels, pixmap, x, y, opacity, clip,
                        );
                    }
                    crate::fast::Clipping::Unclipped
                    | crate::fast::Clipping::Mask(_) => {
                        pixels.draw_pixmap(
                            x,
                            y,
                            pixmap,
                            &tiny_skia::PixmapPaint {
                                opacity,
                                ..tiny_skia::PixmapPaint::default()
                            },
                            tiny_skia::Transform::identity(),
                            clip_mask.mask(),
                        );
                    }
                }
            }
        }
    }
}

fn from_color(color: cosmic_text::Color) -> Color {
    let [r, g, b, a] = color.as_rgba();

    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

/// Leviculum's patch (1468): a glyph that has no picture (a space, a
/// glyph the font lacks) is kept as such, and the scaler's context lives
/// as long as the cache. Upstream kept only glyphs with pixels and built
/// a context per paragraph, so every space on the screen went to the
/// scaler on every frame, and the first one of each paragraph rebuilt
/// the font's hinting instance (its `prep` program run): callgrind over
/// loquax's window bench, a touchpad scroll of the chat at 1527x2099,
/// found 47 % of a frame's present in `HintingInstance::new`, 14 thousand
/// of them for 204 glyphs actually hinted. skrifa runs a glyph's program
/// on a copy of the instance's tables, so a context shared between
/// glyphs draws each glyph as a fresh one did.
#[derive(Debug, Default)]
struct GlyphCache {
    entries: FxHashMap<
        (cosmic_text::CacheKey, [u8; 3]),
        Option<(Vec<u32>, cosmic_text::Placement)>,
    >,
    recently_used: FxHashSet<(cosmic_text::CacheKey, [u8; 3])>,
    trim_count: usize,
    swash: Swash,
}

/// The scaler's context, kept between glyphs and frames (1468).
struct Swash(cosmic_text::SwashCache);

impl Default for Swash {
    fn default() -> Self {
        Self(cosmic_text::SwashCache::new())
    }
}

impl std::fmt::Debug for Swash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl GlyphCache {
    const TRIM_INTERVAL: usize = 300;
    const CAPACITY_LIMIT: usize = 16 * 1024;

    fn new() -> Self {
        GlyphCache::default()
    }

    fn allocate(
        &mut self,
        cache_key: cosmic_text::CacheKey,
        color: Color,
        font_system: &mut cosmic_text::FontSystem,
    ) -> Option<(&[u8], cosmic_text::Placement)> {
        let [r, g, b, _a] = color.into_rgba8();
        let key = (cache_key, [r, g, b]);

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(key) {
            let _ = entry.insert(rasterise(
                &mut self.swash.0,
                font_system,
                cache_key,
                [r, g, b],
            ));
        }

        let _ = self.recently_used.insert(key);

        self.entries.get(&key).and_then(Option::as_ref).map(
            |(buffer, placement)| {
                (bytemuck::cast_slice(buffer.as_slice()), *placement)
            },
        )
    }

    pub fn trim(&mut self) {
        if self.trim_count > Self::TRIM_INTERVAL
            || self.recently_used.len() >= Self::CAPACITY_LIMIT
        {
            self.entries
                .retain(|key, _| self.recently_used.contains(key));

            self.recently_used.clear();

            self.entries.shrink_to(Self::CAPACITY_LIMIT);
            self.recently_used.shrink_to(Self::CAPACITY_LIMIT);

            self.trim_count = 0;
        } else {
            self.trim_count += 1;
        }
    }
}

/// The picture of the glyph `cache_key` in the colour `[r, g, b]`, as
/// upstream's cache made it; `None` for a glyph without one.
fn rasterise(
    swash: &mut cosmic_text::SwashCache,
    font_system: &mut cosmic_text::FontSystem,
    cache_key: cosmic_text::CacheKey,
    [r, g, b]: [u8; 3],
) -> Option<(Vec<u32>, cosmic_text::Placement)> {
    // TODO: Outline support
    let image = swash.get_image_uncached(font_system, cache_key)?;

    let glyph_size =
        image.placement.width as usize * image.placement.height as usize;

    if glyph_size == 0 {
        return None;
    }

    let mut buffer = vec![0u32; glyph_size];

    match image.content {
        cosmic_text::SwashContent::Mask => {
            let mut i = 0;

            // TODO: Blend alpha

            for _y in 0..image.placement.height {
                for _x in 0..image.placement.width {
                    buffer[i] = bytemuck::cast(
                        tiny_skia::ColorU8::from_rgba(b, g, r, image.data[i])
                            .premultiply(),
                    );

                    i += 1;
                }
            }
        }
        cosmic_text::SwashContent::Color => {
            let mut i = 0;

            for _y in 0..image.placement.height {
                for _x in 0..image.placement.width {
                    // TODO: Blend alpha
                    buffer[i >> 2] = bytemuck::cast(
                        tiny_skia::ColorU8::from_rgba(
                            image.data[i + 2],
                            image.data[i + 1],
                            image.data[i],
                            image.data[i + 3],
                        )
                        .premultiply(),
                    );

                    i += 4;
                }
            }
        }
        cosmic_text::SwashContent::SubpixelMask => {
            // TODO
        }
    }

    Some((buffer, image.placement))
}
