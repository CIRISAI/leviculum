//! `leviculum-mapfill`: the one piece of map renderer the vector map needs
//! that `embedded-graphics` 0.8 lacks, a polygon fill (Codeberg #474, #293,
//! `docs/drafts/map-measurement-2026-10.md`).
//!
//! - [`fill`]: a scanline filler with integer edges and per-row buckets,
//!   holes, even-odd and nonzero, drawing spans into any
//!   [`DrawTarget`](embedded_graphics::draw_target::DrawTarget).
//! - [`stroke`]: wide lines as quads through the filler.
//! - [`fixture`]: the flat i16 geometry the host harness cuts from vector
//!   tiles, read in place from flash.
//! - [`render`]: one view from a fixture, phase by phase, under a clock.
//! - [`style`]: the two flat colour tables and the two layer profiles.
//! - [`frame`]: an Rgb565 framebuffer in the ST7789's byte order, and an
//!   RGBA one for screens with a compositor.
//! - [`coverage`]: span ends blended by their coverage, for a target that
//!   blends.
//!
//! `no_std`, no `alloc`, no float: the caller lends every buffer. The host
//! harness is `measure/`, the M9 measurement firmware `m9/`.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod coverage;
pub mod fill;
pub mod fixture;
pub mod frame;
pub mod render;
pub mod stroke;
pub mod style;

pub use fill::{FillRule, FillStats, Filler, Scratch, SUBPIXEL};
