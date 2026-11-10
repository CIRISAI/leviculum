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

/// The damage the display server is handed (1442): every changed pixel
/// inside a rectangle, the rectangles rounded outward at the window's
/// scale and kept on the buffer, none empty.
#[test]
fn the_display_server_gets_every_changed_pixel() {
    use iced_tiny_skia::window::compositor::physical_damage;
    let mut rng = Rng(0x5eed_f00d_cafe_babe);
    for _ in 0..2000 {
        let scale = [1.0, 1.25, 1.5, 2.0][rng.below(4) as usize];
        let rects: Vec<Rectangle> = (0..1 + rng.below(4)).map(|_| rng.rect(W, H)).collect();
        let (bw, bh) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
        let out = physical_damage(&rects, scale, bw, bh);
        for &[x, y, w, h] in &out {
            assert!(w > 0 && h > 0 && x + w <= bw && y + h <= bh, "{out:?}");
        }
        for r in &rects {
            let p = *r * scale;
            // A pixel whose centre a changed rectangle covers is handed.
            for py in 0..bh {
                for px in 0..bw {
                    let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                    if cx > p.x && cx < p.x + p.width && cy > p.y && cy < p.y + p.height {
                        assert!(
                            out.iter().any(|&[x, y, w, h]| px >= x
                                && px < x + w
                                && py >= y
                                && py < y + h),
                            "{px},{py} of {r:?} at {scale} not in {out:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(physical_damage(
        &[Rectangle::new(
            iced::Point::new(-20.0, -20.0),
            iced::Size::new(5.0, 5.0)
        )],
        1.0,
        W,
        H
    )
    .is_empty());
}

/// A glyph blended in place (1442): the bytes tiny-skia's `draw_pixmap`
/// (identity transform, the glyph's opacity, nearest) leaves, for
/// glyphs of transparent, partial and opaque premultiplied pixels and
/// colour ones, at opacities at, below and beyond one, unclipped and
/// through a clip, partly off the target.
#[test]
fn a_glyph_blends_as_tiny_skia_blends_it() {
    // Elsewhere the renderer keeps tiny-skia's draw for text.
    if !fast::BLEND_EXACT {
        return;
    }
    let mut rng = Rng(0xfeed_beef_dead_c0de);
    let premultiplied = |rng: &mut Rng| {
        let a = match rng.below(4) {
            0 => 0,
            1 => 255,
            _ => rng.below(256) as u8,
        };
        let mut c = || rng.below(u64::from(a) + 1) as u8;
        [c(), c(), c(), a]
    };
    for _ in 0..3000 {
        let (sw, sh) = (1 + rng.below(24) as u32, 1 + rng.below(24) as u32);
        let mut src = tiny_skia::Pixmap::new(sw, sh).expect("glyph");
        for p in src.data_mut().chunks_exact_mut(4) {
            p.copy_from_slice(&premultiplied(&mut rng));
        }
        let x = rng.below(u64::from(W) + 30) as i32 - 15;
        let y = rng.below(u64::from(H) + 30) as i32 - 15;
        let opacity = match rng.below(5) {
            0 | 1 => 1.0,
            2 => rng.below(256) as f32 / 255.0,
            3 => 1.5,
            _ => 0.3,
        };
        let ground: Vec<u8> = (0..W * H).flat_map(|_| premultiplied(&mut rng)).collect();
        let clipped = rng.below(2) == 0;
        let r = rng.rect(W, H);
        let mut mask = tiny_skia::Mask::new(W, H).expect("mask");
        reference_mask(&mut mask, r);
        let mut reference = ground.clone();
        tiny_skia::PixmapMut::from_bytes(&mut reference, W, H)
            .expect("pixmap")
            .draw_pixmap(
                x,
                y,
                src.as_ref(),
                &tiny_skia::PixmapPaint {
                    opacity,
                    ..Default::default()
                },
                tiny_skia::Transform::identity(),
                clipped.then_some(&mask),
            );
        let mut ours = ground;
        let mut target = tiny_skia::PixmapMut::from_bytes(&mut ours, W, H).expect("pixmap");
        match (clipped, fast::covered(r, W, H)) {
            (true, None) => {}
            (true, Some(px)) => fast::blit_over(&mut target, src.as_ref(), x, y, opacity, Some(px)),
            (false, _) => fast::blit_over(&mut target, src.as_ref(), x, y, opacity, None),
        }
        let first = reference.iter().zip(&ours).position(|(a, b)| a != b);
        assert!(
            first.is_none(),
            "{sw}x{sh} at {x},{y} opacity {opacity} clip {clipped}: byte {first:?}, {} differ",
            reference.iter().zip(&ours).filter(|(a, b)| a != b).count()
        );
    }
}

/// The shortcuts of the glyph blend over every byte: a transparent
/// source pixel at full opacity keeps every destination byte, an opaque
/// one is the source, as tiny-skia's f32 load, blend and store give.
#[test]
fn a_glyphs_empty_and_full_pixels_are_what_the_arithmetic_gives() {
    const F: f32 = 1.0 / 255.0;
    // tiny-skia's `unnorm`, max then min as it writes it.
    #[allow(clippy::manual_clamp)]
    let store = |v: f32| (v.max(0.0).min(1.0) * 255.0).round_ties_even() as u8;
    for d in 0..=255u8 {
        assert_eq!(store(f32::from(d) * F * (1.0 - 0.0) + 0.0), d);
        for s in 0..=255u8 {
            assert_eq!(store(f32::from(d) * F * (1.0 - 1.0) + f32::from(s) * F), s);
        }
    }
}

/// The blend's rounding without libm (1468) is `round_ties_even` on
/// every value a channel takes: every half from 0 to 255 and its two
/// neighbours, and every 97th float in between.
#[test]
fn the_blends_rounding_is_round_ties_even() {
    let top = 255.0f32.to_bits();
    let mut values: Vec<f32> = (0..top).step_by(97).map(f32::from_bits).collect();
    for half in 0..=510u32 {
        let v = half as f32 / 2.0;
        values.extend([v, f32::from_bits(v.to_bits() + 1), v.next_down()]);
    }
    values.push(255.0);
    for v in values.into_iter().filter(|v| (0.0..=255.0).contains(v)) {
        assert_eq!(
            fast::ties_even(v).to_bits(),
            v.round_ties_even().to_bits(),
            "{v}"
        );
    }
}

/// An opaque quad without corners on whole pixels (1468) is the bytes
/// tiny-skia's anti-aliased fill of its rectangle stores, with the clip's
/// mask and without: `whole_rect`'s pixels filled with `solid_pixel`.
#[test]
fn an_opaque_square_quad_is_the_fill_tiny_skia_draws() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..2000 {
        let whole = |rng: &mut Rng, max: u32| rng.below(u64::from(max) + 8) as f32 - 4.0;
        let x = whole(&mut rng, W);
        let y = whole(&mut rng, H);
        let r = Rectangle {
            x,
            y,
            width: 1.0 + rng.below(u64::from(W)) as f32,
            height: 1.0 + rng.below(u64::from(H)) as f32,
        };
        let clip = match rng.below(3) {
            0 => Rectangle {
                x: 0.0,
                y: 0.0,
                width: W as f32,
                height: H as f32,
            },
            _ => rng.rect(W, H),
        };
        let masked = rng.below(2) == 0;
        let color = tiny_skia::Color::from_rgba(
            rng.below(256) as f32 / 255.0,
            rng.below(256) as f32 / 255.0,
            rng.below(256) as f32 / 255.0,
            1.0,
        )
        .expect("colour");
        let ground: Vec<u8> = (0..W * H * 4).map(|_| rng.below(256) as u8).collect();
        let mut reference = ground.clone();
        let mut mask = tiny_skia::Mask::new(W, H).expect("mask");
        reference_mask(&mut mask, clip);
        {
            let mut pixmap =
                tiny_skia::PixmapMut::from_bytes(&mut reference, W, H).expect("pixmap");
            if let Some(path) = path(r) {
                pixmap.fill_path(
                    &path,
                    &tiny_skia::Paint {
                        shader: tiny_skia::Shader::SolidColor(color),
                        anti_alias: true,
                        ..Default::default()
                    },
                    tiny_skia::FillRule::EvenOdd,
                    tiny_skia::Transform::identity(),
                    masked.then_some(&mask),
                );
            }
        }
        let mut ours = ground;
        let all = Rectangle {
            x: 0.0,
            y: 0.0,
            width: W as f32,
            height: H as f32,
        };
        if let Some(px) = fast::whole_rect(r, if masked { clip } else { all }, W, H) {
            fast::fill(&mut ours, W, px, fast::solid_pixel(color));
        }
        assert_eq!(
            reference, ours,
            "{r:?} clip {clip:?} masked {masked} {color:?}"
        );
    }
}

/// An opaque picture's pixels (1468) are the words 1442's byte-wise
/// conversion made: red and blue swapped, alpha 255.
#[test]
fn an_opaque_pictures_pixels_swap_red_and_blue() {
    let mut rng = Rng(0x0bad_cafe_dead_beef);
    let raw: Vec<u8> = (0..4096)
        .flat_map(|_| {
            [
                rng.below(256) as u8,
                rng.below(256) as u8,
                rng.below(256) as u8,
                255,
            ]
        })
        .collect();
    let reference: Vec<u32> = raw
        .chunks_exact(4)
        .map(|p| u32::from_ne_bytes([p[2], p[1], p[0], 255]))
        .collect();
    assert_eq!(fast::swap_red_blue(&raw), reference);
}

/// A picture is opaque when every alpha byte is 255 (1468), read as words
/// or as bytes: one translucent pixel anywhere makes it not.
#[test]
fn a_picture_is_opaque_when_every_alpha_is_full() {
    let mut rng = Rng(0x5eed_5eed_5eed_5eed);
    for n in [1usize, 2, 3, 7, 64, 1000] {
        let mut raw: Vec<u8> = (0..n)
            .flat_map(|_| {
                [
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                    255,
                ]
            })
            .collect();
        assert!(fast::all_opaque(&raw), "{n}");
        assert!(fast::all_opaque(&raw[..raw.len()]), "{n}");
        let at = rng.below(n as u64) as usize * 4 + 3;
        raw[at] = 254;
        assert!(!fast::all_opaque(&raw), "{n} at {at}");
        // Misaligned: the bytes after the first.
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&raw);
        assert!(!fast::all_opaque(&shifted[1..]), "{n} misaligned");
        raw[at] = 255;
        shifted[at + 1] = 255;
        assert!(fast::all_opaque(&shifted[1..]), "{n} misaligned, opaque");
    }
}

/// A byte loaded as tiny-skia's high-precision pipeline loads it (`* 1/255`)
/// stores as itself, and an opaque one loads with an alpha of exactly one:
/// the two facts `fast::draw_scaled`'s copy of an opaque picture read at its
/// nearest texel rests on, and a clipped draw's pixels outside the clip.
#[test]
fn a_byte_loaded_stores_as_itself_and_full_alpha_is_one() {
    const FACTOR: f32 = 1.0 / 255.0;
    for c in 0..=255u8 {
        assert_eq!(fast::unnorm(f32::from(c) * FACTOR), c, "byte {c}");
    }
    assert_eq!(255.0 * FACTOR, 1.0);
}

/// One random scaled draw of `fast_paths`' sampler test: a picture, where
/// it is put, how it is scaled, sampled and clipped.
struct Scaled {
    src: tiny_skia::Pixmap,
    opaque: bool,
    x: i32,
    y: i32,
    quality: tiny_skia::FilterQuality,
    opacity: f32,
    transform: tiny_skia::Transform,
    clip: Option<Rectangle>,
}

impl Scaled {
    fn random(rng: &mut Rng, w: u32, h: u32, big: bool) -> Scaled {
        let (sw, sh) = match big {
            true => (
                w - rng.below(u64::from(w / 4)) as u32,
                h - rng.below(u64::from(h / 4)) as u32,
            ),
            false => (1 + rng.below(40) as u32, 1 + rng.below(40) as u32),
        };
        let opaque = rng.below(3) != 0;
        let mut src = tiny_skia::Pixmap::new(sw, sh).expect("picture");
        for p in src.data_mut().chunks_exact_mut(4) {
            let a = match (opaque, rng.below(4)) {
                (true, _) | (false, 0) => 255,
                (false, 1) => 0,
                _ => rng.below(256) as u8,
            };
            let mut c = || rng.below(u64::from(a) + 1) as u8;
            p.copy_from_slice(&[c(), c(), c(), a]);
        }
        let opaque = opaque || src.data().chunks_exact(4).all(|p| p[3] == 255);
        // Scales a zoom glides through, and odd ones.
        let scale = |rng: &mut Rng| match rng.below(6) {
            0 => 1.0,
            1 => 1.0 + rng.below(1 << 16) as f32 / (1 << 18) as f32,
            2 => 1.0 - rng.below(1 << 16) as f32 / (1 << 18) as f32,
            3 => 0.1 + rng.below(1000) as f32 / 250.0,
            4 => (1 + rng.below(8)) as f32 / 2.0,
            _ => -(0.2 + rng.below(1000) as f32 / 400.0),
        };
        let sx = scale(rng);
        let sy = match rng.below(3) {
            0 => sx,
            _ => scale(rng),
        };
        let tx = rng.coord(w) - w as f32 / 4.0;
        let ty = rng.coord(h) - h as f32 / 4.0;
        let transform = tiny_skia::Transform::from_row(sx, 0.0, 0.0, sy, tx, ty);
        let x = rng.below(41) as i32 - 20;
        let y = rng.below(41) as i32 - 20;
        let quality = match rng.below(2) {
            0 => tiny_skia::FilterQuality::Nearest,
            _ => tiny_skia::FilterQuality::Bilinear,
        };
        let opacity = match rng.below(5) {
            0..=2 => 1.0,
            3 => rng.below(256) as f32 / 255.0,
            _ => 0.6,
        };
        let clip = (rng.below(2) == 0).then(|| rng.rect(w, h));
        Scaled {
            src,
            opaque,
            x,
            y,
            quality,
            opacity,
            transform,
            clip,
        }
    }

    /// The draw through tiny-skia and through `fast::draw_scaled` onto the
    /// same ground; `None` where the sampler declined.
    fn both(&self, ground: &[u8], w: u32, h: u32) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut mask = tiny_skia::Mask::new(w, h).expect("mask");
        if let Some(r) = self.clip {
            reference_mask(&mut mask, r);
        }
        let mut reference = ground.to_vec();
        tiny_skia::PixmapMut::from_bytes(&mut reference, w, h)
            .expect("pixmap")
            .draw_pixmap(
                self.x,
                self.y,
                self.src.as_ref(),
                &tiny_skia::PixmapPaint {
                    quality: self.quality,
                    opacity: self.opacity,
                    ..Default::default()
                },
                self.transform,
                self.clip.is_some().then_some(&mask),
            );
        let mut ours = ground.to_vec();
        let mut target = tiny_skia::PixmapMut::from_bytes(&mut ours, w, h).expect("pixmap");
        let drew = match self.clip.map(|r| fast::covered(r, w, h)) {
            Some(None) => true,
            clip => fast::draw_scaled(
                &mut target,
                self.src.as_ref(),
                self.opaque,
                self.x,
                self.y,
                self.quality,
                self.opacity,
                self.transform,
                clip.flatten(),
            ),
        };
        drew.then_some((reference, ours))
    }
}

/// The map's zoom frame sampled by `fast::draw_scaled` (1473) is the frame
/// tiny-skia's `draw_pixmap` draws, byte for byte: thousands of random
/// pictures, offsets and scales (a zoom's near one, odd ones, mirrored
/// ones), nearest and bilinear, opaque or not, at full opacity or less,
/// clipped by a rectangle (tiny-skia by its mask) or not; and pictures the
/// size of the target scaled as a zoom scales the map's frame.
#[test]
fn a_scaled_picture_samples_as_tiny_skia_samples_it() {
    if !fast::BLEND_EXACT {
        return;
    }
    let premultiplied = |rng: &mut Rng| {
        let a = rng.below(256) as u8;
        [
            rng.below(u64::from(a) + 1) as u8,
            rng.below(u64::from(a) + 1) as u8,
            0,
            a,
        ]
    };
    let mut rng = Rng(0x5ca1_ed0f_f5e7_0001);
    let mut drawn = 0;
    let cases = (0..6000)
        .map(|_| (W, H, false))
        .chain((0..200).map(|_| (240, 180, true)));
    for (i, (w, h, big)) in cases.enumerate() {
        let case = Scaled::random(&mut rng, w, h, big);
        let ground: Vec<u8> = (0..w * h).flat_map(|_| premultiplied(&mut rng)).collect();
        let Some((reference, ours)) = case.both(&ground, w, h) else {
            continue;
        };
        drawn += 1;
        let differ: Vec<usize> = (0..reference.len())
            .filter(|&b| reference[b] != ours[b])
            .collect();
        assert!(
            differ.is_empty(),
            "case {i}: {}x{} at {},{} {:?} {:?} opacity {} opaque {} clip {:?}: {} bytes differ, \
             first pixel {:?} ({:?} vs {:?})",
            case.src.width(),
            case.src.height(),
            case.x,
            case.y,
            case.quality,
            case.transform,
            case.opacity,
            case.opaque,
            case.clip,
            differ.len(),
            differ
                .first()
                .map(|b| (b / 4 % w as usize, b / 4 / w as usize)),
            differ.first().map(|b| &reference[b / 4 * 4..b / 4 * 4 + 4]),
            differ.first().map(|b| &ours[b / 4 * 4..b / 4 * 4 + 4]),
        );
    }
    // The sampler declines only what it cannot draw exactly: none of these.
    assert_eq!(drawn, 6200, "the sampler declined {} draws", 6200 - drawn);
}
