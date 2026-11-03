//! `leviculum-rrc`: Reticulum Relay Chat for Leviculum (#232). RRC is IRC
//! over Reticulum: a hub on the destination `rrc.hub`, one link per client,
//! rooms as names inside a CBOR envelope, no store and forward. This crate
//! is the codec and a client's state on a link, written from the RRC
//! specification 0.1.3 and the envelopes the reference hub (rrcd 0.3.2)
//! exchanged in periculum's RRC cells (`tests/vectors/`), not from any
//! client's or hub's code.
//!
//! - [`cbor`]: the CBOR subset an envelope uses, order-keeping maps and
//!   shortest heads, written here; no CBOR crate.
//! - [`envelope`]: the envelope's nine keys and thirteen types, and the
//!   hub's limits.
//! - [`session`]: what a client sends and what the hub's answers mean,
//!   without I/O, so it runs on a board as well as a desktop.
//!
//! The crate is `no_std` plus `alloc`, as Loquax's core is, so the M9 can
//! carry it.

#![no_std]

extern crate alloc;

pub mod cbor;
pub mod envelope;
pub mod session;

pub use cbor::{DecodeError, Float, Value};
pub use envelope::{Envelope, EnvelopeError, Kind, Limits, HUB_APP, HUB_ASPECT};
pub use session::{room_name, Event, LineKind, Received, Refusal, Session};
