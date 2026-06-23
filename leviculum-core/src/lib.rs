//! Cryptographic mesh networking protocol for resilient communication over any
//! medium.
//!
//! `leviculum-core` implements all protocol logic as `no_std + alloc`, making it
//! suitable for both full operating systems (Linux, macOS, Windows) and bare-metal
//! embedded targets (ESP32, nRF52, STM32). Platform-specific I/O is injected via
//! the [`Clock`] and [`Storage`] traits. See the [`traits`] module.
//!
//! # Core Concepts
//!
//! | Concept | Type | Purpose |
//! |---------|------|---------|
//! | Identity | [`Identity`] | Dual keypair (X25519 + Ed25519) for encryption and signing |
//! | Destination | [`Destination`] | Addressable endpoint identified by a 16-byte hash |
//! | Announce | [`ReceivedAnnounce`] | Broadcast presence notification with public keys |
//! | Transport | [`transport`] | Routing, path discovery, and packet forwarding |
//! | NodeCore | [`NodeCore`] | High-level unified API that ties everything together |
//!
//! # Typical Usage Flow
//!
//! 1. Create an [`Identity`] (or load an existing one)
//! 2. Register a [`Destination`] on a [`NodeCore`]
//! 3. Send an announce so the network learns about this destination
//! 4. Receive announces from peers and open links to them
//! 5. Exchange messages over established links
//!
//! # Platform Dependencies
//!
//! Functions that need platform services take explicit parameters:
//! - `rng: &mut impl CryptoRngCore` - for randomness
//! - `now_ms: u64` - for timestamps
//! - `storage: &mut impl Storage` - for persistence
//!
//! ```
//! use rand_core::OsRng;
//! use leviculum_core::identity::Identity;
//!
//! let identity = Identity::generate(&mut OsRng);
//! ```
//!
//! # Crate Hierarchy
//!
//! ```text
//! leviculum-core   (no_std + alloc)  — all protocol logic
//!     │
//!     ▼
//! leviculum-std    (std)             — platform impls: SystemClock, TcpInterface, FileStorage
//!     │
//!     ▼
//! leviculum-cli / leviculum-ffi     — binaries and C-API
//! ```
//!
//! # Feature Flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `compression` | off | BZ2 compression via `libbz2-rs-sys` (pure-Rust) |

#![no_std]
#![warn(unreachable_pub)]
// With the `tracing` feature off (atomic-less MCU builds), the level macros are
// no-ops, so code that exists only to format trace messages (hex-formatting
// wrappers, trace-only locals) becomes unused. Tolerate that in the no-tracing
// config only; the default build keeps full warnings.
#![cfg_attr(
    not(feature = "tracing"),
    allow(dead_code, unused_variables, unused_imports)
)]

extern crate alloc;

/// Logging facade: the crate logs via `crate::tracing::{debug,trace,info,warn}!`.
///
/// With the `tracing` feature on, these re-export the real `tracing` crate's
/// macros. With it off — e.g. Cortex-M0 / thumbv6m, where tracing-core's
/// atomic-CAS callsite registry will not compile — they become no-ops, so the
/// crate builds on atomic-less MCUs without touching any call site.
#[cfg(feature = "tracing")]
pub(crate) mod tracing {
    pub(crate) use ::tracing::{debug, info, trace, warn};
}
#[cfg(not(feature = "tracing"))]
pub(crate) mod tracing {
    macro_rules! noop {
        ($($tt:tt)*) => {{}};
    }
    pub(crate) use noop as debug;
    pub(crate) use noop as info;
    pub(crate) use noop as trace;
    pub(crate) use noop as warn;
}

pub(crate) mod announce;
#[cfg(feature = "compression")]
pub mod compression;
pub mod constants;
pub mod crypto;
pub(crate) mod destination;
pub mod embedded_storage;
pub mod framing;
mod hex_fmt;
pub mod identity;
pub mod identity_store;
pub mod ifac;
pub mod known_destinations;
pub mod link;
pub mod memory_storage;
pub mod node;
pub mod packet;
pub mod packet_hash_store;
pub(crate) mod ratchet;
pub mod ratchet_store;
pub(crate) mod receipt;
pub mod resource;
pub mod rnode;
pub mod storage_types;
#[cfg(test)]
pub(crate) mod test_utils;
pub mod traits;
pub mod transport;

// Re-export key types
pub use announce::{AnnounceControl, AnnounceError, ReceivedAnnounce};
pub use destination::{
    Destination, DestinationError, DestinationHash, DestinationType, Direction, ProofStrategy,
};
pub use identity::Identity;
pub use link::{LinkCloseReason, LinkError, LinkId, PeerKeys};
pub use node::{
    DeliveryError, LinkStats, NodeCore, NodeCoreBuilder, NodeEvent, RequestError, RequestPolicy,
    SendError,
};
pub use resource::{
    ResourceAdvertisement, ResourceError, ResourceFlags, ResourceStatus, ResourceStrategy,
};
pub use transport::{Action, InterfaceId, TickOutput, TransportStats};

// Re-export traits
pub use embedded_storage::EmbeddedStorage;
pub use memory_storage::MemoryStorage;
pub use traits::{
    Clock, Interface, InterfaceError, InterfaceMode, NoStorage, Storage, StorageError,
};
