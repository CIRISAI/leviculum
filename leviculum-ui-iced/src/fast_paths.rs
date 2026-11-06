//! The vendored renderer's fast paths (1442, `iced_tiny_skia::fast`) held
//! to tiny-skia's own result, byte for byte: the pixels a rectangle
//! without anti-aliasing covers, its fill, the clip mask kept lazily, and
//! the row copy of an opaque picture. A difference here is a pixel that
//! moved in every window.

use iced::Rectangle;
use iced_tiny_skia::fast::{self, Clip};

/// A small deterministic generator, so a red run repeats.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// A coordinate from -8 to `max` + 8, on a quarter, an eighth or a
    /// sixty-fourth of a pixel or anywhere.
    fn coord(&mut self, max: u32) -> f32 {
        let whole = self.below(u64::from(max) + 16) as f32 - 8.0;
        let frac = match self.below(4) {
            0 => 0.0,
            1 => self.below(4) as f32 / 4.0,
            2 => self.below(64) as f32 / 64.0,
            _ => self.below(1 << 20) as f32 / (1 << 20) as f32,
        };
        whole + frac
    }
    fn rect(&mut self, w: u32, h: u32) -> Rectangle {
        let x = self.coord(w);
        let y = self.coord(h);
        let width = self.coord(w / 2).abs() + 0.01;
        let height = self.coord(h / 2).abs() + 0.01;
        Rectangle {
            x,
            y,
            width,
            height,
        }
    }
}

const W: u32 = 61;
const H: u32 = 47;

fn path(r: Rectangle) -> Option<tiny_skia::Path> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.width, r.height).map(tiny_skia::PathBuilder::from_rect)
}

/// What tiny-skia's `adjust_clip_mask` (iced_tiny_skia 0.14.1,
/// `engine.rs`) left in the mask: cleared, the rectangle filled without
/// anti-aliasing.
fn reference_mask(mask: &mut tiny_skia::Mask, r: Rectangle) {
    mask.clear();
    if let Some(path) = path(r) {
        mask.fill_path(
            &path,
            tiny_skia::FillRule::EvenOdd,
            false,
            tiny_skia::Transform::default(),
        );
    }
}

/// The pixels a fill without anti-aliasing covers are the rectangle
/// `fast::covered` names: in a mask and on a pixmap, for rectangles on
/// fractions of pixels, partly or wholly off the target.
#[test]
fn a_rectangle_covers_the_pixels_tiny_skia_fills() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..4000 {
        let r = rng.rect(W, H);
        let mut mask = tiny_skia::Mask::new(W, H).expect("mask");
        reference_mask(&mut mask, r);
        let mut want = vec![0u8; (W * H) as usize];
        if let Some(px) = fast::covered(r, W, H) {
            fast::fill_mask(&mut want, W, px, 255);
        }
        assert_eq!(mask.data(), &want[..], "{r:?}");
    }
}

/// The background as rows: the bytes tiny-skia's fill of the damage
/// rectangle with the `Source` blend stores, for colours with and
/// without alpha.
#[test]
fn the_background_is_the_bytes_tiny_skia_stores() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..2000 {
        let r = rng.rect(W, H);
        let alpha = match rng.below(3) {
            0 => 1.0,
            1 => 0.0,
            _ => rng.below(256) as f32 / 255.0,
        };
        let color = tiny_skia::Color::from_rgba(
            rng.below(256) as f32 / 255.0,
            rng.below(256) as f32 / 255.0,
            rng.below(256) as f32 / 255.0,
            alpha,
        )
        .expect("colour");
        let ground: Vec<u8> = (0..W * H * 4).map(|_| rng.below(256) as u8).collect();
        let mut reference = ground.clone();
        {
            let mut pixmap =
                tiny_skia::PixmapMut::from_bytes(&mut reference, W, H).expect("pixmap");
            if let Some(path) = path(r) {
                pixmap.fill_path(
                    &path,
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
            }
        }
        let mut ours = ground;
        if let Some(px) = fast::covered(r, W, H) {
            fast::fill(&mut ours, W, px, fast::solid_pixel(color));
        }
        assert_eq!(reference, ours, "{r:?} {color:?}");
    }
}

