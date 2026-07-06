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

//! Phase 4b adds the [`browser`] module: the REPL command grammar, the
//! navigation state machine, and the fetch/parse/render/print loop that the
//! `lnomad` binary drives.

pub mod browser;
pub mod cli;
pub mod discovery;
pub mod fetch;
pub mod render;
pub mod url;

pub use browser::{parse_command, resolve_link, BrowserOptions, Command, Nav};
pub use cli::{resolve_args, Mode};
pub use discovery::{
    is_nomad_node_announce, name_hash_is_nomad_node, nomad_node_name_hash, DiscoveredNode,
    NomadNodeRegistry,
};
pub use fetch::{FetchError, Session};
pub use render::{render_with_options, RenderedLink, RenderedPage};
pub use url::{parse_url, Target, UrlError};
