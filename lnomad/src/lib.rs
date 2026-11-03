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

//! Phase 4a adds the network backend: [`url`] parses NomadNet page URLs and
//! [`fetch`] connects to a running `lnsd`/`rnsd` shared instance and retrieves a
//! page's bytes over the RNS request/response path.

//! Phase 4b adds the [`browser`] module: link resolution, the fetch/parse step
//! and the one-shot print and download sinks the `lnomad` binary drives when it
//! is not running the interactive [`tui`].

//! Without the default `std` feature the crate is the browser core, `no_std`
//! plus `alloc` (slice 4a of `docs/src/concepts/lnomad-everywhere.md`): the
//! model and [`update`], the layout ([`render`]), the scene, the intents and
//! the catalogue, the page and image caches, discovery, and the lists of
//! bookmarks and pins. `std` adds the platform layer: [`fetch`], [`browser`],
//! [`transfer`], the files ([`download`], [`identify`], [`identity`], the
//! stores of [`bookmarks`] and [`names`]), the image decoder and the command
//! line. `just lnomad-core-no-std` builds the core for the firmware triples.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod app;
pub mod bookmarks;
#[cfg(feature = "std")]
pub mod browser;
#[cfg(feature = "std")]
pub mod cli;
pub mod color;
pub mod discovery;
pub mod download;
#[cfg(feature = "std")]
pub mod fetch;
#[cfg(feature = "gui")]
pub mod gui;
#[cfg(feature = "std")]
pub mod identify;
pub mod identity;
pub mod image;
pub mod image_cache;
pub mod names;
pub mod page_cache;
pub mod render;
pub mod theme;
#[cfg(feature = "std")]
pub mod transfer;
#[cfg(feature = "tui")]
pub mod tui;
pub mod url;

pub use app::{update, AppEvent, Effect, Intent, Model};
pub use bookmarks::{Bookmark, Bookmarks};
#[cfg(feature = "std")]
pub use browser::BrowserOptions;
#[cfg(feature = "std")]
pub use cli::{resolve_args, Mode};
pub use color::{resolve_tier, rgb_to_ansi256, ColorDepth, ColorFlag, Ink, Tier};
pub use discovery::{
    is_nomad_node_announce, name_hash_is_nomad_node, nomad_node_name_hash, DiscoveredNode,
    NomadNodeRegistry,
};
#[cfg(feature = "std")]
pub use fetch::{FetchError, LentNode, NodeLend, NodeRef, OwnNode, Session};
#[cfg(feature = "std")]
pub use identify::IdentifyStore;
pub use page_cache::{CacheEntry, PageCache};
pub use render::{
    layout, render_with_options, RLine, RStyle, RenderedLink, RenderedPage, StyledChar,
};
pub use theme::{resolve_theme, Bg, Theme, ThemeFlag};
#[cfg(feature = "tui")]
pub use tui::{run_tui, to_ratatui_text, view, TerminalGuard, TerminalOps};
pub use url::{classify_link, parse_url, resolve_link, LinkKind, Target, UrlError};
