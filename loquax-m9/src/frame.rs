//! The panel's frame: 320 x 240 RGB565 on the heap, 153,600 B, as the M9
//! mounts its ST7789 (`docs/src/concepts/thinknode-m9.md`, "The board as
//! the UI sees it"). The host draws into it for the goldens; on the board
//! it is what a flush sends.

use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;

/// The panel's width as mounted, landscape.
pub const WIDTH: u32 = 320;
/// Its height.
pub const HEIGHT: u32 = 240;

/// One frame of the panel, row-major, a raw RGB565 word a pixel.
pub struct Frame {
    pixels: Vec<u16>,
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    /// A black frame.
    pub fn new() -> Self {
        Self {
            pixels: vec![0; (WIDTH * HEIGHT) as usize],
        }
    }

    /// The raw RGB565 words, row by row.
    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    /// The frame as 8-bit RGB, three bytes a pixel, each channel widened by
    /// repeating its high bits, so white stays 255: what a PNG holds.
    pub fn to_rgb888(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 3);
        for &p in &self.pixels {
            let r = ((p >> 11) & 0x1f) as u8;
            let g = ((p >> 5) & 0x3f) as u8;
            let b = (p & 0x1f) as u8;
            out.extend([
                (r << 3) | (r >> 2),
                (g << 2) | (g >> 4),
                (b << 3) | (b >> 2),
            ]);
        }
        out
    }
}

impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Frame {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(at, colour) in pixels {
            let (Ok(x), Ok(y)) = (u32::try_from(at.x), u32::try_from(at.y)) else {
                continue;
            };
            if x < WIDTH && y < HEIGHT {
                self.pixels[(y * WIDTH + x) as usize] = RawU16::from(colour).into_inner();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_lands_where_it_was_drawn_and_off_screen_ones_do_not() {
        let mut frame = Frame::new();
        let _ = frame.draw_iter([
            Pixel(Point::new(1, 2), Rgb565::WHITE),
            Pixel(Point::new(-1, 0), Rgb565::WHITE),
            Pixel(Point::new(320, 0), Rgb565::WHITE),
        ]);
        let lit: Vec<usize> = (0..frame.pixels().len())
            .filter(|&i| frame.pixels()[i] != 0)
            .collect();
        assert_eq!(lit, [(2 * WIDTH + 1) as usize]);
        assert_eq!(
            &frame.to_rgb888()[lit[0] * 3..lit[0] * 3 + 3],
            &[255, 255, 255]
        );
    }
}
