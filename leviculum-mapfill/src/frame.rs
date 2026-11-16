//! The framebuffers a view is drawn into, over borrowed memory.
//!
//! [`Frame`]: Rgb565 in the panel's byte order. The ST7789 takes RGB565
//! high byte first, so each pixel is stored as a big-endian `u16`: the
//! buffer goes out over SPI as it is, with no swap pass. Borrowed so the
//! same type sits in internal SRAM or in PSRAM.
//!
//! [`RgbaFrame`]: opaque 8-bit RGBA, what a screen with a compositor (the
//! phone's Bitmap, the window's image, the tile cache) holds. The colours
//! are the same Rgb565 ones, stored as [`rgb888`] widens them, so a view
//! drawn into either frame is the same picture; the RGBA one needs no
//! pass to widen it afterwards, and has the bits a blend needs (1422).

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use crate::coverage::Coverage;

/// `c` as 8-bit red, green and blue, each channel's top bits repeated
/// into its bottom ones so full intensity stays 255: the one conversion
/// every consumer of a frame outside the panel (a PPM, a terminal picture)
/// goes through.
pub fn rgb888(c: Rgb565) -> [u8; 3] {
    let v = RawU16::from(c).into_inner();
    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

/// A `width` x `height` Rgb565 frame, rows top to bottom.
pub struct Frame<'a> {
    px: &'a mut [u16],
    width: u32,
    height: u32,
}

impl<'a> Frame<'a> {
    /// A frame over `px`, which holds at least `width * height` pixels.
    /// `None` when it is too short.
    pub fn new(px: &'a mut [u16], width: u32, height: u32) -> Option<Self> {
        let n = (width as usize).checked_mul(height as usize)?;
        let px = px.get_mut(..n)?;
        Some(Self { px, width, height })
    }

    /// The pixels as the panel takes them.
    #[allow(unsafe_code)]
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: a `u16` slice is a valid byte slice of twice the length,
        // and `u8` has alignment 1.
        unsafe { core::slice::from_raw_parts(self.px.as_ptr().cast::<u8>(), self.px.len() * 2) }
    }

    /// Width and height in pixels.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame as packed 8-bit RGB, rows top to bottom, through
    /// [`rgb888`], appended to `out`.
    pub fn rgb888_into(&self, out: &mut impl Extend<u8>) {
        for &px in self.px.iter() {
            let c = Rgb565::from(RawU16::new(u16::from_be(px)));
            out.extend(rgb888(c));
        }
    }

    /// The colour at `(x, y)`, or `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb565> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let v = u16::from_be(self.px[(y * self.width + x) as usize]);
        Some(Rgb565::from(RawU16::new(v)))
    }
}

impl OriginDimensions for Frame<'_> {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Frame<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        it: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in it {
            if p.x >= 0 && p.y >= 0 && (p.x as u32) < self.width && (p.y as u32) < self.height {
                self.px[(p.y as u32 * self.width + p.x as u32) as usize] =
                    RawU16::from(c).into_inner().to_be();
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let a = area.intersection(&self.bounding_box());
        if a.size.width == 0 || a.size.height == 0 {
            return Ok(());
        }
        let v = RawU16::from(color).into_inner().to_be();
        let (x0, w) = (a.top_left.x as usize, a.size.width as usize);
        for y in a.top_left.y as u32..a.top_left.y as u32 + a.size.height {
            let row = (y * self.width) as usize + x0;
            self.px[row..row + w].fill(v);
        }
        Ok(())
    }

    fn clear(&mut self, color: Rgb565) -> Result<(), Self::Error> {
        self.px.fill(RawU16::from(color).into_inner().to_be());
        Ok(())
    }
}

/// A `width` x `height` frame of opaque 8-bit RGBA, rows top to bottom,
/// four bytes a pixel; drawn in Rgb565 colours widened by [`rgb888`].
pub struct RgbaFrame<'a> {
    px: &'a mut [u8],
    width: u32,
    height: u32,
    /// Span ends blended by their coverage ([`Coverage`]).
    smooth: bool,
}

impl<'a> RgbaFrame<'a> {
    /// A frame over `px`, which holds at least `width * height * 4`
    /// bytes. `None` when it is too short.
    pub fn new(px: &'a mut [u8], width: u32, height: u32) -> Option<Self> {
        let n = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        let px = px.get_mut(..n)?;
        Some(Self {
            px,
            width,
            height,
            smooth: false,
        })
    }

