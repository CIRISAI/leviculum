//! `loquax` — a terminal LXMF messenger for Reticulum.
//!
//! This crate carries the non-interactive slices of the program described in
//! `docs/src/concepts/loquax.md`: the send path — direct, propagated, or
//! `auto` with the Columba-style fallback to a propagation node — and the
//! mailbox drain (`loquax fetch`) — and the model of `loquax tui`'s screen
//! ([`ui`]), whose terminal frontend is the binary's, behind the `tui`
//! feature.
//!
//! # The two structural constraints
//!
//! **The LXMF router lives here, never in `lnsd`.** The `lnsd`-resident
//! variant is rejected twice over in the architecture record
//! (`docs/src/concepts/loquax-architecture.md` §4): it would put LXMF knowledge
//! into the transport daemon that the Codeberg #196 seam exists to keep clean,
//! and client programs do not merge into `lnsd`.
//!
//! **`loquax` attaches to a running shared instance.** It does not bring up a
//! Reticulum stack of its own. With no daemon running, `loquax send` fails and
//! says so; it never silently stands up a private stack, because that fallback
//! is a later decision and not part of this slice.
//!
//! # The layering, and why it is drawn where it is
//!
//! The 2026-08-08 process-model decision is "C, built as A first": one process
//! now, with the router and the store behind an interface so that daemon mode
//! is later a wiring change rather than a rewrite. That interface is
//! [`outbox::Outbox`] — commands in, events out, no Reticulum type in the
//! signatures beyond a destination hash and a message id.
//!
//! ```text
//!   main.rs  ──> send::run_send(&mut impl Outbox, …)      the frontend
//!                        │
//!                        │  Command / OutboxEvent          <- the seam
//!                        v
//!   engine::InProcessOutbox ── channels ── engine::Engine   the backend
//!                                            (its own queues, driven from a
//!                                             CoreProcessor holding it, with
//!                                             an LxmfRouter, under the core lock)
//! ```
//!
//! [`send::run_send`] never names an `LxmfRouter`, a `ReticulumNode` or a
//! channel, which is what makes it testable against a fake outbox with no
//! network anywhere; a daemon-mode `Outbox` speaking to `loquaxd` over a socket
//! would drop into the same slot.
//!
//! # Honesty about delivery
//!
//! Decided 2026-08-10 (`loquax-architecture.md` §6): `send` does not wait for a
//! delivery proof, and its success must never be worded as "delivered" or
//! "sent to" a person. The word this program uses is *queued*, and the one
//! stronger thing it will claim is that the message was *handed on* — that the
//! stack took the bytes and put them on the network. Whether anything received
//! them is a history, not a result, and `loquax status` / `--wait` are the
//! (not-yet-built) answers to that question.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// The version this build carries, with the commit it was built from:
/// `0.1.0 (1a2b3c…)`, what `loquax --version` prints (`build.rs`). The
/// Android app shows it on its first screen.
pub const VERSION: &str = env!("LEVICULUM_VERSION");

pub mod address;
pub mod body;
#[cfg(feature = "browse")]
pub mod browse_fetch;
pub mod clock;
pub mod code;
pub mod config;
pub mod display_name;
pub mod effects;
pub mod emoji;
pub mod engine;
pub mod events;
#[cfg(feature = "history")]
pub mod export;
#[cfg(feature = "std")]
pub mod fanout;
pub mod fetch;
#[cfg(feature = "std")]
pub mod file_save;
#[cfg(feature = "history")]
pub mod heard;
pub mod home;
#[cfg(feature = "std")]
pub mod host_load;
#[cfg(feature = "std")]
pub mod identity;
#[cfg(feature = "history")]
pub mod import;
pub mod keys;
pub mod lxma;
pub mod mailbox;
pub mod mailbox_state;
#[cfg(feature = "screenmap")]
pub mod mapframe;
#[cfg(feature = "mapload")]
pub mod mapload;
#[cfg(feature = "mapnames")]
pub mod mapnames;
pub mod mesh_position;
pub mod name_match;
pub mod names;
pub mod outbox;
#[cfg(feature = "photos")]
pub mod photo;
#[cfg(feature = "picture")]
pub mod picture;
pub mod pn_status;
#[cfg(feature = "std")]
pub mod rooms_link;
#[cfg(feature = "scan")]
pub mod scan;
#[cfg(feature = "screenmap")]
pub mod screenmap;
pub mod seen;
pub mod send;
#[cfg(feature = "history")]
pub mod store;
pub mod sync;
pub mod telemetry;
#[cfg(feature = "picture")]
pub mod tiles;
pub mod ui;
#[cfg(feature = "voice")]
pub mod voice;
pub mod when;
#[cfg(feature = "picture")]
pub mod zoneclock;
