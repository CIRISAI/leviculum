//! The panel's frame: 320 x 240 RGB565 on the heap, 153,600 B, as the M9
//! mounts its ST7789 (`docs/src/concepts/thinknode-m9.md`, "The board as
//! the UI sees it"). The host draws into it for the goldens; on the board
//! it is what a flush sends.
//!
//! It remembers which rows a draw touched since the board last flushed
//! ([`Frame::take_dirty`]): a full flush is 30.8 ms on the shared 40 MHz
//! bus (`thinknode-m9.md`, "The board as the UI sees it"), and the ST7789's
//! row window lets the board send only the band that changed, which for a
//! moved cursor is two entries of the list, not the screen. A redraw that
//! changes nothing on the screen flushes nothing (1455): a pixel drawn in
//! the colour it has is no change, and a row drawn over and back within
//! one redraw (the map's picture over its pins, the pins again) is held
//! against a hash of the row as it was last flushed.

use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

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
    /// The first and last row drawn into since the last
    /// [`Frame::take_dirty`]. A cell, because the renderer hands the frame
    /// back only by shared reference.
    dirty: Cell<Option<(u32, u32)>>,
    /// Each row's hash as it was last flushed; `None` before its first.
    flushed: RefCell<Vec<Option<u64>>>,
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    /// A black frame, all of it still to be flushed: the panel's memory
    /// holds anything after its reset, and a pixel drawn black would
    /// otherwise never be sent (1455).
    pub fn new() -> Self {
        Self {
            pixels: vec![0; (WIDTH * HEIGHT) as usize],
            dirty: Cell::new(Some((0, HEIGHT - 1))),
            flushed: RefCell::new(vec![None; HEIGHT as usize]),
        }
    }

    /// A frame of these raw RGB565 words, row by row: a dump read back.
    /// `None` unless there are exactly [`WIDTH`] x [`HEIGHT`] of them.
    pub fn from_pixels(pixels: Vec<u16>) -> Option<Self> {
        (pixels.len() == (WIDTH * HEIGHT) as usize).then(|| Self {
            pixels,
            dirty: Cell::new(None),
            flushed: RefCell::new(vec![None; HEIGHT as usize]),
        })
    }

    /// The raw RGB565 words, row by row.
    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    /// The raw RGB565 word at `(x, y)`, black off the frame.
    pub fn pixel(&self, x: u32, y: u32) -> u16 {
        match x < WIDTH && y < HEIGHT {
            true => self.pixels[(y * WIDTH + x) as usize],
            false => 0,
        }
    }

    /// The rows `first..=last` drawn into since the last call that differ
    /// from what the last call handed out, and none from now on: what the
    /// board flushes after a draw. `None` when the draw changed no row
    /// (1455).
    pub fn take_dirty(&self) -> Option<(u32, u32)> {
        let (first, last) = self.dirty.take()?;
        let mut flushed = self.flushed.borrow_mut();
        let mut band: Option<(u32, u32)> = None;
        for y in first..=last.min(HEIGHT - 1) {
            let start = (y * WIDTH) as usize;
            let hash = row_hash(&self.pixels[start..start + WIDTH as usize]);
            let slot = &mut flushed[y as usize];
            if *slot != Some(hash) {
                *slot = Some(hash);
                band = Some(band.map_or((y, y), |(a, _)| (a, y)));
            }
        }
        band
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

/// A row's FNV-1a hash, 64 bits: a row drawn back to what was flushed
/// hashes the same.
fn row_hash(row: &[u16]) -> u64 {
    row.iter().fold(0xcbf2_9ce4_8422_2325, |h, &p| {
        let h = (h ^ u64::from(p & 0xff)).wrapping_mul(0x0000_0100_0000_01b3);
        (h ^ u64::from(p >> 8)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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
                // A pixel drawn again in the colour it has changes nothing
                // the panel shows, and is not flushed (1455): the map's
                // picture and the cells are drawn whole on every redraw.
                let at = &mut self.pixels[(y * WIDTH + x) as usize];
                let raw = RawU16::from(colour).into_inner();
                if *at == raw {
                    continue;
                }
                *at = raw;
                let band = match self.dirty.get() {
                    Some((first, last)) => (first.min(y), last.max(y)),
                    None => (y, y),
                };
                self.dirty.set(Some(band));
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

    #[test]
    fn the_dirty_band_is_the_rows_drawn_since_the_last_flush() {
        let mut frame = Frame::new();
        assert_eq!(
            frame.take_dirty(),
            Some((0, HEIGHT - 1)),
            "the first flush is whole"
        );
        assert_eq!(frame.take_dirty(), None);
        let _ = frame.draw_iter([
            Pixel(Point::new(5, 30), Rgb565::WHITE),
            Pixel(Point::new(9, 12), Rgb565::WHITE),
            Pixel(Point::new(0, 999), Rgb565::WHITE),
        ]);
        assert_eq!(frame.take_dirty(), Some((12, 30)));
        assert_eq!(frame.take_dirty(), None, "a flush empties the band");
    }

    /// A pixel drawn again in its own colour is no change (1455): the
    /// same pixels drawn a second time leave nothing to flush; one of
    /// them in another colour is its row.
    #[test]
    fn a_pixel_drawn_unchanged_is_not_flushed() {
        let mut frame = Frame::new();
        let _ = frame.take_dirty();
        let draw = |frame: &mut Frame, colour| {
            let _ = frame.draw_iter([
                Pixel(Point::new(5, 30), Rgb565::WHITE),
                Pixel(Point::new(9, 12), colour),
            ]);
        };
        draw(&mut frame, Rgb565::WHITE);
        assert_eq!(frame.take_dirty(), Some((12, 30)));
        draw(&mut frame, Rgb565::WHITE);
        assert_eq!(frame.take_dirty(), None, "the same pixels flushed again");
        draw(&mut frame, Rgb565::RED);
        assert_eq!(frame.take_dirty(), Some((12, 12)));
        // Drawn over and back within one redraw: nothing to flush.
        draw(&mut frame, Rgb565::BLUE);
        draw(&mut frame, Rgb565::RED);
        assert_eq!(frame.take_dirty(), None, "drawn back to what was flushed");
    }
}
