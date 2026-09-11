//! lnpnd — the LXMF propagation-node daemon (Codeberg #384, parts 1–2).
//!
//! The host-side mailbox role: announce `lxmf.propagation`, accept client
//! uploads, answer `/get`, peer with other propagation nodes over `/offer`
//! and sync stored messages both ways, on the file-backed store. The
//! protocol logic is `leviculum_lxmf::propagation_node` and
//! `leviculum_lxmf::peering`; this crate is wiring, a CLI and a run loop.
//!
//! # Why a separate binary and not an `lnmsg` mode
//!
//! The reference ships the role the same way: `lxmd` is its own console
//! daemon next to the client tools, with its own identity and its own
//! storage (`reference/LXMF/LXMF/Utilities/lxmd.py`), and Sideband or
//! `lnmsg`-shaped clients talk *to* it. Folding the role into `lnmsg` would
//! conflate the operator's messaging address with the node's service
//! identity — a propagation node's destination hash is what every client
//! configures, and it must not change because somebody reinstalled their
//! messenger. The daemon attaches to a running `lnsd`/`rnsd` shared
//! instance exactly like `lnmsg` and `lblogd` do; it does not start a stack
//! of its own.

pub mod engine;
pub mod identity;
pub(crate) mod peering;