    /// The frame with the filler's span ends blended by their coverage
    /// (1422), or, with `false`, every pixel hard as the Rgb565 frame has
    /// it.
    pub fn smoothed(mut self, smooth: bool) -> Self {
        self.smooth = smooth;
        self
    }

    /// Width and height in pixels.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The colour at `(x, y)` as RGBA, or `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = ((y * self.width + x) * 4) as usize;
        let mut c = [0u8; 4];
        c.copy_from_slice(&self.px[at..at + 4]);
        Some(c)
    }
}

/// `c` as the four bytes an [`RgbaFrame`] stores.
fn rgba(c: Rgb565) -> [u8; 4] {
    let [r, g, b] = rgb888(c);
    [r, g, b, 0xff]
}

impl OriginDimensions for RgbaFrame<'_> {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for RgbaFrame<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        it: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in it {
            if p.x >= 0 && p.y >= 0 && (p.x as u32) < self.width && (p.y as u32) < self.height {
                let at = ((p.y as u32 * self.width + p.x as u32) * 4) as usize;
                self.px[at..at + 4].copy_from_slice(&rgba(c));
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let a = area.intersection(&self.bounding_box());
        if a.size.width == 0 || a.size.height == 0 {
            return Ok(());
        }
        let v = rgba(color);
        let (x0, w) = (a.top_left.x as usize, a.size.width as usize);
        for y in a.top_left.y as u32..a.top_left.y as u32 + a.size.height {
            let row = ((y * self.width) as usize + x0) * 4;
            for px in self.px[row..row + w * 4].chunks_exact_mut(4) {
                px.copy_from_slice(&v);
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Rgb565) -> Result<(), Self::Error> {
        let v = rgba(color);
        for px in self.px.chunks_exact_mut(4) {
            px.copy_from_slice(&v);
        }
        Ok(())
    }
}

impl Coverage for Frame<'_> {}

impl Coverage for RgbaFrame<'_> {
    fn smooth(&self) -> bool {
        self.smooth
    }

    fn blend(&mut self, x: i32, y: i32, color: Rgb565, alpha: u8) -> Result<(), Self::Error> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return Ok(());
        }
        let at = ((y as u32 * self.width + x as u32) * 4) as usize;
        let (a, keep) = (u16::from(alpha), 256 - u16::from(alpha));
        let src = rgba(color);
        for (d, s) in self.px[at..at + 3].iter_mut().zip(src) {
            *d = ((u16::from(s) * a + u16::from(*d) * keep) >> 8) as u8;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::primitives::{Circle, PrimitiveStyle};

    /// What is drawn into the RGBA frame is what the Rgb565 frame holds,
    /// each pixel widened by [`rgb888`]: the same picture.
    #[test]
    fn the_rgba_frame_is_the_rgb565_frame_widened() {
        let (w, h) = (37u32, 23u32);
        let mut fb = [0u16; 37 * 23];
        let mut px = [0u8; 37 * 23 * 4];
        let (Some(mut a), Some(mut b)) = (Frame::new(&mut fb, w, h), RgbaFrame::new(&mut px, w, h))
        else {
            panic!("frames");
        };
        let _ = a.clear(Rgb565::BLACK);
        let _ = b.clear(Rgb565::BLACK);
        for (i, raw) in [0x0000u16, 0xffff, 0x1234, 0xf81f, 0x07e0]
            .into_iter()
            .enumerate()
        {
            let c = Rgb565::from(RawU16::new(raw));
            let i = i as i32;
            let r = Rectangle::new(Point::new(i * 5 - 3, i * 3), Size::new(14, 9));
            let _ = a.fill_solid(&r, c);
            let _ = b.fill_solid(&r, c);
            let circle =
                Circle::new(Point::new(i * 7, 2), 9).into_styled(PrimitiveStyle::with_fill(c));
            let _ = circle.draw(&mut a);
            let _ = circle.draw(&mut b);
        }
        for y in 0..h {
            for x in 0..w {
                let want = a.pixel(x, y).map(|c| {
                    let [r, g, b] = rgb888(c);
                    [r, g, b, 0xff]
                });
                assert_eq!(b.pixel(x, y), want, "({x}, {y})");
            }
        }
        assert!(RgbaFrame::new(&mut px, w + 1, h).is_none());
    }
}
