//! `lnmapd`: the map store daemon, and the store it serves
//! (`docs/src/concepts/map-server.md`; Codeberg #232, order 1686).
//!
//! The store is a directory of immutable, content-addressed pieces plus
//! one signed index ([`store`]). `lnmapcut` (this crate's second
//! binary) cuts a region file into pieces and signs the index;
//! `lnmapd` serves the directory over HTTP ([`serve`]) and over
//! Reticulum as the `lev.maps` destination ([`node`]) — one artefact,
//! two transports, the same piece ids. A device verifies everything it
//! receives against the one setting it pins, the server's Reticulum
//! destination ([`client`]), so any carrier may hold the pieces.

pub mod client;
pub mod node;
pub mod serve;
pub mod store;
