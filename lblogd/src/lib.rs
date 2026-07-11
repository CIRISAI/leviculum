//! `lblogd`: a dev-blog server that serves Markdown posts over HTTP/HTTPS and
//! as a NomadNet node over Reticulum.
//!
//! Batch A provides the pure, offline logic only: the [`post`] module (the
//! post/content model, frontmatter parsing, slugs, directory loading) and the
//! [`render`] module (Markdown to HTML and Markdown to Micron, plus the index
//! and post page templates for both output formats). No networking lives here;
//! the HTTP server and the Reticulum node wiring come in later batches.

pub mod post;
pub mod render;
