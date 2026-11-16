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
//!
//! Since 1507 the frame holds its words in the panel's byte order, high
//! byte first in memory, and starts on a [`PANEL_ALIGN`] boundary, so the
//! board's DMA sends a run of rows straight from it (PSRAM's cache line
//! is the alignment the ESP32-S3's DMA asks of external memory), and
//! [`Frame::take_runs`] hands the changed rows as runs, one panel window
//! each, instead of one band from the first change to the last.

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

/// The byte boundary the frame's first pixel sits on: 64, the widest
/// cache line the ESP32-S3's PSRAM DMA may ask for. A row is 640 bytes,
/// ten of them, so every row starts on it too.
pub const PANEL_ALIGN: usize = 64;

/// Words a frame holds: one a pixel.
const WORDS: usize = (WIDTH * HEIGHT) as usize;

/// A raw RGB565 word as the panel takes it in memory: high byte first.
fn to_panel(raw: u16) -> u16 {
    u16::from_ne_bytes(raw.to_be_bytes())
}

/// The raw RGB565 word of one the panel takes.
fn from_panel(word: u16) -> u16 {
    u16::from_be_bytes(word.to_ne_bytes())
}

/// One frame of the panel, row-major, a raw RGB565 word a pixel, in the
/// panel's byte order.
pub struct Frame {
    /// The words with room to align them: the frame is
    /// `pixels[start..start + WORDS]`.
    pixels: Vec<u16>,
    start: usize,
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
        let (pixels, start) = aligned();
        Self {
            pixels,
            start,
            dirty: Cell::new(Some((0, HEIGHT - 1))),
            flushed: RefCell::new(vec![None; HEIGHT as usize]),
        }
    }

    /// A frame of these raw RGB565 words, row by row: a dump read back.
    /// `None` unless there are exactly [`WIDTH`] x [`HEIGHT`] of them.
    pub fn from_pixels(pixels: Vec<u16>) -> Option<Self> {
        if pixels.len() != WORDS {
            return None;
        }
        let (mut words, start) = aligned();
        for (to, raw) in words[start..start + WORDS].iter_mut().zip(pixels) {
            *to = to_panel(raw);
        }
        Some(Self {
            pixels: words,
            start,
            dirty: Cell::new(None),
            flushed: RefCell::new(vec![None; HEIGHT as usize]),
        })
    }

    /// The words, row by row, in the panel's byte order (1507): what a
    /// flush sends. [`Frame::pixel`] reads one as RGB565.
    pub fn pixels(&self) -> &[u16] {
        &self.pixels[self.start..self.start + WORDS]
    }

    /// Rows `first..=last`, whole, in the panel's byte order: one window's
    /// bytes, starting on [`PANEL_ALIGN`]. Rows off the frame are cut.
    pub fn rows(&self, first: u32, last: u32) -> &[u16] {
        let last = last.min(HEIGHT - 1);
        let first = first.min(last);
        &self.pixels()[(first * WIDTH) as usize..((last + 1) * WIDTH) as usize]
    }

    /// The raw RGB565 word at `(x, y)`, black off the frame.
    pub fn pixel(&self, x: u32, y: u32) -> u16 {
        match x < WIDTH && y < HEIGHT {
            true => from_panel(self.pixels()[(y * WIDTH + x) as usize]),
            false => 0,
        }
    }

    /// The rows `first..=last` drawn into since the last call that differ
    /// from what the last call handed out, and none from now on: what the
    /// board flushes after a draw. `None` when the draw changed no row
    /// (1455).
    pub fn take_dirty(&self) -> Option<(u32, u32)> {
        let runs = self.take_runs();
        Some((runs.first()?.0, runs.last()?.1))
    }

    /// As [`Frame::take_dirty`], the changed rows as runs of contiguous
    /// rows, top first (1507): a run is one window on the panel, and the
    /// rows between two runs, unchanged, are not sent.
    pub fn take_runs(&self) -> Vec<(u32, u32)> {
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let Some((first, last)) = self.dirty.take() else {
            return runs;
        };
        let mut flushed = self.flushed.borrow_mut();
        for y in first..=last.min(HEIGHT - 1) {
            let start = (y * WIDTH) as usize;
            let hash = row_hash(&self.pixels()[start..start + WIDTH as usize]);
            let slot = &mut flushed[y as usize];
            if *slot == Some(hash) {
                continue;
            }
            *slot = Some(hash);
            match runs.last_mut() {
                Some(run) if run.1 + 1 == y => run.1 = y,
                _ => runs.push((y, y)),
            }
        }
        runs
    }

    /// Every row still to be written, as after [`Frame::new`]: a write
    /// that did not reach the panel leaves the panel's memory unknown
    /// (1485), so the next flush is whole.
    pub fn forget_flushed(&self) {
        self.dirty.set(Some((0, HEIGHT - 1)));
        self.flushed.borrow_mut().fill(None);
    }

    /// The frame as 8-bit RGB, three bytes a pixel, each channel widened by
    /// repeating its high bits, so white stays 255: what a PNG holds.
    pub fn to_rgb888(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(WORDS * 3);
        for &word in self.pixels() {
            let p = from_panel(word);
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

/// A black frame's words with room to align them, and where the frame
/// starts in them: the first word on a [`PANEL_ALIGN`] boundary. The
/// vector never grows, so its words never move.
fn aligned() -> (Vec<u16>, usize) {
    let pad = PANEL_ALIGN / 2;
    let pixels = vec![0u16; WORDS + pad];
    let addr = pixels.as_ptr() as usize;
    let start = ((PANEL_ALIGN - addr % PANEL_ALIGN) % PANEL_ALIGN) / 2;
    (pixels, start)
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
                let start = self.start;
                let at = &mut self.pixels[start + (y * WIDTH + x) as usize];
                let raw = to_panel(RawU16::from(colour).into_inner());
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

/// The panel's frame blends (1565): the map's picture drawn straight
/// onto it, without the kept copy (the census's draw, a host's), has its
/// edges and names smoothed as the kept picture has them
/// ([`crate::map::Kept`]), mixed by the same [`crate::map::mix`].
impl leviculum_mapfill::coverage::Coverage for Frame {
    fn smooth(&self) -> bool {
        true
    }

    fn blend(&mut self, x: i32, y: i32, color: Rgb565, alpha: u8) -> Result<(), Self::Error> {
        let (Ok(ux), Ok(uy)) = (u32::try_from(x), u32::try_from(y)) else {
            return Ok(());
        };
        if ux >= WIDTH || uy >= HEIGHT {
            return Ok(());
        }
        let mixed = crate::map::mix(self.pixel(ux, uy), color, alpha);
        self.draw_iter(core::iter::once(Pixel(
            Point::new(x, y),
            Rgb565::from(RawU16::new(mixed)),
        )))
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
        assert_eq!(frame.pixel(1, 2), 0xFFFF);
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

    /// The words are in the panel's byte order, high byte first in
    /// memory, the frame starts on the DMA's boundary, and a pixel reads
    /// back as the colour drawn (1507).
    #[test]
    fn the_frame_is_in_panel_byte_order_on_the_dma_boundary() {
        let mut frame = Frame::new();
        assert_eq!(frame.pixels().as_ptr() as usize % PANEL_ALIGN, 0);
        assert_eq!(frame.rows(3, 5).as_ptr() as usize % PANEL_ALIGN, 0);
        assert_eq!(frame.rows(3, 5).len(), 3 * WIDTH as usize);
        let red = Rgb565::new(31, 0, 0);
        let _ = frame.draw_iter([Pixel(Point::new(0, 0), red)]);
        assert_eq!(frame.pixel(0, 0), 0xF800);
        let bytes = frame.pixels()[0].to_ne_bytes();
        assert_eq!(bytes, [0xF8, 0x00], "high byte first in memory");
        let back = Frame::from_pixels(
            (0..HEIGHT)
                .flat_map(|y| (0..WIDTH).map(move |x| (x + y) as u16))
                .collect(),
        )
        .expect("a whole frame");
        assert_eq!(back.pixel(7, 2), 9);
        assert_eq!(back.pixels().as_ptr() as usize % PANEL_ALIGN, 0);
    }

    /// Two changes apart are two runs, and the rows between them are no
    /// one's: the panel gets two windows (1507).
    #[test]
    fn the_changed_rows_come_as_runs() {
        let mut frame = Frame::new();
        assert_eq!(frame.take_runs(), [(0, HEIGHT - 1)], "the first is whole");
        let _ = frame.draw_iter([
            Pixel(Point::new(5, 30), Rgb565::WHITE),
            Pixel(Point::new(9, 31), Rgb565::WHITE),
            Pixel(Point::new(9, 200), Rgb565::WHITE),
        ]);
        assert_eq!(frame.take_runs(), [(30, 31), (200, 200)]);
        assert!(frame.take_runs().is_empty());
        // Drawn back to what was flushed between two changes: not a row.
        let _ = frame.draw_iter([
            Pixel(Point::new(1, 10), Rgb565::RED),
            Pixel(Point::new(1, 11), Rgb565::RED),
            Pixel(Point::new(1, 11), Rgb565::BLACK),
            Pixel(Point::new(1, 12), Rgb565::RED),
        ]);
        assert_eq!(frame.take_runs(), [(10, 10), (12, 12)]);
    }

    /// A flush that failed left the panel unknown (1493): `take_dirty`
    /// hands the hashes in before the SPI write, so the board calls
    /// `forget_flushed` on an error and the next flush is whole — without
    /// it, the failed band stays stale for ever.
    #[test]
    fn a_failed_flush_owes_the_whole_frame() {
        let mut frame = Frame::new();
        let _ = frame.take_dirty();
        let _ = frame.draw_iter([Pixel(Point::new(5, 30), Rgb565::WHITE)]);
        assert_eq!(frame.take_dirty(), Some((30, 30)));
        // The SPI refused; without forget_flushed the row is never sent.
        assert_eq!(frame.take_dirty(), None, "the finding: nothing owed");
        frame.forget_flushed();
        assert_eq!(frame.take_dirty(), Some((0, HEIGHT - 1)));
    }
}
