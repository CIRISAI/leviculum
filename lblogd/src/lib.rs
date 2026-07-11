//! `lblogd`: a dev-blog server that serves Markdown posts over HTTP/HTTPS and
//! as a NomadNet node over Reticulum.
//!
//! The [`post`] module holds the post/content model (frontmatter parsing,
//! slugs, directory loading), the [`render`] module the Markdown to HTML and
//! Markdown to Micron renderers plus the index and post page templates. The
//! [`node`] module is the NomadNet page node: a shared-instance client of a
//! running `lnsd` daemon that serves the rendered Micron pages over
//! Reticulum. The HTTP server and the daemon wiring (main, config) come in a
//! later batch.

pub mod node;
pub mod post;
pub mod render;
