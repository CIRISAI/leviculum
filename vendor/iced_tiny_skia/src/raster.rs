use crate::core::image as raster;
use crate::core::{Rectangle, Size};
use crate::graphics;

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::collections::hash_map;

#[derive(Debug)]
pub struct Pipeline {
    cache: RefCell<Cache>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(Cache::default()),
        }
    }

    pub fn load(
        &self,
        handle: &raster::Handle,
    ) -> Result<raster::Allocation, raster::Error> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.allocate(handle)?;

        #[allow(unsafe_code)]
        Ok(unsafe {
            raster::allocate(handle, Size::new(image.width(), image.height()))
        })
    }

    pub fn dimensions(&self, handle: &raster::Handle) -> Option<Size<u32>> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.allocate(handle).ok()?;

        Some(Size::new(image.width(), image.height()))
    }

    pub fn draw(
        &mut self,
        handle: &raster::Handle,
        filter_method: raster::FilterMethod,
        bounds: Rectangle,
        opacity: f32,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        transform: tiny_skia::Transform,
        clip_mask: Option<&tiny_skia::Mask>,
    ) {
        let mut cache = self.cache.borrow_mut();

        let Ok(image) = cache.allocate(handle) else {
            return;
        };

        let width_scale = bounds.width / image.width() as f32;
        let height_scale = bounds.height / image.height() as f32;

        let transform = transform.pre_scale(width_scale, height_scale);

        let quality = match filter_method {
            raster::FilterMethod::Linear => tiny_skia::FilterQuality::Bilinear,
            raster::FilterMethod::Nearest => tiny_skia::FilterQuality::Nearest,
        };

        pixels.draw_pixmap(
            (bounds.x / width_scale) as i32,
            (bounds.y / height_scale) as i32,
            image,
            &tiny_skia::PixmapPaint {
                quality,
                opacity,
                ..Default::default()
            },
            transform,
            clip_mask,
        );
    }

    /// Leviculum's patch (1473): `handle` drawn as [`Self::draw`] draws
    /// it, through [`crate::fast::draw_scaled`] instead of tiny-skia's
    /// pipeline; `clip` the clip's pixels when the draw is clipped
    /// (`Some(None)` for a clip that covers none). Whether it drew; where
    /// not, the caller draws through tiny-skia.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_fast(
        &mut self,
        handle: &raster::Handle,
        filter_method: raster::FilterMethod,
        bounds: Rectangle,
        opacity: f32,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        transform: tiny_skia::Transform,
        clip: Option<Option<crate::fast::Px>>,
    ) -> bool {
        if !crate::fast::BLEND_EXACT {
            return false;
        }
        let mut cache = self.cache.borrow_mut();
        let Ok((image, opaque)) = cache.allocate_opaque(handle) else {
            return false;
        };
        let width_scale = bounds.width / image.width() as f32;
        let height_scale = bounds.height / image.height() as f32;
        let transform = transform.pre_scale(width_scale, height_scale);
        let quality = match filter_method {
            raster::FilterMethod::Linear => tiny_skia::FilterQuality::Bilinear,
            raster::FilterMethod::Nearest => tiny_skia::FilterQuality::Nearest,
        };
        match clip {
            // Clipped by a clip that covers nothing: nothing to draw.
            Some(None) => true,
            clip => crate::fast::draw_scaled(
                pixels,
                image,
                opaque,
                (bounds.x / width_scale) as i32,
                (bounds.y / height_scale) as i32,
                quality,
                opacity,
                transform,
                clip.flatten(),
            ),
        }
    }

    /// Leviculum's patch (1442): `handle` drawn as a row copy when that
    /// is what `draw` would draw: the picture opaque, at full opacity,
    /// at its own size and an identity transform, so tiny-skia's pattern
    /// samples each pixel at itself and source-over keeps the source;
    /// `clip` the clip's pixels when the draw is clipped (`Some(None)`
    /// for a clip that covers none). Whether it drew.
    pub fn blit(
        &mut self,
        handle: &raster::Handle,
        bounds: Rectangle,
        opacity: f32,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        transform: tiny_skia::Transform,
        clip: Option<Option<crate::fast::Px>>,
    ) -> bool {
        if opacity != 1.0 || !transform.is_identity() {
            return false;
        }
        let mut cache = self.cache.borrow_mut();
        let Ok((image, opaque)) = cache.allocate_opaque(handle) else {
            return false;
        };
        if !opaque
            || bounds.width / image.width() as f32 != 1.0
            || bounds.height / image.height() as f32 != 1.0
        {
            return false;
        }
        match clip {
            // Clipped by a clip that covers nothing: nothing to draw.
            Some(None) => {}
            clip => crate::fast::blit_opaque(
                pixels,
                image,
                bounds.x as i32,
                bounds.y as i32,
                clip.flatten(),
            ),
        }
        true
    }

    pub fn trim_cache(&mut self) {
        self.cache.borrow_mut().trim();
    }
}

#[derive(Debug, Default)]
struct Cache {
    entries: FxHashMap<raster::Id, Option<Entry>>,
    hits: FxHashSet<raster::Id>,
}

impl Cache {
    pub fn allocate(
        &mut self,
        handle: &raster::Handle,
    ) -> Result<tiny_skia::PixmapRef<'_>, raster::Error> {
        self.allocate_opaque(handle).map(|(image, _)| image)
    }

    /// The picture, and whether every pixel of it is opaque (1442).
    pub fn allocate_opaque(
        &mut self,
        handle: &raster::Handle,
    ) -> Result<(tiny_skia::PixmapRef<'_>, bool), raster::Error> {
        let id = handle.id();

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(id) {
            let image = match graphics::image::load(handle) {
                Ok(image) => image,
                Err(error) => {
                    let _ = entry.insert(None);

                    return Err(error);
                }
            };

            if image.width() == 0 || image.height() == 0 {
                return Err(raster::Error::Empty);
            }

            // Leviculum's patch (1442): an opaque picture only swaps red
            // and blue, premultiplying by 255 keeps every channel; the
            // conversion of a map frame was 17.5 % of its present. Two
            // passes without a branch per pixel, which vectorise.
            let raw = image.as_raw();
            let opaque = crate::fast::all_opaque(raw);

            let buffer: Vec<u32> = if opaque {
                crate::fast::swap_red_blue(raw)
            } else {
                raw.chunks_exact(4)
                    .map(|p| {
                        bytemuck::cast(
                            tiny_skia::ColorU8::from_rgba(
                                p[2], p[1], p[0], p[3],
                            )
                            .premultiply(),
                        )
                    })
                    .collect()
            };

            let _ = entry.insert(Some(Entry {
                width: image.width(),
                height: image.height(),
                pixels: buffer,
                opaque,
            }));
        }

        let _ = self.hits.insert(id);

        Ok(self
            .entries
            .get(&id)
            .unwrap()
            .as_ref()
            .map(|entry| {
                (
                    tiny_skia::PixmapRef::from_bytes(
                        bytemuck::cast_slice(&entry.pixels),
                        entry.width,
                        entry.height,
                    )
                    .expect("Build pixmap from image bytes"),
                    entry.opaque,
                )
            })
            .expect("Image should be allocated"))
    }

    fn trim(&mut self) {
        self.entries.retain(|key, _| self.hits.contains(key));
        self.hits.clear();
    }
}

#[derive(Debug)]
struct Entry {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    opaque: bool,
}
