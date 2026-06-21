//! reticulum-std: Standard library extensions for reticulum
//!
//! This crate provides std-dependent functionality:
//! - Network interfaces (TCP, UDP, Local/IPC)
//! - Serial interfaces (KISS, RNode)
//! - Configuration loading and persistence
//! - File-based storage
//! - Async runtime integration (tokio)
//!
//! Use reticulum-core for the no_std compatible core functionality,
//! including the buffer system types (RawChannelReader, RawChannelWriter).

#![warn(unreachable_pub)]

pub mod api;
pub(crate) mod clock;
pub mod config;
pub mod driver;
pub mod error;
pub mod event_log;
pub mod file_identity_store;
pub(crate) mod file_known_destinations_store;
pub(crate) mod file_packet_hash_store;
pub(crate) mod file_ratchet_store;
pub(crate) mod ini_config;
pub mod interfaces;
pub(crate) mod known_destinations;
pub(crate) mod packet_hashlist;
pub mod reticulum;
pub(crate) mod rpc;
pub(crate) mod storage;
pub mod test_support;

// Re-export commonly used core types for the high-level API
pub use reticulum_core::node::{DeliveryError, EventClass, LinkStats, NodeEvent};
pub use reticulum_core::transport::PathTableExport;
pub use reticulum_core::{
    AnnounceError, Destination, DestinationHash, DestinationType, Direction, Identity,
    LinkCloseReason, LinkError, LinkId, PeerKeys, ProofStrategy, ReceivedAnnounce, SendError,
    TransportStats,
};

/// Generate a new random identity using the system RNG.
///
/// Convenience wrapper around `Identity::generate(&mut OsRng)` for std apps.
/// Embedded code should use `Identity::generate()` with a platform-specific RNG.
pub fn generate_identity() -> Identity {
    Identity::generate(&mut rand_core::OsRng)
}

pub use config::Config;
pub use driver::{
    EventReceiver, InterfaceStatusSnapshot, LinkHandle, PacketSender, ReticulumNode,
    ReticulumNodeBuilder,
};
pub use error::{Error, Result};
pub use reticulum::Reticulum;
/// Client for the shared-instance RPC socket (`rnstatus`/`rnpath` protocol).
/// Used by `lns diag` to query a running `lnsd`/`rnsd`.
pub use rpc::rpc_query;
