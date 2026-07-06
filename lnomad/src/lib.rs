//! `lnomad`: a terminal browser for NomadNet micron pages.
//!
//! Phase 3 provides the [`render`] module: it turns a parsed
//! [`leviculum_micron::MicronDocument`] into ANSI terminal output for a given
//! width and collects the page's links for navigation. It is pure and testable,
//! with no networking. Phase 4 adds the fetch layer and the interactive CLI to
//! this same crate.
//!
//! ```
//! use leviculum_micron::parse;
//! use lnomad::render::render;
//!
//! let doc = parse(">> Title\n\nSome `!bold`! text and a `[link`:/page/x].");
//! let page = render(&doc, 80);
//! assert!(page.text.contains("Title"));
//! assert_eq!(page.links.len(), 1);
//! ```

pub mod render;

pub use render::{render, render_with_options, RenderedLink, RenderedPage};
