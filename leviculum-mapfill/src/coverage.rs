//! Coverage antialiasing at the ends of the filler's spans (1422, the eye
//! candy study's 2.1a): a target that blends says so, and the filler then
//! mixes the first and last pixel of each span by how much of it the span
//! covers, from the 16.16 crossing it already knows. Two blends a span, no
//! edge more; steep edges come out smooth, flat ones partly.
//!
//! A target that does not blend keeps every pixel hard, the picture the
//! filler drew before: the panel's Rgb565 frame on the M9, the terminal's
//! frame, the goldens. Only a frame with eight bits a channel blends
//! ([`crate::frame::RgbaFrame`] when asked), so the M9 never pays for it.

use embedded_graphics::draw_target::{Clipped, Cropped};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

/// A target the filler can draw partly covered pixels into.
pub trait Coverage: DrawTarget {
    /// Whether span ends are blended by their coverage. When not, the
    /// filler draws every span hard and never calls [`blend`](Self::blend).
    fn smooth(&self) -> bool {
        false
    }

    /// Blend `color` over the pixel at `(x, y)` by `alpha` of 256, 1 to
    /// 255; a pixel outside the target is left.
    fn blend(&mut self, x: i32, y: i32, color: Self::Color, alpha: u8) -> Result<(), Self::Error> {
        let _ = (x, y, color, alpha);
        Ok(())
    }
}

impl<T: DrawTarget> Coverage for Cropped<'_, T> {}

impl<T: DrawTarget> Coverage for Clipped<'_, T> {}

/// `target` drawn into within `area` only: embedded-graphics' `clipped`,
/// with the blend passed through.
pub struct Clip<'a, D> {
    target: &'a mut D,
    area: Rectangle,
}

impl<'a, D: DrawTarget> Clip<'a, D> {
    /// `target` within `area`, itself within the target.
    pub fn new(target: &'a mut D, area: &Rectangle) -> Self {
        let area = area.intersection(&target.bounding_box());
        Self { target, area }
    }
}

impl<D: DrawTarget> Dimensions for Clip<'_, D> {
    /// The area, as embedded-graphics' `Clipped` has it: what the filler
    /// clips its edges to.
    fn bounding_box(&self) -> Rectangle {
        self.area
    }
}

impl<D: DrawTarget> DrawTarget for Clip<'_, D> {
    type Color = D::Color;
    type Error = D::Error;

    fn draw_iter<I: IntoIterator<Item = Pixel<D::Color>>>(
        &mut self,
        it: I,
    ) -> Result<(), D::Error> {
        let area = self.area;
        self.target
            .draw_iter(it.into_iter().filter(|Pixel(p, _)| area.contains(*p)))
    }

    fn fill_solid(&mut self, area: &Rectangle, color: D::Color) -> Result<(), D::Error> {
        self.target
            .fill_solid(&area.intersection(&self.area), color)
    }

    fn clear(&mut self, color: D::Color) -> Result<(), D::Error> {
        self.target.fill_solid(&self.area, color)
    }
}

impl<D: Coverage> Coverage for Clip<'_, D> {
    fn smooth(&self) -> bool {
        self.target.smooth()
    }

    fn blend(&mut self, x: i32, y: i32, color: D::Color, alpha: u8) -> Result<(), D::Error> {
        match self.area.contains(Point::new(x, y)) {
            true => self.target.blend(x, y, color, alpha),
            false => Ok(()),
        }
    }
}

/// `target` with every pixel drawn hard, whatever it blends: a layer the
/// other paths draw hard too, so a picture drawn whole and one composed
/// from tiles agree there (the world outline under no region, 1422).
pub struct Hard<'a, D>(pub &'a mut D);

impl<D: DrawTarget> Dimensions for Hard<'_, D> {
    fn bounding_box(&self) -> Rectangle {
        self.0.bounding_box()
    }
}

impl<D: DrawTarget> DrawTarget for Hard<'_, D> {
    type Color = D::Color;
    type Error = D::Error;

    fn draw_iter<I: IntoIterator<Item = Pixel<D::Color>>>(
        &mut self,
        it: I,
    ) -> Result<(), D::Error> {
        self.0.draw_iter(it)
    }

    fn fill_solid(&mut self, area: &Rectangle, color: D::Color) -> Result<(), D::Error> {
        self.0.fill_solid(area, color)
    }

    fn clear(&mut self, color: D::Color) -> Result<(), D::Error> {
        self.0.clear(color)
    }
}

impl<D: DrawTarget> Coverage for Hard<'_, D> {}