/// The lazy clip: whatever rectangles a draw names and in whatever order
/// it asks for the mask, the mask it is handed is the one
/// `adjust_clip_mask` would have left, also over a mask the last draw
/// left dirty.
#[test]
fn the_lazy_clip_is_the_mask_adjust_clip_mask_left() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut mask = tiny_skia::Mask::new(W, H).expect("mask");
    let mut clip = Clip::default();
    for draw in 0..200 {
        clip.reset();
        if draw % 7 == 0 {
            // Foreign bytes: a draw may be handed any mask.
            for b in mask.data_mut() {
                *b = rng.below(256) as u8;
            }
        }
        let mut current = None;
        for _ in 0..rng.below(12) {
            let r = rng.rect(W, H);
            clip.set(r);
            current = Some(r);
            if rng.below(2) == 0 {
                let mut want = tiny_skia::Mask::new(W, H).expect("mask");
                reference_mask(&mut want, r);
                assert_eq!(clip.mask(&mut mask).data(), want.data(), "{r:?}");
                if let Some(px) = clip.rect(W, H) {
                    assert_eq!(Some(px), fast::covered(r, W, H));
                }
            }
        }
        let _ = current;
    }
}

/// An opaque picture at a whole-pixel offset, copied: the bytes tiny-
/// skia's `draw_pixmap` (identity transform, full opacity, either
/// filter) leaves, unclipped and through a clip, partly off the target.
#[test]
fn an_opaque_picture_is_copied_as_tiny_skia_draws_it() {
    let mut rng = Rng(0x0dd_b1a5_e5ba_d5ee);
    for _ in 0..1500 {
        let (sw, sh) = (1 + rng.below(40) as u32, 1 + rng.below(40) as u32);
        let mut src = tiny_skia::Pixmap::new(sw, sh).expect("picture");
        for p in src.data_mut().chunks_exact_mut(4) {
            p.copy_from_slice(&[
                rng.below(256) as u8,
                rng.below(256) as u8,
                rng.below(256) as u8,
                255,
            ]);
        }
        let x = rng.below(u64::from(W) + 40) as i32 - 20;
        let y = rng.below(u64::from(H) + 40) as i32 - 20;
        let ground: Vec<u8> = (0..W * H)
            .flat_map(|_| {
                let a = rng.below(256) as u8;
                [rng.below(u64::from(a) + 1) as u8, 0, a, a]
            })
            .collect();
        let clipped = rng.below(2) == 0;
        let r = rng.rect(W, H);
        let mut mask = tiny_skia::Mask::new(W, H).expect("mask");
        reference_mask(&mut mask, r);
        let quality = match rng.below(2) {
            0 => tiny_skia::FilterQuality::Nearest,
            _ => tiny_skia::FilterQuality::Bilinear,
        };
        let mut reference = ground.clone();
        tiny_skia::PixmapMut::from_bytes(&mut reference, W, H)
            .expect("pixmap")
            .draw_pixmap(
                x,
                y,
                src.as_ref(),
                &tiny_skia::PixmapPaint {
                    quality,
                    ..Default::default()
                },
                tiny_skia::Transform::identity(),
                clipped.then_some(&mask),
            );
        let mut ours = ground;
        let mut target = tiny_skia::PixmapMut::from_bytes(&mut ours, W, H).expect("pixmap");
        match (clipped, fast::covered(r, W, H)) {
            (true, None) => {}
            (true, Some(px)) => fast::blit_opaque(&mut target, src.as_ref(), x, y, Some(px)),
            (false, _) => fast::blit_opaque(&mut target, src.as_ref(), x, y, None),
        }
        let first = reference.iter().zip(&ours).position(|(a, b)| a != b);
        assert!(
            first.is_none(),
            "{sw}x{sh} at {x},{y} clip {clipped} {r:?}: first byte differing {first:?} ({} differ)",
            reference.iter().zip(&ours).filter(|(a, b)| a != b).count()
        );
    }
}

/// Premultiplying an opaque pixel keeps every channel, which the
/// picture cache's conversion relies on.
#[test]
fn premultiplying_an_opaque_pixel_keeps_it() {
    for c in 0..=255u8 {
        let p = tiny_skia::ColorU8::from_rgba(c, 255 - c, c / 2, 255).premultiply();
        assert_eq!(
            [p.red(), p.green(), p.blue(), p.alpha()],
            [c, 255 - c, c / 2, 255]
        );
    }
}
