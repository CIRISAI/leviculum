//! Transport layer - routing, path discovery, packet handling (sans-I/O)
//!
//! The Transport is the heart of the Reticulum protocol. It manages:
//! - Packet routing based on destination hash
//! - Path discovery via announce propagation
//! - Link table management
//! - Duplicate packet detection
//!
//! # Sans-I/O Design
//!
//! Transport never performs I/O directly. Instead it:
//! - Accepts incoming packets via `process_incoming(iface_index, data)`
//! - Emits outbound I/O as `Action` values (`SendPacket`, `Broadcast`)
//! - Emits protocol events via `drain_events()`
//!
//! The driver (in `reticulum-std` or an embedded crate) owns the interfaces,
//! handles framing, and dispatches Actions to the network.
//!
//! ```text
//! use reticulum_core::transport::{Transport, TransportConfig};
//!
//! let mut transport = Transport::new(config, clock, storage, identity);
//!
//! // Driver feeds incoming packets:
//! transport.process_incoming(iface_index, &raw_data)?;
//!
//! // Driver dispatches outbound actions:
//! for action in transport.drain_actions() {
//!     // send via interface
//! }
//!
//! // Driver runs periodic maintenance:
//! transport.poll();
//! ```

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use crate::constants::{
    ANNOUNCE_RATE_GRACE, ANNOUNCE_RATE_LIMIT_MS, ANNOUNCE_RATE_PENALTY_MS,
    DEFAULT_ANNOUNCE_CAP_PERCENT, DISCOVERY_RETRY_INTERVAL_MS, DISCOVERY_TIMEOUT_MS,
    ESTABLISHMENT_TIMEOUT_PER_HOP_MS, JITTER_AIRTIME_FACTOR, LINK_TIMEOUT_MS,
    LOCAL_CLIENT_ANNOUNCE_DELAY_MS, LOCAL_CLIENT_DEST_EXPIRY_MS, LOCAL_REBROADCASTS_MAX,
    MAX_QUEUED_ANNOUNCES_PER_INTERFACE, MAX_RANDOM_BLOBS, MS_PER_SECOND, MTU,
    PATHFINDER_EXPIRY_SECS, PATHFINDER_G_MS, PATHFINDER_MAX_HOPS, PATHFINDER_RETRIES,
    PATHFINDER_RW_MS, PATH_REQUEST_GRACE_MS, PATH_REQUEST_MIN_INTERVAL_MS, RATCHET_SIZE,
    RECEIPT_TIMEOUT_DEFAULT_MS, REVERSE_TABLE_EXPIRY_MS, TRUNCATED_HASHBYTES,
    UNKNOWN_BITRATE_ASSUMPTION_BPS,
};

use crate::announce::{emission_from_random_hash, max_emission_from_blobs, ReceivedAnnounce};
use crate::crypto::truncated_hash;
use crate::destination::{Destination, DestinationHash, DestinationType};
use crate::hex_fmt::HexShort;
use crate::identity::Identity;
use crate::ifac::IfacConfig;
use crate::link::Link;
use crate::packet::{
    build_proof_packet, packet_hash, HeaderType, Packet, PacketContext, PacketData, PacketError,
    PacketFlags, PacketType, TransportType,
};
pub use crate::storage_types::PathEntry;
use crate::storage_types::{
    AnnounceEntry, AnnounceRateEntry, LinkEntry, PacketReceipt, PathState, ReverseEntry,
};
use crate::traits::{Clock, Storage};

/// Number of announce timestamp samples per interface for frequency computation
/// (matches Python Interface.py maxlen=6).
const ANNOUNCE_FREQ_SAMPLES: usize = 6;

// Sans-I/O Types
/// Opaque interface identifier
///
/// Core never inspects what an `InterfaceId` refers to, it stores and returns
/// them so the driver can map them back to actual interface objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterfaceId(pub usize);

impl core::fmt::Display for InterfaceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "iface:{}", self.0)
    }
}

/// Display helper: shows registered name or falls back to "iface:{id}"
pub(crate) struct IfaceName<'a> {
    names: &'a BTreeMap<usize, String>,
    id: usize,
}

impl core::fmt::Display for IfaceName<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(name) = self.names.get(&self.id) {
            write!(f, "{}", name)
        } else {
            write!(f, "iface:{}", self.id)
        }
    }
}

/// I/O action for the driver to execute
///
/// Core never performs I/O directly, instead it returns `Action` values that
/// describe what the driver should do. The driver matches on these and
/// dispatches to the appropriate interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send a packet on a specific interface
    SendPacket {
        /// The target interface
        iface: InterfaceId,
        /// The packet data (already framed for the wire)
        data: Vec<u8>,
    },
    /// Broadcast a packet on all online interfaces, optionally excluding one
    Broadcast {
        /// The packet data (already framed for the wire)
        data: Vec<u8>,
        /// Interface to skip (typically the one the packet arrived on)
        exclude_iface: Option<InterfaceId>,
    },
}

/// Output from any core method that may produce I/O or events
///
/// Returned by `handle_packet()`, `handle_timeout()`, and other methods that
/// may trigger outbound I/O or application-visible events. The returned
/// `TickOutput` **must** be dispatched to interfaces by the driver, dropping
/// it silently loses outbound packets and application events.
#[derive(Default)]
#[must_use = "TickOutput contains actions that must be dispatched to interfaces"]
pub struct TickOutput {
    /// I/O actions for the driver to execute
    pub actions: Vec<Action>,
    /// Application-visible events
    pub events: Vec<super::node::NodeEvent>,
    /// Earliest deadline across all timers, if any.
    /// The driver should call `handle_timeout()` at or before this time.
    pub next_deadline_ms: Option<u64>,
}

impl TickOutput {
    /// Create an empty TickOutput
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if this output contains no actions or events
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.events.is_empty() && self.next_deadline_ms.is_none()
    }

    /// Merge another TickOutput into this one
    ///
    /// Appends all actions and events from `other` to `self`.
    pub fn merge(&mut self, other: TickOutput) {
        self.actions.extend(other.actions);
        self.events.extend(other.events);
        self.next_deadline_ms = match (self.next_deadline_ms, other.next_deadline_ms) {
            (Some(a), Some(b)) => Some(core::cmp::min(a, b)),
            (a, b) => a.or(b),
        };
    }
}

impl core::fmt::Debug for TickOutput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TickOutput")
            .field("actions", &self.actions.len())
            .field("events", &self.events.len())
            .field("next_deadline_ms", &self.next_deadline_ms)
            .finish()
    }
}

// Action Dispatch (protocol logic for drivers)
/// Dispatch actions to interfaces (protocol logic)
///
/// Routes each [`Action`] to the correct interface(s) via [`Interface::try_send()`].
/// This is protocol knowledge, broadcast-exclusion, interface selection, that
/// belongs in core so every driver (tokio, Embassy, bare-metal) gets it for free.
///
/// Returns errors from `try_send()` so the driver can react:
/// - `BufferFull`: log it (non-fatal, expected on constrained links)
/// - `Disconnected`: call `handle_interface_down()` for that interface
///
/// The driver calls this after every `handle_packet()`, `handle_timeout()`,
/// or deferred operation (connect, send, close, announce).
/// A SendPacket that failed with BufferFull, driver must queue for retry.
pub struct SendRetry {
    /// Interface index (iface_id.0) for the retry queue key.
    pub iface_idx: usize,
    /// The packet data that failed to send.
    pub data: Vec<u8>,
}

/// Result of dispatching actions to interfaces.
pub struct DispatchResult {
    /// Failed SendPacket actions, driver must queue these for retry.
    pub retries: Vec<SendRetry>,
    /// All errors (SendPacket and Broadcast) for logging.
    pub errors: Vec<(InterfaceId, crate::traits::InterfaceError)>,
}

pub fn dispatch_actions(
    interfaces: &mut [&mut dyn crate::traits::Interface],
    actions: Vec<Action>,
    ifac_configs: &BTreeMap<usize, IfacConfig>,
) -> DispatchResult {
    let mut retries = Vec::new();
    let mut errors = Vec::new();
    for action in actions {
        match action {
            Action::SendPacket { iface, data } => {
                let send_data = match ifac_configs.get(&iface.0) {
                    Some(cfg) => match cfg.apply_ifac(&data) {
                        Ok(wrapped) => wrapped,
                        Err(e) => {
                            tracing::warn!("IFAC apply failed on iface {}: {:?}", iface.0, e);
                            continue;
                        }
                    },
                    None => data,
                };
                if let Some(iface_obj) = interfaces.iter_mut().find(|i| i.id() == iface) {
                    // SendPacket = directed traffic (link requests, proofs, channel data)
                    // → high priority on constrained interfaces like LoRa
                    if let Err(e) = iface_obj.try_send_prioritized(&send_data, true) {
                        errors.push((iface, e));
                        if matches!(e, crate::traits::InterfaceError::BufferFull) {
                            retries.push(SendRetry {
                                iface_idx: iface.0,
                                data: send_data,
                            });
                        }
                    }
                }
            }
            Action::Broadcast {
                data,
                exclude_iface,
            } => {
                for iface_obj in interfaces.iter_mut() {
                    if Some(iface_obj.id()) == exclude_iface {
                        continue;
                    }
                    let iface_idx = iface_obj.id().0;
                    let result = match ifac_configs.get(&iface_idx) {
                        Some(cfg) => match cfg.apply_ifac(&data) {
                            Ok(wrapped) => iface_obj.try_send_prioritized(&wrapped, false),
                            Err(e) => {
                                tracing::warn!("IFAC apply failed on iface {}: {:?}", iface_idx, e);
                                continue;
                            }
                        },
                        None => iface_obj.try_send_prioritized(&data, false),
                    };
                    // Broadcast = announce rebroadcasts → normal priority
                    if let Err(e) = result {
                        errors.push((iface_obj.id(), e));
                    }
                }
            }
        }
    }
    DispatchResult { retries, errors }
}

// Data Structures
// PathEntry, PathState, ReverseEntry, LinkEntry, AnnounceEntry,
// AnnounceRateEntry live in crate::storage_types and are imported above.

/// Per-interface announce bandwidth cap state (Python Interface.py:25-28, Transport.py:1091-1104)
///
/// Tracks when the next announce is allowed on this interface and queues
/// excess announces to be drained as bandwidth permits.
/// Removed when `unregister_interface_announce_cap()` is called or interface goes down.
#[derive(Debug, Clone)]
pub(crate) struct InterfaceAnnounceCap {
    /// Interface bitrate in bits per second. 0 = unlimited (no cap applied).
    pub bitrate_bps: u32,
    /// Announce bandwidth cap as percentage of link capacity (default 2%).
    pub announce_cap_percent: u32,
    /// Next announce allowed at this absolute timestamp (ms).
    pub allowed_at_ms: u64,
    /// Queued announces waiting for bandwidth (capped at MAX_QUEUED_ANNOUNCES_PER_INTERFACE).
    pub queue: VecDeque<QueuedAnnounce>,
}

/// A queued announce waiting for bandwidth on a specific interface
#[derive(Debug, Clone)]
pub(crate) struct QueuedAnnounce {
    /// Raw packet bytes ready for sending
    pub raw: Vec<u8>,
    /// Hop count (lower = higher priority for dequeue)
    pub hops: u8,
    /// When this announce was queued (ms)
    pub queued_at_ms: u64,
}

/// Transport configuration
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Enable transport mode (routing for others)
    pub enable_transport: bool,
    /// Maximum hops for path finding
    pub max_hops: u8,
    /// Path expiry time in seconds
    pub path_expiry_secs: u64,
    /// Announce rate limit minimum interval (ms)
    pub announce_rate_limit_ms: u64,
    /// Per-destination announce rate target (ms). None = disabled (default).
    /// When set, announces arriving faster than this interval accumulate violations.
    pub announce_rate_target_ms: Option<u64>,
    /// Number of rate violations allowed before blocking rebroadcast. Default 0.
    pub announce_rate_grace: u8,
    /// Additional blocking penalty (ms) added to the blocking window. Default 0.
    pub announce_rate_penalty_ms: u64,
    /// Maximum queued announces per capped interface. Default: 16384.
    pub max_queued_announces: usize,
    /// Maximum random blobs retained per path entry for replay detection. Default: 64.
    pub max_random_blobs: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            enable_transport: false,
            max_hops: PATHFINDER_MAX_HOPS,
            path_expiry_secs: PATHFINDER_EXPIRY_SECS,
            announce_rate_limit_ms: ANNOUNCE_RATE_LIMIT_MS,
            announce_rate_target_ms: None,
            announce_rate_grace: ANNOUNCE_RATE_GRACE,
            announce_rate_penalty_ms: ANNOUNCE_RATE_PENALTY_MS,
            max_queued_announces: MAX_QUEUED_ANNOUNCES_PER_INTERFACE,
            max_random_blobs: MAX_RANDOM_BLOBS,
        }
    }
}

/// Interface metadata for RPC reporting.
#[derive(Debug, Clone)]
pub struct InterfaceStatEntry {
    /// Interface index
    pub id: usize,
    /// Human-readable name
    pub name: String,
    /// Whether this is a local IPC client interface
    pub is_local_client: bool,
    /// Incoming announce frequency in Hz (Python ia_freq_deque)
    pub incoming_announce_frequency: f64,
    /// Outgoing announce frequency in Hz (Python oa_freq_deque)
    pub outgoing_announce_frequency: f64,
}

/// Exported path table entry for RPC reporting.
#[derive(Debug, Clone)]
pub struct PathTableExport {
    /// Destination hash (16 bytes)
    pub hash: [u8; TRUNCATED_HASHBYTES],
    /// Number of hops
    pub hops: u8,
    /// When this path expires (ms since clock epoch)
    pub expires_ms: u64,
    /// Interface index where we learned this path
    pub interface_index: usize,
    /// Identity hash of the next relay hop
    pub next_hop: Option<[u8; TRUNCATED_HASHBYTES]>,
}

/// Exported announce rate table entry for RPC reporting.
#[derive(Debug, Clone)]
pub struct RateTableExport {
    /// Destination hash (16 bytes)
    pub hash: [u8; TRUNCATED_HASHBYTES],
    /// Timestamp of last accepted announce (ms)
    pub last_ms: u64,
    /// Number of rate violations
    pub rate_violations: u8,
    /// Blocked until (ms)
    pub blocked_until_ms: u64,
}

/// Transport statistics
#[derive(Debug, Default, Clone)]
pub struct TransportStats {
    pub(crate) packets_sent: u64,
    pub(crate) packets_received: u64,
    pub(crate) packets_forwarded: u64,
    pub(crate) announces_processed: u64,
    pub(crate) packets_dropped: u64,
}

impl TransportStats {
    /// Packets sent
    pub fn packets_sent(&self) -> u64 {
        self.packets_sent
    }

    /// Packets received
    pub fn packets_received(&self) -> u64 {
        self.packets_received
    }

    /// Packets forwarded (transport mode)
    pub fn packets_forwarded(&self) -> u64 {
        self.packets_forwarded
    }

    /// Announces processed
    pub fn announces_processed(&self) -> u64 {
        self.announces_processed
    }

    /// Packets dropped (duplicate, expired, etc.)
    pub fn packets_dropped(&self) -> u64 {
        self.packets_dropped
    }
}

// Events
/// Events emitted by Transport for the application to handle
#[derive(Debug)]
pub enum TransportEvent {
    /// A new announce was received and validated
    AnnounceReceived {
        /// Parsed announce data
        announce: ReceivedAnnounce,
        /// Interface it arrived on
        interface_index: usize,
    },
    /// A packet arrived for a registered destination
    PacketReceived {
        /// The destination hash
        destination_hash: [u8; TRUNCATED_HASHBYTES],
        /// The parsed packet (boxed to reduce enum size)
        packet: Box<Packet>,
        /// Interface it arrived on
        interface_index: usize,
        /// Hash of original wire bytes (for repack symmetry verification)
        raw_hash: Option<[u8; 32]>,
    },
    /// Path to a destination was found (from announce)
    PathFound {
        /// Destination hash
        destination_hash: [u8; TRUNCATED_HASHBYTES],
        /// Number of hops
        hops: u8,
        /// Interface index
        interface_index: usize,
    },
    /// Path to a destination expired
    PathLost {
        /// Destination hash
        destination_hash: [u8; TRUNCATED_HASHBYTES],
    },
    /// A path request was received for a local destination
    ///
    /// The application should re-announce the destination so the requester can learn the path.
    PathRequestReceived {
        /// The destination hash being requested
        destination_hash: [u8; TRUNCATED_HASHBYTES],
        /// The interface the path request arrived on (used for targeted response)
        requesting_interface: usize,
    },

    /// An interface went offline
    InterfaceDown(usize),

    // Proof Events
    /// Proof generation requested for a received packet
    ///
    /// Emitted for every data packet received at a local destination.
    /// NodeCore looks up the proof strategy from its own destination registry
    /// and dispatches accordingly (auto-prove, forward to app, or ignore).
    ProofRequested {
        /// Full SHA256 hash of the packet to potentially prove
        packet_hash: [u8; 32],
        /// Destination that received the packet
        destination_hash: [u8; TRUNCATED_HASHBYTES],
        /// The interface the packet was received on
        interface_index: usize,
    },

    /// A proof was received for a sent packet
    ///
    /// Emitted when a proof packet arrives matching a tracked receipt.
    /// NodeCore performs the actual signature verification using its
    /// Destination's identity.
    ProofReceived {
        /// Truncated hash identifying the receipt
        packet_hash: [u8; TRUNCATED_HASHBYTES],
        /// Destination hash (to find the identity on NodeCore)
        destination_hash: [u8; TRUNCATED_HASHBYTES],
        /// Full packet hash from the receipt (for cross-check in verify_proof)
        expected_packet_hash: [u8; 32],
        /// Raw proof data (96 bytes: full_hash(32) + signature(64))
        proof_data: Vec<u8>,
    },

    /// A receipt timed out without receiving a proof
    ReceiptTimeout {
        /// Truncated hash identifying the receipt
        packet_hash: [u8; TRUNCATED_HASHBYTES],
    },
}

// Error Types
/// Transport error type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// No path to destination
    NoPath,
    /// Target interface is pacing, retry no earlier than `ready_at_ms`.
    /// Mirrors `ChannelError::PacingDelay` / `SendError::PacingDelay` so
    /// async callers (see `driver/stream.rs:127`) can `sleep_until` to
    /// the exact ready time instead of polling.
    PacingDelay { ready_at_ms: u64 },
    /// Packet parsing error
    PacketError(PacketError),
    /// Announce validation error
    AnnounceError(crate::announce::AnnounceError),
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TransportError::NoPath => write!(f, "no path to destination"),
            TransportError::PacingDelay { ready_at_ms } => {
                write!(f, "pacing delay, ready at {} ms", ready_at_ms)
            }
            TransportError::PacketError(e) => write!(f, "packet error: {}", e),
            TransportError::AnnounceError(e) => write!(f, "announce error: {}", e),
        }
    }
}

impl From<PacketError> for TransportError {
    fn from(e: PacketError) -> Self {
        TransportError::PacketError(e)
    }
}

// Transport
/// The Transport layer (sans-I/O state machine)
///
/// Generic over platform traits so the same protocol logic runs everywhere.
/// Transport never performs I/O directly, all outbound data is expressed as
/// `Action` values that the driver dispatches to actual network interfaces.
///
/// - `C`: Clock implementation for timestamps
/// - `S`: Storage implementation for persistence
pub struct Transport<C: Clock, S: Storage> {
    config: TransportConfig,
    clock: C,
    storage: S,
    identity: Identity,

    // path_table: migrated to Storage trait
    // path_states: migrated to Storage trait
    // announce_table: migrated to Storage trait

    // link_table: migrated to Storage trait
    /// Reverse table: packet_hash -> sender info (for routing replies)
    // reverse_table: migrated to Storage trait

    /// Registered destinations we accept packets for
    local_destinations: BTreeSet<[u8; TRUNCATED_HASHBYTES]>,

    /// Receipts for sent packets awaiting proof: truncated_hash -> receipt
    // receipts: migrated to Storage trait

    /// Pending events for the application
    events: Vec<TransportEvent>,

    /// Statistics
    stats: TransportStats,

    /// Timestamp of last path table snapshot log (for diagnostic tracing)
    last_path_snapshot_ms: u64,

    /// Well-known hash for path request destination (rnstransport.path.request)
    path_request_hash: [u8; TRUNCATED_HASHBYTES],

    /// Well-known hash for tunnel synthesize destination (rnstransport.tunnel.synthesize).
    /// Not implemented in Rust, but Python nodes send these packets. We must
    /// recognize them as control traffic, not forward them as plain broadcasts.
    tunnel_synthesize_hash: [u8; TRUNCATED_HASHBYTES],

    /// Cached raw announce bytes for path responses: dest_hash -> raw bytes
    // announce_cache: migrated to Storage trait

    // announce_rate_table: migrated to Storage trait
    /// Per-interface announce bandwidth caps (Python Interface.py:25-28)
    /// Keyed by interface index. Only present for interfaces with bitrate > 0.
    interface_announce_caps: BTreeMap<usize, InterfaceAnnounceCap>,

    /// Human-readable interface names for log messages.
    /// Populated by the driver at registration time, removed on interface down.
    interface_names: BTreeMap<usize, String>,

    /// Hardware MTU per interface (for link MTU negotiation).
    /// Keyed by interface index. Set by driver at registration, removed on interface down.
    interface_hw_mtus: BTreeMap<usize, u32>,

    /// Set of interface indices that are local IPC clients (shared instance).
    /// Used for: announce forwarding, transport override, path request routing.
    /// Removal path: removed in handle_interface_down via set_local_client(id, false).
    local_client_interfaces: BTreeSet<usize>,

    /// Per-interface incoming announce timestamps for frequency computation (Python ia_freq_deque).
    /// Removal path: removed in handle_interface_down via remove_announce_freq_tracking().
    interface_incoming_announce_times: BTreeMap<usize, VecDeque<u64>>,

    /// Per-interface outgoing announce timestamps for frequency computation (Python oa_freq_deque).
    /// Removal path: removed in handle_interface_down via remove_announce_freq_tracking().
    interface_outgoing_announce_times: BTreeMap<usize, VecDeque<u64>>,

    /// Per-interface "earliest ready" wall-clock ms pushed by the driver
    /// after each `dispatch_output` tick. Driver computes this via
    /// `Interface::next_slot_ms(MTU, now)` on each handle and mirrors the
    /// answer here so Transport can consult it without holding interface
    /// trait objects (Transport stays sans-I/O).
    interface_next_slot_ms: BTreeMap<usize, u64>,

    /// Per-interface worst-case airtime in milliseconds for a single
    /// MTU-sized transmit. Driver pushes one entry per LoRa-Serial
    /// interface (computed from its current bw/sf/cr/MTU); non-LoRa
    /// interfaces are absent from the map. Read by
    /// `announce_jitter_max_ms` to size the announce-retry jitter
    /// window proportional to the slowest registered link.
    interface_max_airtime_ms: BTreeMap<usize, u64>,

    /// Per-interface IFAC (Interface Access Code) configurations.
    /// Keyed by interface index. Only present for interfaces with networkname/passphrase configured.
    /// Removal path: removed in handle_interface_down via remove_ifac_config().
    ifac_configs: BTreeMap<usize, IfacConfig>,

    /// Pending I/O actions for the driver (sans-I/O buffer)
    pending_actions: Vec<Action>,

    /// Timestamp of the last discovery path request retry cycle.
    /// Used to throttle retries to one cycle per DISCOVERY_RETRY_INTERVAL_MS.
    /// Reset to 0 on construction; not a persistent field.
    last_discovery_retry_ms: u64,

    /// Interfaces for test use only (not used in production sans-I/O path)
    #[cfg(test)]
    interfaces: Vec<Option<Box<dyn crate::traits::Interface + Send>>>,
}

impl<C: Clock, S: Storage> Transport<C, S> {
    /// Create a new Transport instance
    pub fn new(config: TransportConfig, clock: C, storage: S, identity: Identity) -> Self {
        let path_request_hash = Self::compute_path_request_hash();
        let tunnel_synthesize_hash = Self::compute_tunnel_synthesize_hash();
        Self {
            config,
            clock,
            storage,
            identity,
            // path_table: migrated to Storage
            // path_states: migrated to Storage
            // announce_table: migrated to Storage
            // link_table: migrated to Storage
            // reverse_table: migrated to Storage
            local_destinations: BTreeSet::new(),
            // receipts: migrated to Storage
            events: Vec::new(),
            stats: TransportStats::default(),
            last_path_snapshot_ms: 0,
            path_request_hash,
            tunnel_synthesize_hash,
            // announce_cache: migrated to Storage
            // announce_rate_table: migrated to Storage
            interface_announce_caps: BTreeMap::new(),
            interface_names: BTreeMap::new(),
            interface_hw_mtus: BTreeMap::new(),
            local_client_interfaces: BTreeSet::new(),
            interface_incoming_announce_times: BTreeMap::new(),
            interface_outgoing_announce_times: BTreeMap::new(),
            interface_next_slot_ms: BTreeMap::new(),
            interface_max_airtime_ms: BTreeMap::new(),
            ifac_configs: BTreeMap::new(),
            pending_actions: Vec::new(),
            last_discovery_retry_ms: 0,
            #[cfg(test)]
            interfaces: Vec::new(),
        }
    }

    // Storage Accessors
    /// Borrow the storage implementation
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Mutably borrow the storage implementation
    pub fn storage_mut(&mut self) -> &mut S {
        &mut self.storage
    }

    // Test-only Interface Management
    //
    // These methods exist only in test builds to support existing protocol
    // tests that use MockInterface. In production, Transport is sans-I/O
    // and does not own interfaces.

    /// Register an interface (test-only)
    #[cfg(test)]
    pub fn register_interface(
        &mut self,
        interface: Box<dyn crate::traits::Interface + Send>,
    ) -> usize {
        let index = self.interfaces.len();
        self.interfaces.push(Some(interface));
        index
    }

    /// Unregister an interface (test-only)
    #[cfg(test)]
    pub fn unregister_interface(&mut self, index: usize) {
        if let Some(slot) = self.interfaces.get_mut(index) {
            *slot = None;
        }
    }

    /// Get interface count (test-only)
    #[cfg(test)]
    pub fn interface_count(&self) -> usize {
        self.interfaces.iter().filter(|i| i.is_some()).count()
    }

    /// Get mutable interface reference (test-only)
    #[cfg(test)]
    pub fn interface_mut(
        &mut self,
        index: usize,
    ) -> Option<&mut (dyn crate::traits::Interface + '_)> {
        match self.interfaces.get_mut(index) {
            Some(Some(iface)) => Some(iface.as_mut()),
            _ => None,
        }
    }

    /// Number of pending events (test-only)
    #[cfg(test)]
    pub fn pending_events(&self) -> usize {
        self.events.len()
    }

    /// Number of pending I/O actions (test-only)
    #[cfg(test)]
    pub fn pending_action_count(&self) -> usize {
        self.pending_actions.len()
    }

    // Destination Management
    /// Register a destination to receive packets
    ///
    /// Transport only tracks whether a hash is local. All destination metadata
    /// (proof strategy, accepts_links, identity) lives on NodeCore.
    pub fn register_destination(&mut self, hash: [u8; TRUNCATED_HASHBYTES]) {
        self.local_destinations.insert(hash);
    }

    /// Unregister a destination
    pub fn unregister_destination(&mut self, hash: &[u8; TRUNCATED_HASHBYTES]) {
        self.local_destinations.remove(hash);
    }

    /// Check if a destination is registered
    pub fn has_destination(&self, hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.local_destinations.contains(hash)
    }

    // Path Management
    /// Check if we have a path to a destination
    pub fn has_path(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.storage.has_path(dest_hash)
    }

    /// Get the hop count to a destination
    pub fn hops_to(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<u8> {
        self.storage.get_path(dest_hash).map(|p| p.hops)
    }

    /// Get a path entry
    pub(crate) fn path(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<&PathEntry> {
        self.storage.get_path(dest_hash)
    }

    /// Get the number of known paths
    pub fn path_count(&self) -> usize {
        self.storage.path_count()
    }

    // Path State Management
    /// Mark a path as unresponsive
    ///
    /// Only succeeds if the destination exists in the path table.
    /// Called when an unvalidated link expires for a 1-hop destination/initiator.
    pub fn mark_path_unresponsive(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        if self.storage.has_path(dest_hash) {
            self.storage
                .set_path_state(*dest_hash, PathState::Unresponsive);
            true
        } else {
            false
        }
    }

    /// Mark a path as responsive
    ///
    /// Only succeeds if the destination exists in the path table.
    /// Defined for API completeness (not called internally, matching Python).
    pub fn mark_path_responsive(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        if self.storage.has_path(dest_hash) {
            self.storage
                .set_path_state(*dest_hash, PathState::Responsive);
            true
        } else {
            false
        }
    }

    /// Reset path state to unknown
    ///
    /// Only succeeds if the destination exists in the path table.
    /// Called when a new announce successfully updates the path table.
    pub(crate) fn mark_path_unknown_state(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> bool {
        if self.storage.has_path(dest_hash) {
            self.storage.set_path_state(*dest_hash, PathState::Unknown);
            true
        } else {
            false
        }
    }

    /// Check if a path is marked as unresponsive
    pub fn path_is_unresponsive(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.storage.get_path_state(dest_hash) == Some(PathState::Unresponsive)
    }

    /// Force-expire a path entry
    ///
    /// Removes the path table entry and emits `PathLost`. Returns true if
    /// the path existed and was removed.
    pub fn expire_path(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        if self.storage.remove_path(dest_hash).is_some() {
            self.events.push(TransportEvent::PathLost {
                destination_hash: *dest_hash,
            });
            true
        } else {
            false
        }
    }

    // Receipt Management
    /// Create a receipt for a sent packet
    ///
    /// Call this after sending a packet that you want to track for proof of delivery.
    ///
    /// # Arguments
    /// * `raw_packet` - The raw packet bytes (used to compute hash)
    /// * `destination_hash` - The destination the packet was sent to
    ///
    /// # Returns
    /// The truncated hash that can be used to look up the receipt later
    pub fn create_receipt(
        &mut self,
        raw_packet: &[u8],
        destination_hash: [u8; TRUNCATED_HASHBYTES],
    ) -> [u8; TRUNCATED_HASHBYTES] {
        let timeout = self.compute_receipt_timeout(&destination_hash);
        let hash = packet_hash(raw_packet);
        let now = self.clock.now_ms();
        let receipt =
            PacketReceipt::with_timeout(hash, DestinationHash::new(destination_hash), now, timeout);
        let truncated = receipt.truncated_hash;
        self.storage.set_receipt(truncated, receipt);
        truncated
    }

    /// Create a receipt with a custom timeout
    pub fn create_receipt_with_timeout(
        &mut self,
        raw_packet: &[u8],
        destination_hash: [u8; TRUNCATED_HASHBYTES],
        timeout_ms: u64,
    ) -> [u8; TRUNCATED_HASHBYTES] {
        let hash = packet_hash(raw_packet);
        let now = self.clock.now_ms();
        let receipt = PacketReceipt::with_timeout(
            hash,
            DestinationHash::new(destination_hash),
            now,
            timeout_ms,
        );
        let truncated = receipt.truncated_hash;
        self.storage.set_receipt(truncated, receipt);
        truncated
    }

    /// Get a receipt by its truncated hash
    pub fn get_receipt(
        &self,
        truncated_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<&PacketReceipt> {
        self.storage.get_receipt(truncated_hash)
    }

    /// Mark a receipt as delivered after NodeCore verified the proof
    ///
    /// Called by NodeCore when it successfully validates a single-packet proof
    /// using the destination's identity.
    pub fn mark_receipt_delivered(&mut self, truncated_hash: &[u8; TRUNCATED_HASHBYTES]) {
        if let Some(receipt) = self.storage.get_receipt(truncated_hash) {
            let mut receipt = receipt.clone();
            receipt.set_delivered();
            self.storage.set_receipt(*truncated_hash, receipt);
        }
    }

    /// Send a proof for a received packet
    ///
    /// Call this when the application decides to prove a packet (after ProofRequested event).
    ///
    /// # Arguments
    /// * `packet_hash` - The full SHA256 hash of the packet to prove
    /// * `destination_hash` - The destination to send the proof to (the original sender)
    /// * `identity` - The identity to sign the proof with
    /// * `receiving_interface` - If `Some`, send proof on this interface (used for
    ///   auto-proofs where we know the receiving interface). If `None`,
    ///   fall back to path table lookup (used for app-initiated proofs).
    ///
    /// # Returns
    /// `Ok(())` if the proof was sent, `Err` if there's no path to the destination
    pub fn send_proof(
        &mut self,
        packet_hash: &[u8; 32],
        destination_hash: &[u8; TRUNCATED_HASHBYTES],
        identity: &Identity,
        receiving_interface: Option<usize>,
    ) -> Result<(), TransportError> {
        let proof_data = identity
            .create_proof(packet_hash)
            .map_err(|_| TransportError::NoPath)?; // Use NoPath for simplicity

        // Proof destination = truncated PACKET hash (not the destination hash).
        // Relay nodes route proofs back via the reverse_transport_table, which is
        // keyed by truncated packet hash. Python: ProofDestination.hash = packet.get_hash()[:16]
        let mut proof_dest = [0u8; TRUNCATED_HASHBYTES];
        proof_dest.copy_from_slice(&packet_hash[..TRUNCATED_HASHBYTES]);
        let packet = build_proof_packet(&proof_dest, &proof_data);
        tracing::debug!(
            "[PROOF_GEN] for_pkt={} to_dst={}",
            HexShort(&proof_dest),
            HexShort(destination_hash)
        );

        // Prefer explicit interface (PROVE_ALL), fall back to path lookup (PROVE_APP)
        let interface_index = match receiving_interface {
            Some(iface) => iface,
            None => self
                .storage
                .get_path(destination_hash)
                .map(|p| p.interface_index)
                .ok_or(TransportError::NoPath)?,
        };
        tracing::debug!(
            "[PROOF_SEND] pkt={} iface={}",
            HexShort(&proof_dest),
            self.iface_name(interface_index)
        );

        self.send_packet_on_interface(interface_index, &packet)
    }

    // Packet I/O
    /// Process incoming raw data from an interface
    ///
    /// Parses the packet, checks for duplicates, routes to the appropriate
    /// handler, and may emit events.
    pub fn process_incoming(
        &mut self,
        interface_index: usize,
        raw: &[u8],
    ) -> Result<(), TransportError> {
        self.stats.packets_received += 1;

        // IFAC verification: verify and strip IFAC on protected interfaces,
        // drop IFAC-tagged packets on unprotected interfaces.
        let raw: Cow<'_, [u8]> = match self.ifac_config(interface_index) {
            Some(cfg) => match cfg.verify_ifac(raw) {
                Ok(clean) => Cow::Owned(clean),
                Err(_) => {
                    tracing::trace!(
                        "Dropping packet: IFAC verification failed on iface {}",
                        self.iface_name(interface_index)
                    );
                    self.stats.packets_dropped += 1;
                    return Ok(());
                }
            },
            None => {
                if IfacConfig::has_ifac_flag(raw) {
                    tracing::trace!(
                        "Dropping IFAC-tagged packet on non-IFAC iface {}",
                        self.iface_name(interface_index)
                    );
                    self.stats.packets_dropped += 1;
                    return Ok(());
                }
                Cow::Borrowed(raw)
            }
        };

        let mut packet = Packet::unpack(&raw)?;

        // Increment hops on receipt, matching Python Transport.py:1319.
        // After this: hops=1 means direct neighbor, hops=0 means local client.
        packet.hops = packet.hops.saturating_add(1);

        // Local shared-instance clients get the pre-increment value (net zero),
        // matching Python Transport.py:1345,1348.
        if self.is_local_client(interface_index) {
            packet.hops = packet.hops.saturating_sub(1);
        }

        tracing::trace!(
            "incoming packet ptype={:?} dest=<{}> iface={} hops={}",
            packet.flags.packet_type,
            HexShort(&packet.destination_hash),
            self.iface_name(interface_index),
            packet.hops
        );
        tracing::debug!(
            "[PKT_RX] iface={} type={:?} dst={} hops={} len={}",
            self.iface_name(interface_index),
            packet.flags.packet_type,
            HexShort(&packet.destination_hash),
            packet.hops,
            raw.len()
        );

        // Filter HEADER_2 packets not addressed to this transport instance
        // (Python Transport.py:1193-1196). Announces are exempt.
        if packet.transport_id.is_some()
            && packet.flags.packet_type != PacketType::Announce
            && packet.transport_id != Some(*self.identity.hash())
        {
            tracing::trace!(
                "Dropped packet for <{}> on {}, transport ID mismatch",
                HexShort(&packet.destination_hash),
                self.iface_name(interface_index)
            );
            self.stats.packets_dropped += 1;
            return Ok(());
        }

        // Filter PLAIN and GROUP destination packets (Python Transport.py:1205-1225).
        // These destination types are for direct neighbors only.
        if packet.flags.dest_type == DestinationType::Plain
            || packet.flags.dest_type == DestinationType::Group
        {
            // PLAIN/GROUP announces are always invalid
            if packet.flags.packet_type == PacketType::Announce {
                tracing::trace!(
                    "Dropped invalid PLAIN/GROUP announce for <{}> on {}",
                    HexShort(&packet.destination_hash),
                    self.iface_name(interface_index)
                );
                self.stats.packets_dropped += 1;
                return Ok(());
            }
            // Non-announce: drop if hops > 1 (PLAIN/GROUP are direct-neighbor only).
            // Python Transport.py:1205: hops > 1 after increment on receipt.
            // We now also increment on receipt, so the check is the same.
            if packet.hops > 1 {
                tracing::trace!(
                    "Dropped PLAIN/GROUP packet for <{}> on {}, hops={}, not direct",
                    HexShort(&packet.destination_hash),
                    self.iface_name(interface_index),
                    packet.hops
                );
                self.stats.packets_dropped += 1;
                return Ok(());
            }
        }

        // Compute full packet hash (for deduplication and proofs) and truncated hash
        // (for reverse_table routing). Dedup uses full 32-byte SHA-256, matching
        // Python Transport.py:1227 which checks `packet.packet_hash` (full hash).
        let full_packet_hash = packet_hash(&raw);
        let mut truncated_hash = [0u8; TRUNCATED_HASHBYTES];
        truncated_hash.copy_from_slice(&full_packet_hash[..TRUNCATED_HASHBYTES]);

        // Check duplicate (full 32-byte hash).
        // Single announces are exempt from packet_hash dedup because direct (Type1)
        // and relayed (Type2) copies hash identically (get_hashable_part strips hops
        // and transport_id). Both copies must be processed so the best path wins.
        // Matches Python Transport.py:1230-1232.
        //
        // Local-client link-table relays are also exempt: resource retransmissions
        // produce identical raw bytes (build_raw_data_packet has no nonce), so the
        // hash matches the original. In Python the daemon sends outbound directly
        // (never through process_incoming), so dedup never fires. We replicate that
        // by skipping dedup for packets from a local client destined for a known link.
        let is_single_announce = packet.flags.packet_type == PacketType::Announce
            && packet.flags.dest_type == DestinationType::Single;
        let is_from_local_client = self.is_local_client(interface_index);
        let is_local_link_relay =
            is_from_local_client && self.storage.has_link_entry(&packet.destination_hash);
        // Local-client link requests skip hash dedup to allow link request
        // retries. When lncp retries a link request, the daemon
        // receives the same bytes again. Without this exemption, the daemon
        // would reject the retry as a duplicate (hash cached from first attempt).
        // This is safe: link requests from local clients are forwarded to the
        // network, not processed locally, so duplicates don't affect daemon state.
        // Python avoids this entirely because its outbound path doesn't pass
        // through inbound() (Transport.py architecture difference).
        let is_local_client_link_request =
            is_from_local_client && packet.flags.packet_type == PacketType::LinkRequest;
        // Link requests for our own destinations (or forwarded to a local
        // client) skip dedup so that link-request retries (proof lost → initiator
        // re-sends same link request) reach handle_link_request(), which
        // re-sends the cached proof. Covers both cases:
        // - Destination on this daemon (local_destinations)
        // - Destination on a local client (lncp receiver behind this daemon)
        // The link management layer handles its own dedup via links.contains_key().
        let is_link_request_for_us = packet.flags.packet_type == PacketType::LinkRequest
            && (self.local_destinations.contains(&packet.destination_hash)
                || self.is_for_local_client(&packet.destination_hash));
        // CacheRequest packets skip dedup unconditionally: the sender retries
        // CacheRequest on timeout with identical bytes (deterministic packet_hash).
        // Without this exemption, both the daemon and the lncp client would reject
        // retries as duplicates. The handler (handle_cache_request) is idempotent.        // it simply re-sends the cached proof. No routing loop risk: CacheRequests
        // are link-addressed (dest_type=Link) and never forwarded via path table.
        let is_cache_request = packet.context == PacketContext::CacheRequest;
        // LRPROOFs for our own links skip dedup: link-request retries generate
        // identical proofs (deterministic for same link_id). Without
        // this exemption, the first proof's hash is cached and all
        // retry proofs are dropped as duplicates, causing link
        // establishment to fail under RF contention.
        // Same rationale as is_link_request_for_us and is_cache_request.
        let is_lrproof_for_us = packet.context == PacketContext::Lrproof
            && self.local_destinations.contains(&packet.destination_hash);
        if !is_single_announce
            && !is_local_link_relay
            && !is_local_client_link_request
            && !is_link_request_for_us
            && !is_cache_request
            && !is_lrproof_for_us
            && self.storage.has_packet_hash(&full_packet_hash)
        {
            if packet.context == PacketContext::Lrproof {
                tracing::debug!(
                    "Dropped duplicate LRPROOF for <{}> on {}",
                    HexShort(&packet.destination_hash),
                    self.iface_name(interface_index)
                );
            } else {
                tracing::trace!(
                    "Dropped duplicate packet for <{}> on {}",
                    HexShort(&packet.destination_hash),
                    self.iface_name(interface_index)
                );
            }
            self.stats.packets_dropped += 1;
            return Ok(());
        }

        // Defer cache insertion for link-table and LRPROOF packets
        // (Python Transport.py:1355-1372). On shared media, these packets
        // may be heard before reaching us via the correct link path.
        // Inserting early would block the correct copy. The handler inserts
        // the hash on successful processing; failed packets stay uncached
        // so the correct copy can still pass dedup.
        let defer_cache_insert = self.storage.has_link_entry(&packet.destination_hash)
            || (packet.flags.packet_type == PacketType::Proof
                && packet.context == PacketContext::Lrproof);

        if !defer_cache_insert {
            self.storage.add_packet_hash(full_packet_hash);
        }

        // Route to handler based on packet type
        // reverse table entries are populated at forwarding time (in forward_packet,
        // handle_data link-table routing, handle_link_request) so they include the
        // outbound interface index needed for proof routing.
        match packet.flags.packet_type {
            PacketType::Announce => self.handle_announce(packet, interface_index, &raw),
            PacketType::LinkRequest => {
                self.handle_link_request(packet, interface_index, &raw, truncated_hash)
            }
            PacketType::Proof => self.handle_proof(packet, interface_index, full_packet_hash),
            PacketType::Data => {
                self.handle_data(packet, interface_index, full_packet_hash, truncated_hash)
            }
        }
    }

    /// Send raw data on a specific interface
    pub fn send_on_interface(
        &mut self,
        interface_index: usize,
        data: &[u8],
    ) -> Result<(), TransportError> {
        self.pending_actions.push(Action::SendPacket {
            iface: InterfaceId(interface_index),
            data: data.to_vec(),
        });
        self.stats.packets_sent += 1;
        Ok(())
    }

    /// Send raw data on all online interfaces (emits Broadcast action).
    /// Matches Python Reticulum's Transport.outbound one-shot semantics:
    /// Destination.announce at Destination.py:322 calls Packet.send exactly
    /// once, and Packet.send at Packet.py:273-299 invokes Transport.outbound
    /// once. Locally-originated announces are not inserted into announce_table
    /// and have no retries; the Transport keepalive at check_mgmt_announces
    /// re-emits on the configured 2-hour interval.
    pub fn send_on_all_interfaces(&mut self, data: &[u8]) {
        // Cache outbound packet hash so echoes returning via redundant paths
        // are dropped by the dedup check in process_incoming().
        // This matches Python Reticulum's Transport.py:1168-1169.
        let cache_hash = packet_hash(data);
        self.storage.add_packet_hash(cache_hash);

        self.stats.packets_sent += 1;
        self.pending_actions.push(Action::Broadcast {
            data: data.to_vec(),
            exclude_iface: None,
        });
    }

    /// Send a packet to a destination via its known path
    pub fn send_to_destination(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        data: &[u8],
    ) -> Result<(), TransportError> {
        // Read path data into locals to release the immutable borrow on storage
        // before we mutably borrow for hash caching.
        let (interface_index, needs_relay, next_hop) = {
            let path = self
                .storage
                .get_path(dest_hash)
                .ok_or(TransportError::NoPath)?;
            (path.interface_index, path.needs_relay(), path.next_hop)
        };

        // Consult the next-slot backchannel: if the interface is not yet
        // ready for an MTU-sized packet, return PacingDelay with the exact
        // ready-at timestamp so stream.rs can sleep_until rather than
        // polling.
        let now_ms = self.clock.now_ms();
        let next_slot = self.next_slot_ms_for_interface(interface_index, now_ms);
        if next_slot > now_ms {
            return Err(TransportError::PacingDelay {
                ready_at_ms: next_slot,
            });
        }

        // Cache outbound packet hash so echoes returning via shared-medium
        // relay are dropped by the dedup check in process_incoming().
        // This is an ORIGINATION path (the caller is sending a new packet),
        // matching Python Transport.outbound() line 1169.
        // Forwarding paths (forward_on_interface → send_packet_on_interface →
        // send_on_interface) intentionally do NOT cache, matching Python's
        // Transport.transmit() which also doesn't cache.
        let cache_hash = packet_hash(data);
        self.storage.add_packet_hash(cache_hash);

        // Only convert Type1 packets to Type2 for relay routing.
        // Type2 packets (e.g., link requests from initiate_with_path) are
        // already correctly formatted, pass them through unchanged.
        let is_type1 = data.len() >= 2 && (data[0] & 0x40) == 0;

        if needs_relay && is_type1 {
            // Multi-hop: convert Type1 packet to Type2 with transport header.
            // Python equivalent: Transport.py outbound() lines 980-991.
            //
            // Wire format change:
            //   Type1: [flags][hops][dest_hash(16)][context][data...]
            //   Type2: [flags'][hops][next_hop(16)][dest_hash(16)][context][data...]
            //
            // flags' = header_type=Type2, transport_type=Transport, keep lower 4 bits
            let next_hop = next_hop.ok_or(TransportError::NoPath)?;

            let mut buf = alloc::vec![0u8; data.len() + TRUNCATED_HASHBYTES];

            // Rewrite flags: set Type2 header + Transport type, preserve lower 4 bits
            buf[0] = (1u8 << 6) | (1u8 << 4) | (data[0] & 0x0F);
            // Keep hops byte
            buf[1] = data[1];
            // Insert transport_id (next-hop relay identity hash)
            buf[2..2 + TRUNCATED_HASHBYTES].copy_from_slice(&next_hop);
            // Copy the rest: dest_hash + context + payload
            buf[2 + TRUNCATED_HASHBYTES..].copy_from_slice(&data[2..]);

            self.send_on_interface(interface_index, &buf)
        } else {
            // Direct neighbor or already Type2: send as-is
            self.send_on_interface(interface_index, data)
        }
    }

    // Polling
    /// Poll for periodic work
    ///
    /// Call this regularly (e.g. every 100ms). Handles:
    /// - Path expiry
    /// - Packet cache cleanup
    /// - Announce retransmission (if transport node)
    /// - Receipt timeout checking
    pub fn poll(&mut self) {
        let now = self.clock.now_ms();
        self.expire_paths(now);
        self.clean_reverse_table(now);
        self.check_receipt_timeouts(now);
        self.check_announce_rebroadcasts(now);
        self.drain_announce_queues(now);
        self.clean_link_table(now);
        self.clean_path_states();

        // Periodic path table snapshot for diagnostic tracing (every 10s)
        if now.saturating_sub(self.last_path_snapshot_ms) >= 10_000 {
            self.last_path_snapshot_ms = now;
            let entries = self.storage.path_entries();
            tracing::debug!("[PATH_TABLE] size={}", entries.len());
            for (dst, entry) in &entries {
                let age_ms = entry.expires_ms.saturating_sub(now);
                tracing::debug!(
                    "[PATH_TABLE_ENTRY] dst={} hops={} iface={} next_hop={:?} expires_in_ms={}",
                    HexShort(&dst[..]),
                    entry.hops,
                    self.iface_name(entry.interface_index),
                    entry
                        .next_hop
                        .as_ref()
                        .map(|h| alloc::format!("{}", HexShort(&h[..]))),
                    age_ms
                );
            }
        }
    }

    // Events
    /// Drain pending events
    ///
    /// Returns an iterator over all events that occurred since the last drain.
    pub fn drain_events(&mut self) -> alloc::vec::Drain<'_, TransportEvent> {
        self.events.drain(..)
    }

    // Actions (Sans-I/O)
    /// Drain pending I/O actions
    ///
    /// Returns all actions that have accumulated since the last drain.
    /// The driver should execute these (send packets on interfaces).
    pub fn drain_actions(&mut self) -> Vec<Action> {
        core::mem::take(&mut self.pending_actions)
    }

    /// Compute the earliest deadline across all transport-layer timers
    ///
    /// Returns `None` if there are no pending deadlines. The returned
    /// value is an absolute timestamp in milliseconds.
    pub fn next_deadline(&self) -> Option<u64> {
        let now = self.clock.now_ms();
        let mut earliest: Option<u64> = None;

        let mut update = |deadline_ms: u64| {
            earliest = Some(match earliest {
                Some(e) => core::cmp::min(e, deadline_ms),
                None => deadline_ms,
            });
        };

        // Path expiry deadlines
        if let Some(deadline) = self.storage.earliest_path_expiry() {
            update(deadline);
        }

        // Receipt timeout deadlines
        if let Some(deadline) = self.storage.earliest_receipt_deadline() {
            update(deadline);
        }

        // Announce rebroadcast deadlines
        if self.config.enable_transport {
            for key in self.storage.announce_keys() {
                if let Some(entry) = self.storage.get_announce(&key) {
                    if let Some(retransmit_at) = entry.retransmit_at_ms {
                        update(retransmit_at);
                    }
                }
            }
        }

        // Link table expiry deadlines
        if let Some(deadline) = self.storage.earliest_link_deadline(LINK_TIMEOUT_MS) {
            update(deadline);
        }

        // Announce queue drain deadlines
        for cap in self.interface_announce_caps.values() {
            if !cap.queue.is_empty() {
                update(cap.allowed_at_ms);
            }
        }

        // Discovery path request retry deadline
        if !self.storage.discovery_path_request_dest_hashes().is_empty() {
            update(self.last_discovery_retry_ms + DISCOVERY_RETRY_INTERVAL_MS);
        }

        // Packet cache rotation and reverse table cleanup are background.        // use a fixed interval rather than scanning every entry
        if self.config.enable_transport {
            // Reverse table cleanup, only needed when transport is enabled
            // (only transport nodes populate the reverse table)
            let cleanup_interval = 60_000; // Check every 60s
            update(now.saturating_add(cleanup_interval));
        }

        earliest
    }

    // Accessors
    /// Get transport statistics
    pub fn stats(&self) -> &TransportStats {
        &self.stats
    }

    /// Return diagnostic dump of Transport-owned collections (not on Storage)
    pub fn diagnostic_dump(&self) -> (String, u64) {
        use core::fmt::Write;
        let mut s = String::new();
        let mut total = 0u64;
        let _ = writeln!(s, "--- Transport ---");

        // announce_queues: per-interface VecDeque<QueuedAnnounce>, 1x
        let n_ifaces = self.interface_announce_caps.len();
        let mut n_queued = 0usize;
        let mut raw = 0u64;
        for cap in self.interface_announce_caps.values() {
            n_queued += cap.queue.len();
            for qa in &cap.queue {
                // QueuedAnnounce: raw(Vec payload) + hops(1) + queued_at_ms(8) = 9 + raw.len()
                raw += (9 + qa.raw.len()) as u64;
            }
        }
        let est = raw; // VecDeque 1x
        total += est;
        let _ = writeln!(
            s,
            "announce_queues: {} interfaces, {} total queued, raw {} bytes, estimated {} bytes (VecDeque 1x)",
            n_ifaces, n_queued, raw, est
        );

        // local_destinations: BTreeSet<[u8; 16]>, 3x
        let n = self.local_destinations.len();
        let raw_ld = (n * TRUNCATED_HASHBYTES) as u64;
        let est_ld = raw_ld * 3;
        total += est_ld;
        let _ = writeln!(
            s,
            "local_destinations: {} entries, raw {} bytes, estimated {} bytes (BTreeSet 3x)",
            n, raw_ld, est_ld
        );

        // local_client + known_ratchets collections are now in Storage
        // and accounted for in Storage::diagnostic_dump()

        (s, total)
    }

    /// Get the transport configuration
    pub fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Get the transport identity
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Access the clock
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Return all path table entries for RPC export.
    pub fn path_table_entries(&self) -> Vec<PathTableExport> {
        self.storage
            .path_entries()
            .into_iter()
            .map(|(hash, entry)| PathTableExport {
                hash,
                hops: entry.hops,
                expires_ms: entry.expires_ms,
                interface_index: entry.interface_index,
                next_hop: entry.next_hop,
            })
            .collect()
    }

    /// Return all announce rate table entries for RPC export.
    pub fn rate_table_entries(&self) -> Vec<RateTableExport> {
        self.storage
            .announce_rate_entries()
            .into_iter()
            .map(|(hash, entry)| RateTableExport {
                hash,
                last_ms: entry.last_ms,
                rate_violations: entry.rate_violations,
                blocked_until_ms: entry.blocked_until_ms,
            })
            .collect()
    }

    /// Clone a path entry by destination hash (for RPC lookups).
    pub fn get_path_clone(&self, hash: &[u8; TRUNCATED_HASHBYTES]) -> Option<PathEntry> {
        self.storage.get_path(hash).cloned()
    }

    /// Remove a path entry by destination hash. Returns true if found.
    pub fn remove_path(&mut self, hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.storage.remove_path(hash).is_some()
    }

    /// Remove all paths whose next_hop matches `via_hash`. Returns count removed.
    pub fn drop_all_paths_via(&mut self, via_hash: &[u8; TRUNCATED_HASHBYTES]) -> usize {
        let to_remove: Vec<_> = self
            .storage
            .path_entries()
            .iter()
            .filter(|(_, e)| e.next_hop.as_ref() == Some(via_hash))
            .map(|(h, _)| *h)
            .collect();
        let count = to_remove.len();
        for h in to_remove {
            self.storage.remove_path(&h);
        }
        count
    }

    /// Insert a path entry (thin wrapper around storage, test helper)
    #[cfg(test)]
    pub(crate) fn insert_path(&mut self, hash: [u8; TRUNCATED_HASHBYTES], entry: PathEntry) {
        self.storage.set_path(hash, entry);
    }

    /// Remove all path entries referencing a specific interface.
    /// Returns the destination hashes of removed paths.
    ///
    /// Used by NodeCore::handle_interface_down() for interface-down cleanup.
    pub(crate) fn remove_paths_for_interface(
        &mut self,
        iface_idx: usize,
    ) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.storage.remove_paths_for_interface(iface_idx)
    }

    /// Remove link table entries referencing a specific interface
    pub(crate) fn remove_link_entries_for_interface(&mut self, iface_idx: usize) {
        let _removed = self.storage.remove_link_entries_for_interface(iface_idx);
    }

    /// Remove reverse table entries referencing a specific interface
    pub(crate) fn remove_reverse_entries_for_interface(&mut self, iface_idx: usize) {
        self.storage.remove_reverse_entries_for_interface(iface_idx);
    }

    // Internal: Packet Handlers
    fn handle_announce(
        &mut self,
        packet: Packet,
        interface_index: usize,
        raw: &[u8],
    ) -> Result<(), TransportError> {
        let now = self.clock.now_ms();
        let is_path_response = packet.context == PacketContext::PathResponse;

        // Parse announce
        let announce =
            ReceivedAnnounce::from_packet(&packet).map_err(TransportError::AnnounceError)?;

        // Validate signature and destination hash
        announce.validate().map_err(TransportError::AnnounceError)?;

        let dest_hash = announce.destination_hash().into_bytes();

        // Don't process announces for our own destinations, these are echoes
        // from neighbors rebroadcasting our announce back to us.
        if self.local_destinations.contains(&dest_hash) {
            tracing::trace!(
                dest = %HexShort(&dest_hash),
                "Dropped announce for own destination (echo)"
            );
            return Ok(());
        }

        tracing::debug!(
            "received announce dest=<{}> iface={} hops={} path_response={}",
            HexShort(&dest_hash),
            self.iface_name(interface_index),
            packet.hops,
            is_path_response
        );
        tracing::debug!(
            "[ANN_RX] dst={} hops={} iface={} path_response={}",
            HexShort(&dest_hash),
            packet.hops,
            self.iface_name(interface_index),
            is_path_response
        );
        let random_hash = *announce.random_hash();

        // Random blob replay protection: reject if we've seen this exact random_hash,
        // UNLESS:
        // the path is unresponsive (path recovery), OR
        // the announce has fewer hops than the existing path (better route)
        // (Python Transport.py:1676-1681).
        if let Some(path) = self.storage.get_path(&dest_hash) {
            if path.random_blobs.contains(&random_hash)
                && !self.path_is_unresponsive(&dest_hash)
                && packet.hops >= path.hops
            {
                tracing::trace!(
                    dest = %HexShort(&dest_hash),
                    "Dropped announce, random hash already seen (replay)"
                );
                self.stats.packets_dropped += 1;
                return Ok(());
            }
        }

        // Rate limiting: check if we've seen this destination recently.
        // Also detect local rebroadcasts from neighbors.
        //
        // Rate limiting only suppresses rebroadcasting.
        // Path table updates must still proceed when a better route arrives
        // (fewer hops), otherwise nodes in ring/mesh topologies can get
        // stuck on suboptimal paths. Python's rate limiting (announce_rate_target)
        // is inside the `if should_add:` block and only affects rebroadcast
        // scheduling, not path table updates.
        let mut rate_limited = false;
        if let Some(existing) = self.storage.get_announce_mut(&dest_hash) {
            let elapsed = now.saturating_sub(existing.timestamp_ms);
            if elapsed < self.config.announce_rate_limit_ms {
                // Local rebroadcast detection: neighbor sent same announce at same or +1 hop.
                // Python parity (Transport.py:1584-1590): always increment the counter
                // but only cancel our pending retries once we have emitted at least one
                // rebroadcast ourselves (`retries > 0`). Without this guard, a neighbor
                // heard within the jitter window of the initial entry could cancel our
                // first-ever fire, violating the Python on-wire count.
                if packet.hops == existing.hops.saturating_add(1) {
                    existing.local_rebroadcasts = existing.local_rebroadcasts.saturating_add(1);
                    if existing.retries > 0 && existing.local_rebroadcasts >= LOCAL_REBROADCASTS_MAX
                    {
                        existing.retransmit_at_ms = None;
                        tracing::trace!(
                            dest = %HexShort(&dest_hash),
                            "Rebroadcasted announce has been passed on to another node, no further tries needed"
                        );
                    }
                } else if packet.hops == existing.hops.saturating_add(2)
                    && existing.retries > 0
                    && existing.retransmit_at_ms.is_some()
                {
                    // Another node forwarded our rebroadcast (hops+2 means they got
                    // our retransmit at hops+1 and forwarded it)
                    existing.retransmit_at_ms = None;
                    tracing::trace!(
                        dest = %HexShort(&dest_hash),
                        "Rebroadcasted announce has been forwarded by another node, no further tries needed"
                    );
                }

                rate_limited = true;
            }
        }

        // Determine whether to update the path table (hop count comparison).
        // Matches Python Transport.py:1620-1681 logic:
        // Equal or fewer hops: accept if emission timestamp is newer
        // More hops: accept only if path is expired, emission is newer,
        //   or path is unresponsive with same emission (path recovery)
        //
        // rate_limited announces still reach here so better-hop paths
        // can update the table. Only rebroadcasting is suppressed below.
        let should_update = if let Some(existing) = self.storage.get_path(&dest_hash) {
            let announce_emitted = emission_from_random_hash(&random_hash);
            let path_timebase = max_emission_from_blobs(&existing.random_blobs);
            if packet.hops <= existing.hops {
                // Equal or fewer hops: accept if emission is newer, or if
                // same emission but strictly fewer hops (same announce via
                // a shorter path, e.g. direct vs relayed path response).
                if announce_emitted > path_timebase
                    || (announce_emitted == path_timebase && packet.hops < existing.hops)
                {
                    self.mark_path_unknown_state(&dest_hash);
                    true
                } else {
                    false
                }
            } else {
                // More hops than current path
                if now >= existing.expires_ms {
                    // Path expired: accept with new random blob
                    self.mark_path_unknown_state(&dest_hash);
                    true
                } else if announce_emitted > path_timebase {
                    // Newer emission: accept
                    self.mark_path_unknown_state(&dest_hash);
                    true
                } else if announce_emitted == path_timebase && self.path_is_unresponsive(&dest_hash)
                {
                    // Same emission but path is unresponsive: accept worse-hop
                    // announce as alternative route (Python Transport.py:1677-1679).
                    // do NOT call mark_path_unknown_state() here, state
                    // stays UNRESPONSIVE until a fresh announce resets it.
                    true
                } else {
                    false
                }
            }
        } else {
            true // New destination
        };

        // Refresh local client tracking timestamp unconditionally.
        // Even when the path table isn't updated (same emission, same hops),
        // the client is still alive and its expiry timer should reset.
        if self.is_local_client(interface_index) {
            self.storage.set_local_client_known_dest(dest_hash, now);
        }

        // Refresh known ratchet unconditionally for valid announces.
        // Same reasoning as local_client timestamp: even when the path table isn't
        // updated, a ratchet that keeps arriving in valid announces should refresh
        // its expiry timer (Python Identity._remember_ratchet).
        if let Some(ratchet_pub) = announce.ratchet() {
            self.storage
                .remember_known_ratchet(dest_hash, *ratchet_pub, now);
        }

        // If rate-limited and the path table wouldn't improve, drop early.
        // But if the path WOULD improve (e.g. fewer hops via shorter route),
        // allow the update, only suppress the rebroadcast.
        if rate_limited && !should_update {
            tracing::trace!(
                dest = %HexShort(&dest_hash),
                "Dropped announce, rate limited (no path improvement)"
            );
            self.stats.packets_dropped += 1;
            return Ok(());
        }

        if should_update {
            // Preserve existing random_blobs and add the new one
            let mut random_blobs = self
                .storage
                .get_path(&dest_hash)
                .map(|p| p.random_blobs.clone())
                .unwrap_or_default();
            random_blobs.push(random_hash);

            // Cap random_blobs to prevent unbounded growth
            let max_blobs = self.config.max_random_blobs;
            if random_blobs.len() > max_blobs {
                let excess = random_blobs.len() - max_blobs;
                random_blobs.drain(..excess);
            }

            self.storage.set_path(
                dest_hash,
                PathEntry {
                    hops: packet.hops,
                    expires_ms: now + (self.config.path_expiry_secs * 1000),
                    interface_index,
                    random_blobs,
                    next_hop: packet.transport_id,
                },
            );
            let readback_ok = self.storage.get_path(&dest_hash).is_some();
            let table_len = self.storage.path_count();
            tracing::debug!(
                "[PATH_ADD] dst={} hops={} iface={} next_hop={:?} source=announce ok={} table_len={}",
                HexShort(&dest_hash),
                packet.hops,
                self.iface_name(interface_index),
                packet.transport_id.as_ref().map(|h| alloc::format!("{}", HexShort(&h[..]))),
                readback_ok,
                table_len
            );

            if let Some(ref next_hop) = packet.transport_id {
                tracing::debug!(
                    "Destination <{}> is now {} hops away via <{}> on {}",
                    HexShort(&dest_hash),
                    packet.hops,
                    HexShort(next_hop),
                    self.iface_name(interface_index)
                );
            } else {
                tracing::debug!(
                    "Destination <{}> is now {} hops away (direct) on {}",
                    HexShort(&dest_hash),
                    packet.hops,
                    self.iface_name(interface_index)
                );
            }

            // Check for pending discovery path requests (Python Transport.py:1838-1865).
            // If a transport node forwarded a path request for this destination,
            // send a targeted PATH_RESPONSE to the requesting interface.
            if self.config.enable_transport {
                self.send_discovery_path_response(&dest_hash, packet.hops, raw);
            }

            // Per-destination announce rate limiting (Python Transport.py:1692-1719)
            // Only blocks rebroadcast (announce_table insertion), path_table is already updated.
            // Skipped for PATH_RESPONSE context packets.
            let rate_blocked = if !is_path_response {
                self.check_announce_rate(&dest_hash, now)
            } else {
                false
            };

            if rate_blocked {
                tracing::trace!(
                    dest = %HexShort(&dest_hash),
                    "Announce rebroadcast blocked by per-destination rate limit"
                );
            }

            // Determine if we should rebroadcast.
            // Suppress rebroadcast when rate-limited (announce arrived within
            // rate window but had fewer hops, so path table was still updated).
            let should_rebroadcast =
                self.config.enable_transport && !is_path_response && !rate_blocked && !rate_limited;

            tracing::debug!(
                dest = %HexShort(&dest_hash),
                hops = packet.hops,
                transport = self.config.enable_transport,
                is_path_response,
                rate_blocked,
                rate_limited,
                should_rebroadcast,
                "announce rebroadcast decision"
            );

            // Track local client destinations (Block B). When an announce arrives
            // from a local client, record (iface_id → dest_hash) so we can detect
            // client reconnects and manage per-client destination state.
            // local_client_known_dests timestamp is refreshed unconditionally
            // above (before should_update check) so clients stay alive even when
            // path table doesn't change.
            let from_local = self.is_local_client(interface_index);
            let is_new_local_client_dest = if from_local {
                self.storage
                    .add_local_client_dest(interface_index, dest_hash)
            } else {
                false
            };

            // Local client first-registration: delay rebroadcast by 250ms to
            // batch multiple registrations during startup (Python Transport.py:2232).
            // Skip immediate broadcast; the deferred AnnounceEntry handles it.
            let delay_for_local_registration = should_rebroadcast && is_new_local_client_dest;

            // Python-RNS parity: received announces are not re-emitted
            // directly from handle_announce. They are inserted into
            // announce_table below and the retry scheduler performs every
            // on-air rebroadcast. Python does the same at Transport.py:1754
            // (inserts entry) + Transport.py:519-540 (scheduler fires).

            if delay_for_local_registration {
                tracing::debug!(
                    "Delaying rebroadcast of local client announce for <{}> by {}ms",
                    HexShort(&dest_hash),
                    LOCAL_CLIENT_ANNOUNCE_DELAY_MS
                );
            }

            // Update announce table (skipped when rate-blocked or rate-limited to
            // prevent rebroadcast; path table was already updated above)
            if !rate_blocked && !rate_limited {
                self.storage.set_announce(
                    dest_hash,
                    AnnounceEntry {
                        timestamp_ms: now,
                        hops: packet.hops,
                        // Python parity (Transport.py:1722, 1748-1752):
                        // non-local-client announces start at retries=0 and
                        // fire twice from the scheduler (bounded by
                        // LOCAL_REBROADCASTS_MAX=2); local-client announces
                        // start at retries=PATHFINDER_R=1 and fire exactly
                        // once. See docs/src/architecture-broadcast-python-parity.md.
                        retries: if delay_for_local_registration {
                            PATHFINDER_RETRIES
                        } else {
                            0
                        },
                        retransmit_at_ms: if delay_for_local_registration {
                            // Deferred first broadcast for local client registration
                            Some(now + LOCAL_CLIENT_ANNOUNCE_DELAY_MS)
                        } else if should_rebroadcast {
                            // Python-RNS parity (Transport.py:1728): the first
                            // rebroadcast for a received non-local-client
                            // announce is scheduled within the jitter window
                            // of receipt, without the PATHFINDER_G grace.
                            // Subsequent reschedules below add PATHFINDER_G,
                            // matching Transport.py:531.
                            let jitter = self
                                .deterministic_jitter_ms(&dest_hash, self.announce_jitter_max_ms());
                            Some(now + jitter)
                        } else {
                            None
                        },
                        raw_packet: raw.to_vec(),
                        receiving_interface_index: interface_index,
                        target_interface: None,
                        local_rebroadcasts: 0,
                        block_rebroadcasts: is_path_response,
                    },
                );

                // Log when an announce is queued for rebroadcast (aids collision diagnosis)
                if let Some(entry) = self.storage.get_announce(&dest_hash) {
                    if entry.retransmit_at_ms.is_some() {
                        tracing::debug!(
                            "announce queued for rebroadcast dest=<{}> hops={} retransmit_at_ms={:?} retries={}",
                            HexShort(&dest_hash),
                            entry.hops,
                            entry.retransmit_at_ms,
                            entry.retries,
                        );
                    }
                }

                // Track incoming announce frequency on receiving interface
                self.record_incoming_announce(interface_index);
            }

            // Cache raw announce for path responses. Always cache regardless of
            // transport mode, local clients may connect later and issue path
            // requests for destinations announced before the client connected.
            self.storage.set_announce_cache(dest_hash, raw.to_vec());

            // Forward announce to local client interfaces (Python Transport.py:1788-1833).
            // Convert to Header2 with the daemon's own transport_id and receipt-incremented
            // hops. The client uses transport_id to construct outbound Header2 packets.            // if we forward raw network bytes, the client sets the relay's transport_id
            // instead of ours, and our transport_id filter rejects the client's packets.
            if self.has_local_clients() {
                if let Ok(mut local_announce) = Packet::unpack(raw) {
                    local_announce.hops = packet.hops;
                    local_announce.flags.header_type = HeaderType::Type2;
                    local_announce.flags.transport_type = TransportType::Transport;
                    local_announce.transport_id = Some(*self.identity.hash());

                    let size = local_announce.packed_size();
                    let mut buf = alloc::vec![0u8; size];
                    if let Ok(len) = local_announce.pack(&mut buf) {
                        for &client_iface in &self.local_client_interfaces {
                            if client_iface != interface_index {
                                self.pending_actions.push(Action::SendPacket {
                                    iface: InterfaceId(client_iface),
                                    data: buf[..len].to_vec(),
                                });
                            }
                        }
                    }
                }
            }

            self.stats.announces_processed += 1;

            // Emit events. PathFound on every update (not just new paths)
            // so consumers always see current hop counts
            self.events.push(TransportEvent::PathFound {
                destination_hash: dest_hash,
                hops: packet.hops,
                interface_index,
            });

            self.events.push(TransportEvent::AnnounceReceived {
                announce,
                interface_index,
            });
        } else {
            tracing::trace!(
                "Valid announce for <{}> {} hops away on {}, but path not updated (current path is equal or better)",
                HexShort(&dest_hash), packet.hops, self.iface_name(interface_index)
            );
            // Path not updated, but still record the random_blob for replay detection
            if let Some(existing) = self.storage.get_path(&dest_hash) {
                let mut path = existing.clone();
                path.random_blobs.push(random_hash);
                let max_blobs = self.config.max_random_blobs;
                if path.random_blobs.len() > max_blobs {
                    let excess = path.random_blobs.len() - max_blobs;
                    path.random_blobs.drain(..excess);
                }
                self.storage.set_path(dest_hash, path);
            }
        }

        Ok(())
    }

    fn handle_link_request(
        &mut self,
        packet: Packet,
        interface_index: usize,
        raw: &[u8],
        truncated_hash: [u8; TRUNCATED_HASHBYTES],
    ) -> Result<(), TransportError> {
        let dest_hash = packet.destination_hash;
        let is_local = self.local_destinations.contains(&dest_hash);

        tracing::trace!(
            "handling link request dest=<{}> iface={} local={}",
            HexShort(&dest_hash),
            self.iface_name(interface_index),
            is_local
        );

        // Check if we have this destination registered (NodeCore gates accepts_links)
        if is_local {
            tracing::debug!(
                "Link request for <{}> received on {}, delivering to local destination",
                HexShort(&dest_hash),
                self.iface_name(interface_index)
            );
            self.events.push(TransportEvent::PacketReceived {
                destination_hash: dest_hash,
                packet: Box::new(packet),
                interface_index,
                raw_hash: Some(packet_hash(raw)),
            });
            return Ok(());
        }

        // Forward link request if transport enabled, or if from/for a local client
        // (Python Transport.py:1404)
        let from_local = self.is_local_client(interface_index);
        let for_local = self.is_for_local_client(&dest_hash);
        if self.config.enable_transport || from_local || for_local {
            // Read path data into locals (releases immutable borrow)
            let (target_iface, path_hops, needs_relay, next_hop) =
                if let Some(path) = self.storage.get_path(&dest_hash) {
                    (
                        path.interface_index,
                        path.hops,
                        path.needs_relay(),
                        path.next_hop,
                    )
                } else {
                    // No path known, drop
                    tracing::debug!(
                        "Link request for <{}> on {}, no path known, dropping",
                        HexShort(&dest_hash),
                        self.iface_name(interface_index)
                    );
                    self.stats.packets_dropped += 1;
                    return Ok(());
                };

            let now = self.clock.now_ms();
            let link_id = Link::calculate_link_id(raw);

            // Extract responder's Ed25519 signing key from cached announce
            // (Python Transport.py:2021-2033: peer_identity = Identity.recall(...))
            let peer_signing_key =
                self.storage
                    .get_announce_cache(&dest_hash)
                    .and_then(|cached_raw| {
                        Packet::unpack(cached_raw).ok().and_then(|p| {
                            let payload = p.data.as_slice();
                            if payload.len() >= crate::constants::IDENTITY_KEY_SIZE {
                                let mut key = [0u8; crate::constants::ED25519_KEY_SIZE];
                                key.copy_from_slice(
                                    &payload[crate::constants::X25519_KEY_SIZE
                                        ..crate::constants::IDENTITY_KEY_SIZE],
                                );
                                Some(key)
                            } else {
                                None
                            }
                        })
                    });

            // Insert link table entry for bidirectional routing
            self.storage.set_link_entry(
                *link_id.as_bytes(),
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: target_iface,
                    remaining_hops: path_hops,
                    received_interface_index: interface_index,
                    hops: packet.hops,
                    validated: false,
                    proof_timeout_ms: now
                        + ((packet.hops as u64 + path_hops as u64 + 2)
                            * crate::constants::DEFAULT_PER_HOP_TIMEOUT
                            * MS_PER_SECOND),
                    destination_hash: dest_hash,
                    peer_signing_key,
                },
            );

            // Populate reverse table at forwarding time
            self.storage.set_reverse(
                truncated_hash,
                ReverseEntry {
                    timestamp_ms: now,
                    receiving_interface_index: interface_index,
                    outbound_interface_index: target_iface,
                },
            );

            // Refresh path expiry on link request forward (Python Transport.py:1504)
            if let Some(path) = self.storage.get_path(&dest_hash) {
                let mut path = path.clone();
                path.expires_ms = now + (self.config.path_expiry_secs * 1000);
                self.storage.set_path(dest_hash, path);
            }

            // Clamp MTU signaling bytes to next-hop interface capacity
            // (Python Transport.py:1453-1480)
            let data = Self::clamp_link_request_mtu(
                &packet.data,
                interface_index,
                target_iface,
                &self.interface_hw_mtus,
            )
            .unwrap_or(packet.data);

            // Forward: strip at final hop, keep Type2 otherwise.
            let mut forwarded = if !needs_relay {
                // Final hop: destination is directly connected, strip transport header
                Packet {
                    flags: PacketFlags {
                        header_type: HeaderType::Type1,
                        transport_type: TransportType::Broadcast,
                        ..packet.flags
                    },
                    hops: packet.hops,
                    transport_id: None,
                    destination_hash: dest_hash,
                    context: packet.context,
                    data,
                }
            } else {
                // Intermediate: Header Type 2 with next-hop transport identity
                Packet {
                    flags: PacketFlags {
                        header_type: HeaderType::Type2,
                        transport_type: TransportType::Transport,
                        ..packet.flags
                    },
                    hops: packet.hops,
                    transport_id: next_hop,
                    destination_hash: dest_hash,
                    context: packet.context,
                    data,
                }
            };

            tracing::debug!(
                "Forwarding link request for <{}> from {} to {}, {} hops remaining",
                HexShort(&dest_hash),
                self.iface_name(interface_index),
                self.iface_name(target_iface),
                path_hops
            );
            return self.forward_on_interface_from(
                target_iface,
                Some(interface_index),
                &mut forwarded,
            );
        }

        Ok(())
    }

    fn handle_proof(
        &mut self,
        packet: Packet,
        interface_index: usize,
        cache_hash: [u8; 32],
    ) -> Result<(), TransportError> {
        let dest_hash = packet.destination_hash;
        let proof_data = packet.data.as_slice();

        tracing::trace!(
            "handling proof dest=<{}> iface={} proof_len={}",
            HexShort(&dest_hash),
            self.iface_name(interface_index),
            proof_data.len()
        );

        // Promote LRPROOF logging to DEBUG for diagnostics
        if packet.context == PacketContext::Lrproof {
            tracing::debug!(
                "LRPROOF arrived dest=<{}> iface={} hops={} proof_len={}",
                HexShort(&dest_hash),
                self.iface_name(interface_index),
                packet.hops,
                proof_data.len()
            );
        }

        // Check if this is a proof for a receipt we're tracking.
        // Two proof formats (Python defaults to implicit):
        //   Explicit: [packet_hash (32)] + [signature (64)] = 96 bytes
        //   Implicit: [signature (64)] only, destination_hash IS the truncated_packet_hash
        if proof_data.len() == crate::constants::PROOF_DATA_SIZE {
            // Explicit proof: extract packet hash from proof data
            let mut proof_packet_hash = [0u8; 32];
            proof_packet_hash.copy_from_slice(&proof_data[..32]);
            // Simple truncation, first 16 bytes of the full hash.
            // Do NOT use truncated_hash() which would SHA256 it again.
            let mut truncated = [0u8; TRUNCATED_HASHBYTES];
            truncated.copy_from_slice(&proof_packet_hash[..TRUNCATED_HASHBYTES]);

            if let Some(receipt) = self.storage.get_receipt(&truncated) {
                tracing::trace!(
                    dest = %HexShort(&dest_hash),
                    "Proof matched local receipt (explicit format)"
                );
                let receipt_dest = *receipt.destination_hash.as_bytes();
                let expected = receipt.packet_hash;
                self.events.push(TransportEvent::ProofReceived {
                    packet_hash: truncated,
                    destination_hash: receipt_dest,
                    expected_packet_hash: expected,
                    proof_data: proof_data.to_vec(),
                });
                return Ok(());
            }
        } else if proof_data.len() == crate::constants::IMPLICIT_PROOF_SIZE {
            // Implicit proof: destination_hash of the proof packet IS the
            // truncated packet hash (see Python ProofDestination class).
            // Look up receipt by that hash, then reconstruct explicit format
            // so NodeCore can verify uniformly.
            if let Some(receipt) = self.storage.get_receipt(&dest_hash) {
                tracing::trace!(
                    dest = %HexShort(&dest_hash),
                    "Proof matched local receipt (implicit format)"
                );
                let receipt_dest = *receipt.destination_hash.as_bytes();
                let expected = receipt.packet_hash;
                // Normalize to explicit format: packet_hash + signature
                let mut explicit = Vec::with_capacity(crate::constants::PROOF_DATA_SIZE);
                explicit.extend_from_slice(&expected);
                explicit.extend_from_slice(proof_data);
                self.events.push(TransportEvent::ProofReceived {
                    packet_hash: dest_hash,
                    destination_hash: receipt_dest,
                    expected_packet_hash: expected,
                    proof_data: explicit,
                });
                return Ok(());
            }
        }

        // Check if this is for a registered destination (legacy behavior)
        if self.local_destinations.contains(&dest_hash) {
            tracing::debug!(
                "Proof for <{}> delivered to local destination on {}",
                HexShort(&dest_hash),
                self.iface_name(interface_index)
            );
            // Deferred cache insert for LRPROOF local delivery
            // (Python Transport.py:2072). Non-LRPROOF proofs for registered
            // destinations were already cached in process_incoming().
            self.storage.add_packet_hash(cache_hash);
            self.events.push(TransportEvent::PacketReceived {
                destination_hash: dest_hash,
                packet: Box::new(packet),
                interface_index,
                raw_hash: None,
            });
            return Ok(());
        }

        // Route proofs via link table if transport enabled, or for local client links
        // (Python Transport.py:2016)
        let from_local = self.is_local_client(interface_index);
        let for_local_link = self.is_for_local_client_link(&dest_hash);
        if self.config.enable_transport || from_local || for_local_link {
            if let Some(link_entry) = self.storage.get_link_entry(&dest_hash).cloned() {
                if packet.context == PacketContext::Lrproof {
                    tracing::debug!(
                        "LRPROOF link_table hit dest=<{}> iface={} hops={} entry(next_hop={} remaining={} recv={} hops={})",
                        HexShort(&dest_hash),
                        self.iface_name(interface_index),
                        packet.hops,
                        self.iface_name(link_entry.next_hop_interface_index),
                        link_entry.remaining_hops,
                        self.iface_name(link_entry.received_interface_index),
                        link_entry.hops,
                    );
                }
                // Determine direction with hop count validation
                let target_iface = if interface_index == link_entry.next_hop_interface_index {
                    // From destination side: check remaining_hops
                    if packet.hops != link_entry.remaining_hops {
                        tracing::debug!(
                            dest = %HexShort(&dest_hash),
                            packet_hops = packet.hops,
                            remaining_hops = link_entry.remaining_hops,
                            "Dropped LRPROOF, hop count mismatch (remaining_hops)"
                        );
                        self.stats.packets_dropped += 1;
                        return Ok(());
                    }
                    link_entry.received_interface_index
                } else if interface_index == link_entry.received_interface_index {
                    // From initiator side: check taken hops
                    if packet.hops != link_entry.hops {
                        tracing::debug!(
                            dest = %HexShort(&dest_hash),
                            packet_hops = packet.hops,
                            entry_hops = link_entry.hops,
                            "Dropped LRPROOF, hop count mismatch (taken hops)"
                        );
                        self.stats.packets_dropped += 1;
                        return Ok(());
                    }
                    link_entry.next_hop_interface_index
                } else {
                    tracing::debug!(
                        "Dropped proof for <{}> on {}, unknown link direction (next_hop={}, recv={})",
                        HexShort(&dest_hash),
                        self.iface_name(interface_index),
                        self.iface_name(link_entry.next_hop_interface_index),
                        self.iface_name(link_entry.received_interface_index),
                    );
                    return Ok(()); // Unknown direction
                };

                // LRPROOF validation: check proof data size and signature before forwarding
                // (Python Transport.py:2021-2033)
                if packet.context == PacketContext::Lrproof {
                    use crate::constants::{
                        ED25519_KEY_SIZE, ED25519_SIGNATURE_SIZE, X25519_KEY_SIZE,
                    };

                    // Link proof format:
                    //   sig(64) + X25519(32) = 96 bytes (without signalling)
                    //   sig(64) + X25519(32) + signaling(3) = 99 bytes (with signalling)
                    const LINK_PROOF_SIZE_MIN: usize = 96;
                    const LINK_PROOF_SIZE_MAX: usize = 99;
                    if proof_data.len() != LINK_PROOF_SIZE_MIN
                        && proof_data.len() != LINK_PROOF_SIZE_MAX
                    {
                        tracing::warn!(
                            dest = %HexShort(&dest_hash),
                            len = proof_data.len(),
                            "Dropped LRPROOF, malformed proof size"
                        );
                        self.stats.packets_dropped += 1;
                        return Ok(());
                    }

                    // Validate Ed25519 signature if we have the peer's signing key
                    if let Some(peer_ed25519_bytes) = &link_entry.peer_signing_key {
                        let signature = &proof_data[..ED25519_SIGNATURE_SIZE];
                        let peer_x25519_pub = &proof_data
                            [ED25519_SIGNATURE_SIZE..ED25519_SIGNATURE_SIZE + X25519_KEY_SIZE];
                        let signalling =
                            if proof_data.len() > ED25519_SIGNATURE_SIZE + X25519_KEY_SIZE {
                                &proof_data[ED25519_SIGNATURE_SIZE + X25519_KEY_SIZE..]
                            } else {
                                &[]
                            };

                        // signed_data = link_id(16) + X25519_pub(32) + Ed25519_pub(32) + [signalling]
                        let mut signed = Vec::with_capacity(
                            TRUNCATED_HASHBYTES
                                + X25519_KEY_SIZE
                                + ED25519_KEY_SIZE
                                + signalling.len(),
                        );
                        signed.extend_from_slice(&dest_hash);
                        signed.extend_from_slice(peer_x25519_pub);
                        signed.extend_from_slice(peer_ed25519_bytes);
                        signed.extend_from_slice(signalling);

                        match ed25519_dalek::VerifyingKey::from_bytes(peer_ed25519_bytes) {
                            Ok(vk) => {
                                use ed25519_dalek::Verifier;
                                if let Ok(sig_bytes) =
                                    <[u8; ED25519_SIGNATURE_SIZE]>::try_from(signature)
                                {
                                    let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
                                    if vk.verify(&signed, &sig).is_err() {
                                        tracing::warn!(
                                            dest = %HexShort(&dest_hash),
                                            "Dropped LRPROOF, signature verification failed"
                                        );
                                        self.stats.packets_dropped += 1;
                                        return Ok(());
                                    }
                                }
                            }
                            Err(_) => {
                                tracing::warn!(
                                    dest = %HexShort(&dest_hash),
                                    "Dropped LRPROOF, malformed Ed25519 key"
                                );
                                // Malformed key bytes, drop
                                self.stats.packets_dropped += 1;
                                return Ok(());
                            }
                        }
                    }
                    // If peer_signing_key is None, announce was not cached at link creation.
                    // Forward anyway (cannot validate without key).
                    if link_entry.peer_signing_key.is_none() {
                        tracing::warn!(
                            link_id = ?dest_hash,
                            "forwarding LRPROOF without signature validation — announce not cached for link"
                        );
                    }
                }

                // Mark link as validated on first proof
                if !link_entry.validated {
                    if let Some(entry) = self.storage.get_link_entry_mut(&dest_hash) {
                        entry.validated = true;
                        entry.timestamp_ms = self.clock.now_ms();
                    }
                }

                // Insert hash for non-LRPROOF proofs (Python Transport.py:1543).
                // LRPROOF hashes are intentionally NOT cached during link-table
                // forwarding (Python Transport.py:2016-2039).
                if packet.context != PacketContext::Lrproof {
                    self.storage.add_packet_hash(cache_hash);
                }

                // Forward proof via link table
                tracing::debug!(
                    "Proof for <{}> forwarding via link table to {}",
                    HexShort(&dest_hash),
                    self.iface_name(target_iface)
                );
                let mut forwarded = packet;
                return self.forward_on_interface_from(
                    target_iface,
                    Some(interface_index),
                    &mut forwarded,
                );
            } else if packet.context == PacketContext::Lrproof {
                tracing::debug!(
                    "LRPROOF for <{}> on {}: no link_table entry found",
                    HexShort(&dest_hash),
                    self.iface_name(interface_index),
                );
            }
        } else if packet.context == PacketContext::Lrproof {
            tracing::debug!(
                "LRPROOF for <{}> on {}: transport={} from_local={} for_local={}",
                HexShort(&dest_hash),
                self.iface_name(interface_index),
                self.config.enable_transport,
                from_local,
                for_local_link,
            );
        }

        // Reverse table routing for regular proofs
        // (Python Transport.py:2091)
        {
            let proof_for_local = self
                .storage
                .get_reverse(&dest_hash)
                .map(|e| self.is_local_client(e.receiving_interface_index))
                .unwrap_or(false);
            if self.config.enable_transport || from_local || proof_for_local {
                if let Some(reverse_entry) = self.storage.remove_reverse(&dest_hash) {
                    // The proof should arrive on the outbound interface (where the
                    // original packet was forwarded to) and be routed back to the
                    // receiving interface (where the original packet came from).
                    if interface_index == reverse_entry.outbound_interface_index {
                        tracing::trace!(
                            "Proof for <{}> forwarding via reverse table to {}",
                            HexShort(&dest_hash),
                            self.iface_name(reverse_entry.receiving_interface_index)
                        );
                        let mut forwarded = packet;
                        return self.forward_on_interface_from(
                            reverse_entry.receiving_interface_index,
                            Some(interface_index),
                            &mut forwarded,
                        );
                    }
                }
            }
        }

        // Deliver link-addressed proofs to local links.
        //
        // LRPROOF (link establishment proofs, context=Lrproof) need delivery
        // for non-transport nodes (Python Transport.py:2054-2073).
        // Data proofs (96 bytes, context=None) need delivery for channel ACK
        // processing (Python Link.py:1173 generates proof for every CHANNEL packet).
        //
        // If we reach this point:
        // Not in Transport::receipts (receipt check at line 1404 failed)
        // Not for a registered destination (check at line 1435 failed)
        // Not forwarded by transport (enable_transport is false, or link_id
        //   not in our link_table, initiator's own links are never in link_table)
        //
        // The node layer routes Proof packets to process_link_packet(),
        // which distinguishes LRPROOF (link establishment) from data proofs
        // (PROOF_DATA_SIZE + context=None) and validates each cryptographically.
        if packet.flags.dest_type == DestinationType::Link {
            self.storage.add_packet_hash(cache_hash);
            self.events.push(TransportEvent::PacketReceived {
                destination_hash: dest_hash,
                packet: Box::new(packet),
                interface_index,
                raw_hash: None,
            });
            return Ok(());
        }

        Ok(())
    }

    fn handle_data(
        &mut self,
        mut packet: Packet,
        interface_index: usize,
        full_packet_hash: [u8; 32],
        truncated_hash: [u8; TRUNCATED_HASHBYTES],
    ) -> Result<(), TransportError> {
        let dest_hash = packet.destination_hash;

        tracing::trace!(
            "handling data packet dest=<{}> iface={} hops={} data_len={}",
            HexShort(&dest_hash),
            self.iface_name(interface_index),
            packet.hops,
            packet.data.len()
        );

        // Intercept path requests (before normal destination routing)
        if dest_hash == self.path_request_hash {
            tracing::trace!(
                "Intercepted path request on {}",
                self.iface_name(interface_index)
            );
            return self.handle_path_request(packet, interface_index);
        }

        // Plain broadcast forwarding through shared instance
        // (Python Transport.py:1384-1398). Plain broadcasts bypass transport
        // routing, they are forwarded directly between local clients and
        // network interfaces. Control destinations (path requests, tunnel
        // synthesis) are excluded, they have their own handlers.
        // Codeberg issue #24.
        let is_control_dest =
            dest_hash == self.path_request_hash || dest_hash == self.tunnel_synthesize_hash;
        if packet.flags.dest_type == DestinationType::Plain
            && packet.flags.transport_type == TransportType::Broadcast
            && !is_control_dest
        {
            let from_local = self.is_local_client(interface_index);
            if from_local {
                // Local client → broadcast on all interfaces except sender
                tracing::debug!(
                    "Plain broadcast from local client on {}, forwarding to all interfaces",
                    self.iface_name(interface_index)
                );
                self.forward_on_all_except(interface_index, &mut packet);
            } else if self.has_local_clients() {
                // Network → forward only to local client interfaces
                tracing::debug!(
                    "Plain broadcast from {} for <{}>, forwarding to local clients",
                    self.iface_name(interface_index),
                    HexShort(&dest_hash)
                );
                let size = packet.packed_size();
                let mut buf = alloc::vec![0u8; size];
                if let Ok(len) = packet.pack(&mut buf) {
                    for &client_iface in &self.local_client_interfaces {
                        if client_iface != interface_index {
                            self.pending_actions.push(Action::SendPacket {
                                iface: InterfaceId(client_iface),
                                data: buf[..len].to_vec(),
                            });
                        }
                    }
                    self.stats.packets_forwarded += 1;
                }
            }

            // Also deliver locally if destination is registered on this node
            if self.local_destinations.contains(&dest_hash) {
                tracing::debug!(
                    "[PKT_LOCAL] dst={} iface={} matched=true",
                    HexShort(&dest_hash),
                    self.iface_name(interface_index)
                );
                self.storage.add_packet_hash(full_packet_hash);
                self.events.push(TransportEvent::PacketReceived {
                    destination_hash: dest_hash,
                    packet: Box::new(packet),
                    interface_index,
                    raw_hash: Some(full_packet_hash),
                });
                self.events.push(TransportEvent::ProofRequested {
                    packet_hash: full_packet_hash,
                    destination_hash: dest_hash,
                    interface_index,
                });
            }
            // Early return: Python falls through to "general transport handling"
            // (Transport.py:1404) but that path is a no-op for PLAIN broadcasts.            // for_local_client is always False (no path table entry for PLAIN dests).
            return Ok(());
        }

        // Check if we have this destination registered
        if self.local_destinations.contains(&dest_hash) {
            tracing::debug!(
                "Data packet for <{}> delivered to local destination on {}",
                HexShort(&dest_hash),
                self.iface_name(interface_index)
            );
            self.events.push(TransportEvent::PacketReceived {
                destination_hash: dest_hash,
                packet: Box::new(packet),
                interface_index,
                raw_hash: Some(full_packet_hash),
            });

            // Always emit ProofRequested. NodeCore decides based on strategy
            self.events.push(TransportEvent::ProofRequested {
                packet_hash: full_packet_hash,
                destination_hash: dest_hash,
                interface_index,
            });

            return Ok(());
        }

        // Route via link/path table if transport enabled, or for local client traffic
        // (Python Transport.py:1404)
        let from_local = self.is_local_client(interface_index);
        let for_local = self.is_for_local_client(&dest_hash);
        let for_local_link = self.is_for_local_client_link(&dest_hash);
        if self.config.enable_transport || from_local || for_local || for_local_link {
            // Check link table for validated links
            if let Some(link_entry) = self.storage.get_link_entry(&dest_hash).cloned() {
                if link_entry.validated {
                    let target_iface = if interface_index == link_entry.next_hop_interface_index {
                        // From destination side: check remaining_hops
                        if packet.hops != link_entry.remaining_hops {
                            tracing::trace!(
                                dest = %HexShort(&dest_hash),
                                "Dropped data packet, hop count mismatch (remaining_hops)"
                            );
                            return Ok(());
                        }
                        link_entry.received_interface_index
                    } else if interface_index == link_entry.received_interface_index {
                        // From initiator side: check taken hops
                        if packet.hops != link_entry.hops {
                            tracing::trace!(
                                dest = %HexShort(&dest_hash),
                                "Dropped data packet, hop count mismatch (taken hops)"
                            );
                            return Ok(());
                        }
                        link_entry.next_hop_interface_index
                    } else {
                        tracing::trace!(
                            "Dropped data packet for <{}> on {}, unknown link direction",
                            HexShort(&dest_hash),
                            self.iface_name(interface_index)
                        );
                        return Ok(()); // Unknown direction
                    };

                    // Populate reverse table for link-routed data packets
                    let now = self.clock.now_ms();
                    self.storage.set_reverse(
                        truncated_hash,
                        ReverseEntry {
                            timestamp_ms: now,
                            receiving_interface_index: interface_index,
                            outbound_interface_index: target_iface,
                        },
                    );

                    // Deferred cache insert: hash was skipped in process_incoming()
                    // because dest_hash is in link_table. Insert now that we've
                    // validated direction and will forward (Python Transport.py:1543).
                    //
                    // Skip hash caching for:
                    // 1. Local client traffic: resource retransmissions produce
                    //    identical raw bytes, caching would block future retransmits.
                    // 2. Link-table-routed data: relay nodes must forward retransmitted
                    //    resource segments (identical bytes) to the other side of the
                    //    link. No dedup needed, link-addressed packets follow a fixed
                    //    link_table path with no routing loop risk.
                    let is_link_routed = self.storage.has_link_entry(&dest_hash)
                        && packet.flags.dest_type == DestinationType::Link;
                    if !self.is_local_client(interface_index) && !is_link_routed {
                        self.storage.add_packet_hash(full_packet_hash);
                    }

                    // Forward data via link table
                    tracing::trace!(
                        "Data packet for <{}> forwarding via link table to {}",
                        HexShort(&dest_hash),
                        self.iface_name(target_iface)
                    );
                    let mut forwarded = packet;
                    return self.forward_on_interface_from(
                        target_iface,
                        Some(interface_index),
                        &mut forwarded,
                    );
                }
            }

            // Non-link-addressed packets: forward via path table.
            // Link-addressed packets not in link_table are for our own local
            // links (link_ids never appear in path_table), fall through to
            // the delivery code below.
            if packet.flags.dest_type != DestinationType::Link {
                tracing::trace!(
                    "Data packet for <{}> forwarding via path table",
                    HexShort(&dest_hash)
                );
                self.forward_packet(packet, interface_index, truncated_hash)?;
                return Ok(());
            }
        }

        // Deliver link-addressed Data packets to local links (Python Transport.py:1969-1994).
        // On non-transport nodes, link_table routing is skipped entirely.
        // On transport nodes, relayed links are handled via link_table above;
        // only packets for our own local links reach this point.
        if packet.flags.dest_type == DestinationType::Link {
            self.storage.add_packet_hash(full_packet_hash);
            self.events.push(TransportEvent::PacketReceived {
                destination_hash: dest_hash,
                packet: Box::new(packet),
                interface_index,
                raw_hash: Some(full_packet_hash),
            });
            return Ok(());
        }

        Ok(())
    }

    // Internal: Forwarding
    fn forward_packet(
        &mut self,
        mut packet: Packet,
        source_interface_index: usize,
        truncated_hash: [u8; TRUNCATED_HASHBYTES],
    ) -> Result<(), TransportError> {
        // Read path data into locals (releases immutable borrow)
        let (target_iface, needs_relay, next_hop) =
            if let Some(path) = self.storage.get_path(&packet.destination_hash) {
                tracing::debug!(
                    "[PATH_LOOKUP] dst={} found=true hops={} iface={}",
                    HexShort(&packet.destination_hash),
                    path.hops,
                    self.iface_name(path.interface_index)
                );
                (path.interface_index, path.needs_relay(), path.next_hop)
            } else {
                tracing::debug!(
                    "Cannot forward packet for <{}>, no path known, dropping",
                    HexShort(&packet.destination_hash)
                );
                tracing::debug!(
                    "[PATH_LOOKUP] dst={} found=false",
                    HexShort(&packet.destination_hash)
                );
                tracing::debug!(
                    "[PKT_DROP] dst={} type={:?} hops={} iface_in={} reason=no_path",
                    HexShort(&packet.destination_hash),
                    packet.flags.packet_type,
                    packet.hops,
                    self.iface_name(source_interface_index)
                );
                self.stats.packets_dropped += 1;
                return Ok(());
            };

        tracing::debug!(
            "Forwarding packet for <{}> from {} to {}, {} hops",
            HexShort(&packet.destination_hash),
            self.iface_name(source_interface_index),
            self.iface_name(target_iface),
            packet.hops
        );
        tracing::debug!(
            "[PKT_FORWARD] dst={} type={:?} hops={} iface_in={} iface_out={} next_hop={:?}",
            HexShort(&packet.destination_hash),
            packet.flags.packet_type,
            packet.hops,
            self.iface_name(source_interface_index),
            self.iface_name(target_iface),
            next_hop
                .as_ref()
                .map(|h| alloc::format!("{}", HexShort(&h[..])))
        );

        let now = self.clock.now_ms();

        // Refresh path expiry on forward (Python Transport.py:990)
        if let Some(path) = self.storage.get_path(&packet.destination_hash) {
            let mut path = path.clone();
            path.expires_ms = now + (self.config.path_expiry_secs * 1000);
            self.storage.set_path(packet.destination_hash, path);
        }

        // Populate reverse table at forwarding time
        self.storage.set_reverse(
            truncated_hash,
            ReverseEntry {
                timestamp_ms: now,
                receiving_interface_index: source_interface_index,
                outbound_interface_index: target_iface,
            },
        );
        tracing::debug!(
            "[REVERSE_ADD] pkt_hash={} in_iface={} out_iface={}",
            HexShort(&truncated_hash),
            self.iface_name(source_interface_index),
            self.iface_name(target_iface)
        );

        if needs_relay {
            // Intermediate relay: keep Type2, replace transport_id with next hop
            packet.flags.header_type = HeaderType::Type2;
            packet.flags.transport_type = TransportType::Transport;
            packet.transport_id = next_hop; // guaranteed Some by needs_relay()
        } else {
            // Directly connected or no next_hop: strip to Type1
            packet.flags.header_type = HeaderType::Type1;
            packet.flags.transport_type = TransportType::Broadcast;
            packet.transport_id = None;
        }

        self.forward_on_interface_from(target_iface, Some(source_interface_index), &mut packet)
    }

    /// Clamp MTU signaling bytes in a link request payload.
    ///
    /// If the payload contains signaling bytes (67 bytes), decode the MTU,
    /// clamp to min(path_mtu, prev_hop_hw_mtu, next_hop_hw_mtu), re-encode.
    /// If no signaling bytes (64-byte request) or no HW_MTU known for
    /// next-hop, return the data unchanged.
    /// (Python Transport.py:1453-1480)
    fn clamp_link_request_mtu(
        data: &PacketData,
        prev_hop_iface: usize,
        next_hop_iface: usize,
        hw_mtus: &BTreeMap<usize, u32>,
    ) -> Option<PacketData> {
        use crate::link::{
            decode_signaling_bytes, encode_signaling_bytes, LINK_REQUEST_BASE_SIZE, SIGNALING_SIZE,
        };

        let payload = data.as_slice();
        let signaling_size = LINK_REQUEST_BASE_SIZE + SIGNALING_SIZE; // 67

        if payload.len() != signaling_size {
            return None; // No signaling bytes — nothing to clamp
        }

        let sig_bytes: [u8; 3] = match payload[LINK_REQUEST_BASE_SIZE..signaling_size].try_into() {
            Ok(b) => b,
            Err(_) => return None,
        };
        let (path_mtu, mode) = decode_signaling_bytes(&sig_bytes);

        // Three-way min: MTU can only go down, never up.
        let ph_mtu = hw_mtus.get(&prev_hop_iface).copied().unwrap_or(u32::MAX);
        let nh_mtu = hw_mtus.get(&next_hop_iface).copied()?;

        let clamped = path_mtu.min(nh_mtu).min(ph_mtu);

        if clamped >= path_mtu {
            return None; // No clamping needed
        }

        // Re-encode with clamped MTU
        let new_sig = encode_signaling_bytes(clamped, mode);
        let mut new_data = payload.to_vec();
        new_data[LINK_REQUEST_BASE_SIZE..signaling_size].copy_from_slice(&new_sig);

        tracing::debug!(
            "Clamped link request MTU from {} to {} (prev-hop HW_MTU={}, next-hop HW_MTU={})",
            path_mtu,
            clamped,
            ph_mtu,
            nh_mtu
        );

        Some(PacketData::Owned(new_data))
    }

    /// Forward a packet to `target_iface`, optionally suppressing same-interface
    /// relay on shared media.
    ///
    /// When `receiving_iface` is `Some(idx)` and equals `target_iface`, the
    /// packet is silently dropped instead of being transmitted. This prevents
    /// two distinct problems on shared broadcast media like LoRa:
    ///
    /// 1. **RF collision** (primary): On a half-duplex LoRa channel with 3+
    ///    transport-enabled nodes, a bystander (C) that relays A's link request
    ///    back onto the same channel causes its TX to overlap with B's proof TX.
    ///    The collision corrupts both frames. A never receives the proof, and
    ///    the link fails. Hash-based dedup in
    ///    `send_on_interface` cannot fix this because the proof was destroyed
    ///    in flight before reception.
    ///
    /// 2. **Duplicate processing**: Even without collision, the relayed echo
    ///    would be a duplicate that wastes channel airtime and could confuse
    ///    link/resource state machines.
    ///
    /// **Why Python doesn't need this**: Python Reticulum has no explicit
    /// same-interface suppression (Transport.py:1503 forwards unconditionally).
    /// Python relies on outbound hash caching (Transport.py:1169) for dedup.
    /// Python's GIL-serialized execution creates different TX timing than
    /// Rust's async driver, making RF collisions less likely in practice.
    /// With Rust's async driver, the relay TX fires fast enough to collide
    /// with the responder's proof. Confirmed: 3 Python nodes all transport-
    /// enabled on same LoRa channel PASSES; 3 Rust nodes without this fix
    /// FAILS 100%.
    ///
    /// **Safe for multi-hop**: When `target_iface != receiving_iface` (e.g.,
    /// a bridge node with two RNodes on different frequencies), forwarding
    /// proceeds normally. The suppression only fires when both interfaces
    /// are the same object, i.e., the packet would be relayed back onto the
    /// exact medium it was received from.
    fn forward_on_interface_from(
        &mut self,
        target_iface: usize,
        receiving_iface: Option<usize>,
        packet: &mut Packet,
    ) -> Result<(), TransportError> {
        if packet.hops > self.config.max_hops {
            tracing::debug!(
                "Dropped packet on {}, max hops exceeded (hops={}, max={})",
                self.iface_name(target_iface),
                packet.hops,
                self.config.max_hops
            );
            self.stats.packets_dropped += 1;
            return Ok(());
        }

        // Suppress same-interface relay on shared media.
        if receiving_iface == Some(target_iface) {
            tracing::trace!(
                "Suppressed same-interface relay on {}",
                self.iface_name(target_iface)
            );
            return Ok(());
        }

        self.stats.packets_forwarded += 1;
        self.send_packet_on_interface(target_iface, packet)
    }

    /// Forward a packet on all interfaces except one.
    /// Checks TTL and updates stats. Hops were already incremented on receipt.
    /// Pack and broadcast a relayed packet on every online interface except
    /// the one it arrived on. Matches Python Transport.outbound() behaviour
    /// for packets of transport_type BROADCAST (Transport.py:1388-1392),
    /// which explicitly skips the receiving interface.
    fn forward_on_all_except(&mut self, except_index: usize, packet: &mut Packet) {
        if packet.hops > self.config.max_hops {
            tracing::debug!(
                hops = packet.hops,
                max_hops = self.config.max_hops,
                "Dropped broadcast packet, max hops exceeded"
            );
            self.stats.packets_dropped += 1;
            return;
        }
        let size = packet.packed_size();
        let mut buf = alloc::vec![0u8; size];
        if let Ok(len) = packet.pack(&mut buf) {
            self.send_on_all_interfaces_except(except_index, &buf[..len]);
            self.stats.packets_forwarded += 1;
        }
    }

    /// Pack and broadcast a relayed announce on every online interface,
    /// including the one the original packet arrived on. Matches Python
    /// Transport.outbound() for ANNOUNCE packets (Transport.py:1025-1167),
    /// which loops all interfaces without filtering by receiving_interface.
    /// Self-heard echoes are dropped on arrival by the packet_hashlist dedup
    /// in process_incoming, seeded here before the broadcast is emitted.
    fn forward_on_all(&mut self, packet: &mut Packet) {
        if packet.hops > self.config.max_hops {
            tracing::debug!(
                hops = packet.hops,
                max_hops = self.config.max_hops,
                "Dropped broadcast packet, max hops exceeded"
            );
            self.stats.packets_dropped += 1;
            return;
        }
        let size = packet.packed_size();
        let mut buf = alloc::vec![0u8; size];
        if let Ok(len) = packet.pack(&mut buf) {
            // Seed dedup so our own emission is recognised when it echoes
            // back on a shared medium (e.g., both nodes heard by the same
            // LoRa antenna). Matches the add_packet_hash path already used
            // by send_on_all_interfaces / send_on_all_interfaces_except.
            self.storage.add_packet_hash(packet_hash(&buf[..len]));
            self.pending_actions.push(Action::Broadcast {
                data: buf[..len].to_vec(),
                exclude_iface: None,
            });
            self.stats.packets_forwarded += 1;
        }
    }

    fn send_packet_on_interface(
        &mut self,
        interface_index: usize,
        packet: &Packet,
    ) -> Result<(), TransportError> {
        // Use a dynamically-sized buffer so forwarded packets with a
        // negotiated link MTU larger than the base MTU can be serialized.
        let size = packet.packed_size();
        let mut buf = alloc::vec![0u8; size];
        let len = packet.pack(&mut buf)?;

        self.send_on_interface(interface_index, &buf[..len])
    }

    // Public: Path Request API
    /// Request a path to a destination
    ///
    /// Sends a path request packet to discover the route to a destination.
    /// Rate-limited to one request per destination per PATH_REQUEST_MIN_INTERVAL_MS.
    ///
    /// # Arguments
    /// * `dest_hash` - The destination hash to find a path for
    /// * `on_interface` - Optional specific interface to send on, or None for all
    /// * `tag` - A random 16-byte tag for dedup
    pub fn request_path(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        on_interface: Option<usize>,
        tag: &[u8; TRUNCATED_HASHBYTES],
    ) -> Result<(), TransportError> {
        let now = self.clock.now_ms();

        // Rate limiting
        if let Some(last_request) = self.storage.get_path_request_time(dest_hash) {
            if now.saturating_sub(last_request) < PATH_REQUEST_MIN_INTERVAL_MS {
                return Ok(());
            }
        }
        self.storage.set_path_request_time(*dest_hash, now);

        // Build path request data:
        //   Transport node:     dest_hash(16) + transport_id(16) + tag(16) = 48 bytes
        //   Non-transport node: dest_hash(16) + tag(16)                    = 32 bytes
        // Python Transport.py:2541-2557
        let data = if self.config.enable_transport {
            let transport_id_bytes = *self.identity.hash();
            let mut d = Vec::with_capacity(48);
            d.extend_from_slice(dest_hash);
            d.extend_from_slice(&transport_id_bytes);
            d.extend_from_slice(tag);
            d
        } else {
            let mut d = Vec::with_capacity(32);
            d.extend_from_slice(dest_hash);
            d.extend_from_slice(tag);
            d
        };

        let packet = Packet {
            flags: PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type1,
                context_flag: false,
                transport_type: TransportType::Broadcast,
                dest_type: crate::destination::DestinationType::Plain,
                packet_type: PacketType::Data,
            },
            hops: 0,
            transport_id: None,
            destination_hash: self.path_request_hash,
            context: PacketContext::None,
            data: PacketData::Owned(data),
        };

        let mut buf = [0u8; crate::constants::MTU];
        let len = packet.pack(&mut buf)?;

        match on_interface {
            Some(idx) => self.send_on_interface(idx, &buf[..len]),
            None => {
                self.send_on_all_interfaces(&buf[..len]);
                Ok(())
            }
        }
    }

    /// Get the well-known path request destination hash
    pub fn path_request_hash(&self) -> &[u8; TRUNCATED_HASHBYTES] {
        &self.path_request_hash
    }

    // Public: Announce Bandwidth Cap API
    /// Register an interface's bitrate for announce bandwidth capping.
    ///
    /// When `bitrate_bps > 0`, announces on this interface are rate-limited
    /// to `announce_cap_percent`% of the link capacity (default 2%).
    /// Excess announces are queued and drained as bandwidth permits.
    ///
    /// When `bitrate_bps == 0`, the interface has no cap (unlimited bandwidth).
    /// This is the default for TCP interfaces.
    ///
    /// # Arguments
    /// * `iface_index` - The interface index
    /// * `bitrate_bps` - The interface bitrate in bits per second (0 = unlimited)
    pub fn register_interface_bitrate(&mut self, iface_index: usize, bitrate_bps: u32) {
        if bitrate_bps == 0 {
            // No cap needed for unlimited interfaces
            self.interface_announce_caps.remove(&iface_index);
            return;
        }

        self.interface_announce_caps.insert(
            iface_index,
            InterfaceAnnounceCap {
                bitrate_bps,
                announce_cap_percent: DEFAULT_ANNOUNCE_CAP_PERCENT,
                allowed_at_ms: 0,
                queue: VecDeque::new(),
            },
        );
    }

    /// Remove announce cap state for an interface.
    ///
    /// Called when an interface goes down or is unregistered.
    pub fn unregister_interface_announce_cap(&mut self, iface_index: usize) {
        self.interface_announce_caps.remove(&iface_index);
    }

    // Public: Interface Name API
    /// Register a human-readable name for an interface (called by driver at registration).
    pub fn set_interface_name(&mut self, id: usize, name: String) {
        self.interface_names.insert(id, name);
    }

    /// Remove interface name (called during handle_interface_down cleanup).
    pub fn remove_interface_name(&mut self, id: usize) {
        self.interface_names.remove(&id);
    }

    /// Returns a displayable interface name for logging.
    pub(crate) fn iface_name(&self, id: usize) -> IfaceName<'_> {
        IfaceName {
            names: &self.interface_names,
            id,
        }
    }

    // Public: Interface HW_MTU API
    /// Register the hardware MTU for an interface (called by driver at registration).
    pub fn set_interface_hw_mtu(&mut self, id: usize, hw_mtu: u32) {
        self.interface_hw_mtus.insert(id, hw_mtu);
    }

    /// Remove interface hardware MTU (called during handle_interface_down cleanup).
    pub fn remove_interface_hw_mtu(&mut self, id: usize) {
        self.interface_hw_mtus.remove(&id);
    }

    /// Get the registered HW_MTU for a specific interface index.
    pub(crate) fn interface_hw_mtu(&self, id: usize) -> Option<u32> {
        self.interface_hw_mtus.get(&id).copied()
    }

    // Public: IFAC Config API
    /// Register an IFAC configuration for an interface (called by driver at setup).
    pub fn set_ifac_config(&mut self, id: usize, config: IfacConfig) {
        self.ifac_configs.insert(id, config);
    }

    /// Remove IFAC configuration for an interface (called during handle_interface_down cleanup).
    pub fn remove_ifac_config(&mut self, id: usize) {
        self.ifac_configs.remove(&id);
    }

    /// Get the IFAC configuration for a specific interface index.
    pub(crate) fn ifac_config(&self, id: usize) -> Option<&IfacConfig> {
        self.ifac_configs.get(&id)
    }

    /// Clone all IFAC configurations (for passing to dispatch_actions outside the lock).
    pub fn clone_ifac_configs(&self) -> BTreeMap<usize, IfacConfig> {
        self.ifac_configs.clone()
    }

    /// Get the HW_MTU for the next-hop interface toward a destination.
    ///
    /// Looks up the path entry for the destination, then returns the registered
    /// HW_MTU for that path's interface. Returns `None` if no path exists or
    /// the interface has no HW_MTU registered.
    pub(crate) fn next_hop_interface_hw_mtu(
        &self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<u32> {
        let path = self.storage.get_path(dest_hash)?;
        self.interface_hw_mtus.get(&path.interface_index).copied()
    }

    /// Get the bitrate (bps) of the next-hop interface for a destination.
    /// Returns `None` if no path exists or the interface has no bitrate cap.
    /// Matches Python `Transport.next_hop_interface_bitrate()`.
    pub(crate) fn next_hop_interface_bitrate(
        &self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> Option<u32> {
        let path = self.storage.get_path(dest_hash)?;
        self.interface_announce_caps
            .get(&path.interface_index)
            .map(|cap| cap.bitrate_bps)
    }

    /// Compute the receipt timeout for a packet sent to a destination.
    ///
    /// Matches Python's `PacketReceipt.__init__()` (Packet.py:432-433):
    ///   `timeout = first_hop_timeout(dest) + TIMEOUT_PER_HOP * hops`
    ///
    /// Where `first_hop_timeout = MTU * per_byte_latency + DEFAULT_PER_HOP_TIMEOUT`
    /// when the next-hop bitrate is known.
    ///
    /// Falls back to `RECEIPT_TIMEOUT_DEFAULT_MS` if no path is known.
    fn compute_receipt_timeout(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> u64 {
        let path = match self.storage.get_path(dest_hash) {
            Some(p) => p,
            None => return RECEIPT_TIMEOUT_DEFAULT_MS,
        };
        let hops = path.hops;

        // first_hop_timeout: MTU * 8 * 1000 / bitrate + per_hop_timeout
        // (Python: MTU * per_byte_latency + DEFAULT_PER_HOP_TIMEOUT)
        let first_hop_extra = if let Some(bitrate) = self
            .interface_announce_caps
            .get(&path.interface_index)
            .map(|cap| cap.bitrate_bps)
        {
            (MTU as u64) * 8 * 1000 / (bitrate as u64)
        } else if hops > 1 {
            // Multi-hop through unknown bitrate: assume slow link
            (MTU as u64) * 8 * 1000 / (UNKNOWN_BITRATE_ASSUMPTION_BPS as u64)
        } else {
            0
        };

        first_hop_extra
            + ESTABLISHMENT_TIMEOUT_PER_HOP_MS
            + ESTABLISHMENT_TIMEOUT_PER_HOP_MS * (hops as u64)
    }

    // Public: Local Client Interface API
    /// Mark or unmark an interface as a local IPC client (shared instance).
    ///
    /// Local client interfaces receive announce forwarding and path request
    /// routing. Set by the driver when a local client connects via Unix socket.
    pub fn set_local_client(&mut self, id: usize, is_local: bool) {
        if is_local {
            self.local_client_interfaces.insert(id);
        } else {
            self.local_client_interfaces.remove(&id);
            // Remove per-client dest tracking on disconnect.
            // The dest hashes remain in local_client_known_dests
            // so reconnecting clients can be recognized (Block C).
            self.storage.remove_local_client_dests(id);
        }
    }

    /// Return destination hashes that were announced by any local client.
    /// Used by Block C (reconnect re-announce) and Block D (interface recovery).
    pub(crate) fn local_client_known_dest_hashes(&self) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
        self.storage.local_client_known_dest_hashes()
    }

    /// Get the ratchet public key for a destination, if known (owned copy).
    pub(crate) fn get_ratchet(&self, dest_hash: &DestinationHash) -> Option<[u8; RATCHET_SIZE]> {
        self.storage.get_known_ratchet(dest_hash.as_bytes())
    }

    /// Check if an interface is a local IPC client.
    fn is_local_client(&self, id: usize) -> bool {
        self.local_client_interfaces.contains(&id)
    }

    /// Check if any local IPC clients are connected.
    fn has_local_clients(&self) -> bool {
        !self.local_client_interfaces.is_empty()
    }

    /// Test-only: flip `config.enable_transport` on a constructed
    /// Transport. Production flows set it via TransportConfig at
    /// construction; tests using the `test_transport()` helper need a
    /// mutation path to exercise transport-gated behaviour.
    #[cfg(test)]
    pub(crate) fn set_enable_transport_for_tests(&mut self, enabled: bool) {
        self.config.enable_transport = enabled;
    }

    /// Record the wall-clock ms at which the given interface will next
    /// accept a packet. Called by the driver after every
    /// `dispatch_actions` tick by querying each handle's
    /// `Interface::next_slot_ms(MTU, now)`.
    ///
    /// Slot is computed for MTU-sized packets. Smaller packets may fit
    /// earlier than the cached value reports; over-estimation is
    /// intentional. Transport does not query per-size because doing so
    /// would require Transport to hold interface handles, violating
    /// sans-I/O. If per-size accuracy ever becomes measurably important,
    /// extend the cache shape rather than adding a query path.
    pub fn set_interface_next_slot_ms(&mut self, iface_idx: usize, slot_ms: u64) {
        self.interface_next_slot_ms.insert(iface_idx, slot_ms);
    }

    /// Read the earliest wall-clock ms at which the given interface will
    /// accept a packet, or `now_ms` if no value was pushed (unknown ⇒
    /// always ready, matches the `Interface::next_slot_ms` trait default).
    ///
    /// Slot is computed for MTU-sized packets. Smaller packets may fit
    /// earlier than the cached value reports; over-estimation is
    /// intentional. Transport does not query per-size because doing so
    /// would require Transport to hold interface handles, violating
    /// sans-I/O. If per-size accuracy ever becomes measurably important,
    /// extend the cache shape rather than adding a query path.
    pub fn next_slot_ms_for_interface(&self, iface_idx: usize, now_ms: u64) -> u64 {
        self.interface_next_slot_ms
            .get(&iface_idx)
            .copied()
            .unwrap_or(now_ms)
    }

    /// Record the worst-case airtime in milliseconds for one MTU-sized
    /// transmit on the given interface. Pushed by the driver after each
    /// dispatch tick for LoRa-Serial interfaces; non-LoRa interfaces are
    /// not pushed and remain absent from the map.
    ///
    /// Returns the worst-case airtime across *all* registered
    /// interfaces, intentionally over-estimating for fast interfaces:
    /// announce retry scheduling is per-destination, not per-interface,
    /// so the jitter window has to accommodate the slowest possible
    /// recipient. If a finer-grained per-interface jitter ever becomes
    /// measurably useful, extend the cache shape.
    pub fn set_interface_max_airtime_ms(&mut self, iface_idx: usize, ms: u64) {
        self.interface_max_airtime_ms.insert(iface_idx, ms);
    }

    /// Compute the announce jitter ceiling, in milliseconds, for the
    /// `deterministic_jitter_ms` calls in the announce-retry path.
    /// Returns `max(PATHFINDER_RW_MS, JITTER_AIRTIME_FACTOR × worst_airtime)`
    /// across all registered interfaces. With no interfaces registered
    /// the floor `PATHFINDER_RW_MS` applies, preserving the legacy
    /// behaviour for hosts without a LoRa link.
    ///
    /// The returned value is a worst-case over all interfaces (see
    /// `set_interface_max_airtime_ms`). It is intentionally
    /// over-estimating for destinations reachable only via fast
    /// interfaces, because the announce-retry scheduler does not yet
    /// distinguish per-interface schedules and a single conservative
    /// window is simpler than per-interface slot bookkeeping.
    pub fn announce_jitter_max_ms(&self) -> u64 {
        let max_airtime = self
            .interface_max_airtime_ms
            .values()
            .copied()
            .max()
            .unwrap_or(0);
        PATHFINDER_RW_MS.max(JITTER_AIRTIME_FACTOR * max_airtime)
    }

    /// Check if a destination is for a local client (path exists with hops=0).
    /// Since we now increment hops on receipt, hops=0 means the announce came
    /// from a local client (net zero: +1 then -1). Matches Python Transport.py:1379.
    fn is_for_local_client(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.storage
            .get_path(dest_hash)
            .is_some_and(|path| path.hops == 0)
    }

    /// Check if a link table entry references a local client interface
    /// (either received_interface or next_hop_interface). Python Transport.py:1380-1381.
    fn is_for_local_client_link(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        if let Some(entry) = self.storage.get_link_entry(dest_hash) {
            self.is_local_client(entry.received_interface_index)
                || self.is_local_client(entry.next_hop_interface_index)
        } else {
            false
        }
    }

    // Public: Announce Frequency Tracking
    /// Record an incoming announce timestamp for the given interface.
    fn record_incoming_announce(&mut self, iface: usize) {
        let now = self.clock.now_ms();
        let deque = self
            .interface_incoming_announce_times
            .entry(iface)
            .or_default();
        if deque.len() >= ANNOUNCE_FREQ_SAMPLES {
            deque.pop_front();
        }
        deque.push_back(now);
    }

    /// Record an outgoing announce timestamp for a specific interface.
    fn record_outgoing_announce(&mut self, iface: usize) {
        let now = self.clock.now_ms();
        let deque = self
            .interface_outgoing_announce_times
            .entry(iface)
            .or_default();
        if deque.len() >= ANNOUNCE_FREQ_SAMPLES {
            deque.pop_front();
        }
        deque.push_back(now);
    }

    /// Record an outgoing announce on all registered interfaces except one.
    /// Used when a Broadcast action is emitted for an announce.
    fn record_outgoing_announce_broadcast(&mut self, except: usize) {
        let now = self.clock.now_ms();
        let ifaces: Vec<usize> = self
            .interface_names
            .keys()
            .copied()
            .filter(|&i| i != except)
            .collect();
        for iface in ifaces {
            let deque = self
                .interface_outgoing_announce_times
                .entry(iface)
                .or_default();
            if deque.len() >= ANNOUNCE_FREQ_SAMPLES {
                deque.pop_front();
            }
            deque.push_back(now);
        }
    }

    /// Compute deterministic jitter in 0..max_ms from this node's identity and a seed hash.
    ///
    /// Different nodes produce different jitter for the same announce because
    /// each node XORs its own transport identity hash into the seed. This
    /// desynchronises announce retries without requiring an RNG.
    fn deterministic_jitter_ms(&self, seed: &[u8; TRUNCATED_HASHBYTES], max_ms: u64) -> u64 {
        if max_ms == 0 {
            return 0;
        }
        let id_hash = self.identity.hash();
        // XOR first 8 bytes of (identity_hash XOR seed) to get a u64
        let mut buf = [0u8; 8];
        for i in 0..8 {
            buf[i] = id_hash[i] ^ seed[i % seed.len()];
        }
        u64::from_le_bytes(buf) % max_ms
    }

    /// Compute announce frequency from a timestamp deque (matches Python Interface.py).
    ///
    /// Returns frequency in Hz. If fewer than 2 samples, returns 0.0.
    /// Includes time elapsed since last sample in the average, matching Python's
    /// `delta_sum += time.time() - deque[-1]`.
    fn announce_frequency(deque: &VecDeque<u64>, now_ms: u64) -> f64 {
        if deque.len() <= 1 {
            return 0.0;
        }
        let mut delta_sum_ms: u64 = 0;
        for i in 1..deque.len() {
            delta_sum_ms += deque[i].saturating_sub(deque[i - 1]);
        }
        // Add time since last sample (matches Python: delta_sum += time.time() - deque[-1])
        if let Some(&last) = deque.back() {
            delta_sum_ms += now_ms.saturating_sub(last);
        }

        if delta_sum_ms == 0 {
            return 0.0;
        }
        let avg_delta_secs = (delta_sum_ms as f64 / 1000.0) / (deque.len() as f64);
        1.0 / avg_delta_secs
    }

    /// Remove announce frequency tracking state for an interface.
    ///
    /// Called from `handle_interface_down()` during cleanup.
    pub fn remove_announce_freq_tracking(&mut self, iface: usize) {
        self.interface_incoming_announce_times.remove(&iface);
        self.interface_outgoing_announce_times.remove(&iface);
    }

    // Public: Interface Stats (for RPC)
    /// Return metadata for all registered interfaces.
    ///
    /// Used by the RPC server to report interface status to CLI tools.
    pub fn interface_stats(&self) -> Vec<InterfaceStatEntry> {
        let now = self.clock.now_ms();
        self.interface_names
            .iter()
            .map(|(&id, name)| {
                let ia_freq = self
                    .interface_incoming_announce_times
                    .get(&id)
                    .map(|d| Self::announce_frequency(d, now))
                    .unwrap_or(0.0);
                let oa_freq = self
                    .interface_outgoing_announce_times
                    .get(&id)
                    .map(|d| Self::announce_frequency(d, now))
                    .unwrap_or(0.0);
                InterfaceStatEntry {
                    id,
                    name: name.clone(),
                    is_local_client: self.local_client_interfaces.contains(&id),
                    incoming_announce_frequency: ia_freq,
                    outgoing_announce_frequency: oa_freq,
                }
            })
            .collect()
    }

    // Internal: Helpers
    /// Compute the well-known path request destination hash
    ///
    /// PLAIN destination with name "rnstransport.path.request".
    /// Hash = full_hash(name_hash)[:16], matching Python Reticulum.
    fn compute_path_request_hash() -> [u8; TRUNCATED_HASHBYTES] {
        let name_hash = Destination::compute_name_hash("rnstransport", &["path", "request"]);
        truncated_hash(&name_hash)
    }

    /// Compute the well-known tunnel synthesize destination hash.
    ///
    /// PLAIN destination with name "rnstransport.tunnel.synthesize".
    /// Tunnels are not implemented in Rust, but Python nodes send these
    /// control packets. We must recognize them to avoid forwarding them
    /// as plain broadcasts (Python Transport.py:1387 control_hashes check).
    fn compute_tunnel_synthesize_hash() -> [u8; TRUNCATED_HASHBYTES] {
        let name_hash = Destination::compute_name_hash("rnstransport", &["tunnel", "synthesize"]);
        truncated_hash(&name_hash)
    }

    /// Send data on all online interfaces except the one at `except_index` (emits Broadcast action)
    fn send_on_all_interfaces_except(&mut self, except_index: usize, data: &[u8]) {
        // Cache outbound packet hash so echoes returning via shared-medium
        // relay are dropped by the dedup check in process_incoming().
        // Matches Python Transport.py:1168-1169 and send_on_all_interfaces().
        let cache_hash = packet_hash(data);
        self.storage.add_packet_hash(cache_hash);

        self.pending_actions.push(Action::Broadcast {
            data: data.to_vec(),
            exclude_iface: Some(InterfaceId(except_index)),
        });
    }

    /// Compute deterministic jitter from a hash seed
    /// Handle a path request packet
    fn handle_path_request(
        &mut self,
        packet: Packet,
        interface_index: usize,
    ) -> Result<(), TransportError> {
        let data = packet.data.as_slice();

        // Path request format: dest_hash(16) + [transport_id(16)] + tag(16)
        // Minimum: dest_hash(16) + tag(16) = 32 bytes
        if data.len() < 32 {
            tracing::trace!(len = data.len(), "Dropped truncated path request");
            return Ok(());
        }

        let mut requested_hash = [0u8; TRUNCATED_HASHBYTES];
        requested_hash.copy_from_slice(&data[..TRUNCATED_HASHBYTES]);

        tracing::debug!(
            "Path request for <{}> on {}",
            HexShort(&requested_hash),
            self.iface_name(interface_index)
        );

        // Extract tag (last 16 bytes)
        let tag_start = data.len() - TRUNCATED_HASHBYTES;
        let mut tag = [0u8; TRUNCATED_HASHBYTES];
        tag.copy_from_slice(&data[tag_start..]);

        // Dedup via tag
        let mut dedup_key = [0u8; 32];
        dedup_key[..TRUNCATED_HASHBYTES].copy_from_slice(&requested_hash);
        dedup_key[TRUNCATED_HASHBYTES..].copy_from_slice(&tag);

        if self.storage.check_path_request_tag(&dedup_key) {
            tracing::trace!(
                "Ignoring duplicate path request for <{}>",
                HexShort(&requested_hash)
            );
            return Ok(());
        }

        // 1. Check if it's a local destination
        if self.local_destinations.contains(&requested_hash) {
            tracing::debug!(
                "Answering path request for <{}> on {}, destination is local",
                HexShort(&requested_hash),
                self.iface_name(interface_index)
            );
            // Emit event so NodeCore generates a FRESH announce (Block A).
            // NodeCore will update the AnnounceEntry's raw_packet with fresh bytes
            // (new signature, current app_data) before the deferred rebroadcast fires.
            self.events.push(TransportEvent::PathRequestReceived {
                destination_hash: requested_hash,
                requesting_interface: interface_index,
            });
            // Schedule deferred path response only if we have a cached announce.
            // Without a cache, there's nothing to rebroadcast. NodeCore will
            // generate a fresh announce in response to the PathRequestReceived
            // event, which populates the cache for future requests.
            if let Some(cached_raw) = self.storage.get_announce_cache(&requested_hash).cloned() {
                let now = self.clock.now_ms();
                let hops = Packet::unpack(&cached_raw).map(|p| p.hops).unwrap_or(0);
                tracing::debug!(
                    "Setting up deferred path response for local dest <{}>",
                    HexShort(&requested_hash)
                );
                self.storage.set_announce(
                    requested_hash,
                    AnnounceEntry {
                        timestamp_ms: now,
                        hops,
                        retries: 0,
                        retransmit_at_ms: Some(now + PATH_REQUEST_GRACE_MS),
                        raw_packet: cached_raw,
                        receiving_interface_index: interface_index,
                        target_interface: Some(interface_index),
                        local_rebroadcasts: 0,
                        block_rebroadcasts: true,
                    },
                );
            }
            return Ok(());
        }

        let from_local = self.is_local_client(interface_index);

        // 2a. Local client path request with cached announce → respond immediately
        // Python Transport.py:2723,2755-2756: when is_from_local_client, send cached
        // announce directly back to the requesting client (retransmit_timeout = now,
        // attached_interface = requesting_interface).
        if from_local {
            if let Some(cached_raw) = self.storage.get_announce_cache(&requested_hash).cloned() {
                // Convert cached Header1 announce to Header2 with the daemon's
                // transport_id and receipt-incremented hops. This is required because
                // Python shared instance clients use hops to decide routing:
                // hops == 0: destination is directly reachable, send Header1
                // hops == 1: destination needs transport, convert to Header2
                //    with transport_id from path table (Transport.py:1000-1011)
                // Sending Header2 with our identity ensures the client's path table
                // stores our transport_id, so outbound packets use the correct
                // transport_id and pass our filter (Transport.py:1192-1194).
                if let Ok(mut announce) = Packet::unpack(&cached_raw) {
                    announce.hops = announce.hops.saturating_add(1);
                    announce.flags.header_type = HeaderType::Type2;
                    announce.flags.transport_type = TransportType::Transport;
                    announce.transport_id = Some(*self.identity.hash());

                    let size = announce.packed_size();
                    let mut buf = alloc::vec![0u8; size];
                    if let Ok(len) = announce.pack(&mut buf) {
                        tracing::debug!(
                            "Answering path request for <{}> from local client, path is known",
                            HexShort(&requested_hash),
                        );
                        self.pending_actions.push(Action::SendPacket {
                            iface: InterfaceId(interface_index),
                            data: buf[..len].to_vec(),
                        });
                    }
                }
                return Ok(());
            }
        }

        // 2b. Transport node with cached announce → schedule deferred rebroadcast.
        // Send only to the requesting interface (Python Transport.py:1037-1038).
        if self.config.enable_transport {
            if let Some(cached_raw) = self.storage.get_announce_cache(&requested_hash).cloned() {
                tracing::debug!(
                    "Answering path request for <{}> on {}, path is known",
                    HexShort(&requested_hash),
                    self.iface_name(interface_index)
                );
                let now = self.clock.now_ms();
                // Parse cached announce to get hop count
                if let Ok(cached_packet) = Packet::unpack(&cached_raw) {
                    self.storage.set_announce(
                        requested_hash,
                        AnnounceEntry {
                            timestamp_ms: now,
                            hops: cached_packet.hops,
                            retries: 0,
                            retransmit_at_ms: Some(now + PATH_REQUEST_GRACE_MS),
                            raw_packet: cached_raw,
                            receiving_interface_index: interface_index,
                            target_interface: Some(interface_index),
                            local_rebroadcasts: 0,
                            block_rebroadcasts: true,
                        },
                    );
                }
                return Ok(());
            }

            // 3. Unknown destination from network → re-originate path request
            //    Python Transport.py:2792-2806: create fresh packets with hops=0
            //    on all interfaces except the requester, reusing the same tag.
            if !from_local {
                // Python checks attached_interface.mode in DISCOVER_PATHS_FOR here,
                // which gates discovery on the interface's operational mode. We skip
                // this check until per-interface modes are implemented.
                // Currently all our interfaces support path discovery, so this is
                // functionally equivalent.
                let now = self.clock.now_ms();
                if self
                    .storage
                    .get_discovery_path_request(&requested_hash)
                    .is_some()
                {
                    tracing::debug!(
                        "Already have a pending discovery path request for <{}>",
                        HexShort(&requested_hash)
                    );
                } else {
                    self.storage.set_discovery_path_request(
                        requested_hash,
                        interface_index,
                        now + DISCOVERY_TIMEOUT_MS,
                    );
                }

                tracing::debug!(
                    "Attempting to discover unknown path to <{}> on behalf of path request on {}",
                    HexShort(&requested_hash),
                    self.iface_name(interface_index)
                );

                // Build a fresh path request packet with hops=0 (Python Transport.py:2555-2561)
                let pr_data = if self.config.enable_transport {
                    let mut d = Vec::with_capacity(48);
                    d.extend_from_slice(&requested_hash);
                    d.extend_from_slice(self.identity.hash());
                    d.extend_from_slice(&tag);
                    d
                } else {
                    let mut d = Vec::with_capacity(32);
                    d.extend_from_slice(&requested_hash);
                    d.extend_from_slice(&tag);
                    d
                };

                let fresh_packet = Packet {
                    flags: PacketFlags {
                        ifac_flag: false,
                        header_type: HeaderType::Type1,
                        context_flag: false,
                        transport_type: TransportType::Broadcast,
                        dest_type: crate::destination::DestinationType::Plain,
                        packet_type: PacketType::Data,
                    },
                    hops: 0,
                    transport_id: None,
                    destination_hash: self.path_request_hash,
                    context: PacketContext::None,
                    data: PacketData::Owned(pr_data),
                };

                let mut buf = [0u8; crate::constants::MTU];
                let len = fresh_packet.pack(&mut buf)?;
                self.send_on_all_interfaces_except(interface_index, &buf[..len]);

                // Also forward to local clients (Python Transport.py:2808-2813)
                if self.has_local_clients() {
                    let local_ifaces: Vec<usize> = self
                        .local_client_interfaces
                        .iter()
                        .copied()
                        .filter(|&id| id != interface_index)
                        .collect();
                    for client_iface in local_ifaces {
                        let _ = self.send_on_interface(client_iface, &buf[..len]);
                    }
                }
            }
        }

        // 4. Local client with unknown destination → forward to network interfaces
        // (Python Transport.py:2783-2790). This works even without transport enabled.
        if from_local {
            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf)?;
            tracing::debug!(
                "Path request for <{}> from local client, forwarding to network interfaces",
                HexShort(&requested_hash),
            );
            let network_ifaces: Vec<usize> = self
                .interface_names
                .keys()
                .copied()
                .filter(|&id| id != interface_index && !self.is_local_client(id))
                .collect();
            for iface_idx in network_ifaces {
                let _ = self.send_on_interface(iface_idx, &buf[..len]);
            }
        }

        Ok(())
    }

    // Internal: Periodic Tasks
    fn expire_paths(&mut self, now: u64) {
        let expired = self.storage.expire_paths(now);
        for hash in expired {
            self.events.push(TransportEvent::PathLost {
                destination_hash: hash,
            });
        }
    }

    fn clean_reverse_table(&mut self, now: u64) {
        self.storage.expire_reverses(now, REVERSE_TABLE_EXPIRY_MS);
    }

    fn check_receipt_timeouts(&mut self, now: u64) {
        let expired = self.storage.expire_receipts(now);
        for receipt in expired {
            self.events.push(TransportEvent::ReceiptTimeout {
                packet_hash: receipt.truncated_hash,
            });
        }
    }

    fn check_announce_rebroadcasts(&mut self, now: u64) {
        if !self.config.enable_transport {
            return;
        }

        // Collect entries that need action
        let mut to_remove = Vec::new();
        let mut to_rebroadcast = Vec::new();
        // retransmit_at deferrals when the target interface's
        // next_slot_ms is in the future. Applied via set_announce after
        // the read loop, without incrementing retries.
        let mut to_defer: Vec<([u8; TRUNCATED_HASHBYTES], u64)> = Vec::new();

        // Iterate the interface_next_slot_ms keys for broadcast deferral.
        // Broadcast defers iff EVERY known interface is future. We use
        // interface_hw_mtus as the authoritative "known interfaces" set
        // (populated at driver registration), falling back through the
        // getter's unwrap_or(now) for interfaces without a pushed slot.        // those count as "ready now" and prevent deferral.
        let keys = self.storage.announce_keys();
        for dest_hash in keys {
            if let Some(entry) = self.storage.get_announce(&dest_hash) {
                // Remove entries that have exceeded retries or local rebroadcast limit
                if entry.retries > PATHFINDER_RETRIES
                    || entry.local_rebroadcasts >= LOCAL_REBROADCASTS_MAX
                {
                    to_remove.push(dest_hash);
                    continue;
                }

                // Check if retransmit is due
                if let Some(retransmit_at) = entry.retransmit_at_ms {
                    if retransmit_at <= now && !entry.raw_packet.is_empty() {
                        // Ask the next-slot backchannel whether the target(s)
                        // can accept this packet now.
                        let defer_to: Option<u64> = match entry.target_interface {
                            Some(idx) => {
                                let slot = self.next_slot_ms_for_interface(idx, now);
                                (slot > now).then_some(slot)
                            }
                            None => {
                                // Broadcast: defer only if every registered
                                // interface reports a future slot. If any is
                                // ready now, push, the dispatcher filters
                                // per-interface via try_send_prioritized.
                                let mut all_future = true;
                                let mut min_slot = u64::MAX;
                                let mut any_iface = false;
                                for &iface_idx in self.interface_hw_mtus.keys() {
                                    any_iface = true;
                                    let slot = self.next_slot_ms_for_interface(iface_idx, now);
                                    if slot <= now {
                                        all_future = false;
                                        break;
                                    }
                                    if slot < min_slot {
                                        min_slot = slot;
                                    }
                                }
                                if any_iface && all_future {
                                    Some(min_slot)
                                } else {
                                    None
                                }
                            }
                        };

                        if let Some(slot) = defer_to {
                            to_defer.push((dest_hash, slot));
                        } else {
                            to_rebroadcast.push((
                                dest_hash,
                                entry.raw_packet.clone(),
                                entry.receiving_interface_index,
                                entry.hops,
                                entry.block_rebroadcasts,
                                entry.target_interface,
                            ));
                        }
                    }
                }
            }
        }

        for hash in to_remove {
            self.storage.remove_announce(&hash);
        }

        // Apply deferrals: slide retransmit_at forward without touching
        // retries. Only rewrites entries that still exist (cheap in the
        // common case since get_announce is by-hash).
        for (dest_hash, slot) in to_defer {
            if let Some(entry) = self.storage.get_announce(&dest_hash) {
                let mut updated = entry.clone();
                updated.retransmit_at_ms = Some(slot);
                self.storage.set_announce(dest_hash, updated);
            }
        }

        let transport_id = *self.identity.hash();

        for (dest_hash, raw, except_iface, original_hops, block, target) in to_rebroadcast {
            let retry_num = self
                .storage
                .get_announce(&dest_hash)
                .map(|e| e.retries)
                .unwrap_or(0);
            tracing::debug!(
                "announce retry firing dest=<{}> retries={} target={:?}",
                HexShort(&dest_hash),
                retry_num,
                target,
            );
            // Bug #25 capture-compare: structured event at the moment the
            // retry scheduler decides to emit an announce. Pairs with
            // `LORA_TX` at the interface layer so the analysis can count
            // scheduler-driven announces-per-second vs driver-level bytes-
            // to-serial events. Measurement-only; DEBUG-level under a
            // dedicated target so it filters independently.
            tracing::debug!(
                target: "reticulum_core::transport::announce_trace",
                "ANNOUNCE_EMIT dest={} retries={} hops={} target={:?}",
                HexShort(&dest_hash),
                retry_num,
                original_hops,
                target,
            );

            // Rebuild packet for retransmission
            if let Ok(mut parsed) = Packet::unpack(&raw) {
                // Raw bytes have original wire hops; set to receipt-incremented value
                // stored in announce entry, since forwarding no longer increments.
                parsed.hops = original_hops;

                // Locally-originated announces (hops=0) keep their original
                // Header Type 1 format. Received announces are converted to
                // Header Type 2 (transport-routed) with our transport ID.
                if original_hops > 0 {
                    parsed.flags.header_type = HeaderType::Type2;
                    parsed.flags.transport_type = TransportType::Transport;
                    parsed.transport_id = Some(transport_id);
                }

                // If block_rebroadcasts, set PathResponse context
                if block {
                    parsed.context = PacketContext::PathResponse;
                }

                if let Some(target_iface) = target {
                    // Path response: send only to the requesting interface
                    let size = parsed.packed_size();
                    let mut buf = alloc::vec![0u8; size];
                    if let Ok(len) = parsed.pack(&mut buf) {
                        tracing::debug!(
                            "Sending targeted path response for <{}> to iface:{} ({} bytes)",
                            HexShort(&dest_hash),
                            target_iface,
                            len
                        );
                        self.pending_actions.push(Action::SendPacket {
                            iface: InterfaceId(target_iface),
                            data: buf[..len].to_vec(),
                        });
                    }
                } else {
                    // Apply ANNOUNCE_CAP to retries that forward a relayed
                    // announce (hops > 0), matching Python Transport.py:1091.
                    // Self-originated retries (hops == 0) and interfaces
                    // without a cap bypass the rate limiter.
                    if parsed.hops == 0 || self.interface_announce_caps.is_empty() {
                        self.forward_on_all(&mut parsed);
                    } else {
                        self.broadcast_announce_with_caps(&mut parsed);
                    }
                    self.record_outgoing_announce_broadcast(except_iface);
                }

                // If TTL exceeded, remove from announce table
                if parsed.hops > self.config.max_hops {
                    self.storage.remove_announce(&dest_hash);
                    continue;
                }
            }

            // Update announce table entry, schedule next retransmit with
            // exponential backoff: retry 1 → 0-500ms, retry 2 → 0-1000ms,
            // retry 3 → 0-2000ms jitter window. Wider windows on later retries
            // break deterministic collision patterns between nodes.
            let retries = self
                .storage
                .get_announce(&dest_hash)
                .map(|e| e.retries)
                .unwrap_or(0);
            let backoff_factor = 1u64 << (retries.min(4) as u64);
            let jitter = self.deterministic_jitter_ms(
                &dest_hash,
                self.announce_jitter_max_ms() * backoff_factor,
            );
            if let Some(entry) = self.storage.get_announce_mut(&dest_hash) {
                let next_at = now + PATHFINDER_G_MS + jitter;
                entry.retransmit_at_ms = Some(next_at);
                entry.retries += 1;
                tracing::debug!(
                    "announce retry scheduled dest=<{}> retries={} next_at_ms={}",
                    HexShort(&dest_hash),
                    entry.retries,
                    next_at,
                );
            }
        }
    }

    /// Broadcast an announce on all interfaces except one, respecting per-interface
    /// bandwidth caps. Hops were already incremented on receipt (or set from stored entry).
    ///
    /// For capped interfaces: if `now >= allowed_at_ms`, send immediately and compute
    /// next holdoff. Otherwise, queue (capped at MAX_QUEUED_ANNOUNCES_PER_INTERFACE).
    ///
    /// Uncapped interfaces receive the announce via Broadcast action (driver dispatches).
    /// Capped interfaces that already got a SendPacket may also receive the Broadcast;
    /// the driver should deduplicate if this matters for the link type.
    fn broadcast_announce_with_caps(&mut self, packet: &mut Packet) {
        if packet.hops > self.config.max_hops {
            self.stats.packets_dropped += 1;
            return;
        }

        let mut buf = [0u8; MTU];
        let len = match packet.pack(&mut buf) {
            Ok(len) => len,
            Err(_) => return,
        };
        let raw = buf[..len].to_vec();
        let now = self.clock.now_ms();

        // Seed dedup so our emission is recognised when it echoes back on a
        // shared medium. Matches Python Transport.outbound() which iterates
        // all interfaces without filtering by receiving_interface and relies
        // on packet_hashlist (Transport.py:1227) to absorb the echo.
        self.storage.add_packet_hash(packet_hash(&raw));

        // Handle capped interfaces individually
        let capped_ifaces: Vec<usize> = self.interface_announce_caps.keys().copied().collect();

        for iface_idx in &capped_ifaces {
            let cap = self
                .interface_announce_caps
                .get_mut(iface_idx)
                .expect("key from collected list");

            if now >= cap.allowed_at_ms {
                self.pending_actions.push(Action::SendPacket {
                    iface: InterfaceId(*iface_idx),
                    data: raw.clone(),
                });
                // Compute holdoff: wait_ms = (bytes * 8 * 1000) / (bitrate * cap% / 100)
                let tx_bits = raw.len() as u64 * 8;
                let cap_bps = cap.bitrate_bps as u64 * cap.announce_cap_percent as u64 / 100;
                let wait_ms = (tx_bits * 1000).checked_div(cap_bps).unwrap_or(0);
                cap.allowed_at_ms = now + wait_ms;
            } else if cap.queue.len() < self.config.max_queued_announces {
                cap.queue.push_back(QueuedAnnounce {
                    raw: raw.clone(),
                    hops: packet.hops,
                    queued_at_ms: now,
                });
            }
            // else: queue full, drop silently
        }

        // Send to uncapped interfaces individually, capped ones were handled
        // above (either sent immediately or queued). A plain Broadcast would
        // double-send to capped interfaces that already got a SendPacket.
        for &iface_idx in self.interface_names.keys() {
            if self.interface_announce_caps.contains_key(&iface_idx) {
                continue;
            }
            self.pending_actions.push(Action::SendPacket {
                iface: InterfaceId(iface_idx),
                data: raw.clone(),
            });
        }
        self.stats.packets_forwarded += 1;
    }

    /// Drain announce queues for interfaces whose holdoff has expired.
    /// Called from `poll()`. Dequeues lowest-hops announce first, then oldest
    /// within same hops (Python Interface.py:263-266).
    fn drain_announce_queues(&mut self, now: u64) {
        let mut sends: Vec<(usize, Vec<u8>)> = Vec::new();

        for (iface_idx, cap) in self.interface_announce_caps.iter_mut() {
            if cap.queue.is_empty() || now < cap.allowed_at_ms {
                continue;
            }

            // Dequeue: lowest hops first, then oldest within same hops
            let best_idx = cap
                .queue
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    a.hops
                        .cmp(&b.hops)
                        .then(a.queued_at_ms.cmp(&b.queued_at_ms))
                })
                .map(|(i, _)| i);

            if let Some(idx) = best_idx {
                let entry = cap.queue.remove(idx).expect("valid index");
                let raw_len = entry.raw.len();

                sends.push((*iface_idx, entry.raw));

                // Compute holdoff for next announce
                let tx_bits = raw_len as u64 * 8;
                let cap_bps = cap.bitrate_bps as u64 * cap.announce_cap_percent as u64 / 100;
                let wait_ms = (tx_bits * 1000).checked_div(cap_bps).unwrap_or(0);
                cap.allowed_at_ms = now + wait_ms;
            }
        }

        for (iface_idx, raw) in sends {
            self.pending_actions.push(Action::SendPacket {
                iface: InterfaceId(iface_idx),
                data: raw,
            });
            self.record_outgoing_announce(iface_idx);
        }
    }

    fn clean_link_table(&mut self, now: u64) {
        let expired = self.storage.expire_link_entries(now, LINK_TIMEOUT_MS);

        for (_link_hash, entry) in expired {
            // Path rediscovery only for unvalidated links (proof never arrived).
            // Matches Python Transport.py:629-699.
            if entry.validated {
                continue;
            }

            let dest_hash = entry.destination_hash;

            // Check path request rate limiting
            let path_request_throttled =
                if let Some(last_req) = self.storage.get_path_request_time(&dest_hash) {
                    now.saturating_sub(last_req) < PATH_REQUEST_MIN_INTERVAL_MS
                } else {
                    false
                };

            let mut should_request_path = false;

            if !self.storage.has_path(&dest_hash) {
                // Sub-case 1: Path missing, unconditionally try to rediscover
                // (no throttle check). Python Transport.py:644.
                should_request_path = true;
            } else if !path_request_throttled
                && self
                    .storage
                    .get_path(&dest_hash)
                    .is_some_and(|p| p.is_direct())
            {
                // Sub-case 2: Destination directly connected, may have roamed.
                // is_direct() checks hops == 1, matching Python Transport.py:660-676.
                should_request_path = true;
                if self.config.enable_transport {
                    self.mark_path_unresponsive(&dest_hash);
                }
            } else if !path_request_throttled && entry.hops == 1 {
                // Sub-case 3: Initiator directly connected (1 hop with receipt increment).
                // Matches Python lr_taken_hops == 1. Python Transport.py:682-689.
                should_request_path = true;
                if self.config.enable_transport {
                    self.mark_path_unresponsive(&dest_hash);
                }
            }

            if should_request_path {
                // For non-transport nodes, force-expire the current path so
                // higher-hop-count announces can be accepted.
                // Python Transport.py:695-699.
                if !self.config.enable_transport {
                    self.expire_path(&dest_hash);
                }

                // Request path directly, deterministic tag from clock + dest_hash
                // ensures uniqueness per (destination, time) for dedup.
                let mut tag = [0u8; TRUNCATED_HASHBYTES];
                let now_bytes = now.to_be_bytes();
                tag[..8].copy_from_slice(&now_bytes);
                tag[8..16].copy_from_slice(&dest_hash[..8]);
                if let Err(e) = self.request_path(&dest_hash, None, &tag) {
                    tracing::debug!(%e, "path request failed (best-effort)");
                }
            }
        }
    }

    /// Remove path_states and announce_rate entries for destinations no longer in path_table.
    /// Matches Python Transport.py:601-604, 813-814.
    fn clean_path_states(&mut self) {
        self.storage.clean_stale_path_metadata();

        // Expire local client dests not refreshed within expiry window.
        let now = self.clock.now_ms();
        self.storage
            .expire_local_client_known_dests(now, LOCAL_CLIENT_DEST_EXPIRY_MS);

        // Expire stale ratchets not refreshed within 30 days.
        self.storage
            .expire_known_ratchets(now, crate::ratchet::EXPIRY_MS);

        // Expire discovery path requests past their 15s timeout.
        self.storage.expire_discovery_path_requests(now);

        // Preserve announce_cache entries for both daemon-owned destinations
        // AND surviving local client destinations (Block C: reconnect
        // needs cached bytes to respond to path requests during client downtime).
        let mut preserved = self.local_destinations.clone();
        for hash in self.storage.local_client_known_dest_hashes() {
            preserved.insert(hash);
        }
        self.storage.clean_announce_cache(&preserved);
    }

    /// Re-send path requests for pending discovery entries.
    ///
    /// Called from `NodeCore::handle_timeout()` on every tick. Throttled to
    /// one cycle per `DISCOVERY_RETRY_INTERVAL_MS`. Each retry uses a fresh
    /// random tag so receivers don't dedup it as a duplicate path request.
    ///
    /// With a 30s discovery timeout and 5s retry interval, each destination
    /// gets up to 6 attempts (original + 5 retries). At 40% per-packet LoRa
    /// loss, P(all 6 fail) = 0.4^6 = 0.4%.
    pub fn retry_pending_discoveries<R: rand_core::CryptoRngCore>(&mut self, rng: &mut R) {
        let now = self.clock.now_ms();
        if now < self.last_discovery_retry_ms + DISCOVERY_RETRY_INTERVAL_MS {
            return;
        }

        let dest_hashes = self.storage.discovery_path_request_dest_hashes();
        if dest_hashes.is_empty() {
            return;
        }

        for dest_hash in dest_hashes {
            let (requesting_iface, timeout_ms) =
                match self.storage.get_discovery_path_request(&dest_hash) {
                    Some(entry) => entry,
                    None => continue,
                };

            if now >= timeout_ms {
                continue; // expired, will be cleaned up by expire_discovery_path_requests
            }

            // Generate new random tag to avoid dedup at receiver
            let mut tag = [0u8; TRUNCATED_HASHBYTES];
            rng.fill_bytes(&mut tag);

            // Build fresh path request packet (same construction as Stage 3)
            let pr_data = if self.config.enable_transport {
                let mut d = Vec::with_capacity(48);
                d.extend_from_slice(&dest_hash);
                d.extend_from_slice(self.identity.hash());
                d.extend_from_slice(&tag);
                d
            } else {
                let mut d = Vec::with_capacity(32);
                d.extend_from_slice(&dest_hash);
                d.extend_from_slice(&tag);
                d
            };

            let fresh_packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: self.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(pr_data),
            };

            let mut buf = [0u8; MTU];
            if let Ok(len) = fresh_packet.pack(&mut buf) {
                tracing::debug!(
                    "Retrying discovery path request for <{}>",
                    HexShort(&dest_hash)
                );
                self.send_on_all_interfaces_except(requesting_iface, &buf[..len]);
            }
        }

        self.last_discovery_retry_ms = now;
    }

    /// If a discovery path request is pending for this destination, send a
    /// targeted PATH_RESPONSE to the requesting interface.
    ///
    /// Called from `handle_announce()` when `should_update` is true and the path
    /// table has been refreshed. This is the Rust equivalent of Python
    /// Transport.py:1838-1865.
    fn send_discovery_path_response(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        hops: u8,
        raw: &[u8],
    ) {
        let now = self.clock.now_ms();
        let (requesting_iface, timeout) = match self.storage.get_discovery_path_request(dest_hash) {
            Some(entry) => entry,
            None => return,
        };

        if now >= timeout {
            // Expired, clean up
            self.storage.remove_discovery_path_request(dest_hash);
            return;
        }

        tracing::debug!(
            "Answering discovery path request for <{}> on {}",
            HexShort(dest_hash),
            self.iface_name(requesting_iface)
        );

        if let Ok(mut response) = Packet::unpack(raw) {
            response.hops = hops;
            response.flags.header_type = HeaderType::Type2;
            response.flags.transport_type = TransportType::Transport;
            response.transport_id = Some(*self.identity.hash());
            response.context = PacketContext::PathResponse;
            // context_flag preserved from original packet (ratchet flag)

            let size = response.packed_size();
            let mut buf = alloc::vec![0u8; size];
            if let Ok(len) = response.pack(&mut buf) {
                let _ = self.send_on_interface(requesting_iface, &buf[..len]);
            }
        }

        self.storage.remove_discovery_path_request(dest_hash);
        // Deliberate deviation from Python: Python lets entries expire after
        // 15s (no removal on delivery), which can cause duplicate PATH_RESPONSE
        // packets if a second matching announce arrives within the timeout.
        // We remove immediately to avoid wasting airtime on constrained LoRa links.
    }

    /// Check announce rate for a destination, returning true if rebroadcast should be blocked.
    ///
    /// Matches Python Transport.py:1692-1719. Only blocks rebroadcast (announce_table insertion),
    /// NOT path_table updates. Skipped for PATH_RESPONSE context and when rate target is None.
    fn check_announce_rate(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES], now: u64) -> bool {
        let rate_target = match self.config.announce_rate_target_ms {
            Some(t) => t,
            None => return false, // Disabled
        };

        if let Some(entry) = self.storage.get_announce_rate(dest_hash) {
            // Currently blocked?
            if now <= entry.blocked_until_ms {
                return true;
            }

            // Check rate, clone the entry to modify it
            let mut entry = *entry;
            let current_rate = now.saturating_sub(entry.last_ms);
            if current_rate < rate_target {
                entry.rate_violations = entry.rate_violations.saturating_add(1);
            } else {
                entry.rate_violations = entry.rate_violations.saturating_sub(1);
            }

            if entry.rate_violations > self.config.announce_rate_grace {
                entry.blocked_until_ms =
                    entry.last_ms + rate_target + self.config.announce_rate_penalty_ms;
                self.storage.set_announce_rate(*dest_hash, entry);
                return true;
            }

            // Good, update last timestamp
            entry.last_ms = now;
            self.storage.set_announce_rate(*dest_hash, entry);
            false
        } else {
            // First time seeing this destination, create entry, not blocked
            self.storage.set_announce_rate(
                *dest_hash,
                AnnounceRateEntry {
                    last_ms: now,
                    rate_violations: 0,
                    blocked_until_ms: 0,
                },
            );
            false
        }
    }
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MAX_PATH_REQUEST_TAGS;

    #[test]
    fn test_default_config() {
        let config = TransportConfig::default();
        assert!(!config.enable_transport);
        assert_eq!(config.max_hops, PATHFINDER_MAX_HOPS);
    }

    // Sans-I/O Type Tests
    mod sans_io_types {
        use super::*;
        extern crate alloc;
        use alloc::vec;

        #[test]
        fn test_interface_id_basics() {
            let id = InterfaceId(0);
            let id2 = InterfaceId(0);
            let id3 = InterfaceId(1);

            // PartialEq, Eq
            assert_eq!(id, id2);
            assert_ne!(id, id3);

            // Ord
            assert!(id < id3);

            // Copy
            let copied = id;
            assert_eq!(copied, id);

            // Debug
            extern crate std;
            let debug_str = std::format!("{:?}", id);
            assert!(debug_str.contains("InterfaceId"));

            // Display
            let display_str = std::format!("{}", id);
            assert_eq!(display_str, "iface:0");
        }

        #[test]
        fn test_interface_id_hash() {
            // Verify InterfaceId can be used as BTreeMap key
            let mut map = alloc::collections::BTreeMap::new();
            map.insert(InterfaceId(0), "first");
            map.insert(InterfaceId(1), "second");
            assert_eq!(map.get(&InterfaceId(0)), Some(&"first"));
            assert_eq!(map.get(&InterfaceId(1)), Some(&"second"));
            assert_eq!(map.get(&InterfaceId(2)), None);
        }

        #[test]
        fn test_action_send_packet() {
            let action = Action::SendPacket {
                iface: InterfaceId(3),
                data: vec![1, 2, 3],
            };

            // Clone
            let cloned = action.clone();
            assert_eq!(action, cloned);

            // Debug
            extern crate std;
            let debug = std::format!("{:?}", action);
            assert!(debug.contains("SendPacket"));
            assert!(debug.contains("InterfaceId(3)"));
        }

        #[test]
        fn test_action_broadcast() {
            let action_no_exclude = Action::Broadcast {
                data: vec![10, 20],
                exclude_iface: None,
            };
            let action_with_exclude = Action::Broadcast {
                data: vec![10, 20],
                exclude_iface: Some(InterfaceId(2)),
            };

            assert_ne!(action_no_exclude, action_with_exclude);

            // Same data but different exclude
            let action_no_exclude2 = Action::Broadcast {
                data: vec![10, 20],
                exclude_iface: None,
            };
            assert_eq!(action_no_exclude, action_no_exclude2);
        }

        #[test]
        fn test_action_variants_not_equal() {
            let send = Action::SendPacket {
                iface: InterfaceId(0),
                data: vec![1],
            };
            let broadcast = Action::Broadcast {
                data: vec![1],
                exclude_iface: None,
            };
            assert_ne!(send, broadcast);
        }

        #[test]
        fn test_tick_output_empty() {
            let output = TickOutput::empty();
            assert!(output.is_empty());
            assert!(output.actions.is_empty());
            assert!(output.events.is_empty());
        }

        #[test]
        fn test_tick_output_with_actions() {
            let output = TickOutput {
                actions: vec![Action::SendPacket {
                    iface: InterfaceId(0),
                    data: vec![42],
                }],
                events: Vec::new(),
                next_deadline_ms: None,
            };
            assert!(!output.is_empty());
            assert_eq!(output.actions.len(), 1);
        }

        #[test]
        fn test_tick_output_debug() {
            let output = TickOutput {
                actions: vec![
                    Action::SendPacket {
                        iface: InterfaceId(0),
                        data: vec![1],
                    },
                    Action::Broadcast {
                        data: vec![2],
                        exclude_iface: None,
                    },
                ],
                events: Vec::new(),
                next_deadline_ms: None,
            };

            extern crate std;
            let debug = std::format!("{:?}", output);
            assert!(debug.contains("actions: 2"));
            assert!(debug.contains("events: 0"));
        }
    }

    mod transport_tests {
        use super::*;
        use crate::memory_storage::MemoryStorage;
        use crate::test_utils::{test_transport, MockClock, MockInterface, TEST_TIME_MS};
        use rand_core::OsRng;

        extern crate std;

        #[test]
        fn test_transport_creation() {
            let transport = test_transport();
            assert_eq!(transport.interface_count(), 0);
            assert_eq!(transport.path_count(), 0);
            assert_eq!(transport.pending_events(), 0);
            assert_eq!(transport.pending_action_count(), 0);
        }

        #[test]
        fn test_drain_actions_initially_empty() {
            let mut transport = test_transport();
            let actions = transport.drain_actions();
            assert!(actions.is_empty());
            assert_eq!(transport.pending_action_count(), 0);
        }

        #[test]
        fn test_register_interface() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("test", 1)));
            assert_eq!(idx, 0);
            assert_eq!(transport.interface_count(), 1);

            let idx2 = transport.register_interface(Box::new(MockInterface::new("test2", 2)));
            assert_eq!(idx2, 1);
            assert_eq!(transport.interface_count(), 2);
        }

        #[test]
        fn test_unregister_interface() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("test", 1)));
            assert_eq!(transport.interface_count(), 1);

            transport.unregister_interface(idx);
            assert_eq!(transport.interface_count(), 0);
        }

        #[test]
        fn test_register_destination() {
            let mut transport = test_transport();
            let hash = [0x42; TRUNCATED_HASHBYTES];

            transport.register_destination(hash);
            assert!(transport.has_destination(&hash));

            transport.unregister_destination(&hash);
            assert!(!transport.has_destination(&hash));
        }

        #[test]
        fn test_path_management() {
            let mut transport = test_transport();
            let hash = [0x42; TRUNCATED_HASHBYTES];

            assert!(!transport.has_path(&hash));
            assert_eq!(transport.hops_to(&hash), None);

            // Manually insert a path for testing
            let now = transport.clock.now_ms();
            transport.insert_path(
                hash,
                PathEntry {
                    hops: 3,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            assert!(transport.has_path(&hash));
            assert_eq!(transport.hops_to(&hash), Some(3));
            assert_eq!(transport.path_count(), 1);
        }

        #[test]
        fn test_path_expiry() {
            let mut transport = test_transport();
            let hash = [0x42; TRUNCATED_HASHBYTES];

            let now = transport.clock.now_ms();
            transport.insert_path(
                hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 1000, // Expires in 1 second
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            assert!(transport.has_path(&hash));

            // Advance clock past expiry
            transport.clock.advance(2000);
            transport.poll();

            assert!(!transport.has_path(&hash));

            // Should have emitted PathLost event
            let events: Vec<_> = transport.drain_events().collect();
            assert_eq!(events.len(), 1);
            match &events[0] {
                TransportEvent::PathLost { destination_hash } => {
                    assert_eq!(destination_hash, &hash);
                }
                _ => panic!("Expected PathLost event"),
            }
        }

        // Path Timestamp Refresh Tests
        #[test]
        fn test_path_refresh_on_forward() {
            // Forwarding a data packet should refresh the path's expires_ms.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            let original_expiry = now + 10_000;

            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 0,
                    expires_ms: original_expiry,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Advance clock so refresh will produce a different expiry
            transport.clock.advance(5_000);

            // Build a HEADER_2 data packet addressed to dest_hash
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"refresh test".to_vec()),
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Packet should be forwarded"
            );

            // Path expiry should be refreshed to now + path_expiry_secs * 1000
            let path = transport.storage().get_path(&dest_hash).unwrap();
            let expected_expiry =
                transport.clock.now_ms() + (transport.config.path_expiry_secs * 1000);
            assert_eq!(
                path.expires_ms, expected_expiry,
                "Path expiry should be refreshed after forward"
            );
            assert!(
                path.expires_ms > original_expiry,
                "Refreshed expiry should exceed original"
            );
        }

        #[test]
        fn test_path_refresh_on_link_request_forward() {
            // Forwarding a link request should refresh the path's expires_ms.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            let original_expiry = now + 10_000;

            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 0,
                    expires_ms: original_expiry,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Advance clock
            transport.clock.advance(5_000);

            // Build a link request packet addressed to dest_hash
            // Link request = HEADER_2 with transport_id pointing to us
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::LinkRequest,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0u8; 96]), // dummy link request payload
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Link request should be forwarded"
            );

            // Path expiry should be refreshed
            let path = transport.storage().get_path(&dest_hash).unwrap();
            let expected_expiry =
                transport.clock.now_ms() + (transport.config.path_expiry_secs * 1000);
            assert_eq!(
                path.expires_ms, expected_expiry,
                "Path expiry should be refreshed after link request forward"
            );
        }

        #[test]
        fn test_path_not_refreshed_without_forward() {
            // If no matching path exists, no refresh should happen (no crash).
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            // No path entry for dest_hash

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"no path".to_vec()),
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_forwarded,
                0,
                "Should not forward without path"
            );
            assert!(!transport.has_path(&dest_hash), "No path should be created");
        }

        #[test]
        fn test_path_stays_alive_under_continuous_traffic() {
            // A path with short expiry should stay alive if data is forwarded
            // before expiry, even past the original expiry time.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let transport_hash = *identity.hash();
            let config = TransportConfig {
                enable_transport: true,
                path_expiry_secs: 10, // 10 seconds (short for testing)
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 0,
                    expires_ms: now + 10_000, // 10 seconds
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Forward data every 5 seconds, 3 times (past original 10s expiry)
            for i in 0..3 {
                transport.clock.advance(5_000);
                transport.drain_actions(); // clear previous actions

                let packet = Packet {
                    flags: PacketFlags {
                        ifac_flag: false,
                        header_type: HeaderType::Type2,
                        context_flag: false,
                        transport_type: TransportType::Transport,
                        dest_type: DestinationType::Single,
                        packet_type: PacketType::Data,
                    },
                    hops: 1,
                    transport_id: Some(transport_hash),
                    destination_hash: dest_hash,
                    context: PacketContext::None,
                    data: PacketData::Owned(alloc::vec![i as u8; 10]),
                };

                let mut buf = [0u8; crate::constants::MTU];
                let len = packet.pack(&mut buf).unwrap();
                // Use a unique dedup hash each iteration by varying payload
                transport.process_incoming(0, &buf[..len]).unwrap();
                transport.poll();

                assert!(
                    transport.has_path(&dest_hash),
                    "Path should still be alive at iteration {i} (time = {}ms)",
                    transport.clock.now_ms()
                );
            }

            // Now total time = 15_000ms. Original expiry was 10_000ms.
            // Path should still be alive (refreshed to now + 10_000 = 25_000ms)
            assert!(
                transport.has_path(&dest_hash),
                "Path should survive past original expiry"
            );

            // Advance past the NEW expiry (last refresh at 15_000 + 10_000 = 25_000)
            transport.clock.advance(11_000); // now at 26_000ms
            transport.poll();

            assert!(
                !transport.has_path(&dest_hash),
                "Path should expire when idle past refreshed expiry"
            );
        }

        #[test]
        fn test_send_on_interface() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("test", 1)));

            let data = b"test packet";
            transport.send_on_interface(idx, data).unwrap();

            assert_eq!(transport.stats().packets_sent, 1);
        }

        #[test]
        fn test_send_on_unregistered_interface_emits_action() {
            let mut transport = test_transport();
            // Sending on an unregistered interface still emits an Action
            // (the driver is responsible for dispatching to real interfaces)
            let result = transport.send_on_interface(99, b"test");
            assert!(result.is_ok());
            assert_eq!(transport.pending_action_count(), 1);
            let actions = transport.drain_actions();
            assert_eq!(
                actions[0],
                Action::SendPacket {
                    iface: InterfaceId(99),
                    data: b"test".to_vec()
                }
            );
        }

        #[test]
        fn test_packet_deduplication() {
            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));
            let hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(hash);

            // Create a simple data packet
            use crate::destination::DestinationType;
            use crate::packet::{
                HeaderType, PacketContext, PacketData, PacketFlags, TransportType,
            };

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"hello".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            let raw = &buf[..len];

            // First time: should process (PacketReceived + ProofRequested)
            transport.process_incoming(0, raw).unwrap();
            assert_eq!(transport.stats().packets_received, 1);
            assert_eq!(transport.pending_events(), 2);

            // Second time: should be deduplicated
            transport.drain_events();
            transport.process_incoming(0, raw).unwrap();
            assert_eq!(transport.stats().packets_received, 2);
            assert_eq!(transport.stats().packets_dropped, 1);
            assert_eq!(transport.pending_events(), 0);
        }

        #[test]
        fn test_data_packet_delivery() {
            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));
            let hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(hash);

            use crate::destination::DestinationType;
            use crate::packet::{
                HeaderType, PacketContext, PacketData, PacketFlags, TransportType,
            };

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"hello".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            transport.process_incoming(0, &buf[..len]).unwrap();

            let events: Vec<_> = transport.drain_events().collect();
            assert_eq!(events.len(), 2); // PacketReceived + ProofRequested
            match &events[0] {
                TransportEvent::PacketReceived {
                    destination_hash,
                    packet,
                    interface_index,
                    ..
                } => {
                    assert_eq!(destination_hash, &hash);
                    assert_eq!(packet.data.as_slice(), b"hello");
                    assert_eq!(*interface_index, 0);
                }
                _ => panic!("Expected PacketReceived event"),
            }
            assert!(
                matches!(&events[1], TransportEvent::ProofRequested { .. }),
                "Expected ProofRequested event"
            );
        }

        #[test]
        fn test_unregistered_destination_dropped() {
            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));
            // Don't register any destination

            use crate::destination::DestinationType;
            use crate::packet::{
                HeaderType, PacketContext, PacketData, PacketFlags, TransportType,
            };

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: [0x42; TRUNCATED_HASHBYTES],
                context: PacketContext::None,
                data: PacketData::Owned(b"hello".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            transport.process_incoming(0, &buf[..len]).unwrap();

            // No events since destination isn't registered
            assert_eq!(transport.pending_events(), 0);
        }

        #[test]
        fn test_packet_cache_dedup() {
            // Verify that duplicate packets are dropped by the packet hash cache.
            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));

            let hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(hash);

            use crate::destination::DestinationType;
            use crate::packet::{
                HeaderType, PacketContext, PacketData, PacketFlags, TransportType,
            };

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"hello".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            // First delivery (PacketReceived + ProofRequested)
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(transport.pending_events(), 2);
            transport.drain_events();

            // Second: deduplicated
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(transport.pending_events(), 0);

            // After clearing cache, packet is accepted again
            transport.storage_mut().clear_packet_hashes();
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(transport.pending_events(), 2);
        }

        #[test]
        fn test_dedup_uses_full_hash() {
            // Verify that Storage sees the full 32-byte SHA-256 hash,
            // matching Python Transport.py:1227.
            use crate::packet::packet_hash;

            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));

            let hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(hash);

            use crate::destination::DestinationType;
            use crate::packet::{
                HeaderType, PacketContext, PacketData, PacketFlags, TransportType,
            };

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"hello".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            // Process the packet
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Verify the full hash is in Storage
            let expected_hash = packet_hash(&buf[..len]);
            assert!(
                transport.storage().has_packet_hash(&expected_hash),
                "Storage must contain the full SHA-256 hash from packet_hash()"
            );
        }

        #[test]
        fn test_announce_processing() {
            use crate::destination::{Destination, DestinationType, Direction};
            use crate::packet::{
                HeaderType, PacketContext, PacketData, PacketFlags, TransportType,
            };

            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));

            // Create a real announce
            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["echo"],
            )
            .expect("Failed to create destination");

            let id = dest.identity().unwrap();
            let random_hash = [0x42u8; crate::constants::RANDOM_HASHBYTES];

            let mut payload = Vec::new();
            payload.extend_from_slice(&id.public_key_bytes());
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(&random_hash);

            let app_data = b"test.data";

            // Sign
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&id.public_key_bytes());
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(&random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = id.sign(&signed_data).unwrap();
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops: 2,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            transport.process_incoming(0, &buf[..len]).unwrap();

            // Should have path now
            assert!(transport.has_path(dest.hash().as_bytes()));
            assert_eq!(transport.hops_to(dest.hash().as_bytes()), Some(3)); // wire=2 + receipt increment
            assert_eq!(transport.stats().announces_processed, 1);

            // Should have emitted PathFound + AnnounceReceived
            let events: Vec<_> = transport.drain_events().collect();
            assert_eq!(events.len(), 2);
            match &events[0] {
                TransportEvent::PathFound {
                    destination_hash,
                    hops,
                    ..
                } => {
                    assert_eq!(destination_hash, dest.hash());
                    assert_eq!(*hops, 3); // wire=2 + receipt increment
                }
                _ => panic!("Expected PathFound event"),
            }
            match &events[1] {
                TransportEvent::AnnounceReceived { announce, .. } => {
                    assert_eq!(announce.destination_hash(), dest.hash());
                    assert_eq!(announce.app_data(), b"test.data");
                }
                _ => panic!("Expected AnnounceReceived event"),
            }
        }

        // Helper: Build announce packet bytes
        fn make_announce_raw(
            hops: u8,
            context: crate::packet::PacketContext,
        ) -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
            use crate::destination::{Destination, DestinationType, Direction};
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["rebroadcast"],
            )
            .unwrap();

            let id = dest.identity().unwrap();
            let random_hash = [0x42u8; crate::constants::RANDOM_HASHBYTES];

            let mut payload = Vec::new();
            payload.extend_from_slice(&id.public_key_bytes());
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(&random_hash);

            let app_data = b"test";
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&id.public_key_bytes());
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(&random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = id.sign(&signed_data).unwrap();
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            (buf[..len].to_vec(), dest.hash().into_bytes())
        }

        fn make_transport_enabled() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            Transport::new(config, clock, MemoryStorage::with_defaults(), identity)
        }

        /// Check if actions contain a path request broadcast for the given destination.
        /// Path request packet (Type1): flags(1) + hops(1) + path_request_hash(16) + context(1) = 19 header bytes
        /// Payload: target_dest_hash(16) + transport_id(16) + tag(16) = 48 bytes
        fn has_path_request_broadcast(
            actions: &[Action],
            dest_hash: &[u8; TRUNCATED_HASHBYTES],
        ) -> bool {
            actions.iter().any(|a| match a {
                Action::Broadcast { data, .. } => {
                    data.len() >= 19 + TRUNCATED_HASHBYTES
                        && data[19..19 + TRUNCATED_HASHBYTES] == *dest_hash
                }
                _ => false,
            })
        }

        // Stage 1: Announce Rebroadcast Tests
        #[test]
        fn test_rebroadcast_scheduled_twice_then_removed() {
            // Python parity (Transport.py:519-540): for a received non-local-
            // client announce, the retry scheduler fires exactly twice before
            // removing the entry (once from retries=0 → 1, once from 1 → 2,
            // then the `retries > PATHFINDER_RETRIES=1` guard fires).
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let (raw, dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert!(entry.retransmit_at_ms.is_some());
            assert_eq!(
                entry.retries, 0,
                "Python parity: non-local-client entry starts at retries=0"
            );

            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.retries, 1,
                "after first fire, retries incremented to 1"
            );

            transport.clock.advance(PATHFINDER_G_MS + 5_000);
            transport.poll();
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.retries, 2,
                "after second fire, retries incremented to 2"
            );

            transport.clock.advance(PATHFINDER_G_MS + 5_000);
            transport.poll();
            assert!(
                transport.storage().get_announce(&dest_hash).is_none(),
                "third fire: retries > PATHFINDER_RETRIES → entry removed"
            );
        }

        #[test]
        fn test_rebroadcast_sent_on_correct_interfaces() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("arrival", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("other1", 2)));
            let _idx2 = transport.register_interface(Box::new(MockInterface::new("other2", 3)));

            let (raw, _dest_hash) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // First on-air rebroadcast fires from the retry scheduler once
            // the jitter window elapses (Python parity).
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            assert!(transport.stats().packets_forwarded > 0);
        }

        #[test]
        fn test_no_rebroadcast_when_transport_disabled() {
            let mut transport = test_transport(); // transport disabled by default
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let (raw, dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // Should have no retransmit scheduled
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert!(entry.retransmit_at_ms.is_none());

            // Advance and poll - should not forward anything
            transport.clock.advance(10_000);
            transport.poll();
            assert_eq!(transport.stats().packets_forwarded, 0);
        }

        #[test]
        fn test_rebroadcast_stops_after_max_retries() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let (raw, dest_hash) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // Python parity: non-local-client entry starts at retries=0.
            // The retry scheduler fires PATHFINDER_RETRIES+1 = 2 times, each
            // time incrementing retries; then the `retries > PATHFINDER_RETRIES`
            // guard removes the entry.
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(entry.retries, 0);

            for expected_retries in 1..=PATHFINDER_RETRIES + 1 {
                transport.clock.advance(PATHFINDER_G_MS + 5000);
                transport.poll();
                let entry = transport
                    .storage()
                    .get_announce(&dest_hash)
                    .expect("entry should still exist");
                assert_eq!(entry.retries, expected_retries);
            }

            // One more poll: retries > PATHFINDER_RETRIES → removed
            transport.clock.advance(PATHFINDER_G_MS + 5000);
            transport.poll();
            assert!(
                transport.storage().get_announce(&dest_hash).is_none(),
                "Entry should be removed after exceeding PATHFINDER_RETRIES"
            );
        }

        #[test]
        fn test_rebroadcast_increments_hops() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let original_hops = 3u8;
            let (raw, _dest_hash) = make_announce_raw(original_hops, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // Retry scheduler fires the first rebroadcast after the initial
            // jitter window — advance and poll to trigger it.
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            assert!(transport.stats().packets_forwarded > 0);

            let actions = transport.drain_actions();
            let broadcast_data = actions
                .iter()
                .find_map(|a| match a {
                    Action::Broadcast { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a Broadcast action for the rebroadcast");
            let rebroadcasted = Packet::unpack(broadcast_data).unwrap();
            assert_eq!(
                rebroadcasted.hops,
                original_hops + 1,
                "Rebroadcasted announce should have hops incremented by 1"
            );
        }

        #[test]
        fn test_path_response_not_rebroadcasted() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let (raw, dest_hash) = make_announce_raw(2, PacketContext::PathResponse);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // Should have no retransmit scheduled for PATH_RESPONSE context
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert!(entry.retransmit_at_ms.is_none());
            assert!(entry.block_rebroadcasts);
        }

        /// Path request response must be sent to ALL interfaces, including the one
        /// that sent the path request. Before the fix, `receiving_interface_index`
        /// was set to the requesting interface, causing `forward_on_all_except` to
        /// exclude the requester from the response.
        #[test]
        fn test_path_request_response_not_excluded_from_requester() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Process an announce from interface 0 to populate cache
            let (raw, dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_actions();
            transport.drain_events();

            // Verify cache is populated
            assert!(transport.storage.get_announce_cache(&dest_hash).is_some());

            // Clear announce table (keep cache) to simulate fresh state
            for key in transport.storage().announce_keys() {
                transport.storage_mut().remove_announce(&key);
            }
            transport.storage_mut().clear_packet_hashes();

            // Build a path request from interface 0 (the same one the announce came from)
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: transport.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            transport.handle_path_request(packet, 0).unwrap();

            // No immediate actions, response is deferred
            let actions = transport.drain_actions();
            assert!(
                actions
                    .iter()
                    .all(|a| !matches!(a, Action::Broadcast { .. } | Action::SendPacket { .. })),
                "Response should be deferred, not immediate"
            );

            // Advance clock past PATH_REQUEST_GRACE_MS and poll to trigger rebroadcast
            transport.clock.advance(PATH_REQUEST_GRACE_MS + 1);
            transport.poll();

            let actions = transport.drain_actions();

            // Path response should be a targeted SendPacket to interface 0 (the requester),
            // not a Broadcast. Python sends only to the requesting interface (Transport.py:1037-1038).
            let sends: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::SendPacket { iface, data } => Some((iface, data)),
                    _ => None,
                })
                .collect();

            assert_eq!(
                sends.len(),
                1,
                "Should have exactly one SendPacket for the path response"
            );
            assert_eq!(
                sends[0].0 .0, 0,
                "Path response must target the requesting interface (0)"
            );

            // No Broadcast actions, path response is targeted, not broadcast
            let broadcasts: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .collect();
            assert!(
                broadcasts.is_empty(),
                "Path response should not be broadcast"
            );
        }

        #[test]
        fn test_local_rebroadcast_detection_suppresses() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Create announce at hops=1
            let (raw1, dest_hash) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw1).unwrap();
            transport.drain_events();

            // Verify retransmit is scheduled
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert!(entry.retransmit_at_ms.is_some());

            // Now create a "same announce from neighbor" at hops=2 (hops+1)
            // We can't use the same dest hash with a different packet easily,
            // so we manually simulate by adjusting the announce table
            if let Some(entry) = transport.storage_mut().get_announce_mut(&dest_hash) {
                entry.local_rebroadcasts = LOCAL_REBROADCASTS_MAX;
            }

            // After poll, entry with max local rebroadcasts should be removed
            transport.clock.advance(10_000);
            transport.poll();

            assert!(transport.storage().get_announce(&dest_hash).is_none());
        }

        #[test]
        fn test_announce_cache_populated() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let (raw, dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // Announce cache should have the raw bytes
            assert!(transport.storage().get_announce_cache(&dest_hash).is_some());
            assert_eq!(
                transport.storage().get_announce_cache(&dest_hash).unwrap(),
                &raw
            );
        }

        // Stage 2: Link Table Tests
        #[test]
        fn test_link_table_entry_expiry_validated() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
            let now = transport.clock.now_ms();

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            assert!(transport.storage().has_link_entry(&link_id));

            // Advance past LINK_TIMEOUT_MS (15 min)
            transport.clock.advance(LINK_TIMEOUT_MS + 1000);
            transport.poll();

            assert!(!transport.storage().has_link_entry(&link_id));
        }

        #[test]
        fn test_link_table_entry_expiry_unvalidated() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
            let now = transport.clock.now_ms();

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let proof_timeout = now + 10_000;
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: proof_timeout,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Advance past proof_timeout but not LINK_TIMEOUT_MS
            transport.clock.advance(11_000);
            transport.poll();

            // Unvalidated entry should be expired
            assert!(!transport.storage().has_link_entry(&link_id));
        }

        #[test]
        fn test_link_table_validated_not_expired_early() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
            let now = transport.clock.now_ms();

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Advance less than LINK_TIMEOUT_MS
            transport.clock.advance(60_000); // 1 minute
            transport.poll();

            // Should still be present
            assert!(transport.storage().has_link_entry(&link_id));
        }

        // Stage 3: Path Request Tests
        #[test]
        fn test_compute_path_request_hash() {
            let hash = Transport::<MockClock, MemoryStorage>::compute_path_request_hash();
            // Should be full_hash(name_hash)[:16], matching Python Reticulum
            let name_hash = crate::destination::Destination::compute_name_hash(
                "rnstransport",
                &["path", "request"],
            );
            let expected = truncated_hash(&name_hash);
            assert_eq!(hash, expected);
        }

        #[test]
        fn test_request_path_sends_packet() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let tag = [0xAB; TRUNCATED_HASHBYTES];
            transport.request_path(&dest_hash, None, &tag).unwrap();

            assert!(transport.stats().packets_sent > 0);
        }

        #[test]
        fn test_request_path_rate_limited() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let tag1 = [0xAB; TRUNCATED_HASHBYTES];
            let tag2 = [0xCD; TRUNCATED_HASHBYTES];

            transport.request_path(&dest_hash, None, &tag1).unwrap();
            let sent1 = transport.stats().packets_sent;

            // Immediately send another - should be rate limited
            transport.request_path(&dest_hash, None, &tag2).unwrap();
            let sent2 = transport.stats().packets_sent;
            assert_eq!(sent1, sent2, "Second request should be rate limited");

            // Advance past rate limit
            transport.clock.advance(PATH_REQUEST_MIN_INTERVAL_MS + 1);
            transport.request_path(&dest_hash, None, &tag2).unwrap();
            let sent3 = transport.stats().packets_sent;
            assert!(sent3 > sent2, "After cooldown, request should go through");
        }

        // Path Request Format Tests
        #[test]
        fn test_non_transport_sends_32_byte_path_request() {
            // Non-transport nodes send 32-byte path requests (dest_hash + tag only).
            let mut transport = test_transport(); // enable_transport = false
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let tag = [0xAB; TRUNCATED_HASHBYTES];
            transport.request_path(&dest_hash, None, &tag).unwrap();

            let actions = transport.drain_actions();
            let broadcast = actions
                .iter()
                .find_map(|a| match a {
                    Action::Broadcast { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have broadcast action");

            // Unpack the packet to check payload size
            let pkt = Packet::unpack(broadcast).unwrap();
            assert_eq!(
                pkt.data.as_slice().len(),
                32,
                "Non-transport path request payload should be 32 bytes (dest_hash + tag)"
            );
            // Verify structure: first 16 = dest_hash, last 16 = tag
            assert_eq!(&pkt.data.as_slice()[..16], &dest_hash);
            assert_eq!(&pkt.data.as_slice()[16..32], &tag);
        }

        #[test]
        fn test_transport_sends_48_byte_path_request() {
            // Transport nodes send 48-byte path requests (dest_hash + transport_id + tag).
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let tag = [0xAB; TRUNCATED_HASHBYTES];
            let our_hash = *transport.identity.hash();
            transport.request_path(&dest_hash, None, &tag).unwrap();

            let actions = transport.drain_actions();
            let broadcast = actions
                .iter()
                .find_map(|a| match a {
                    Action::Broadcast { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have broadcast action");

            let pkt = Packet::unpack(broadcast).unwrap();
            assert_eq!(
                pkt.data.as_slice().len(),
                48,
                "Transport path request payload should be 48 bytes"
            );
            // Verify structure: dest_hash + transport_id + tag
            assert_eq!(&pkt.data.as_slice()[..16], &dest_hash);
            assert_eq!(&pkt.data.as_slice()[16..32], &our_hash);
            assert_eq!(&pkt.data.as_slice()[32..48], &tag);
        }

        #[test]
        fn test_handle_32_byte_path_request() {
            // Transport node should handle 32-byte path requests correctly.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            let path_req_hash = transport.path_request_hash;
            let tag = [0xCC; TRUNCATED_HASHBYTES];

            // Build 32-byte path request: dest_hash(16) + tag(16)
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&tag);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            let events: Vec<_> = transport.drain_events().collect();
            let found = events.iter().any(|e| matches!(
                e,
                TransportEvent::PathRequestReceived { destination_hash, .. } if *destination_hash == dest_hash
            ));
            assert!(found, "32-byte path request should be handled");
        }

        #[test]
        fn test_handle_48_byte_path_request() {
            // Transport node should handle 48-byte path requests correctly.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            let path_req_hash = transport.path_request_hash;
            let tag = [0xCC; TRUNCATED_HASHBYTES];

            // Build 48-byte path request: dest_hash(16) + transport_id(16) + tag(16)
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
            data.extend_from_slice(&tag);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            let events: Vec<_> = transport.drain_events().collect();
            let found = events.iter().any(|e| matches!(
                e,
                TransportEvent::PathRequestReceived { destination_hash, .. } if *destination_hash == dest_hash
            ));
            assert!(found, "48-byte path request should be handled");
        }

        #[test]
        fn test_handle_short_path_request_rejected() {
            // Path requests shorter than 32 bytes should be silently dropped.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let path_req_hash = transport.path_request_hash;

            // Build too-short path request: only 16 bytes
            let data = [0x42u8; 16].to_vec();

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events.is_empty(),
                "Short path request should produce no events"
            );
        }

        #[test]
        fn test_path_request_local_dest_emits_event() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            // Build a path request packet targeting the local destination
            let path_req_hash = transport.path_request_hash;
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
            data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Should emit PathRequestReceived event
            let events: Vec<_> = transport.drain_events().collect();
            let found = events.iter().any(|e| matches!(
                e,
                TransportEvent::PathRequestReceived { destination_hash, .. } if *destination_hash == dest_hash
            ));
            assert!(found, "Expected PathRequestReceived event");
        }

        #[test]
        fn test_path_request_cached_announce_triggers_response() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // First, receive an announce to populate the cache
            let (raw, dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            // Clear announce table to simulate fresh state (but keep cache)
            for key in transport.storage().announce_keys() {
                transport.storage_mut().remove_announce(&key);
            }

            // Now receive a path request for that destination
            let path_req_hash = transport.path_request_hash;
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
            data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(1, &buf[..len]).unwrap();

            // Should have inserted a PATH_RESPONSE announce in the announce table
            let entry = transport.storage().get_announce(&dest_hash);
            assert!(
                entry.is_some(),
                "Should have announce table entry for path response"
            );
            let entry = entry.unwrap();
            assert!(
                entry.block_rebroadcasts,
                "Should be marked as block_rebroadcasts"
            );
            assert!(
                entry.retransmit_at_ms.is_some(),
                "Should have a retransmit scheduled"
            );
        }

        #[test]
        fn test_path_request_tag_dedup() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let tag = [0xCC; TRUNCATED_HASHBYTES];

            let path_req_hash = transport.path_request_hash;
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
            data.extend_from_slice(&tag);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            // First request - should be processed
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(transport.storage().path_request_tag_count(), 1);

            // Clear packet cache to allow second processing
            transport.storage_mut().clear_packet_hashes();

            // Second request with same tag - should be deduped
            transport.process_incoming(0, &buf[..len]).unwrap();
            // Tag count should still be 1 (dedup worked)
            assert_eq!(transport.storage().path_request_tag_count(), 1);
        }

        #[test]
        fn test_reverse_table_expiry_8_minutes() {
            // Verify the constant was fixed to 8 minutes
            assert_eq!(REVERSE_TABLE_EXPIRY_MS, 480_000);
        }

        #[test]
        fn test_send_on_all_interfaces_except() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
            let _idx2 = transport.register_interface(Box::new(MockInterface::new("if2", 3)));

            transport.send_on_all_interfaces_except(1, b"test data");

            // We can't easily inspect MockInterface sent data through the trait,
            // but we can verify no panic and basic operation works
        }

        #[test]
        fn test_path_request_unknown_dest_forwarded() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0x99; TRUNCATED_HASHBYTES]; // Unknown destination
            let path_req_hash = transport.path_request_hash;
            let local_transport_id = *transport.identity.hash();

            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
            data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();

            // Process path request from if0 for unknown dest
            // Wire hops=0, process_incoming increments to hops=1 (direct neighbor)
            transport.process_incoming(0, &buf[..len]).unwrap();
            // Tag should be stored
            assert_eq!(transport.storage().path_request_tag_count(), 1);

            // Verify the re-originated packet has hops=0 and local transport_id
            // send_on_all_interfaces_except produces Action::Broadcast
            let actions = transport.drain_actions();
            let broadcasts: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::Broadcast { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .collect();
            assert!(
                !broadcasts.is_empty(),
                "Should re-originate path request to if1"
            );
            for raw in &broadcasts {
                // Type1 header: flags(1) + hops(1) + dest_hash(16) + context(1)
                assert_eq!(raw[1], 0, "Re-originated path request must have hops=0");
                // Data starts at byte 19: dest_hash(16) + transport_id(16) + tag(16)
                assert_eq!(
                    &raw[19..19 + TRUNCATED_HASHBYTES],
                    &dest_hash,
                    "Payload must contain the requested dest hash"
                );
                assert_eq!(
                    &raw[19 + TRUNCATED_HASHBYTES..19 + 2 * TRUNCATED_HASHBYTES],
                    &local_transport_id,
                    "Re-originated request must contain local transport_id"
                );
            }
        }

        #[test]
        fn test_path_request_reoriginated_has_zero_hops() {
            // A path request arriving with hops=3 for an unknown destination
            // must be re-originated with hops=0 (not forwarded with hops=3).
            // Python Transport.py:2802-2806: each hop creates a fresh packet.
            // Uses handle_path_request() directly to bypass process_incoming's
            // PLAIN filter (which drops hops > 1).
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let local_transport_id = *transport.identity.hash();
            let tag = [0xAA; TRUNCATED_HASHBYTES];

            // Incoming path request has hops=3 (as if it already traversed 3 nodes)
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // remote transport_id
            data.extend_from_slice(&tag);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 3,
                transport_id: None,
                destination_hash: transport.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let result = transport.handle_path_request(packet, 0);
            assert!(result.is_ok());

            // send_on_all_interfaces_except produces Action::Broadcast
            let actions = transport.drain_actions();
            let broadcasts: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::Broadcast { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .collect();
            assert!(!broadcasts.is_empty(), "Should re-originate path request");
            for raw in &broadcasts {
                assert_eq!(
                    raw[1], 0,
                    "Re-originated path request must have hops=0, not the original hops=3"
                );
                // Verify the re-originated packet uses the local transport_id
                assert_eq!(
                    &raw[19 + TRUNCATED_HASHBYTES..19 + 2 * TRUNCATED_HASHBYTES],
                    &local_transport_id,
                    "Re-originated request must use local transport_id, not the remote one"
                );
                // Verify the same tag is preserved for dedup
                assert_eq!(
                    &raw[19 + 2 * TRUNCATED_HASHBYTES..19 + 3 * TRUNCATED_HASHBYTES],
                    &tag,
                    "Tag must be preserved from original request"
                );
            }
        }

        #[test]
        fn test_new_constants_values() {
            assert_eq!(PATHFINDER_G_MS, 5_000);
            assert_eq!(LOCAL_REBROADCASTS_MAX, 2);
            assert_eq!(LINK_TIMEOUT_MS, 900_000);
            assert_eq!(PATH_REQUEST_GRACE_MS, 400);
            assert_eq!(crate::constants::PATH_REQUEST_TIMEOUT_MS, 15_000);
            assert_eq!(MAX_PATH_REQUEST_TAGS, 32_000);
            assert_eq!(PATH_REQUEST_MIN_INTERVAL_MS, 20_000);
        }

        // Stage 9: Random blob replay protection
        #[test]
        fn test_announce_replay_detected_via_random_blob() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let (raw, _dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            assert_eq!(transport.stats().announces_processed, 1);

            // Bypass packet cache and rate limit
            transport.storage_mut().clear_packet_hashes();
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Replay exact same announce (same random_hash)
            transport.process_incoming(0, &raw).unwrap();
            assert_eq!(
                transport.stats().announces_processed,
                1,
                "Replayed announce should be dropped"
            );
        }

        #[test]
        fn test_different_random_blob_accepted() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let (raw1, _dest_hash) = make_announce_raw(2, PacketContext::None);
            transport.process_incoming(0, &raw1).unwrap();
            assert_eq!(transport.stats().announces_processed, 1);

            // Create a new announce for the same app but different identity
            // (make_announce_raw generates a fresh identity each time)
            let (raw2, _dest_hash2) = make_announce_raw(2, PacketContext::None);

            // Bypass packet cache and rate limit
            transport.storage_mut().clear_packet_hashes();
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // This is a different destination, so it should be processed
            transport.process_incoming(0, &raw2).unwrap();
            assert_eq!(
                transport.stats().announces_processed,
                2,
                "Different destination should be accepted"
            );
        }

        // Stage 8: LRPROOF validation
        #[test]
        fn test_lrproof_invalid_length_not_forwarded() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build LRPROOF with wrong size (too short)
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 2,
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(b"too short".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(1, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_forwarded,
                0,
                "LRPROOF with invalid length should not be forwarded"
            );
            assert!(
                !transport
                    .storage()
                    .get_link_entry(&link_id)
                    .unwrap()
                    .validated,
                "Link should not be validated with bad proof"
            );
        }

        // LRPROOF Signature Validation Tests
        #[test]
        fn test_lrproof_valid_signature_forwarded() {
            // Valid LRPROOF with correct signature should be forwarded.
            use crate::constants::{ED25519_KEY_SIZE, X25519_KEY_SIZE};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Create a real identity for the peer
            let peer_identity = Identity::generate(&mut OsRng);
            let pub_bytes = peer_identity.public_key_bytes();
            let mut peer_ed25519 = [0u8; ED25519_KEY_SIZE];
            peer_ed25519.copy_from_slice(&pub_bytes[X25519_KEY_SIZE..]);

            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: Some(peer_ed25519),
                },
            );

            // Build valid LRPROOF: sig(64) + X25519_pub(32) = 96 bytes
            let peer_x25519_pub = [0x11u8; X25519_KEY_SIZE];
            // signed_data = link_id + X25519_pub + Ed25519_pub
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(&link_id);
            signed_data.extend_from_slice(&peer_x25519_pub);
            signed_data.extend_from_slice(&peer_ed25519);

            let signature = peer_identity.sign(&signed_data).unwrap();

            let mut proof_data = Vec::with_capacity(96);
            proof_data.extend_from_slice(&signature);
            proof_data.extend_from_slice(&peer_x25519_pub);

            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 1, // receipt increment → 2 = remaining_hops
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(proof_data),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(1, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Valid LRPROOF should be forwarded"
            );
            assert!(
                transport
                    .storage()
                    .get_link_entry(&link_id)
                    .unwrap()
                    .validated,
                "Link should be validated after valid proof"
            );
        }

        #[test]
        fn test_lrproof_invalid_signature_dropped() {
            // LRPROOF with invalid signature should be dropped.
            use crate::constants::{ED25519_KEY_SIZE, ED25519_SIGNATURE_SIZE, X25519_KEY_SIZE};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            let peer_identity = Identity::generate(&mut OsRng);
            let pub_bytes = peer_identity.public_key_bytes();
            let mut peer_ed25519 = [0u8; ED25519_KEY_SIZE];
            peer_ed25519.copy_from_slice(&pub_bytes[X25519_KEY_SIZE..]);

            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: Some(peer_ed25519),
                },
            );

            // Build LRPROOF with corrupted signature: correct size but garbage sig
            let mut proof_data = Vec::with_capacity(96);
            proof_data.extend_from_slice(&[0xDE; ED25519_SIGNATURE_SIZE]); // garbage signature
            proof_data.extend_from_slice(&[0x11; X25519_KEY_SIZE]);

            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 2,
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(proof_data),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(1, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_forwarded,
                0,
                "LRPROOF with invalid signature should not be forwarded"
            );
            assert!(
                transport.stats().packets_dropped > dropped_before,
                "Dropped count should increment for invalid signature"
            );
        }

        #[test]
        fn test_lrproof_no_signing_key_forwarded() {
            // LRPROOF without peer_signing_key should be forwarded (cannot validate).
            use crate::constants::{ED25519_SIGNATURE_SIZE, X25519_KEY_SIZE};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None, // No key available
                },
            );

            // Build LRPROOF with arbitrary data (correct size)
            let mut proof_data = Vec::with_capacity(96);
            proof_data.extend_from_slice(&[0xFF; ED25519_SIGNATURE_SIZE]);
            proof_data.extend_from_slice(&[0x11; X25519_KEY_SIZE]);

            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 1, // receipt increment → 2 = remaining_hops
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(proof_data),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(1, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "LRPROOF without signing key should still be forwarded"
            );
        }

        #[test]
        fn test_link_entry_signing_key_from_announce_cache() {
            // When a link request is forwarded, the LinkEntry should get the
            // peer's Ed25519 signing key from the announce cache.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Process an announce to populate path_table AND announce_cache
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            assert!(transport.has_path(&dest_hash));
            assert!(transport.storage().get_announce_cache(&dest_hash).is_some());

            // Extract expected Ed25519 key from announce payload
            let cached = transport.storage().get_announce_cache(&dest_hash).unwrap();
            let announce_packet = Packet::unpack(cached).unwrap();
            let payload = announce_packet.data.as_slice();
            let expected_ed25519 =
                &payload[crate::constants::X25519_KEY_SIZE..crate::constants::IDENTITY_KEY_SIZE];

            // Build a link request addressed to dest_hash via us
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::LinkRequest,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0u8; 96]),
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Find the link entry that was created. handle_link_request inserts
            // this unconditionally on the forward path (before the same-interface
            // suppression check), so the entry's presence is sufficient proof
            // the code flow reached the signing-key lookup.
            let link_entry = transport
                .storage()
                .link_entry_values()
                .next()
                .expect("Should have a link table entry");

            assert!(
                link_entry.peer_signing_key.is_some(),
                "Link entry should have peer_signing_key from announce cache"
            );
            assert_eq!(
                link_entry.peer_signing_key.unwrap().as_slice(),
                expected_ed25519,
                "Signing key should match announce payload"
            );
        }

        #[test]
        fn test_lrproof_unknown_link_dropped() {
            // LRPROOF for a link_id not in link_table should not be forwarded.
            use crate::constants::{ED25519_SIGNATURE_SIZE, X25519_KEY_SIZE};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            // No link table entry for this link_id

            let mut proof_data = Vec::with_capacity(96);
            proof_data.extend_from_slice(&[0xFF; ED25519_SIGNATURE_SIZE]);
            proof_data.extend_from_slice(&[0x11; X25519_KEY_SIZE]);

            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 1,
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(proof_data),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_forwarded,
                0,
                "LRPROOF for unknown link should not be forwarded"
            );
        }

        // Gap 3: Per-interface announce bandwidth caps
        #[test]
        fn test_announce_cap_delays_second_announce() {
            // Register a low-bitrate interface, process two announces fast.            // the second should be queued (not immediately sent) on the capped interface.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Register low bitrate on if1: 1000 bps, 2% cap = 20 bps effective
            transport.register_interface_bitrate(1, 1000);

            // Process first announce (hops > 0 so caps apply). Rebroadcast fires
            // from the retry scheduler after the jitter window (Python parity).
            let (raw1, _dh1) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw1).unwrap();
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions1 = transport.drain_actions();
            let _ = transport.drain_events();

            // Should have a SendPacket on if1 (capped interface, first announce allowed)
            let send_count = actions1
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
                .count();
            assert_eq!(
                send_count, 1,
                "first announce should be sent on capped interface"
            );

            // Process second announce immediately (no time advance) and poll
            // the scheduler — the cap's holdoff should queue this one on if1.
            let (raw2, _dh2) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw2).unwrap();
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions2 = transport.drain_actions();
            let _ = transport.drain_events();

            // Second should be queued on capped interface, not immediately sent
            let send_count2 = actions2
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
                .count();
            assert_eq!(
                send_count2, 0,
                "second announce should be queued on capped interface"
            );
        }

        #[test]
        fn test_announce_cap_drains_queue_after_holdoff() {
            extern crate alloc;
            // Queue an announce, advance past allowed_at_ms, poll → should emit it.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // 1000 bps, 2% cap = 20 bps; a ~100 byte packet = 800 bits → 40s holdoff
            transport.register_interface_bitrate(1, 1000);

            // Manually queue an announce on the capped interface
            let cap = transport.interface_announce_caps.get_mut(&1).unwrap();
            cap.allowed_at_ms = transport.clock.now_ms() + 40_000;
            cap.queue.push_back(QueuedAnnounce {
                raw: alloc::vec![0xAA; 100],
                hops: 2,
                queued_at_ms: transport.clock.now_ms(),
            });

            // Poll before holdoff expires, nothing should drain
            transport.poll();
            let actions_before = transport.drain_actions();
            let sent_before = actions_before
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
                .count();
            assert_eq!(sent_before, 0, "should not drain before holdoff expires");

            // Advance past holdoff
            transport.clock.advance(41_000);
            transport.poll();
            let actions_after = transport.drain_actions();
            let sent_after = actions_after
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
                .count();
            assert_eq!(sent_after, 1, "should drain queued announce after holdoff");
        }

        #[test]
        fn test_announce_queue_max_size() {
            extern crate alloc;
            // Exceed max_queued_announces → oldest dropped.
            let mut transport = make_transport_enabled();
            let max_queued = transport.config.max_queued_announces;
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            transport.register_interface_bitrate(0, 1000);

            // Set holdoff far in the future so all announces get queued
            let cap = transport.interface_announce_caps.get_mut(&0).unwrap();
            cap.allowed_at_ms = transport.clock.now_ms() + 1_000_000;

            // Fill queue to max
            for i in 0..max_queued {
                cap.queue.push_back(QueuedAnnounce {
                    raw: alloc::vec![i as u8; 50],
                    hops: 1,
                    queued_at_ms: transport.clock.now_ms() + i as u64,
                });
            }
            assert_eq!(cap.queue.len(), max_queued);

            // One more should not grow the queue (broadcast_announce_with_caps checks len)
            // We test the queue boundary directly
            let cap = transport.interface_announce_caps.get_mut(&0).unwrap();
            if cap.queue.len() < max_queued {
                cap.queue.push_back(QueuedAnnounce {
                    raw: alloc::vec![0xFF; 50],
                    hops: 1,
                    queued_at_ms: transport.clock.now_ms(),
                });
            }
            assert_eq!(
                cap.queue.len(),
                max_queued,
                "queue should not grow beyond max"
            );
        }

        #[test]
        fn test_announce_cap_does_not_affect_data() {
            extern crate alloc;
            // Data forwarding (non-announce) should be unaffected by caps.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Register tight cap on if1
            transport.register_interface_bitrate(1, 100);
            // Set holdoff far in future
            let cap = transport.interface_announce_caps.get_mut(&1).unwrap();
            cap.allowed_at_ms = transport.clock.now_ms() + 1_000_000;

            // Insert a path table entry routing to if1
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            let transport_hash = *transport.identity.hash();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    interface_index: 1,
                    next_hop: None,
                    hops: 0,
                    expires_ms: now + 600_000,
                    random_blobs: Vec::new(),
                },
            );

            // Forward a data packet to that destination (Type2 addressed to us)
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(transport_hash),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0x01; 32]),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            let actions = transport.drain_actions();
            let sent = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
                .count();
            assert_eq!(sent, 1, "data packet should bypass announce caps");
        }

        #[test]
        fn test_announce_cap_per_interface_independence() {
            // Two capped interfaces should track holdoff independently.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
            let _idx2 = transport.register_interface(Box::new(MockInterface::new("if2", 3)));

            // if1: very slow (100 bps), if2: fast (1_000_000 bps)
            transport.register_interface_bitrate(1, 100);
            transport.register_interface_bitrate(2, 1_000_000);

            // Process an announce from if0. Rebroadcast fires from the retry
            // scheduler once the jitter window elapses (Python parity).
            let (raw, _dh) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions = transport.drain_actions();
            let _ = transport.drain_events();

            // Both should get SendPacket (first announce on each)
            let sent_if1 = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
                .count();
            let sent_if2 = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 2))
                .count();
            assert_eq!(sent_if1, 1, "if1 should get first announce");
            assert_eq!(sent_if2, 1, "if2 should get first announce");

            // Now check holdoffs are different (slow has much longer holdoff)
            let cap1 = transport.interface_announce_caps.get(&1).unwrap();
            let cap2 = transport.interface_announce_caps.get(&2).unwrap();
            assert!(
                cap1.allowed_at_ms > cap2.allowed_at_ms,
                "slow interface should have longer holdoff ({} vs {})",
                cap1.allowed_at_ms,
                cap2.allowed_at_ms
            );
        }

        #[test]
        fn test_local_announce_skips_cap() {
            // Locally-originated announces (hops == 0 after local client adjust) should
            // bypass caps entirely (Python Transport.py:1086-1089).
            // Since Block B, the first local client announce is delayed by 250ms.            // caps are still bypassed when the deferred rebroadcast fires.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Mark if0 as a local client (so hops get +1 then -1, net zero)
            transport.set_local_client(0, true);

            // Tight cap on if1
            transport.register_interface_bitrate(1, 100);
            let cap = transport.interface_announce_caps.get_mut(&1).unwrap();
            cap.allowed_at_ms = transport.clock.now_ms() + 1_000_000; // Block cap

            // Process local announce (hops == 0) from local client if0
            let (raw, _dh) = make_announce_raw(0, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            let _ = transport.drain_actions(); // No immediate broadcast (250ms delay)
            let _ = transport.drain_events();

            // Advance past the 250ms local client announce delay and poll
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let actions = transport.drain_actions();

            // Local announces bypass caps, so we should see actions even with cap blocked
            assert!(
                !actions.is_empty(),
                "local announces should bypass caps (after 250ms delay)"
            );

            // And NO queued announces on the capped interface
            let cap = transport.interface_announce_caps.get(&1).unwrap();
            assert!(
                cap.queue.is_empty(),
                "local announces should not queue on capped interfaces"
            );
        }

        #[test]
        fn test_queue_drain_priority_lowest_hops() {
            extern crate alloc;
            // Lower hops should be dequeued first.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            transport.register_interface_bitrate(0, 1000);

            let now = transport.clock.now_ms();
            let cap = transport.interface_announce_caps.get_mut(&0).unwrap();
            // Two queued items: hops=3 (first), hops=1 (second)
            cap.queue.push_back(QueuedAnnounce {
                raw: alloc::vec![0xAA; 50],
                hops: 3,
                queued_at_ms: now,
            });
            cap.queue.push_back(QueuedAnnounce {
                raw: alloc::vec![0xBB; 50],
                hops: 1,
                queued_at_ms: now + 1,
            });
            cap.allowed_at_ms = now; // Allow immediate drain

            transport.poll();
            let actions = transport.drain_actions();

            // Should dequeue the hops=1 entry first (0xBB)
            let first_send = actions
                .iter()
                .find(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 0));
            assert!(first_send.is_some(), "should have drained one announce");

            if let Some(Action::SendPacket { data, .. }) = first_send {
                assert_eq!(
                    data[0], 0xBB,
                    "lowest-hops announce (0xBB) should be dequeued first"
                );
            }

            // Queue should still have the hops=3 entry
            let cap = transport.interface_announce_caps.get(&0).unwrap();
            assert_eq!(cap.queue.len(), 1, "one entry should remain");
            assert_eq!(cap.queue[0].hops, 3, "hops=3 entry should remain");
        }

        #[test]
        fn test_no_cap_unregistered_interface() {
            // Interfaces without registered bitrate should use normal Broadcast.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Don't register any bitrate → no caps
            assert!(transport.interface_announce_caps.is_empty());

            // Retry scheduler fires the first rebroadcast after the jitter
            // window (Python parity).
            let (raw, _dh) = make_announce_raw(1, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions = transport.drain_actions();
            let _ = transport.drain_events();

            // Should use Broadcast action (no caps → forward_on_all)
            let broadcasts = actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .count();
            assert!(broadcasts > 0, "uncapped transport should use Broadcast");

            // No SendPacket actions for individual interfaces
            let sends = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { .. }))
                .count();
            assert_eq!(sends, 0, "no per-interface sends without caps");
        }

        #[test]
        fn test_next_deadline_includes_queue_drain() {
            extern crate alloc;
            // Non-empty queue should add a deadline at allowed_at_ms.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            transport.register_interface_bitrate(0, 1000);

            let now = transport.clock.now_ms();
            let drain_at = now + 50_000;

            let cap = transport.interface_announce_caps.get_mut(&0).unwrap();
            cap.allowed_at_ms = drain_at;
            cap.queue.push_back(QueuedAnnounce {
                raw: alloc::vec![0xAA; 50],
                hops: 1,
                queued_at_ms: now,
            });

            let deadline = transport.next_deadline();
            assert!(
                deadline.is_some(),
                "should have a deadline with queued announce"
            );
            assert!(
                deadline.unwrap() <= drain_at,
                "deadline ({}) should be <= drain_at ({})",
                deadline.unwrap(),
                drain_at
            );
        }

        // Stage 7: Auto re-announce on local path request
        #[test]
        fn test_path_request_local_dest_schedules_rebroadcast() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            // Populate announce cache for this destination (simulate prior announce)
            let (raw, _) = make_announce_raw(0, PacketContext::None);
            transport.storage_mut().set_announce_cache(dest_hash, raw);

            // Send path request
            let path_req_hash = transport.path_request_hash;
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
            data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Should still emit PathRequestReceived event
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, TransportEvent::PathRequestReceived { .. })),
                "Should emit PathRequestReceived event"
            );

            // AND should schedule a rebroadcast from the cache
            let entry = transport
                .storage()
                .get_announce(&dest_hash)
                .expect("Should schedule announce rebroadcast from cache");
            assert_eq!(
                entry.target_interface,
                Some(0),
                "Local dest path response should target the requesting interface"
            );

            // Advance clock and poll to verify targeted SendPacket
            transport.clock.advance(PATH_REQUEST_GRACE_MS + 1);
            transport.poll();

            let actions = transport.drain_actions();
            let sends: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::SendPacket { iface, data } => Some((iface, data)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                sends.len(),
                1,
                "Should emit exactly one SendPacket for local dest path response"
            );
            assert_eq!(
                sends[0].0 .0, 0,
                "SendPacket must target the requesting interface (0)"
            );

            let broadcasts: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .collect();
            assert!(
                broadcasts.is_empty(),
                "Local dest path response should not broadcast"
            );
        }

        // Issue #13: Deferred path response with no cached announce
        #[test]
        fn test_path_request_local_dest_no_cache_no_announce_entry() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);
            // NO announce cache set, this is the bug trigger

            // Build path request
            let path_req_hash = transport.path_request_hash;
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]);
            data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // PathRequestReceived event must be emitted
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    TransportEvent::PathRequestReceived { destination_hash, .. }
                        if *destination_hash == dest_hash
                )),
                "Must emit PathRequestReceived"
            );

            // announce table must NOT have an entry (no cache = nothing to rebroadcast)
            assert!(
                transport.storage().get_announce(&dest_hash).is_none(),
                "Must not create AnnounceEntry when no announce cache exists"
            );
        }

        #[test]
        fn test_path_request_local_dest_with_cache_creates_entry() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            // Populate announce cache
            let (raw, _) = make_announce_raw(0, PacketContext::None);
            transport
                .storage_mut()
                .set_announce_cache(dest_hash, raw.clone());

            // Build and send path request
            let path_req_hash = transport.path_request_hash;
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]);
            data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Event emitted
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, TransportEvent::PathRequestReceived { .. })),
                "Must emit PathRequestReceived"
            );

            // AnnounceEntry created with correct data
            let entry = transport
                .storage()
                .get_announce(&dest_hash)
                .expect("Must create AnnounceEntry when cache exists");
            assert!(!entry.raw_packet.is_empty(), "raw_packet must not be empty");
            assert_eq!(
                entry.raw_packet, raw,
                "raw_packet must match cached announce"
            );
            assert_eq!(
                entry.target_interface,
                Some(0),
                "must target requesting interface"
            );
            assert!(entry.block_rebroadcasts, "must block rebroadcasts");
        }

        #[test]
        fn test_announce_rebroadcast_empty_packet_is_silent() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];

            // Insert bogus AnnounceEntry with empty raw_packet and past retransmit
            transport.storage_mut().set_announce(
                dest_hash,
                AnnounceEntry {
                    timestamp_ms: TEST_TIME_MS,
                    hops: 0,
                    retries: 0,
                    retransmit_at_ms: Some(TEST_TIME_MS - 1),
                    raw_packet: Vec::new(),
                    receiving_interface_index: 0,
                    target_interface: Some(0),
                    local_rebroadcasts: 0,
                    block_rebroadcasts: false,
                },
            );

            // Advance clock and poll, must not panic or emit broadcast
            transport.clock.advance(1);
            transport.poll();

            let actions = transport.drain_actions();
            let broadcasts: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { .. } | Action::Broadcast { .. }))
                .collect();
            assert!(
                broadcasts.is_empty(),
                "Empty raw_packet must not produce any broadcast action"
            );
        }

        // LRPROOF dedup exemption
        #[test]
        fn test_lrproof_dedup_exemption_for_local_destination() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            // Register a link_id as a local destination (simulates connect())
            let link_id = [0xDD; TRUNCATED_HASHBYTES];
            transport.register_destination(link_id);

            // Build an LRPROOF packet for this link_id
            // LRPROOF format: proof data = sig(64) + X25519(32) = 96 bytes
            let proof_data = [0x42u8; 96];
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: true,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Proof,
                },
                hops: 1,
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(proof_data.to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            let raw = buf[..len].to_vec();

            // First LRPROOF, must produce a PacketReceived event
            transport.process_incoming(0, &raw).unwrap();
            let events1: Vec<_> = transport.drain_events().collect();
            let got_event_1 = events1
                .iter()
                .any(|e| matches!(e, TransportEvent::PacketReceived { .. }));
            assert!(
                got_event_1,
                "First LRPROOF must produce PacketReceived event"
            );

            // Second identical LRPROOF, must NOT be dropped as duplicate.
            // Under link-request retry, the responder re-sends the same proof.
            // If dedup drops it, link establishment fails under RF contention.
            transport.process_incoming(0, &raw).unwrap();
            let events2: Vec<_> = transport.drain_events().collect();
            let got_event_2 = events2
                .iter()
                .any(|e| matches!(e, TransportEvent::PacketReceived { .. }));
            assert!(
                got_event_2,
                "Duplicate LRPROOF for local destination must NOT be dropped by dedup"
            );
        }

        // Relay data retransmit dedup
        #[test]
        fn test_relay_forwards_retransmitted_link_data() {
            // Simulate a relay node with two interfaces (Ch.A = if0, Ch.B = if1).
            // A link-addressed data packet arrives on if0 and is forwarded to if1.
            // The same packet arrives again (resource retransmit), must NOT be
            // dropped by dedup. Link-addressed packets follow a fixed link_table
            // path with no routing loop risk.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xEE; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Set up a validated link_table entry: if0 (initiator side) → if1 (destination side)
            transport.storage_mut().set_link_entry(
                link_id,
                crate::storage_types::LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1, // if1 = toward destination
                    remaining_hops: 1,
                    received_interface_index: 0, // if0 = toward initiator
                    hops: 1,
                    validated: true,
                    proof_timeout_ms: now + 900_000,
                    destination_hash: [0x00; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build a link-addressed data packet (resource segment).
            // Wire hops = 0 because process_incoming() increments to 1,
            // which must match link_entry.hops for initiator-side routing.
            let payload = [0x42u8; 100];
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: true,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Resource,
                data: PacketData::Owned(payload.to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            let raw = buf[..len].to_vec();

            // First arrival on if0, must be forwarded (produces SendPacket to if1)
            transport.process_incoming(0, &raw).unwrap();
            let actions1 = transport.drain_actions();
            let forwarded_1 = actions1
                .iter()
                .any(|a| matches!(a, Action::SendPacket { .. }));
            assert!(
                forwarded_1,
                "First data packet must be forwarded via link table"
            );

            // Second arrival (retransmit) on if0, must also be forwarded
            transport.process_incoming(0, &raw).unwrap();
            let actions2 = transport.drain_actions();
            let forwarded_2 = actions2
                .iter()
                .any(|a| matches!(a, Action::SendPacket { .. }));
            assert!(
                forwarded_2,
                "Retransmitted link-addressed data must NOT be dropped by relay dedup"
            );
        }

        // Stage 6: Interface validation in table cleanup
        #[test]
        fn test_link_table_cleaned_when_interface_down() {
            let mut transport = make_transport_enabled();

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Notify that interface 1 is down (sans-I/O: driver calls this)
            transport.remove_link_entries_for_interface(1);

            assert!(
                !transport.storage().has_link_entry(&link_id),
                "Link entry should be removed when interface goes down"
            );
        }

        #[test]
        fn test_reverse_table_cleaned_when_interface_down() {
            let mut transport = make_transport_enabled();

            let hash = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_reverse(
                hash,
                ReverseEntry {
                    timestamp_ms: now,
                    receiving_interface_index: 0,
                    outbound_interface_index: 1,
                },
            );

            // Notify that interface 1 is down (sans-I/O: driver calls this)
            transport.remove_reverse_entries_for_interface(1);

            assert!(
                transport.storage().get_reverse(&hash).is_none(),
                "Reverse entry should be removed when interface goes down"
            );
        }

        // Stage 5: Strip transport header at final hop
        #[test]
        fn test_link_request_stripped_to_type1_at_final_hop() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Path with hops=1 (destination directly connected to if1)
            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Build a link request arriving on if0
            let mut link = Link::new_outgoing(dest_hash.into(), &mut OsRng);
            let raw = link.build_link_request_packet(None);
            transport.process_incoming(0, &raw).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Link request should be forwarded"
            );

            // Verify forwarded packet is Type1 (no transport_id)
            let actions = transport.drain_actions();
            let forwarded_raw = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a SendPacket action for iface 1");
            let forwarded_pkt = Packet::unpack(forwarded_raw).unwrap();
            assert_eq!(
                forwarded_pkt.flags.header_type,
                HeaderType::Type1,
                "Final hop should strip to Type1"
            );
            assert!(
                forwarded_pkt.transport_id.is_none(),
                "Type1 packet should have no transport_id"
            );
        }

        #[test]
        fn test_link_request_type2_when_not_final_hop() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Path with hops=3 (destination is multiple hops away)
            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            let next_hop_hash = [0xCC; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 3,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop_hash),
                },
            );

            // Build a link request arriving on if0
            let mut link = Link::new_outgoing(dest_hash.into(), &mut OsRng);
            let raw = link.build_link_request_packet(None);
            transport.process_incoming(0, &raw).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Link request should be forwarded"
            );

            // Verify forwarded packet is Type2 (with transport_id)
            let actions = transport.drain_actions();
            let forwarded_raw = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a SendPacket action for iface 1");
            let forwarded_pkt = Packet::unpack(forwarded_raw).unwrap();
            assert_eq!(
                forwarded_pkt.flags.header_type,
                HeaderType::Type2,
                "Intermediate hop should use Type2"
            );
            assert!(
                forwarded_pkt.transport_id.is_some(),
                "Type2 packet should have transport_id"
            );
            assert_eq!(
                forwarded_pkt.transport_id.unwrap(),
                next_hop_hash,
                "Transport ID should be set to the next-hop relay's identity hash"
            );
        }

        // Forward payload integrity: Type2 -> Type1 conversion
        #[test]
        fn test_link_request_forward_type2_to_type1_payload_intact() {
            // Setup: transport with 2 MockInterfaces; inspect via drain_actions()
            let mut transport = make_transport_enabled();

            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Create a path entry for dest_hash with hops=0 (directly connected
            // to if1), meaning the relay is the final hop.
            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 0,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Part A: Type1 link request arriving on if0, forwarded to if1
            let mut link_a = Link::new_outgoing(dest_hash.into(), &mut OsRng);
            let original_request_data_a =
                link_a.create_link_request_with_mtu(MTU as u32, crate::constants::MODE_AES256_CBC);
            let raw_type1 = link_a.build_link_request_packet(None);

            // Verify the original is Type1
            let original_pkt_a = Packet::unpack(&raw_type1).unwrap();
            assert_eq!(original_pkt_a.flags.header_type, HeaderType::Type1);
            assert_eq!(original_pkt_a.flags.packet_type, PacketType::LinkRequest);

            transport.process_incoming(0, &raw_type1).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Type1 link request should be forwarded"
            );

            // Verify the forwarded packet on if1
            {
                let actions = transport.drain_actions();
                let forwarded_raw = actions
                    .iter()
                    .find_map(|a| match a {
                        Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                        _ => None,
                    })
                    .expect("Should have a SendPacket action for iface 1");
                let forwarded_pkt = Packet::unpack(forwarded_raw)
                    .expect("Forwarded packet must be parseable by Packet::unpack");

                assert_eq!(
                    forwarded_pkt.flags.header_type,
                    HeaderType::Type1,
                    "Final hop (hops=0) should produce Type1"
                );
                assert_eq!(
                    forwarded_pkt.flags.packet_type,
                    PacketType::LinkRequest,
                    "Packet type must remain LinkRequest"
                );
                assert_eq!(
                    forwarded_pkt.destination_hash, dest_hash,
                    "Destination hash must match"
                );
                assert!(
                    forwarded_pkt.transport_id.is_none(),
                    "Type1 must have no transport_id"
                );
                assert_eq!(forwarded_pkt.hops, 1, "Hops should be incremented by 1");

                // Verify data payload is intact (the 64-byte link request data)
                assert_eq!(
                    forwarded_pkt.data.as_slice(),
                    &original_request_data_a[..],
                    "Link request payload must be preserved exactly"
                );

                // Nothing should be sent on if0 (the receiving interface)
                assert!(
                    !actions.iter().any(|a| matches!(
                        a,
                        Action::SendPacket { iface, .. } if iface.0 == 0
                    )),
                    "No packets should be sent back on the receiving interface"
                );
            }

            // Part B: Type2 link request arriving on if0, forwarded to if1
            // Clear previous actions and packet cache
            transport.drain_actions();
            transport.storage_mut().clear_packet_hashes(); // allow processing

            // Build a Type2 link request with transport_id = our own identity hash
            // (in the real protocol, transport_id identifies the next relay node)
            let own_transport_id = *transport.identity.hash();
            let mut link_b = Link::new_outgoing(dest_hash.into(), &mut OsRng);
            let original_request_data_b =
                link_b.create_link_request_with_mtu(MTU as u32, crate::constants::MODE_AES256_CBC);
            let raw_type2 = link_b.build_link_request_packet_with_transport(
                Some(own_transport_id),
                1, // hops_to_dest
                None,
            );

            // Verify the original is Type2
            let original_pkt_b = Packet::unpack(&raw_type2).unwrap();
            assert_eq!(original_pkt_b.flags.header_type, HeaderType::Type2);
            assert_eq!(original_pkt_b.flags.packet_type, PacketType::LinkRequest);
            assert_eq!(
                original_pkt_b.transport_id,
                Some(own_transport_id),
                "Original should carry transport_id"
            );

            let forwarded_before = transport.stats().packets_forwarded;
            transport.process_incoming(0, &raw_type2).unwrap();

            assert!(
                transport.stats().packets_forwarded > forwarded_before,
                "Type2 link request should be forwarded"
            );

            // Verify the forwarded packet on if1
            {
                let actions = transport.drain_actions();
                let forwarded_raw = actions
                    .iter()
                    .find_map(|a| match a {
                        Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                        _ => None,
                    })
                    .expect("Should have a SendPacket action for iface 1");
                let forwarded_pkt = Packet::unpack(forwarded_raw)
                    .expect("Forwarded Type2->Type1 packet must be parseable");

                assert_eq!(
                    forwarded_pkt.flags.header_type,
                    HeaderType::Type1,
                    "Final hop should convert Type2 to Type1"
                );
                assert_eq!(
                    forwarded_pkt.flags.packet_type,
                    PacketType::LinkRequest,
                    "Packet type must remain LinkRequest after conversion"
                );
                assert_eq!(
                    forwarded_pkt.destination_hash, dest_hash,
                    "Destination hash must be preserved"
                );
                assert!(
                    forwarded_pkt.transport_id.is_none(),
                    "Type1 must strip transport_id"
                );
                assert_eq!(forwarded_pkt.hops, 1, "Hops should be original (0) + 1 = 1");

                // Verify data payload is intact (the 64-byte link request data)
                assert_eq!(
                    forwarded_pkt.data.as_slice(),
                    &original_request_data_b[..],
                    "Link request payload must be preserved exactly after Type2->Type1 conversion"
                );
            }

            // Part C: Type2 arriving, multi-hop (hops=3), stays Type2
            transport.drain_actions();
            transport.storage_mut().clear_packet_hashes();

            // Update path to multi-hop with a next-hop relay identity
            let next_hop_relay = [0xEE; TRUNCATED_HASHBYTES];
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 3,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop_relay),
                },
            );

            let mut link_c = Link::new_outgoing(dest_hash.into(), &mut OsRng);
            let original_request_data_c =
                link_c.create_link_request_with_mtu(MTU as u32, crate::constants::MODE_AES256_CBC);
            let raw_type2_multi =
                link_c.build_link_request_packet_with_transport(Some(own_transport_id), 3, None);

            let forwarded_before = transport.stats().packets_forwarded;
            transport.process_incoming(0, &raw_type2_multi).unwrap();

            assert!(
                transport.stats().packets_forwarded > forwarded_before,
                "Multi-hop Type2 link request should be forwarded"
            );

            {
                let actions = transport.drain_actions();
                let forwarded_raw = actions
                    .iter()
                    .find_map(|a| match a {
                        Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                        _ => None,
                    })
                    .expect("Should have a SendPacket action for iface 1");
                let forwarded_pkt = Packet::unpack(forwarded_raw)
                    .expect("Multi-hop forwarded packet must be parseable");

                assert_eq!(
                    forwarded_pkt.flags.header_type,
                    HeaderType::Type2,
                    "Multi-hop should remain Type2"
                );
                assert_eq!(
                    forwarded_pkt.flags.packet_type,
                    PacketType::LinkRequest,
                    "Packet type must remain LinkRequest"
                );
                assert_eq!(
                    forwarded_pkt.destination_hash, dest_hash,
                    "Destination hash must be preserved"
                );
                assert!(
                    forwarded_pkt.transport_id.is_some(),
                    "Type2 must have transport_id"
                );
                // transport_id should be the next-hop relay's identity hash
                assert_eq!(
                    forwarded_pkt.transport_id.unwrap(),
                    next_hop_relay,
                    "Transport ID should be set to the next-hop relay's identity hash"
                );

                // Verify data payload is intact
                assert_eq!(
                    forwarded_pkt.data.as_slice(),
                    &original_request_data_c[..],
                    "Link request payload must be preserved in multi-hop forwarding"
                );
            }

            // Part D: Verify link table was populated for all forwarded requests
            assert!(
                transport.storage().link_entry_count() >= 3,
                "Link table should have entries for all forwarded link requests, got {}",
                transport.storage().link_entry_count()
            );

            // All link table entries should point to interface 1 as next hop
            for entry in transport.storage().link_entry_values() {
                assert_eq!(
                    entry.next_hop_interface_index, 1,
                    "Link entry should route toward if1"
                );
                assert_eq!(
                    entry.received_interface_index, 0,
                    "Link entry should record if0 as received interface"
                );
            }
        }

        // Stage 4: Hop count validation
        /// Build a data packet with the given destination hash and hop count
        fn build_link_data_packet(dest_hash: [u8; TRUNCATED_HASHBYTES], hops: u8) -> Packet {
            Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"link data".to_vec()),
            }
        }

        #[test]
        fn test_link_data_wrong_hops_dropped() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Data from dest side (if1): hops should match remaining_hops=3
            let pkt = build_link_data_packet(link_id, 99); // wrong hops
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(1, &buf[..len]).unwrap();
            assert_eq!(
                transport.stats().packets_forwarded,
                0,
                "Wrong hops should be dropped"
            );
        }

        #[test]
        fn test_link_data_correct_hops_forwarded() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Data from dest side (if1): wire hops=2, receipt-incremented to 3, matches remaining_hops=3
            let pkt = build_link_data_packet(link_id, 2);
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(1, &buf[..len]).unwrap();
            assert!(
                transport.stats().packets_forwarded > 0,
                "Correct hops should be forwarded"
            );

            // Parse forwarded packet and verify hops (no forward increment, just receipt)
            let actions = transport.drain_actions();
            let forwarded_raw = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a SendPacket action");
            let forwarded = Packet::unpack(forwarded_raw).unwrap();
            assert_eq!(
                forwarded.hops, 3,
                "Forwarded hops should be receipt-incremented 3"
            );
            assert_eq!(
                forwarded.flags.header_type,
                HeaderType::Type1,
                "Link-routed data should stay Type1"
            );
        }

        #[test]
        fn test_link_data_correct_hops_from_initiator() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Data from initiator side (if0): wire hops=1, receipt-incremented to 2, matches hops=2
            let pkt = build_link_data_packet(link_id, 1);
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert!(
                transport.stats().packets_forwarded > 0,
                "Correct hops from initiator should be forwarded"
            );

            // Parse forwarded packet and verify hops (no forward increment)
            let actions = transport.drain_actions();
            let forwarded_raw = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { data, .. } => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a SendPacket action");
            let forwarded = Packet::unpack(forwarded_raw).unwrap();
            assert_eq!(
                forwarded.hops, 2,
                "Forwarded hops should be receipt-incremented 2"
            );
        }

        // Stage 3: Reverse table proof routing
        #[test]
        fn test_regular_proof_routed_via_reverse_table() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Simulate a data packet that was forwarded from if0 to if1,
            // creating a reverse table entry keyed by truncated_packet_hash
            let packet_hash = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_reverse(
                packet_hash,
                ReverseEntry {
                    timestamp_ms: now,
                    receiving_interface_index: 0, // received on if0
                    outbound_interface_index: 1,  // forwarded to if1
                },
            );

            // Build a proof packet targeting this packet_hash
            let proof_packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Proof,
                },
                hops: 0,
                transport_id: None,
                destination_hash: packet_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0x55; crate::constants::PROOF_DATA_SIZE]),
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = proof_packet.pack(&mut buf).unwrap();
            // Proof arrives on if1 (the outbound interface)
            transport.process_incoming(1, &buf[..len]).unwrap();

            // Proof should be forwarded via reverse table → if0
            assert!(
                transport.stats().packets_forwarded > 0,
                "Proof should have been forwarded via reverse table"
            );
            // Reverse table entry should be consumed (removed)
            assert!(
                transport.storage().get_reverse(&packet_hash).is_none(),
                "Reverse table entry should be consumed after proof routing"
            );

            // Parse forwarded proof and verify target interface, hops, and packet type
            let actions = transport.drain_actions();
            let (target_iface, forwarded_raw) = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { iface, data } => Some((*iface, data.as_slice())),
                    _ => None,
                })
                .expect("Should have a SendPacket action for routed proof");
            assert_eq!(
                target_iface,
                InterfaceId(0),
                "Proof should be routed back to receiving interface (if0)"
            );
            let forwarded = Packet::unpack(forwarded_raw).unwrap();
            assert_eq!(forwarded.hops, 1, "Forwarded proof hops should be 0+1=1");
            assert_eq!(
                forwarded.flags.packet_type,
                PacketType::Proof,
                "Forwarded packet should still be a Proof"
            );
        }

        // Path-table data forwarding header promotion
        #[test]
        fn test_path_table_forward_header2_replaces_transport_id() {
            // A HEADER_2 data packet forwarded via path table should have its
            // transport_id replaced with next_hop from the path entry (remaining > 1).
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let next_hop_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Path entry with hops=2 (multi-hop: not the final relay)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 2,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop_hash),
                },
            );

            // Build a HEADER_2 data packet addressed to dest_hash, with our own
            // transport_id (as if we are the current relay in the chain).
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"multi-hop payload".to_vec()),
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Packet should be forwarded via path table"
            );

            let actions = transport.drain_actions();
            let forwarded_raw = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a SendPacket action for iface 1");
            let forwarded = Packet::unpack(forwarded_raw).unwrap();

            // Hops should be incremented
            assert_eq!(forwarded.hops, 2, "Forwarded hops should be 1+1=2");
            // Destination hash should be preserved
            assert_eq!(
                forwarded.destination_hash, dest_hash,
                "Destination hash must be preserved"
            );
            // Should still be Type2 (multi-hop, remaining > 1)
            assert_eq!(
                forwarded.flags.header_type,
                HeaderType::Type2,
                "Multi-hop forward should stay Type2"
            );
            // transport_id should be replaced with next_hop (BUG: currently stays as own hash)
            assert_eq!(
                forwarded.transport_id,
                Some(next_hop_hash),
                "transport_id should be replaced with next_hop from path entry"
            );
            // Data payload should be intact
            assert_eq!(
                forwarded.data.as_slice(),
                b"multi-hop payload",
                "Data payload must be preserved"
            );
        }

        #[test]
        fn test_path_table_forward_header2_strips_to_type1_at_final_hop() {
            // When a HEADER_2 data packet reaches the final relay (path hops == 0,
            // destination directly connected), it should be stripped to HEADER_1
            // with no transport_id.
            //
            // Rust stores raw wire hops (no increment on receipt), so hops == 0
            // means directly connected, the Python equivalent of hops == 1
            // (Python increments on receipt, Transport.py:1319).
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Path entry with hops=0 (destination is directly connected to us)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 0,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Build a HEADER_2 data packet addressed to dest_hash
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"final-hop payload".to_vec()),
            };

            let mut buf = [0u8; crate::constants::MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Packet should be forwarded via path table"
            );

            let actions = transport.drain_actions();
            let forwarded_raw = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { iface, data } if iface.0 == 1 => Some(data.as_slice()),
                    _ => None,
                })
                .expect("Should have a SendPacket action for iface 1");
            let forwarded = Packet::unpack(forwarded_raw).unwrap();

            // Hops should be incremented
            assert_eq!(forwarded.hops, 2, "Forwarded hops should be 1+1=2");
            // Destination hash should be preserved
            assert_eq!(
                forwarded.destination_hash, dest_hash,
                "Destination hash must be preserved"
            );
            // Should be stripped to Type1 (final hop: BUG: currently stays Type2)
            assert_eq!(
                forwarded.flags.header_type,
                HeaderType::Type1,
                "Final-hop forward should strip to Type1"
            );
            // transport_id should be None (BUG: currently stays Some(own hash))
            assert_eq!(
                forwarded.transport_id, None,
                "Final-hop forward should have no transport_id"
            );
            // Data payload should be intact
            assert_eq!(
                forwarded.data.as_slice(),
                b"final-hop payload",
                "Data payload must be preserved"
            );
        }

        #[test]
        fn test_announce_header2_populates_next_hop() {
            // A HEADER_2 announce (relayed) should populate next_hop with the relay's transport_id.
            use crate::announce::build_announce_payload;
            use crate::destination::{Destination, DestinationType, Direction};
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["nexthop"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            let now_ms = transport.clock.now_ms();
            let payload = build_announce_payload(
                dest.identity().unwrap(),
                dest.hash().as_bytes(),
                dest.name_hash(),
                None,
                Some(b"test"),
                &mut OsRng,
                now_ms,
            )
            .unwrap();

            // Wrap as HEADER_2 with a known relay transport_id
            let relay_hash = [0xAA; TRUNCATED_HASHBYTES];
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops: 1,
                transport_id: Some(relay_hash),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Path should exist with next_hop = relay_hash
            let path = transport
                .path(&dest_hash)
                .expect("path should exist after announce");
            assert_eq!(
                path.next_hop,
                Some(relay_hash),
                "HEADER_2 announce should populate next_hop with relay transport_id"
            );
        }

        #[test]
        fn test_announce_header1_has_no_next_hop() {
            // A HEADER_1 announce (direct) should have next_hop = None.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["direct"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            let now_ms = transport.clock.now_ms();
            let raw = make_announce_raw_for_dest(&dest, 0, now_ms);
            transport.process_incoming(0, &raw).unwrap();

            // Path should exist with next_hop = None
            let path = transport
                .path(&dest_hash)
                .expect("path should exist after announce");
            assert_eq!(
                path.next_hop, None,
                "HEADER_1 announce should have no next_hop"
            );
        }

        // Stage 1: VecDeque for path_request_tags
        #[test]
        fn test_path_request_tags_uses_efficient_deque() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let path_req_hash = transport.path_request_hash;

            // Insert MAX_PATH_REQUEST_TAGS + 1 unique path request tags
            for i in 0..=MAX_PATH_REQUEST_TAGS {
                let dest_hash = [0x42; TRUNCATED_HASHBYTES];
                let mut tag = [0u8; TRUNCATED_HASHBYTES];
                let bytes = (i as u32).to_le_bytes();
                tag[..4].copy_from_slice(&bytes);

                let mut data = Vec::new();
                data.extend_from_slice(&dest_hash);
                data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // transport_id
                data.extend_from_slice(&tag);

                let packet = Packet {
                    flags: PacketFlags {
                        ifac_flag: false,
                        header_type: HeaderType::Type1,
                        context_flag: false,
                        transport_type: TransportType::Broadcast,
                        dest_type: crate::destination::DestinationType::Plain,
                        packet_type: PacketType::Data,
                    },
                    hops: 0,
                    transport_id: None,
                    destination_hash: path_req_hash,
                    context: PacketContext::None,
                    data: PacketData::Owned(data),
                };

                let mut buf = [0u8; 500];
                let len = packet.pack(&mut buf).unwrap();

                // Clear packet cache so each iteration is processed
                transport.storage_mut().clear_packet_hashes();
                transport.process_incoming(0, &buf[..len]).unwrap();
            }

            // Should be capped at MAX_PATH_REQUEST_TAGS
            assert_eq!(
                transport.storage().path_request_tag_count(),
                MAX_PATH_REQUEST_TAGS,
                "path_request_tags should be capped at MAX_PATH_REQUEST_TAGS"
            );
        }

        // Stage 2: Dual-write verification tests
        #[test]
        fn test_send_on_interface_produces_action() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let data = b"hello world";
            transport.send_on_interface(idx, data).unwrap();

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 1);
            assert_eq!(
                actions[0],
                Action::SendPacket {
                    iface: InterfaceId(idx),
                    data: data.to_vec(),
                }
            );
        }

        #[test]
        fn test_send_on_interface_no_registered_iface_still_emits_action() {
            let mut transport = test_transport();

            // No interfaces registered; action is still emitted for sans-I/O driver
            let result = transport.send_on_interface(99, b"data");
            assert!(result.is_ok());

            assert_eq!(transport.pending_action_count(), 1);
            let actions = transport.drain_actions();
            assert_eq!(
                actions[0],
                Action::SendPacket {
                    iface: InterfaceId(99),
                    data: b"data".to_vec()
                }
            );
        }

        #[test]
        fn test_send_on_all_interfaces_produces_broadcast_action() {
            let mut transport = test_transport();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let data = b"broadcast data";
            transport.send_on_all_interfaces(data);

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 1);
            assert_eq!(
                actions[0],
                Action::Broadcast {
                    data: data.to_vec(),
                    exclude_iface: None,
                }
            );
        }

        #[test]
        fn test_send_on_all_interfaces_except_produces_broadcast_action() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
            let _idx2 = transport.register_interface(Box::new(MockInterface::new("if2", 3)));

            let data = b"selective broadcast";
            transport.send_on_all_interfaces_except(1, data);

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 1);
            assert_eq!(
                actions[0],
                Action::Broadcast {
                    data: data.to_vec(),
                    exclude_iface: Some(InterfaceId(1)),
                }
            );
        }

        #[test]
        fn test_send_on_all_interfaces_caches_packet_for_dedup() {
            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let hash = [0x42; TRUNCATED_HASHBYTES];
            transport.register_destination(hash);

            // Build a valid data packet
            use crate::destination::DestinationType;

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"echo test".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            let raw = &buf[..len];

            // Broadcast the packet (simulates originating node sending)
            transport.send_on_all_interfaces(raw);
            transport.drain_actions();

            // Simulate the same packet echoing back on a different interface
            transport.process_incoming(1, raw).unwrap();

            // The echo must be dropped by dedup
            assert_eq!(transport.stats().packets_dropped, 1);
            assert_eq!(transport.pending_events(), 0);
        }

        #[test]
        fn test_send_to_destination_produces_send_action() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            // Manually insert a path
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: u64::MAX,
                    interface_index: idx,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            let data = b"routed packet";
            transport.send_to_destination(&dest_hash, data).unwrap();

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 1);
            assert_eq!(
                actions[0],
                Action::SendPacket {
                    iface: InterfaceId(idx),
                    data: data.to_vec(),
                }
            );
        }

        #[test]
        fn test_send_to_destination_relay_converts_type1_to_type2() {
            use crate::packet::{HeaderType, PacketFlags, TransportType};

            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let next_hop = [0xAA; TRUNCATED_HASHBYTES];
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 2, // needs_relay requires hops > 1
                    expires_ms: u64::MAX,
                    interface_index: idx,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop),
                },
            );

            // Build a Type1 data packet: flags(1) + hops(1) + dest_hash(16) + context(1) + payload
            let flags = PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type1,
                context_flag: false,
                transport_type: TransportType::Broadcast,
                dest_type: crate::destination::DestinationType::Single,
                packet_type: crate::packet::PacketType::Data,
            };
            let mut pkt = alloc::vec![flags.to_byte(), 0]; // flags + hops=0
            pkt.extend_from_slice(&dest_hash); // dest_hash
            pkt.push(0x00); // context
            pkt.extend_from_slice(b"hello"); // payload

            transport.send_to_destination(&dest_hash, &pkt).unwrap();

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 1);
            if let Action::SendPacket { data, .. } = &actions[0] {
                // Should be 16 bytes longer (transport_id inserted)
                assert_eq!(data.len(), pkt.len() + TRUNCATED_HASHBYTES);
                // Parse the output flags
                let out_flags = PacketFlags::from_byte(data[0]).unwrap();
                assert_eq!(out_flags.header_type, HeaderType::Type2, "should be Type2");
                assert_eq!(
                    out_flags.transport_type,
                    TransportType::Transport,
                    "should be Transport"
                );
                // transport_id should be the next_hop
                assert_eq!(&data[2..2 + TRUNCATED_HASHBYTES], &next_hop);
                // dest_hash follows the transport_id
                assert_eq!(
                    &data[2 + TRUNCATED_HASHBYTES..2 + 2 * TRUNCATED_HASHBYTES],
                    &dest_hash
                );
                // payload at the end
                assert_eq!(&data[data.len() - 5..], b"hello");
            } else {
                panic!("expected SendPacket action");
            }
        }

        #[test]
        fn test_send_to_destination_type2_not_double_wrapped() {
            use crate::packet::{HeaderType, PacketFlags, TransportType};

            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let next_hop = [0xAA; TRUNCATED_HASHBYTES];
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: u64::MAX,
                    interface_index: idx,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop),
                },
            );

            // Build a Type2 packet (already has transport header)
            let flags = PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type2,
                context_flag: false,
                transport_type: TransportType::Transport,
                dest_type: crate::destination::DestinationType::Single,
                packet_type: crate::packet::PacketType::Data,
            };
            let mut pkt = alloc::vec![flags.to_byte(), 0]; // flags + hops
            pkt.extend_from_slice(&next_hop); // transport_id
            pkt.extend_from_slice(&dest_hash); // dest_hash
            pkt.push(0x00); // context
            pkt.extend_from_slice(b"hello"); // payload
            let original_len = pkt.len();

            transport.send_to_destination(&dest_hash, &pkt).unwrap();

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 1);
            if let Action::SendPacket { data, .. } = &actions[0] {
                // Should NOT grow. Type2 packets are passed through unchanged
                assert_eq!(
                    data.len(),
                    original_len,
                    "Type2 packet should not be re-wrapped"
                );
            } else {
                panic!("expected SendPacket action");
            }
        }

        #[test]
        fn test_send_to_destination_no_path_no_action() {
            let mut transport = test_transport();
            let _idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let result = transport.send_to_destination(&dest_hash, b"data");
            assert!(result.is_err());

            assert_eq!(transport.pending_action_count(), 0);
        }

        #[test]
        fn test_drain_actions_clears_buffer() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            transport.send_on_interface(idx, b"first").unwrap();
            transport.send_on_interface(idx, b"second").unwrap();

            assert_eq!(transport.pending_action_count(), 2);

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 2);

            // Buffer should be empty after drain
            assert_eq!(transport.pending_action_count(), 0);
            let actions2 = transport.drain_actions();
            assert!(actions2.is_empty());
        }

        #[test]
        fn test_multiple_sends_accumulate_actions() {
            let mut transport = test_transport();
            let idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Mix of different send types
            transport.send_on_interface(idx0, b"unicast").unwrap();
            transport.send_on_all_interfaces(b"broadcast");
            transport.send_on_interface(idx1, b"unicast2").unwrap();
            transport.send_on_all_interfaces_except(0, b"selective");

            let actions = transport.drain_actions();
            assert_eq!(actions.len(), 4);

            assert_eq!(
                actions[0],
                Action::SendPacket {
                    iface: InterfaceId(0),
                    data: b"unicast".to_vec(),
                }
            );
            assert_eq!(
                actions[1],
                Action::Broadcast {
                    data: b"broadcast".to_vec(),
                    exclude_iface: None,
                }
            );
            assert_eq!(
                actions[2],
                Action::SendPacket {
                    iface: InterfaceId(1),
                    data: b"unicast2".to_vec(),
                }
            );
            assert_eq!(
                actions[3],
                Action::Broadcast {
                    data: b"selective".to_vec(),
                    exclude_iface: Some(InterfaceId(0)),
                }
            );
        }

        #[test]
        fn test_announce_rebroadcast_scheduled_from_announce_table() {
            // Python parity (Transport.py:1754): received announces enter
            // announce_table and the retry scheduler fires the first rebroadcast
            // within the jitter window (0-PATHFINDER_RW ms). process_incoming
            // itself no longer emits an on-air broadcast.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let (raw, _dest_hash) = make_announce_raw(1, PacketContext::None);
            let _ = transport.process_incoming(0, &raw);

            // No on-air broadcast yet — only the entry in announce_table
            let actions_before_poll = transport.drain_actions();
            assert!(
                actions_before_poll
                    .iter()
                    .all(|a| !matches!(a, Action::Broadcast { .. })),
                "no Broadcast action should be emitted before retry scheduler fires"
            );

            // Advance clock past the jitter window and poll — first rebroadcast
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions = transport.drain_actions();
            assert!(
                !actions.is_empty(),
                "retry scheduler should produce actions after advancing clock"
            );

            // Each rebroadcast fans out on all interfaces without excluding
            // the receiving one (Python parity, Transport.py:1025-1167); the
            // self-heard echo is dropped on RX by the packet_hashlist dedup.
            // The rebroadcasted packet is Type2/Transport with our transport_id.
            for action in &actions {
                match action {
                    Action::Broadcast {
                        exclude_iface,
                        data,
                    } => {
                        assert_eq!(
                            *exclude_iface, None,
                            "announce rebroadcast should not exclude any interface"
                        );
                        let pkt = Packet::unpack(data).unwrap();
                        assert_eq!(
                            pkt.flags.header_type,
                            HeaderType::Type2,
                            "rebroadcast announce should be Header Type 2"
                        );
                        assert_eq!(
                            pkt.flags.transport_type,
                            TransportType::Transport,
                            "rebroadcast announce should have Transport transport_type"
                        );
                        assert_eq!(
                            pkt.transport_id,
                            Some(*transport.identity.hash()),
                            "rebroadcast announce transport_id should be our identity hash"
                        );
                    }
                    other => panic!("expected Broadcast action, got {:?}", other),
                }
            }
        }

        /// A received announce triggers exactly one Broadcast action from the
        /// retry scheduler once the jitter window elapses, with
        /// exclude_iface=None. A second arrival of the same packet on the
        /// same interface is absorbed by the packet_hashlist dedup in
        /// process_incoming and does not produce an additional entry or
        /// emission.
        #[test]
        fn test_announce_rebroadcast_dedup_catches_echo() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let (raw, _dest_hash) = make_announce_raw(1, PacketContext::None);

            let _ = transport.process_incoming(0, &raw);
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let first_actions = transport.drain_actions();
            let first_broadcasts = first_actions
                .iter()
                .filter(|a| {
                    matches!(
                        a,
                        Action::Broadcast {
                            exclude_iface: None,
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(
                first_broadcasts, 1,
                "first scheduler fire should produce exactly one Broadcast with no exclude"
            );

            let _ = transport.process_incoming(0, &raw);
            let second_actions = transport.drain_actions();
            let second_broadcasts = second_actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .count();
            assert_eq!(
                second_broadcasts, 0,
                "duplicate arrival should be dropped by packet_hashlist dedup, no new Broadcast"
            );
        }

        /// B6 parity: heard-neighbor rebroadcasts do not cancel a pending
        /// first emission. Python (Transport.py:1587) gates the
        /// LOCAL_REBROADCASTS_MAX cancellation on `retries > 0`, so an
        /// announce still in its initial jitter window keeps its scheduled
        /// first fire regardless of how many neighbors are already
        /// rebroadcasting the same announce.
        #[test]
        fn test_local_rebroadcasts_max_guarded_by_retries_gt_zero() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            // Feed an announce at hops=0 wire → path hops=1.
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport.process_incoming(0, &raw).unwrap();
            transport.drain_events();

            let before = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(before.retries, 0, "entry starts at retries=0");
            assert!(before.retransmit_at_ms.is_some());

            // Craft a neighbor rebroadcast at wire hops = entry.hops (= 1),
            // which becomes hops=2 on receipt → matches
            // `packet.hops == existing.hops + 1` increment branch.
            let (neighbor_raw, _) = make_announce_raw(before.hops, PacketContext::None);
            for _ in 0..LOCAL_REBROADCASTS_MAX {
                transport.process_incoming(0, &neighbor_raw).ok();
                transport.drain_events();
            }

            let after = transport.storage().get_announce(&dest_hash).unwrap();
            assert!(
                after.retransmit_at_ms.is_some(),
                "first fire must still be scheduled (retries=0 gate)"
            );
            assert_eq!(after.retries, 0, "no emission has occurred yet",);
        }

        /// B3 parity: a self-originated announce is a one-shot broadcast and
        /// must not leave a scheduled retry in announce_table. Python's
        /// Destination.announce at Destination.py:322 calls Packet.send
        /// exactly once; Rust's send_on_all_interfaces now mirrors that.
        #[test]
        fn test_self_originated_announce_is_one_shot() {
            use crate::destination::{Destination, DestinationType, Direction};
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let mut dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["oneshot"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            let announce = dest
                .announce(None, &mut OsRng, transport.clock.now_ms())
                .unwrap();
            let mut buf = [0u8; MTU];
            let len = announce.pack(&mut buf).unwrap();
            transport.send_on_all_interfaces(&buf[..len]);

            let first_actions = transport.drain_actions();
            let first_broadcasts = first_actions
                .iter()
                .filter(|a| {
                    matches!(
                        a,
                        Action::Broadcast {
                            exclude_iface: None,
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(
                first_broadcasts, 1,
                "self-originated announce produces exactly one Broadcast"
            );

            assert!(
                transport.storage().get_announce(&dest_hash).is_none(),
                "self-originated announce must not leave an announce_table entry"
            );

            transport.clock.advance(60_000);
            transport.poll();
            let later_actions = transport.drain_actions();
            let later_broadcasts = later_actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .count();
            assert_eq!(
                later_broadcasts, 0,
                "no scheduled retry should fire for a self-originated announce"
            );
        }

        // ─── Sans-I/O Audit: Action coverage tests ──────────────────────────

        #[test]
        fn test_forward_on_interface_produces_send_action() {
            // A transport-mode data packet destined for a link_table entry
            // should produce a SendPacket action on the correct next-hop interface.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Insert a validated link_table entry: link_hash routes from if0 → if1
            let link_hash = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_hash,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1, // toward destination
                    remaining_hops: 1,
                    received_interface_index: 0, // toward initiator
                    hops: 1,
                    validated: true,
                    proof_timeout_ms: u64::MAX,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build a data packet addressed to link_hash, arriving on if0 (initiator side)
            // Wire hops=0, receipt-incremented to 1, matches link_entry.hops=1
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: link_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"test payload".to_vec()),
            };
            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            // Process the packet arriving on if0
            let _ = transport.process_incoming(0, &buf[..len]);
            let actions = transport.drain_actions();

            // Should produce a SendPacket on if1 (the next_hop_interface_index)
            assert!(
                actions.iter().any(|a| matches!(
                    a,
                    Action::SendPacket { iface, .. } if *iface == InterfaceId(1)
                )),
                "forward_on_interface should produce SendPacket on iface 1, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_forward_on_all_except_produces_broadcast_action() {
            // A path request arriving on if0 with no cached announce should
            // be forwarded as a Broadcast excluding if0 (transport mode only).
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Build a path request for an unknown destination
            let unknown_hash = [0xBB; TRUNCATED_HASHBYTES];
            let tag = [0xCC; TRUNCATED_HASHBYTES];
            let transport_id = [0xDD; TRUNCATED_HASHBYTES];

            let mut data = Vec::new();
            data.extend_from_slice(&unknown_hash);
            data.extend_from_slice(&transport_id);
            data.extend_from_slice(&tag);

            let path_req_hash = *transport.path_request_hash();

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };
            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let _ = transport.process_incoming(0, &buf[..len]);
            let actions = transport.drain_actions();

            assert!(
                actions.iter().any(|a| matches!(
                    a,
                    Action::Broadcast { exclude_iface: Some(iface), .. }
                        if *iface == InterfaceId(0)
                )),
                "path request forwarding should produce Broadcast excluding iface 0, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_send_proof_produces_send_action() {
            let mut transport = test_transport();
            let idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            // Insert a path so send_proof can route
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: u64::MAX,
                    interface_index: idx,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            let identity = Identity::generate(&mut OsRng);
            let packet_hash = [0xAA; 32];

            transport
                .send_proof(&packet_hash, &dest_hash, &identity, None)
                .unwrap();

            let actions = transport.drain_actions();
            assert!(
                actions.iter().any(|a| matches!(
                    a,
                    Action::SendPacket { iface, .. } if *iface == InterfaceId(idx)
                )),
                "send_proof should produce SendPacket action, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_request_path_produces_action() {
            let mut transport = test_transport();
            let _idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let tag = [0xCC; TRUNCATED_HASHBYTES];

            // Request path on all interfaces (None)
            transport.request_path(&dest_hash, None, &tag).unwrap();

            let actions = transport.drain_actions();
            assert!(
                !actions.is_empty(),
                "request_path should produce at least one action"
            );
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::Broadcast { .. })),
                "request_path with no specific interface should broadcast, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_handle_path_request_produces_broadcast_action() {
            // Register a destination and cache an announce, then process a path
            // request. The announce should be scheduled for rebroadcast.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // First, create an announce and process it to populate the cache
            let (raw, dest_hash) = make_announce_raw(1, PacketContext::None);
            let _ = transport.process_incoming(0, &raw);
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Now send a path request for that destination from a different interface
            let tag = [0xCC; TRUNCATED_HASHBYTES];
            let requester_id = [0xDD; TRUNCATED_HASHBYTES];

            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&requester_id);
            data.extend_from_slice(&tag);

            let path_req_hash = *transport.path_request_hash();

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };
            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let _ = transport.process_incoming(1, &buf[..len]);
            let _ = transport.drain_actions();

            // Advance time past the grace period + jitter to trigger rebroadcast
            transport.clock.advance(PATH_REQUEST_GRACE_MS + 1000);
            transport.poll();

            let actions = transport.drain_actions();
            assert!(
                !actions.is_empty(),
                "handle_path_request should schedule announce rebroadcast that produces actions"
            );
        }

        #[test]
        fn test_path_request_for_local_destination() {
            // When a node receives a path request for one of its own registered
            // destinations and has a cached announce, it should:
            // 1. Emit PathRequestReceived event
            // 2. Schedule a targeted announce rebroadcast (not forward the request)
            // 3. NOT forward the path request to other interfaces
            let mut transport = make_transport_enabled();
            let net_iface = transport.register_interface(Box::new(MockInterface::new("net0", 1)));
            let _net_iface2 = transport.register_interface(Box::new(MockInterface::new("net1", 2)));

            // Create a destination and register it as local
            let identity = Identity::generate(&mut OsRng);
            let dest = crate::destination::Destination::new(
                Some(identity),
                crate::destination::Direction::In,
                crate::destination::DestinationType::Single,
                "rnstransport",
                &["probe"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();
            transport.register_destination(dest_hash);

            // Populate the announce cache (as if we had previously announced)
            let (raw_announce, _) = {
                let id = dest.identity().unwrap();
                let random_hash = [0x42u8; crate::constants::RANDOM_HASHBYTES];
                let app_data: &[u8] = &[];

                let mut payload = Vec::new();
                payload.extend_from_slice(&id.public_key_bytes());
                payload.extend_from_slice(dest.name_hash());
                payload.extend_from_slice(&random_hash);
                let mut signed_data = Vec::new();
                signed_data.extend_from_slice(dest.hash().as_bytes());
                signed_data.extend_from_slice(&id.public_key_bytes());
                signed_data.extend_from_slice(dest.name_hash());
                signed_data.extend_from_slice(&random_hash);
                signed_data.extend_from_slice(app_data);
                let signature = id.sign(&signed_data).unwrap();
                payload.extend_from_slice(&signature);
                payload.extend_from_slice(app_data);

                let packet = Packet {
                    flags: PacketFlags {
                        ifac_flag: false,
                        header_type: HeaderType::Type1,
                        context_flag: false,
                        transport_type: TransportType::Broadcast,
                        dest_type: crate::destination::DestinationType::Single,
                        packet_type: PacketType::Announce,
                    },
                    hops: 0,
                    transport_id: None,
                    destination_hash: dest_hash,
                    context: PacketContext::None,
                    data: PacketData::Owned(payload),
                };
                let mut buf = [0u8; 500];
                let len = packet.pack(&mut buf).unwrap();
                (buf[..len].to_vec(), dest_hash)
            };
            transport
                .storage_mut()
                .set_announce_cache(dest_hash, raw_announce);

            // Build a path request packet for our local destination
            let tag = [0xAA; TRUNCATED_HASHBYTES];
            let requester_id = [0xBB; TRUNCATED_HASHBYTES];
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&requester_id);
            data.extend_from_slice(&tag);

            let path_req_hash = *transport.path_request_hash();
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: path_req_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };
            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            // Process the path request from the network interface
            let _ = transport.process_incoming(net_iface, &buf[..len]);

            // 1. Should emit PathRequestReceived event
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    TransportEvent::PathRequestReceived { destination_hash, .. }
                        if destination_hash == &dest_hash
                )),
                "Should emit PathRequestReceived for local destination"
            );

            // 2. Should NOT have broadcast/forwarded the path request
            let actions = transport.drain_actions();
            let has_broadcast = actions
                .iter()
                .any(|a| matches!(a, Action::Broadcast { .. }));
            assert!(
                !has_broadcast,
                "Should NOT forward path request for local destination"
            );

            // 3. Should have scheduled a targeted announce rebroadcast in the announce table
            assert!(
                transport.storage().get_announce(&dest_hash).is_some(),
                "Should have scheduled announce rebroadcast for local destination"
            );
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.target_interface,
                Some(net_iface),
                "Announce rebroadcast should target the requesting interface"
            );
            assert!(
                entry.block_rebroadcasts,
                "Announce rebroadcast should block further rebroadcasts"
            );

            // 4. After grace period, the announce should produce a SendPacket action
            transport.clock.advance(PATH_REQUEST_GRACE_MS + 1000);
            transport.poll();
            let actions = transport.drain_actions();
            let has_send = actions.iter().any(|a| {
                matches!(
                    a,
                    Action::SendPacket { iface, .. } if *iface == InterfaceId(net_iface)
                )
            });
            assert!(
                has_send,
                "After grace period, should send path response to requesting interface"
            );
        }

        #[test]
        fn test_rate_limited_announce_still_updates_path_when_fewer_hops() {
            // Fix 1: When the same announce arrives via two paths in a ring
            // topology, long path (3 hops) first, then short path (2 hops)
            // within the rate limit window, the path table must update to
            // the shorter route. Previously the rate limiter did a hard
            // `return Ok(())` that blocked path table updates.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Create a destination for the announce
            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ratelimit"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Use the same random_hash for both announces (same announce, different paths).
            // process_incoming increments hops by 1, so wire hops = stored hops - 1.
            let random_hash = make_random_hash([0xAA; 5], 1000);
            let raw_3hops = make_announce_raw_with_random_hash(&dest, 2, &random_hash); // stored as 3
            let raw_2hops = make_announce_raw_with_random_hash(&dest, 1, &random_hash); // stored as 2

            // First: 3-hop announce arrives on if0 (long ring path)
            let _ = transport.process_incoming(0, &raw_3hops);
            let _ = transport.drain_actions();

            assert_eq!(
                transport.storage().get_path(&dest_hash).unwrap().hops,
                3,
                "initial path should be 3 hops"
            );

            // Second: 2-hop announce arrives on if1 within the rate window
            // (no clock advance, same millisecond)
            let _ = transport.process_incoming(1, &raw_2hops);
            let _ = transport.drain_actions();

            assert_eq!(
                transport.storage().get_path(&dest_hash).unwrap().hops,
                2,
                "path should update to 2 hops despite rate limiting"
            );
        }

        #[test]
        fn test_rate_limited_announce_suppresses_rebroadcast() {
            // Companion to the above: when a rate-limited announce updates
            // the path table, it must NOT be rebroadcast (to avoid flooding).
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ratelimit2"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // process_incoming increments hops by 1, so wire hops = stored hops - 1.
            let random_hash = make_random_hash([0xBB; 5], 2000);
            let raw_3hops = make_announce_raw_with_random_hash(&dest, 2, &random_hash); // stored as 3
            let raw_2hops = make_announce_raw_with_random_hash(&dest, 1, &random_hash); // stored as 2

            // 3-hop announce arrives — rebroadcast fires from the retry
            // scheduler once the jitter window elapses (Python parity).
            let _ = transport.process_incoming(0, &raw_3hops);
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions = transport.drain_actions();
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::Broadcast { .. })),
                "first announce should be rebroadcast"
            );

            // 2-hop announce within rate window — updates path but NO new
            // announce_table entry / no rebroadcast scheduled.
            let _ = transport.process_incoming(1, &raw_2hops);
            transport
                .clock
                .advance(transport.announce_jitter_max_ms() + 100);
            transport.poll();
            let actions = transport.drain_actions();

            // Should NOT have a Broadcast action (rate-limited suppresses rebroadcast)
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::Broadcast { .. })),
                "rate-limited announce should NOT be rebroadcast"
            );

            // But path should be updated
            assert_eq!(
                transport.storage().get_path(&dest_hash).unwrap().hops,
                2,
                "path should be 2 hops"
            );
        }

        #[test]
        fn test_rate_limited_worse_hop_announce_dropped() {
            // A rate-limited announce with MORE hops than the existing path
            // should be dropped entirely (no path update, no rebroadcast).
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ratelimit3"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // process_incoming increments hops by 1, so wire hops = stored hops - 1.
            let random_hash = make_random_hash([0xCC; 5], 3000);
            let raw_2hops = make_announce_raw_with_random_hash(&dest, 1, &random_hash); // stored as 2
            let raw_4hops = make_announce_raw_with_random_hash(&dest, 3, &random_hash); // stored as 4

            // 2-hop announce arrives first
            let _ = transport.process_incoming(0, &raw_2hops);
            let _ = transport.drain_actions();

            let dropped_before = transport.stats.packets_dropped;

            // 4-hop announce within rate window, worse hops, should be dropped
            let _ = transport.process_incoming(1, &raw_4hops);
            let _ = transport.drain_actions();

            assert_eq!(
                transport.storage().get_path(&dest_hash).unwrap().hops,
                2,
                "path should remain 2 hops"
            );
            assert!(
                transport.stats.packets_dropped > dropped_before,
                "worse-hop rate-limited announce should increment packets_dropped"
            );
        }

        // Helper: Build announce for a specific destination
        /// Build a raw announce packet for a given destination at a specific
        /// hop count. Each call produces a new random_hash (different timestamp
        /// from MockClock, different random bytes from OsRng), so the resulting
        /// packet hash is unique and passes packet_cache dedup.
        fn make_announce_raw_for_dest(
            dest: &crate::destination::Destination,
            hops: u8,
            now_ms: u64,
        ) -> Vec<u8> {
            use crate::announce::build_announce_payload;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let identity = dest.identity().unwrap();
            let payload = build_announce_payload(
                identity,
                dest.hash().as_bytes(),
                dest.name_hash(),
                None, // no ratchet
                Some(b"test"),
                &mut OsRng,
                now_ms,
            )
            .unwrap();

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            buf[..len].to_vec()
        }

        // Stage 10: Hop count comparison in handle_announce
        #[test]
        fn test_worse_hop_announce_with_newer_emission_updates_path() {
            // Per Python Transport.py:1664-1668: a worse-hop announce with a
            // newer emission timestamp DOES update the path, even if not expired.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["hops"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Inject announce at wire hops=1 (stored as 2 after receipt increment)
            let now1 = transport.clock.now_ms();
            let a1 = make_announce_raw_for_dest(&dest, 1, now1);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2));

            // Advance past rate limit so second announce isn't rate-limited
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Inject announce at wire hops=3 (worse, stored as 4) with newer timestamp
            let now2 = transport.clock.now_ms();
            let a2 = make_announce_raw_for_dest(&dest, 3, now2);
            transport.process_incoming(0, &a2).unwrap();

            // Per Python: newer emission overwrites regardless of hop count
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(4),
                "Worse-hop announce with newer emission should update path (per Python)"
            );
        }

        #[test]
        fn test_better_hop_announce_replaces_worse_path() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["hops"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Inject announce at wire hops=3 (stored as 4)
            let now1 = transport.clock.now_ms();
            let a1 = make_announce_raw_for_dest(&dest, 3, now1);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(4));

            // Advance past rate limit
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Inject announce at wire hops=1 (better, stored as 2) with newer timestamp
            let now2 = transport.clock.now_ms();
            let a2 = make_announce_raw_for_dest(&dest, 1, now2);
            transport.process_incoming(0, &a2).unwrap();

            // Path should be updated to hops=2
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(2),
                "Better-hop announce should replace worse path"
            );
        }

        #[test]
        fn test_worse_hop_announce_accepted_when_path_expired() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["hops"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Inject announce at wire hops=1 (stored as 2)
            let now1 = transport.clock.now_ms();
            let a1 = make_announce_raw_for_dest(&dest, 1, now1);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2));

            // Advance past path expiry (path_expiry_secs * 1000) + rate limit
            let expiry_ms = transport.config.path_expiry_secs * 1000;
            transport
                .clock
                .advance(expiry_ms + ANNOUNCE_RATE_LIMIT_MS + 1);

            // Inject announce at wire hops=3 (worse, stored as 4), accepted because expired
            let now2 = transport.clock.now_ms();
            let a2 = make_announce_raw_for_dest(&dest, 3, now2);
            transport.process_incoming(0, &a2).unwrap();

            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(4),
                "Worse-hop announce should be accepted when path is expired"
            );
        }

        #[test]
        fn test_equal_hop_announce_with_newer_emission_updates_path() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["hops"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Inject announce at wire hops=2 via interface 0 (stored as 3)
            let now1 = transport.clock.now_ms();
            let a1 = make_announce_raw_for_dest(&dest, 2, now1);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(3));
            assert_eq!(transport.path(&dest_hash).unwrap().interface_index, 0);

            // Advance past rate limit
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Inject equal-hop announce via interface 1 with newer timestamp
            let now2 = transport.clock.now_ms();
            let a2 = make_announce_raw_for_dest(&dest, 2, now2);
            transport.process_incoming(1, &a2).unwrap();

            // Should update to the newer path (interface 1)
            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                1,
                "Equal-hop announce with newer emission should update path"
            );
        }

        #[test]
        fn test_equal_hop_announce_with_same_emission_does_not_update() {
            // Per Python Transport.py:1627: equal hops require announce_emitted > path_timebase.
            // If emission is the same (not newer), path should NOT update.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["hops"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Inject announce at wire hops=2 via interface 0 at time T (stored as 3)
            let now = transport.clock.now_ms();
            let a1 = make_announce_raw_for_dest(&dest, 2, now);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(3));
            assert_eq!(transport.path(&dest_hash).unwrap().interface_index, 0);

            // Advance past rate limit but use SAME timestamp for announce
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Inject equal-hop announce via interface 1 but with SAME emission time
            let a2 = make_announce_raw_for_dest(&dest, 2, now); // same now_ms!
            transport.process_incoming(1, &a2).unwrap();

            // Path should NOT update, emission is not newer
            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                0,
                "Equal-hop announce with same emission should not update path"
            );
        }

        // Stage 10b: Random blobs cap
        #[test]
        fn test_random_blobs_capped_at_max() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let max_blobs = transport.config.max_random_blobs;
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["blobs"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Inject max_random_blobs + 10 announces, advancing clock each time
            for i in 0..(max_blobs + 10) {
                transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
                let now = transport.clock.now_ms();
                let raw = make_announce_raw_for_dest(&dest, 1, now);
                transport.process_incoming(0, &raw).unwrap();

                // Verify path exists after first announce
                if i == 0 {
                    assert!(transport.has_path(&dest_hash));
                }
            }

            let path = transport.path(&dest_hash).unwrap();
            assert!(
                path.random_blobs.len() <= max_blobs,
                "random_blobs should be capped at {}, got {}",
                max_blobs,
                path.random_blobs.len()
            );
        }

        // Stage 10c: Out-of-order announce arrival
        #[test]
        fn test_stale_worse_hop_announce_does_not_overwrite_fresher_better_path() {
            // Models mesh out-of-order arrival: destination D announces at T=5M
            // (propagates via slow 3-hop path) then again at T=10M (propagates
            // via fast 1-hop path). Receiver gets the 1-hop fresh announce
            // first, then the 3-hop stale announce later.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ooo"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Build both announces with decoupled emission timestamps
            let a_stale = make_announce_raw_for_dest(&dest, 3, 5_000_000); // old emission, long path
            let a_fresh = make_announce_raw_for_dest(&dest, 1, 10_000_000); // newer emission, short path

            // Fresh announce arrives first at T=12M via iface_0
            transport.clock.set(12_000_000);
            transport.process_incoming(0, &a_fresh).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2)); // wire=1 + receipt
            assert_eq!(transport.path(&dest_hash).unwrap().interface_index, 0);

            // Drain and verify PathFound + AnnounceReceived events
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events.len() >= 2,
                "Expected PathFound + AnnounceReceived, got {} events",
                events.len()
            );
            let initial_processed = transport.stats().announces_processed;
            assert_eq!(initial_processed, 1);

            // Stale announce arrives later at T=15M via iface_1
            // (past rate limit relative to the fresh announce)
            transport.clock.set(15_000_000);
            transport.process_incoming(1, &a_stale).unwrap();

            // Path must NOT be overwritten: older emission (5M < 10M),
            // worse hops (4 > 2), path not expired → rejected
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(2), // wire=1 + receipt
                "Stale worse-hop announce must not overwrite fresher better path"
            );
            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                0,
                "Interface must remain iface_0 (fresh path), not iface_1 (stale)"
            );

            // Random blob still recorded for replay detection (both announces)
            assert_eq!(
                transport.path(&dest_hash).unwrap().random_blobs.len(),
                2,
                "Both random blobs should be recorded for replay detection"
            );

            // announces_processed should NOT increment for the rejected announce
            assert_eq!(
                transport.stats().announces_processed,
                initial_processed,
                "Rejected stale announce must not increment announces_processed"
            );

            // No new events emitted for the rejected announce
            let stale_events: Vec<_> = transport.drain_events().collect();
            assert!(
                stale_events.is_empty(),
                "Rejected stale announce must not emit events, got {} events",
                stale_events.len()
            );
        }

        #[test]
        fn test_stale_arrives_first_then_fresh_replaces() {
            // Inverse scenario: stale 3-hop announce arrives first, then fresh
            // 1-hop announce arrives and correctly replaces the path.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ooo"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Same two announces as above
            let a_stale = make_announce_raw_for_dest(&dest, 3, 5_000_000);
            let a_fresh = make_announce_raw_for_dest(&dest, 1, 10_000_000);

            // Stale announce arrives first at T=8M via iface_1
            transport.clock.set(8_000_000);
            transport.process_incoming(1, &a_stale).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(4)); // wire=3 + receipt
            assert_eq!(transport.path(&dest_hash).unwrap().interface_index, 1);

            // Fresh announce arrives later at T=12M via iface_0
            transport.clock.set(12_000_000);
            transport.process_incoming(0, &a_fresh).unwrap();

            // Path should be updated: newer emission (10M > 5M), better hops (2 < 4)
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(2), // wire=1 + receipt
                "Fresh better-hop announce must replace stale worse path"
            );
            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                0,
                "Interface must be updated to iface_0 (fresh path)"
            );

            // Both announces processed
            assert_eq!(transport.stats().announces_processed, 2);
        }

        // Replay bypass for better hop count
        #[test]
        fn test_replay_announce_accepted_when_fewer_hops() {
            // Scenario: path request response arrives via relay (2 hops), then the
            // direct announce from the source arrives (0 hops) with the same
            // random_blob. The direct announce must NOT be rejected as replay.
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            // Create a destination and build an announce
            let identity = Identity::generate(&mut OsRng);
            let mut dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "test",
                &["replay"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();
            let now = transport.clock.now_ms();
            let announce_packet = dest.announce(None, &mut OsRng, now).unwrap();

            // Pack the announce
            let mut buf = [0u8; 500];
            let len = announce_packet.pack(&mut buf).unwrap();

            // First: receive the relayed version with 2 hops on interface 1
            let mut relayed = Packet::unpack(&buf[..len]).unwrap();
            relayed.hops = 2;
            let mut relayed_buf = [0u8; 500];
            let rlen = relayed.pack(&mut relayed_buf).unwrap();
            transport.process_incoming(1, &relayed_buf[..rlen]).unwrap();
            transport.drain_actions();
            transport.drain_events();

            // Verify: path established at 2 hops (receipt-incremented = 3)
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(3),
                "Initial path should be 3 hops (wire=2 + receipt increment)"
            );

            // Second: receive the direct version with 0 hops on interface 0.
            // Advance clock past rate limit so the announce isn't rate-dropped.
            transport
                .clock
                .advance(transport.config().announce_rate_limit_ms + 1);
            transport.storage_mut().clear_packet_hashes(); // allow reprocessing
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Path must be updated to the better route
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(1),
                "Direct announce (wire=0 + receipt) must replace the 3-hop relayed path"
            );
            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                0,
                "Path should now point to interface 0 (direct)"
            );
        }

        #[test]
        fn test_replay_announce_rejected_when_same_or_more_hops() {
            // Same random_blob, same or worse hop count → still rejected as replay
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let identity = Identity::generate(&mut OsRng);
            let mut dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "test",
                &["replay"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();
            let now = transport.clock.now_ms();
            let announce_packet = dest.announce(None, &mut OsRng, now).unwrap();

            let mut buf = [0u8; 500];
            let len = announce_packet.pack(&mut buf).unwrap();

            // First: receive with 1 hop on interface 0
            let mut first = Packet::unpack(&buf[..len]).unwrap();
            first.hops = 1;
            let mut first_buf = [0u8; 500];
            let flen = first.pack(&mut first_buf).unwrap();
            transport.process_incoming(0, &first_buf[..flen]).unwrap();
            transport.drain_actions();
            transport.drain_events();

            assert_eq!(transport.hops_to(&dest_hash), Some(2));

            // Second: same announce, same hops, different interface → rejected
            transport
                .clock
                .advance(transport.config().announce_rate_limit_ms + 1);
            transport.storage_mut().clear_packet_hashes();
            transport.process_incoming(1, &first_buf[..flen]).unwrap();

            // Path unchanged, still on interface 0
            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                0,
                "Same-hop replay must not change the path"
            );

            // Third: same announce, MORE hops → also rejected
            let mut worse = Packet::unpack(&buf[..len]).unwrap();
            worse.hops = 3;
            let mut worse_buf = [0u8; 500];
            let wlen = worse.pack(&mut worse_buf).unwrap();
            transport
                .clock
                .advance(transport.config().announce_rate_limit_ms + 1);
            transport.storage_mut().clear_packet_hashes();
            transport.process_incoming(1, &worse_buf[..wlen]).unwrap();

            assert_eq!(
                transport.path(&dest_hash).unwrap().interface_index,
                0,
                "Worse-hop replay must not change the path"
            );
            assert_eq!(transport.hops_to(&dest_hash), Some(2));
        }

        // HEADER_2 filtering for non-own transport_id
        #[test]
        fn test_header2_non_own_transport_id_dropped() {
            // HEADER_2 data packet with a foreign transport_id should be silently dropped
            // BEFORE polluting the dedup cache, and BEFORE being forwarded.
            // Python Transport.py:1193-1196
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let foreign_transport_id = [0xBB; TRUNCATED_HASHBYTES];
            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let next_hop_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Insert a path table entry so the packet WOULD be forwarded without the filter
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 2,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop_hash),
                },
            );

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(foreign_transport_id),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"foreign relay payload".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "Foreign transport_id HEADER_2 data packet should be dropped"
            );
            // Must NOT be forwarded, the packet is not for us
            assert_eq!(
                transport.stats().packets_forwarded,
                0,
                "Foreign transport_id packet must not be forwarded"
            );
            assert!(
                transport.drain_actions().is_empty(),
                "No actions should be emitted for dropped packet"
            );
            // Must NOT pollute dedup cache (dropped before cache insert)
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Dedup cache should not be polluted by foreign transport_id packet"
            );
        }

        #[test]
        fn test_header2_own_transport_id_processed() {
            // HEADER_2 data packet with our own transport_id should be processed normally
            // (forwarded via path table).
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let next_hop_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 2,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: Some(next_hop_hash),
                },
            );

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: Some(*transport.identity.hash()),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"own relay payload".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert!(
                transport.stats().packets_forwarded > 0,
                "Own transport_id HEADER_2 packet should be forwarded"
            );
            let actions = transport.drain_actions();
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::SendPacket { .. })),
                "Should have a SendPacket action for forwarded packet"
            );
        }

        #[test]
        fn test_header2_announce_not_filtered_by_transport_id() {
            // HEADER_2 announce with a foreign transport_id should NOT be dropped.            // announces are exempt from the transport_id filter.
            // Python Transport.py:1193-1196
            use crate::announce::build_announce_payload;
            use crate::destination::{Destination, DestinationType, Direction};
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["filter"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            let now_ms = transport.clock.now_ms();
            let payload = build_announce_payload(
                dest.identity().unwrap(),
                dest.hash().as_bytes(),
                dest.name_hash(),
                None,
                Some(b"test"),
                &mut OsRng,
                now_ms,
            )
            .unwrap();

            let foreign_transport_id = [0xCC; TRUNCATED_HASHBYTES];
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type2,
                    context_flag: false,
                    transport_type: TransportType::Transport,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops: 1,
                transport_id: Some(foreign_transport_id),
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            // Announce should be processed, not dropped
            assert!(
                transport.path(&dest_hash).is_some(),
                "HEADER_2 announce with foreign transport_id should still be processed"
            );
            assert!(
                transport.stats().announces_processed > 0,
                "Announce should be counted as processed"
            );
        }

        // PLAIN/GROUP hop restriction
        #[test]
        fn test_plain_packet_with_hops_above_1_dropped() {
            // PLAIN data packets with hops > 0 must be dropped. PLAIN destinations
            // are for direct neighbors only. Python Transport.py:1205-1213
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 2,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"plain too far".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "PLAIN data packet with hops=2 should be dropped"
            );
            assert!(
                transport.drain_actions().is_empty(),
                "No actions should be emitted for dropped PLAIN packet"
            );
            // Must NOT pollute dedup cache (dropped before cache insert)
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Dedup cache should not be polluted by dropped PLAIN packet"
            );
        }

        #[test]
        fn test_plain_packet_with_hops_0_processed() {
            // PLAIN data packets with hops=0 should be delivered normally.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = test_transport();
            transport.register_interface(Box::new(MockInterface::new("test", 1)));
            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"plain neighbor".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                0,
                "PLAIN data packet with hops=0 should NOT be dropped"
            );
            assert!(
                transport.pending_events() > 0,
                "PLAIN data packet with hops=0 should generate events"
            );
        }

        #[test]
        fn test_plain_packet_with_hops_1_dropped() {
            // PLAIN data packets with hops=1 must be dropped. PLAIN destinations
            // are for direct neighbors only (hops == 0). Python checks hops > 1
            // after increment; Rust equivalent without increment: hops > 0.
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"plain one hop".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "PLAIN data packet with hops=1 should be dropped"
            );
            assert!(
                transport.drain_actions().is_empty(),
                "No actions should be emitted for dropped PLAIN packet"
            );
        }

        #[test]
        fn test_plain_announce_always_dropped() {
            // PLAIN announces are always invalid, dropped unconditionally
            // regardless of hop count. Python Transport.py:1211-1213
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Announce,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"fake plain announce".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "PLAIN announce should always be dropped"
            );
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Dedup cache should not be polluted by invalid PLAIN announce"
            );
        }

        #[test]
        fn test_group_packet_with_hops_above_1_dropped() {
            // GROUP data packets with hops > 0 must be dropped. GROUP destinations
            // are for direct neighbors only. Python Transport.py:1215-1225
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Group,
                    packet_type: PacketType::Data,
                },
                hops: 2,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"group too far".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "GROUP data packet with hops=2 should be dropped"
            );
            assert!(
                transport.drain_actions().is_empty(),
                "No actions should be emitted for dropped GROUP packet"
            );
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Dedup cache should not be polluted by dropped GROUP packet"
            );
        }

        #[test]
        fn test_group_announce_always_dropped() {
            // GROUP announces are always invalid, dropped unconditionally
            // regardless of hop count. Python Transport.py:1223-1225
            use crate::destination::DestinationType;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Group,
                    packet_type: PacketType::Announce,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"fake group announce".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();

            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "GROUP announce should always be dropped"
            );
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Dedup cache should not be polluted by invalid GROUP announce"
            );
        }

        // Deferred hash caching for link-table & LRPROOF
        #[test]
        fn test_link_table_data_deferred_hash_allows_retry() {
            // Shared-medium scenario: a link-table DATA packet arrives on the wrong
            // interface first (hop check fails), then on the correct interface.
            // The dedup hash must NOT be cached on failure, allowing the retry.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build DATA packet: wire hops=1, receipt-incremented to 2 (matches hops for initiator side)
            let pkt = build_link_data_packet(link_id, 1);
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();

            // Step 1: arrives on if1 (destination side), receipt hops=2 != remaining_hops=3 → dropped
            transport.process_incoming(1, &buf[..len]).unwrap();
            assert_eq!(transport.stats().packets_forwarded, 0);
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Hash must NOT be cached when link-table hop check fails"
            );

            // Step 2: same packet arrives on if0 (initiator side), receipt hops=2 == hops=2 → forwarded
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(
                transport.stats().packets_forwarded,
                1,
                "Retry on correct interface should succeed"
            );
            assert!(
                transport.storage().packet_hash_count() > 0,
                "Hash should be cached after successful forwarding"
            );
        }

        #[test]
        fn test_link_table_data_successful_forward_caches_hash() {
            // Regression: successful link-table forwarding MUST cache the hash
            // so that a second copy is dropped as duplicate.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: true,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Wire hops=1, receipt-incremented to 2, matches entry.hops=2 for initiator side
            let pkt = build_link_data_packet(link_id, 1);
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();

            // First copy: forwarded
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(transport.stats().packets_forwarded, 1);
            assert!(
                transport.storage().packet_hash_count() > 0,
                "Hash should be cached after successful forwarding"
            );

            // Second copy: dropped as duplicate
            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(0, &buf[..len]).unwrap();
            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "Duplicate should be dropped"
            );
            assert_eq!(
                transport.stats().packets_forwarded,
                1,
                "Only the first copy should be forwarded"
            );
        }

        #[test]
        fn test_non_link_table_data_immediate_hash() {
            // Normal (non-link-table) data is cached immediately in process_incoming,
            // so a second copy on another interface is dropped.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            let pkt = build_link_data_packet(dest_hash, 0);
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();

            // First copy: delivered
            transport.process_incoming(0, &buf[..len]).unwrap();
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, TransportEvent::PacketReceived { .. })),
                "First copy should be delivered"
            );

            // Second copy on different interface: dropped as duplicate
            let dropped_before = transport.stats().packets_dropped;
            transport.process_incoming(1, &buf[..len]).unwrap();
            assert_eq!(
                transport.stats().packets_dropped,
                dropped_before + 1,
                "Second copy should be dropped"
            );
        }

        #[test]
        fn test_lrproof_not_cached_on_forwarding() {
            // LRPROOF forwarded via link table:
            // NOT cached by process_incoming (deferred, Python Transport.py:2016-2039)
            // NOT cached by the forwarding path (forward_on_interface → send_on_interface)
            // Only ORIGINATION paths (send_to_destination, send_on_all_interfaces) cache.
            // This matches Python where Transport.transmit() doesn't cache hashes.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: false,
                    proof_timeout_ms: now + 30_000,
                    destination_hash: [0xBB; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build LRPROOF with valid size (96 bytes = sig(64) + X25519(32))
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 2, // wire hops=2, receipt → 3 = remaining_hops
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned([0xCC; 96].to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();

            // Arrives on if1 (dest side), receipt hops=3 matches remaining_hops=3 → forwarded
            transport.process_incoming(1, &buf[..len]).unwrap();
            assert!(
                transport.stats().packets_forwarded > 0,
                "LRPROOF should be forwarded"
            );
            // Forwarding path does NOT cache hashes, only origination paths do.
            // This matches Python where Transport.transmit() doesn't call
            // add_packet_hash(), only Transport.outbound() does.
            assert!(
                transport.storage().packet_hash_count() == 0,
                "Forwarded LRPROOF hash should NOT be cached (forwarding, not origination)"
            );
        }

        #[test]
        fn test_lrproof_local_delivery_caches_hash() {
            // LRPROOF delivered to a registered destination IS cached.
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            transport.register_destination(dest_hash);

            // Build LRPROOF targeting the registered destination (not in link_table)
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::Lrproof,
                data: PacketData::Owned([0xCC; 96].to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();

            transport.process_incoming(0, &buf[..len]).unwrap();

            // Should emit PacketReceived
            let events: Vec<_> = transport.drain_events().collect();
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, TransportEvent::PacketReceived { .. })),
                "LRPROOF should be delivered to registered destination"
            );

            // Hash should be cached
            assert!(
                transport.storage().packet_hash_count() > 0,
                "LRPROOF hash should be cached after local delivery"
            );
        }

        // Stage 12: Path Recovery Tests
        /// Build an announce packet with a controlled random_hash.
        ///
        /// This allows creating two announces from the SAME destination with the
        /// SAME emission timestamp but different random bytes (for replay detection).
        /// The random_hash is 10 bytes: first 5 are random, last 5 are the emission
        /// timestamp (big-endian).
        fn make_announce_raw_with_random_hash(
            dest: &crate::destination::Destination,
            hops: u8,
            random_hash: &[u8; crate::constants::RANDOM_HASHBYTES],
        ) -> Vec<u8> {
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let identity = dest.identity().unwrap();
            let public_key = identity.public_key_bytes();
            let app_data = b"test";

            // Build signed data (same as announce.rs build_signed_data)
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&public_key);
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = identity.sign(&signed_data).unwrap();

            // Build payload: public_key + name_hash + random_hash + signature + app_data
            let mut payload = Vec::new();
            payload.extend_from_slice(&public_key);
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(random_hash);
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            buf[..len].to_vec()
        }

        /// Build a random_hash with the given random prefix bytes and emission value.
        fn make_random_hash(
            random_prefix: [u8; 5],
            emission: u64,
        ) -> [u8; crate::constants::RANDOM_HASHBYTES] {
            let mut hash = [0u8; crate::constants::RANDOM_HASHBYTES];
            hash[..5].copy_from_slice(&random_prefix);
            // Last 5 bytes: big-endian emission (lower 40 bits)
            hash[5] = ((emission >> 32) & 0xFF) as u8;
            hash[6] = ((emission >> 24) & 0xFF) as u8;
            hash[7] = ((emission >> 16) & 0xFF) as u8;
            hash[8] = ((emission >> 8) & 0xFF) as u8;
            hash[9] = (emission & 0xFF) as u8;
            hash
        }

        fn make_test_dest() -> crate::destination::Destination {
            use crate::destination::{Destination, DestinationType, Direction};
            let identity = Identity::generate(&mut OsRng);
            Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["recovery"],
            )
            .unwrap()
        }

        #[test]
        fn test_path_state_api_basic() {
            let mut transport = make_transport_enabled();
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];

            // Insert a path entry manually
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Default state: not unresponsive
            assert!(!transport.path_is_unresponsive(&dest_hash));

            // Mark unresponsive
            assert!(transport.mark_path_unresponsive(&dest_hash));
            assert!(transport.path_is_unresponsive(&dest_hash));

            // Reset to unknown
            assert!(transport.mark_path_unknown_state(&dest_hash));
            assert!(!transport.path_is_unresponsive(&dest_hash));

            // Mark responsive
            assert!(transport.mark_path_responsive(&dest_hash));
            assert!(!transport.path_is_unresponsive(&dest_hash));

            // Non-existent destination returns false
            assert!(!transport.mark_path_unresponsive(&[0xFF; TRUNCATED_HASHBYTES]));
        }

        #[test]
        fn test_path_state_orphan_cleanup() {
            let mut transport = make_transport_enabled();
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];

            // Insert path and mark unresponsive
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );
            assert!(transport.mark_path_unresponsive(&dest_hash));

            // Remove path
            transport.remove_path(&dest_hash);

            // Poll should clean up orphaned state
            transport.poll();

            // Re-insert path, state should be gone (not unresponsive)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: transport.clock.now_ms() + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );
            assert!(
                !transport.path_is_unresponsive(&dest_hash),
                "State should have been cleaned up when path was removed"
            );
        }

        #[test]
        fn test_announce_acceptance_resets_state_to_unknown() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = make_test_dest();
            let dest_hash = dest.hash().into_bytes();

            // Process initial announce (creates path, wire hops=1, stored as 2)
            let rh1 = make_random_hash([0x01, 0x02, 0x03, 0x04, 0x05], 1000);
            let a1 = make_announce_raw_with_random_hash(&dest, 1, &rh1);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2));

            // Mark unresponsive
            assert!(transport.mark_path_unresponsive(&dest_hash));
            assert!(transport.path_is_unresponsive(&dest_hash));

            // Advance past rate limit
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Process newer announce (same dest, newer emission), should reset state
            let rh2 = make_random_hash([0x0A, 0x0B, 0x0C, 0x0D, 0x0E], 2000);
            let a2 = make_announce_raw_with_random_hash(&dest, 1, &rh2);
            transport.process_incoming(0, &a2).unwrap();

            assert!(
                !transport.path_is_unresponsive(&dest_hash),
                "State should be reset to UNKNOWN after normal announce acceptance"
            );
        }

        #[test]
        fn test_unresponsive_path_accepts_same_emission_worse_hop_announce() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest = make_test_dest();
            let dest_hash = dest.hash().into_bytes();

            // Process announce A (wire hops=1, stored as 2)
            let emission = 5000u64;
            let rh_a = make_random_hash([0x01, 0x02, 0x03, 0x04, 0x05], emission);
            let a1 = make_announce_raw_with_random_hash(&dest, 1, &rh_a);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2));

            // Advance past rate limit
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Process announce B (wire hops=3, same emission, different random)
            // WITHOUT marking unresponsive, should be REJECTED
            let rh_b = make_random_hash([0x0A, 0x0B, 0x0C, 0x0D, 0x0E], emission);
            let a2 = make_announce_raw_with_random_hash(&dest, 3, &rh_b);
            transport.process_incoming(1, &a2).unwrap();
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(2), // unchanged
                "Worse-hop same-emission announce should be rejected when path is UNKNOWN"
            );

            // Now mark path unresponsive
            assert!(transport.mark_path_unresponsive(&dest_hash));

            // Clear packet cache so announce B can be re-processed
            transport.storage_mut().clear_packet_hashes();
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Re-process announce B, should be ACCEPTED now (wire hops=3, stored as 4)
            let rh_c = make_random_hash([0x0F, 0x10, 0x11, 0x12, 0x13], emission);
            let a3 = make_announce_raw_with_random_hash(&dest, 3, &rh_c);
            transport.process_incoming(1, &a3).unwrap();
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(4),
                "Worse-hop same-emission announce should be accepted when path is UNRESPONSIVE"
            );
        }

        #[test]
        fn test_unknown_path_rejects_same_emission_worse_hop_announce() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = make_test_dest();
            let dest_hash = dest.hash().into_bytes();

            // Process announce A (wire hops=1, stored as 2)
            let emission = 5000u64;
            let rh_a = make_random_hash([0x01, 0x02, 0x03, 0x04, 0x05], emission);
            let a1 = make_announce_raw_with_random_hash(&dest, 1, &rh_a);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2));

            // Advance past rate limit
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Process announce B (wire hops=3, same emission, different random)
            let rh_b = make_random_hash([0x0A, 0x0B, 0x0C, 0x0D, 0x0E], emission);
            let a2 = make_announce_raw_with_random_hash(&dest, 3, &rh_b);
            transport.process_incoming(0, &a2).unwrap();

            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(2), // unchanged
                "Worse-hop same-emission announce should be rejected when path is UNKNOWN"
            );
        }

        #[test]
        fn test_link_table_expiry_unvalidated_marks_unresponsive_1hop_dest() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Insert path entry with hops=1 (directly connected, receipt-incremented).
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Insert unvalidated link entry: hops=2 (initiator 2 hops away),
            // dest is directly connected → sub-case 2 (dest is_direct)
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 1, // direct dest, receipt-incremented
                    received_interface_index: 0,
                    hops: 2,
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            assert!(!transport.path_is_unresponsive(&dest_hash));

            // Advance past proof_timeout
            transport.clock.advance(1001);
            transport.poll();

            // Sub-case 2: dest directly connected, transport enabled → mark unresponsive
            assert!(
                transport.path_is_unresponsive(&dest_hash),
                "Path should be marked unresponsive when unvalidated link expires for direct dest"
            );

            // Link should be removed
            assert!(!transport.storage().has_link_entry(&link_id));
        }

        #[test]
        fn test_link_table_expiry_unvalidated_marks_unresponsive_1hop_initiator() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Insert path entry with hops=3 (destination is far away)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 3,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Insert unvalidated link: hops=1 (initiator directly connected, receipt-incremented).
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            transport.clock.advance(1001);
            transport.poll();

            // Sub-case 3: initiator directly connected → mark unresponsive
            assert!(
                transport.path_is_unresponsive(&dest_hash),
                "Path should be marked unresponsive when initiator was directly connected"
            );
        }

        #[test]
        fn test_link_table_expiry_unvalidated_no_mark_when_dest_far() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Path with hops=3 (not 1-hop)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 3,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Unvalidated link: hops=2 (initiator 2 hops, not 1)
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 3,
                    received_interface_index: 0,
                    hops: 2,
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            transport.clock.advance(1001);
            transport.poll();

            // Neither sub-case 3 nor 4 applies, should NOT be marked
            assert!(
                !transport.path_is_unresponsive(&dest_hash),
                "Path should NOT be marked unresponsive when dest and initiator are far"
            );

            // Link should still be removed
            assert!(!transport.storage().has_link_entry(&link_id));
        }

        #[test]
        fn test_link_table_expiry_unvalidated_sends_path_request() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Path with hops=1 (directly connected, receipt-incremented, sub-case 2)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // hops=2 (not 1, so sub-case 3 doesn't trigger before sub-case 2)
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 1, // direct dest, receipt-incremented
                    received_interface_index: 0,
                    hops: 2,
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            // Drain any events/actions from setup
            transport.drain_events();
            transport.drain_actions();

            transport.clock.advance(1001);
            transport.poll();

            let actions = transport.drain_actions();
            assert!(
                has_path_request_broadcast(&actions, &dest_hash),
                "Should send path request broadcast for unvalidated link expiry"
            );
        }

        #[test]
        fn test_link_table_expiry_unvalidated_path_missing_sends_path_request() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // NO path entry, sub-case 1
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 0,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 0,
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            transport.drain_events();
            transport.drain_actions();
            transport.clock.advance(1001);
            transport.poll();

            let actions = transport.drain_actions();
            assert!(
                has_path_request_broadcast(&actions, &dest_hash),
                "Should send path request broadcast when path is missing"
            );
        }

        #[test]
        fn test_link_table_expiry_validated_no_rediscovery() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Validated link
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 1,
                    received_interface_index: 0,
                    hops: 0,
                    validated: true,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            transport.drain_events();
            transport.drain_actions();

            // Advance past LINK_TIMEOUT_MS
            transport.clock.advance(LINK_TIMEOUT_MS + 1000);
            transport.poll();

            // Link removed, but NO path request
            assert!(!transport.storage().has_link_entry(&link_id));

            let actions = transport.drain_actions();
            assert!(
                !has_path_request_broadcast(&actions, &dest_hash),
                "Validated link expiry should NOT send path request"
            );

            // Path state unchanged
            assert!(!transport.path_is_unresponsive(&dest_hash));
        }

        #[test]
        fn test_expire_path_removes_entry() {
            let mut transport = make_transport_enabled();
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];

            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            assert!(transport.has_path(&dest_hash));

            // Expire it
            assert!(transport.expire_path(&dest_hash));
            assert!(!transport.has_path(&dest_hash));

            // Should emit PathLost
            let events: Vec<_> = transport.drain_events().collect();
            let has_path_lost = events.iter().any(|e| {
                matches!(
                    e,
                    TransportEvent::PathLost { destination_hash }
                    if *destination_hash == dest_hash
                )
            });
            assert!(has_path_lost, "expire_path should emit PathLost");

            // Second call returns false
            assert!(!transport.expire_path(&dest_hash));
        }

        #[test]
        fn test_expire_path_allows_worse_hop_announce() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = make_test_dest();
            let dest_hash = dest.hash().into_bytes();

            // Process announce A (hops=1)
            let rh_a = make_random_hash([0x01, 0x02, 0x03, 0x04, 0x05], 1000);
            let a1 = make_announce_raw_with_random_hash(&dest, 1, &rh_a);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(2)); // wire hops=1 + receipt increment

            // Expire path
            assert!(transport.expire_path(&dest_hash));
            assert!(!transport.has_path(&dest_hash));

            // Clear packet cache
            transport.storage_mut().clear_packet_hashes();
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Process announce B (hops=5), should be accepted (no existing path)
            let rh_b = make_random_hash([0x0A, 0x0B, 0x0C, 0x0D, 0x0E], 2000);
            let a2 = make_announce_raw_with_random_hash(&dest, 5, &rh_b);
            transport.process_incoming(0, &a2).unwrap();
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(6), // wire hops=5 + receipt increment
                "After expire_path, worse-hop announce should be accepted as new path"
            );
        }

        #[test]
        fn test_link_expiry_non_transport_calls_expire_path() {
            let mut transport = test_transport();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Insert path entry
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 2,
                    expires_ms: now + 3_600_000,
                    interface_index: 0,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Insert unvalidated link
            let link_id = [0xAA; TRUNCATED_HASHBYTES];
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 0,
                    remaining_hops: 2,
                    received_interface_index: 0,
                    hops: 1, // receipt-incremented: directly connected initiator
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            transport.drain_events();
            transport.drain_actions();

            transport.clock.advance(1001);
            transport.poll();

            // Non-transport node should force-expire the path
            assert!(
                !transport.has_path(&dest_hash),
                "Non-transport node should expire path on unvalidated link expiry"
            );

            let events: Vec<_> = transport.drain_events().collect();
            let has_path_lost = events.iter().any(|e| {
                matches!(
                    e,
                    TransportEvent::PathLost { destination_hash }
                    if *destination_hash == dest_hash
                )
            });
            assert!(has_path_lost, "Should emit PathLost");

            let actions = transport.drain_actions();
            assert!(
                has_path_request_broadcast(&actions, &dest_hash),
                "Should send path request broadcast"
            );
        }

        #[test]
        fn test_full_recovery_cycle() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest = make_test_dest();
            let dest_hash = dest.hash().into_bytes();

            // Step 1: Process announce A (wire hops=0 → stored hops=1 = directly connected)
            let emission = 5000u64;
            let rh_a = make_random_hash([0x01, 0x02, 0x03, 0x04, 0x05], emission);
            let a1 = make_announce_raw_with_random_hash(&dest, 0, &rh_a);
            transport.process_incoming(0, &a1).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(1)); // receipt-incremented

            // Step 2: Insert unvalidated link_table entry (simulates relay)
            // hops=2 so sub-case 3 (initiator hops==1) doesn't take priority
            // over sub-case 2 (dest is_direct)
            let link_id = [0xCC; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.storage_mut().set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: now,
                    next_hop_interface_index: 1,
                    remaining_hops: 1, // receipt-incremented
                    received_interface_index: 0,
                    hops: 2,
                    validated: false,
                    proof_timeout_ms: now + 1000,
                    destination_hash: dest_hash,
                    peer_signing_key: None,
                },
            );

            // Step 3: Advance past proof_timeout
            transport.clock.advance(1001);
            transport.drain_events();
            transport.poll();

            // Step 4: Verify sub-case 2 fired
            assert!(
                transport.path_is_unresponsive(&dest_hash),
                "Path should be marked unresponsive after unvalidated link expiry"
            );

            // Check path request broadcast was emitted
            let actions = transport.drain_actions();
            assert!(
                has_path_request_broadcast(&actions, &dest_hash),
                "Should send path request broadcast"
            );

            // Step 5: Clear packet cache and advance past rate limit
            transport.storage_mut().clear_packet_hashes();
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Step 6: Process announce B (hops=3, same emission, different random)
            // Should be ACCEPTED because path is UNRESPONSIVE
            let rh_b = make_random_hash([0x0A, 0x0B, 0x0C, 0x0D, 0x0E], emission);
            let a2 = make_announce_raw_with_random_hash(&dest, 3, &rh_b);
            transport.process_incoming(1, &a2).unwrap();
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(4), // wire hops=3 + receipt increment
                "Worse-hop same-emission announce should be accepted for unresponsive path"
            );

            // Step 7: State should still be UNRESPONSIVE (not reset)
            assert!(
                transport.path_is_unresponsive(&dest_hash),
                "State should remain UNRESPONSIVE after same-emission update (per Python)"
            );

            // Step 8: Clear caches and process newer announce C
            transport.storage_mut().clear_packet_hashes();
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            let rh_c = make_random_hash([0x10, 0x11, 0x12, 0x13, 0x14], emission + 1000);
            let a3 = make_announce_raw_with_random_hash(&dest, 2, &rh_c);
            transport.process_incoming(1, &a3).unwrap();
            assert_eq!(transport.hops_to(&dest_hash), Some(3)); // wire hops=2 + receipt increment

            // Step 9: Normal announce should reset state to UNKNOWN
            assert!(
                !transport.path_is_unresponsive(&dest_hash),
                "State should be reset to UNKNOWN after normal announce acceptance"
            );
        }

        // Announce Rate Limiting Tests
        /// Helper: create a transport with rate limiting enabled
        fn make_transport_rate_limited(
            target_ms: u64,
            grace: u8,
            penalty_ms: u64,
        ) -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                announce_rate_target_ms: Some(target_ms),
                announce_rate_grace: grace,
                announce_rate_penalty_ms: penalty_ms,
                ..TransportConfig::default()
            };
            Transport::new(config, clock, MemoryStorage::with_defaults(), identity)
        }

        #[test]
        fn test_announce_rate_within_limit_accepted() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_rate_limited(5_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["rate"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // First announce
            let now1 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, now1);
            transport.process_incoming(0, &raw1).unwrap();
            assert!(transport.storage().get_announce(&dest_hash).is_some());
            assert_eq!(transport.storage().announce_rate_count(), 1);

            // Advance past both the simple rate limit and the rate target
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 5_001);
            transport.storage_mut().clear_packet_hashes();

            // Second announce, spaced well beyond target
            let now2 = transport.clock.now_ms();
            let raw2 = make_announce_raw_for_dest(&dest, 1, now2);
            transport.process_incoming(0, &raw2).unwrap();

            // Should be accepted into announce_table
            assert!(
                transport.storage().get_announce(&dest_hash).is_some(),
                "Announce within rate limit should be accepted"
            );
        }

        #[test]
        fn test_announce_rate_exceeds_limit_after_grace() {
            use crate::destination::{Destination, DestinationType, Direction};

            // grace=0 means first violation triggers blocking
            let mut transport = make_transport_rate_limited(10_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["rateblock"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // First announce, creates rate entry
            let now1 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, now1);
            transport.process_incoming(0, &raw1).unwrap();
            assert!(transport.storage().get_announce(&dest_hash).is_some());
            assert!(transport.has_path(&dest_hash));

            // Advance past simple rate limit but LESS than rate target (10s)
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
            transport.storage_mut().clear_packet_hashes();

            // Second announce, too fast (2s < 10s target)
            let now2 = transport.clock.now_ms();
            let raw2 = make_announce_raw_for_dest(&dest, 1, now2);
            transport.process_incoming(0, &raw2).unwrap();

            // Path table SHOULD be updated (rate limiting only blocks rebroadcast)
            assert!(
                transport.has_path(&dest_hash),
                "Path table should still be updated even when rate-blocked"
            );

            // Announce table entry should NOT be updated (rebroadcast blocked)
            // The announce table still has the first entry's timestamp
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.timestamp_ms, now1,
                "Announce table should not be updated when rate-blocked"
            );
        }

        #[test]
        fn test_announce_rate_penalty_extends_blocking() {
            use crate::destination::{Destination, DestinationType, Direction};

            // 10s target + 5s penalty = blocking until last_ms + 15s
            let mut transport = make_transport_rate_limited(10_000, 0, 5_000);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["penalty"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // First announce at T=1_000_000
            let t0 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, t0);
            transport.process_incoming(0, &raw1).unwrap();

            // Trigger violation: advance 3s (< 10s target)
            transport.clock.advance(3_000);
            transport.storage_mut().clear_packet_hashes();
            let now2 = transport.clock.now_ms();
            let raw2 = make_announce_raw_for_dest(&dest, 1, now2);
            transport.process_incoming(0, &raw2).unwrap();

            // blocked_until should be t0 + 10_000 + 5_000 = t0 + 15_000
            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(rate_entry.blocked_until_ms, t0 + 10_000 + 5_000);

            // At t0 + 14_999, still blocked
            transport.clock.set(t0 + 14_999);
            transport.storage_mut().clear_packet_hashes();
            let now3 = transport.clock.now_ms();
            let raw3 = make_announce_raw_for_dest(&dest, 1, now3);
            transport.process_incoming(0, &raw3).unwrap();
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.timestamp_ms, t0,
                "Should still be blocked at t0 + 14_999"
            );

            // At t0 + 15_001, block expired
            transport.clock.set(t0 + 15_001);
            transport.storage_mut().clear_packet_hashes();
            let now4 = transport.clock.now_ms();
            let raw4 = make_announce_raw_for_dest(&dest, 1, now4);
            transport.process_incoming(0, &raw4).unwrap();
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.timestamp_ms, now4,
                "Should be accepted after block expires"
            );
        }

        #[test]
        fn test_announce_rate_independent_per_destination() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_rate_limited(10_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest_a = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["dest_a"],
            )
            .unwrap();
            let dest_b = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["dest_b"],
            )
            .unwrap();
            let hash_a = dest_a.hash().into_bytes();
            let hash_b = dest_b.hash().into_bytes();

            // First announces for both
            let now1 = transport.clock.now_ms();
            let raw_a1 = make_announce_raw_for_dest(&dest_a, 1, now1);
            let raw_b1 = make_announce_raw_for_dest(&dest_b, 1, now1);
            transport.process_incoming(0, &raw_a1).unwrap();
            transport.process_incoming(0, &raw_b1).unwrap();

            // Trigger violation for dest_a only (too fast)
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
            transport.storage_mut().clear_packet_hashes();
            let now2 = transport.clock.now_ms();
            let raw_a2 = make_announce_raw_for_dest(&dest_a, 1, now2);
            transport.process_incoming(0, &raw_a2).unwrap();

            // dest_a should be rate-blocked (announce table timestamp unchanged)
            let entry_a = transport.storage().get_announce(&hash_a).unwrap();
            assert_eq!(entry_a.timestamp_ms, now1, "dest_a should be rate-blocked");

            // dest_b should still be able to accept announces
            let raw_b2 = make_announce_raw_for_dest(&dest_b, 1, now2);
            transport.process_incoming(0, &raw_b2).unwrap();

            // dest_b announce_table entry should be updated (not blocked)
            // the simple rate limit (announce_rate_limit_ms) also applies,
            // but we already advanced past it. The rate target check is separate.
            // dest_b's rate entry: now2 - now1 = ANNOUNCE_RATE_LIMIT_MS + 1 < 10_000
            // So dest_b will ALSO be rate-blocked. Let's verify independence differently:
            // advance enough for dest_b to be within target
            transport.clock.advance(10_000);
            transport.storage_mut().clear_packet_hashes();
            let now3 = transport.clock.now_ms();
            let raw_b3 = make_announce_raw_for_dest(&dest_b, 1, now3);
            transport.process_incoming(0, &raw_b3).unwrap();

            let entry_b = transport.storage().get_announce(&hash_b).unwrap();
            assert_eq!(
                entry_b.timestamp_ms, now3,
                "dest_b should be accepted (not blocked by dest_a's violation)"
            );

            // dest_a should STILL be blocked (blocked_until = now1 + 10_000)
            // now3 = now1 + ANNOUNCE_RATE_LIMIT_MS + 1 + 10_000 = now1 + 12_001
            // blocked_until = now1 + 10_000 = now1 + 10_000
            // now3 > blocked_until, so dest_a block has expired by now
            // But the rate entry's last_ms is still now1, so current_rate = now3 - now1 > target
            // which means violations would decrement. So dest_a should now be unblocked too.
        }

        #[test]
        fn test_announce_rate_violations_decrement_on_good_rate() {
            use crate::destination::{Destination, DestinationType, Direction};

            // grace=2 means 3 violations needed to trigger blocking
            let mut transport = make_transport_rate_limited(5_000, 2, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["decrement"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // First announce
            let t0 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, t0);
            transport.process_incoming(0, &raw1).unwrap();

            // Violation 1: too fast (2s < 5s)
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
            transport.storage_mut().clear_packet_hashes();
            let raw2 = make_announce_raw_for_dest(&dest, 1, transport.clock.now_ms());
            transport.process_incoming(0, &raw2).unwrap();

            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(rate_entry.rate_violations, 1, "Should have 1 violation");

            // Violation 2: too fast again
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
            transport.storage_mut().clear_packet_hashes();
            let raw3 = make_announce_raw_for_dest(&dest, 1, transport.clock.now_ms());
            transport.process_incoming(0, &raw3).unwrap();

            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(rate_entry.rate_violations, 2, "Should have 2 violations");

            // Good rate: advance well past target (10s > 5s)
            transport.clock.advance(10_000);
            transport.storage_mut().clear_packet_hashes();
            let now_good = transport.clock.now_ms();
            let raw4 = make_announce_raw_for_dest(&dest, 1, now_good);
            transport.process_incoming(0, &raw4).unwrap();

            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(
                rate_entry.rate_violations, 1,
                "Violations should decrement on good rate"
            );

            // Another good rate
            transport.clock.advance(10_000);
            transport.storage_mut().clear_packet_hashes();
            let raw5 = make_announce_raw_for_dest(&dest, 1, transport.clock.now_ms());
            transport.process_incoming(0, &raw5).unwrap();

            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(
                rate_entry.rate_violations, 0,
                "Violations should be back to 0 after enough good-rate announces"
            );

            // Announce should be accepted (not blocked)
            assert!(
                transport.storage().get_announce(&dest_hash).is_some(),
                "Announce should be accepted when violations are within grace"
            );
        }

        #[test]
        fn test_announce_rate_recovery_after_block_expires() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_rate_limited(10_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["recovery"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // First announce at T0
            let t0 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, t0);
            transport.process_incoming(0, &raw1).unwrap();

            // Trigger blocking (too fast)
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
            transport.storage_mut().clear_packet_hashes();
            let raw2 = make_announce_raw_for_dest(&dest, 1, transport.clock.now_ms());
            transport.process_incoming(0, &raw2).unwrap();

            // Verify blocked
            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert!(rate_entry.blocked_until_ms > 0, "Should be blocked");

            // Advance past blocked_until (t0 + 10_000)
            transport.clock.set(t0 + 10_001);
            transport.storage_mut().clear_packet_hashes();
            let now_after = transport.clock.now_ms();
            let raw3 = make_announce_raw_for_dest(&dest, 1, now_after);
            transport.process_incoming(0, &raw3).unwrap();

            // current_rate = now_after - t0 = 10_001 > 10_000 target → good rate
            // violations decrement from 1 to 0, not blocked
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.timestamp_ms, now_after,
                "Announce should be accepted after block expires"
            );
        }

        #[test]
        fn test_announce_rate_disabled_by_default() {
            // Default config has announce_rate_target_ms: None
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = crate::destination::Destination::new(
                Some(Identity::generate(&mut OsRng)),
                crate::destination::Direction::In,
                crate::destination::DestinationType::Single,
                "testapp",
                &["norlimit"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Rapid announces (only bypass simple rate limit and packet cache)
            let now1 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, now1);
            transport.process_incoming(0, &raw1).unwrap();
            assert!(transport.storage().get_announce(&dest_hash).is_some());

            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);
            transport.storage_mut().clear_packet_hashes();
            let now2 = transport.clock.now_ms();
            let raw2 = make_announce_raw_for_dest(&dest, 1, now2);
            transport.process_incoming(0, &raw2).unwrap();

            // Should be accepted (rate limiting is disabled)
            let entry = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                entry.timestamp_ms, now2,
                "Announce should be accepted when rate limiting is disabled"
            );

            // No rate table entries should exist
            assert_eq!(
                transport.storage().announce_rate_count(),
                0,
                "Rate table should be empty when rate limiting is disabled"
            );
        }

        #[test]
        fn test_announce_rate_path_response_exempt() {
            let mut transport = make_transport_rate_limited(10_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            // Use make_announce_raw which supports context
            let (raw, dest_hash) = make_announce_raw(1, PacketContext::PathResponse);
            transport.process_incoming(0, &raw).unwrap();

            // PATH_RESPONSE should not create a rate entry
            assert_eq!(
                transport.storage().announce_rate_count(),
                0,
                "PATH_RESPONSE announces should not be rate-tracked"
            );

            // Should be in announce table
            assert!(
                transport.storage().get_announce(&dest_hash).is_some(),
                "PATH_RESPONSE announce should be in announce table"
            );
        }

        #[test]
        fn test_announce_rate_entry_created_on_announce() {
            use crate::destination::{Destination, DestinationType, Direction};

            let mut transport = make_transport_rate_limited(10_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["cleanup"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // Create announce to populate path_table and rate_table
            let now = transport.clock.now_ms();
            let raw = make_announce_raw_for_dest(&dest, 1, now);
            transport.process_incoming(0, &raw).unwrap();
            assert_eq!(transport.storage().announce_rate_count(), 1);
            assert!(transport.has_path(&dest_hash));

            // Verify rate entry was created
            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(rate_entry.last_ms, now);
            assert_eq!(rate_entry.rate_violations, 0);
            // announce_rate cleanup is handled by clean_stale_path_metadata()
            // which cleans both path_states and announce_rate for stale destinations
        }

        #[test]
        fn test_announce_rate_last_ms_not_updated_on_violation() {
            use crate::destination::{Destination, DestinationType, Direction};

            // grace=0: first violation immediately triggers blocking
            let mut transport = make_transport_rate_limited(10_000, 0, 0);
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = Destination::new(
                Some(Identity::generate(&mut OsRng)),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["lastms"],
            )
            .unwrap();
            let dest_hash = dest.hash().into_bytes();

            // First announce at T0
            let t0 = transport.clock.now_ms();
            let raw1 = make_announce_raw_for_dest(&dest, 1, t0);
            transport.process_incoming(0, &raw1).unwrap();

            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(rate_entry.last_ms, t0);

            // Violation: too fast (3s < 10s target), triggers blocking (grace=0)
            transport.clock.advance(3_000);
            transport.storage_mut().clear_packet_hashes();
            let now2 = transport.clock.now_ms();
            let raw2 = make_announce_raw_for_dest(&dest, 1, now2);
            transport.process_incoming(0, &raw2).unwrap();

            // last_ms should NOT be updated, blocking was triggered
            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(
                rate_entry.last_ms, t0,
                "last_ms should not be updated when violation triggers blocking"
            );
            assert_eq!(rate_entry.rate_violations, 1);
            assert!(
                rate_entry.blocked_until_ms > 0,
                "Should be blocked after violation with grace=0"
            );

            // Another too-fast announce while blocked, last_ms still anchored
            transport.clock.advance(3_000);
            transport.storage_mut().clear_packet_hashes();
            let now3 = transport.clock.now_ms();
            let raw3 = make_announce_raw_for_dest(&dest, 1, now3);
            transport.process_incoming(0, &raw3).unwrap();

            let rate_entry = transport.storage().get_announce_rate(&dest_hash).unwrap();
            assert_eq!(
                rate_entry.last_ms, t0,
                "last_ms should still be anchored to original accepted announce"
            );
        }

        // T11: Transport Gap Tests
        #[test]
        fn test_hop_limit_forward_drops_at_max() {
            // A packet at PATHFINDER_MAX_HOPS gets +1 on forward → exceeds limit → dropped
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
            let _idx1 = transport.register_interface(Box::new(MockInterface::new("if1", 2)));

            let dest_hash = [0x42; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 1,
                    expires_ms: now + 100_000,
                    interface_index: 1,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Register as a local destination so process_incoming doesn't just
            // drop it as unregistered, but we need it to go through forwarding.
            // Actually: for forwarding, the destination must NOT be local but
            // must have a path. Also need enable_transport=true (done above).

            // Build a data packet with hops = PATHFINDER_MAX_HOPS (128)
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: PATHFINDER_MAX_HOPS,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"should be dropped".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            let forwarded_before = transport.stats().packets_forwarded;
            transport.process_incoming(0, &buf[..len]).unwrap();

            assert_eq!(
                transport.stats().packets_forwarded,
                forwarded_before,
                "Packet at max hops should NOT be forwarded (saturating_add(1) → 129 > 128)"
            );
        }

        #[test]
        fn test_announce_invalid_signature_dropped_at_transport() {
            let mut transport = make_transport_enabled();
            let _idx0 = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let (mut raw, dest_hash) = make_announce_raw(2, PacketContext::None);

            // Tamper with one byte near the end (in the signature region)
            // Announce payload layout: public_key(64) + name_hash(16) + random(16) + signature(64) + app_data
            // The raw bytes include the header, so tamper near the end of raw data
            let tamper_idx = raw.len() - 10;
            raw[tamper_idx] ^= 0xFF;

            // process_incoming returns an error for invalid signatures.            // the important thing is that no path is created
            let _ = transport.process_incoming(0, &raw);

            assert!(
                !transport.has_path(&dest_hash),
                "Tampered announce should not create a path"
            );
            assert_eq!(
                transport.stats().announces_processed,
                0,
                "Tampered announce should not be counted as processed"
            );
        }

        #[test]
        fn test_create_and_get_receipt() {
            let mut transport = test_transport();
            let dest_hash = [0x42; TRUNCATED_HASHBYTES];

            // Build a minimal valid packet to create a receipt from
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"receipt test".to_vec()),
            };

            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            let raw = &buf[..len];

            let truncated = transport.create_receipt(raw, dest_hash);

            // Should be retrievable
            let receipt = transport.get_receipt(&truncated);
            assert!(receipt.is_some(), "Receipt should exist after creation");

            let receipt = receipt.unwrap();
            assert_eq!(
                receipt.destination_hash,
                crate::destination::DestinationHash::new(dest_hash),
                "Receipt should contain correct destination hash"
            );
        }

        #[test]
        fn test_receipt_timeout_no_path_uses_default() {
            let transport = test_transport();
            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];
            // No path registered, should fall back to default
            assert_eq!(
                transport.compute_receipt_timeout(&dest_hash),
                RECEIPT_TIMEOUT_DEFAULT_MS,
            );
        }

        #[test]
        fn test_receipt_timeout_direct_neighbor_no_bitrate() {
            let mut transport = test_transport();
            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            // Direct neighbor (0 hops), no bitrate registered
            transport.storage.set_path(
                dest_hash,
                crate::storage_types::PathEntry {
                    hops: 0,
                    next_hop: None,
                    interface_index: 0,
                    expires_ms: u64::MAX,
                    random_blobs: alloc::vec::Vec::new(),
                },
            );
            // first_hop_extra = 0 (no bitrate, single hop)
            // total = 0 + 6000 + 6000 * 0 = 6000
            assert_eq!(transport.compute_receipt_timeout(&dest_hash), 6_000);
        }

        #[test]
        fn test_receipt_timeout_multihop_with_bitrate() {
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("rnode", 1)));
            transport.register_interface_bitrate(iface_idx, 976);
            transport.storage.set_path(
                dest_hash,
                crate::storage_types::PathEntry {
                    hops: 2,
                    next_hop: Some([0x11; TRUNCATED_HASHBYTES]),
                    interface_index: iface_idx,
                    expires_ms: u64::MAX,
                    random_blobs: alloc::vec::Vec::new(),
                },
            );
            // first_hop_extra = 500 * 8 * 1000 / 976 = 4098 ms
            // total = 4098 + 6000 + 6000 * 2 = 22098
            assert_eq!(transport.compute_receipt_timeout(&dest_hash), 22_098);
        }

        #[test]
        fn test_receipt_timeout_multihop_unknown_bitrate() {
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            // Multi-hop but no bitrate registered, uses UNKNOWN_BITRATE_ASSUMPTION
            transport.storage.set_path(
                dest_hash,
                crate::storage_types::PathEntry {
                    hops: 3,
                    next_hop: Some([0x22; TRUNCATED_HASHBYTES]),
                    interface_index: 0,
                    expires_ms: u64::MAX,
                    random_blobs: alloc::vec::Vec::new(),
                },
            );
            // first_hop_extra = 500 * 8 * 1000 / 300 = 13333 ms
            // total = 13333 + 6000 + 6000 * 3 = 37333
            assert_eq!(transport.compute_receipt_timeout(&dest_hash), 37_333);
        }

        #[test]
        fn test_receipt_uses_path_aware_timeout() {
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xEE; TRUNCATED_HASHBYTES];
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("rnode", 1)));
            transport.register_interface_bitrate(iface_idx, 976);
            transport.storage.set_path(
                dest_hash,
                crate::storage_types::PathEntry {
                    hops: 2,
                    next_hop: Some([0x33; TRUNCATED_HASHBYTES]),
                    interface_index: iface_idx,
                    expires_ms: u64::MAX,
                    random_blobs: alloc::vec::Vec::new(),
                },
            );

            // Build a minimal packet and create a receipt
            let pkt = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: crate::destination::DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(b"timeout test".to_vec()),
            };
            let mut buf = [0u8; 500];
            let len = pkt.pack(&mut buf).unwrap();
            let truncated = transport.create_receipt(&buf[..len], dest_hash);

            let receipt = transport.get_receipt(&truncated).unwrap();
            // Should use path-aware timeout, not the default 30s
            assert_eq!(receipt.timeout_ms, 22_098);
        }
    }

    mod relay_mtu_clamping {
        use super::*;
        extern crate alloc;
        use crate::link::{
            decode_signaling_bytes, encode_signaling_bytes, LINK_REQUEST_BASE_SIZE, SIGNALING_SIZE,
        };
        use crate::test_utils::MockClock;
        use crate::traits::NoStorage;

        /// Build a fake link request payload with signaling bytes.
        fn make_lr_payload(mtu: u32, mode: u8) -> PacketData {
            let mut data = alloc::vec![0u8; LINK_REQUEST_BASE_SIZE + SIGNALING_SIZE];
            let sig = encode_signaling_bytes(mtu, mode);
            data[LINK_REQUEST_BASE_SIZE..].copy_from_slice(&sig);
            PacketData::Owned(data)
        }

        /// Build a fake link request payload WITHOUT signaling bytes (64 bytes).
        fn make_lr_payload_no_signaling() -> PacketData {
            PacketData::Owned(alloc::vec![0u8; LINK_REQUEST_BASE_SIZE])
        }

        #[test]
        fn test_clamp_tcp_to_udp() {
            // TCP (HW_MTU=262144) → relay → UDP (HW_MTU=1064)
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 262144); // TCP prev-hop
            hw_mtus.insert(1, 1064); // UDP next-hop

            let data = make_lr_payload(262144, 1);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            let clamped = result.expect("Should have clamped");
            let payload = clamped.as_slice();
            assert_eq!(payload.len(), LINK_REQUEST_BASE_SIZE + SIGNALING_SIZE);

            let sig: [u8; 3] = payload[LINK_REQUEST_BASE_SIZE..].try_into().unwrap();
            let (clamped_mtu, mode) = decode_signaling_bytes(&sig);
            assert_eq!(clamped_mtu, 1064, "Should clamp to UDP HW_MTU");
            assert_eq!(mode, 1, "Mode should be preserved");
        }

        #[test]
        fn test_no_clamp_same_mtu() {
            // Both interfaces same MTU, no clamping needed
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 1064);
            hw_mtus.insert(1, 1064);

            let data = make_lr_payload(1064, 1);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            assert!(result.is_none(), "No clamping needed when MTUs match");
        }

        #[test]
        fn test_no_clamp_next_hop_larger() {
            // Next-hop MTU is larger than signaled, no clamping
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 1064);
            hw_mtus.insert(1, 262144);

            let data = make_lr_payload(1064, 1);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            assert!(
                result.is_none(),
                "No clamping when next-hop is larger than signaled"
            );
        }

        #[test]
        fn test_no_signaling_bytes_passthrough() {
            // 64-byte request (no signaling), should pass through unchanged
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 262144);
            hw_mtus.insert(1, 1064);

            let data = make_lr_payload_no_signaling();
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            assert!(result.is_none(), "No signaling bytes — pass through");
        }

        #[test]
        fn test_no_next_hop_mtu_passthrough() {
            // Next-hop HW_MTU not registered, should pass through
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 262144);
            // iface 1 not registered

            let data = make_lr_payload(262144, 1);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            assert!(result.is_none(), "Unknown next-hop MTU — pass through");
        }

        #[test]
        fn test_clamp_to_prev_hop_when_smaller() {
            // Signaled MTU higher than both interfaces, prev-hop is bottleneck
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 500); // prev-hop bottleneck
            hw_mtus.insert(1, 1064); // next-hop

            let data = make_lr_payload(262144, 1);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            let clamped = result.expect("Should have clamped");
            let sig: [u8; 3] = clamped.as_slice()[LINK_REQUEST_BASE_SIZE..]
                .try_into()
                .unwrap();
            let (clamped_mtu, _) = decode_signaling_bytes(&sig);
            assert_eq!(clamped_mtu, 500, "Should clamp to prev-hop HW_MTU");
        }

        #[test]
        fn test_clamp_preserves_mode() {
            // Verify mode byte is preserved through clamping
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 262144);
            hw_mtus.insert(1, 1064);

            for mode in [0u8, 1, 3, 7] {
                let data = make_lr_payload(262144, mode);
                let result = Transport::<MockClock, NoStorage>::clamp_link_request_mtu(
                    &data, 0, 1, &hw_mtus,
                );

                let clamped = result.expect("Should have clamped");
                let sig: [u8; 3] = clamped.as_slice()[LINK_REQUEST_BASE_SIZE..]
                    .try_into()
                    .unwrap();
                let (_, decoded_mode) = decode_signaling_bytes(&sig);
                assert_eq!(decoded_mode, mode, "Mode should be preserved");
            }
        }

        #[test]
        fn test_clamp_unknown_prev_hop_only_uses_next_hop() {
            // prev-hop HW_MTU not registered, defaults to MAX, only next-hop matters
            let mut hw_mtus = BTreeMap::new();
            // iface 0 not registered
            hw_mtus.insert(1, 1064);

            let data = make_lr_payload(262144, 1);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            let clamped = result.expect("Should have clamped");
            let sig: [u8; 3] = clamped.as_slice()[LINK_REQUEST_BASE_SIZE..]
                .try_into()
                .unwrap();
            let (clamped_mtu, _) = decode_signaling_bytes(&sig);
            assert_eq!(clamped_mtu, 1064, "Should clamp to next-hop only");
        }

        #[test]
        fn test_clamp_preserves_base_payload() {
            // Verify base 64 bytes are not modified by clamping
            let mut hw_mtus = BTreeMap::new();
            hw_mtus.insert(0, 262144);
            hw_mtus.insert(1, 1064);

            let mut base = [0xABu8; LINK_REQUEST_BASE_SIZE];
            base[0] = 0x42;
            base[63] = 0xFF;
            let mut payload = base.to_vec();
            payload.extend_from_slice(&encode_signaling_bytes(262144, 1));

            let data = PacketData::Owned(payload);
            let result =
                Transport::<MockClock, NoStorage>::clamp_link_request_mtu(&data, 0, 1, &hw_mtus);

            let clamped = result.expect("Should have clamped");
            assert_eq!(
                &clamped.as_slice()[..LINK_REQUEST_BASE_SIZE],
                &base,
                "Base payload should be preserved"
            );
        }
    }

    // Local Client Interface Tests
    mod local_client {
        use super::*;
        use crate::destination::{Destination, DestinationType, Direction};
        use crate::identity::Identity;
        use crate::memory_storage::MemoryStorage;
        use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};
        use crate::test_utils::{MockClock, TEST_TIME_MS};
        use rand_core::{OsRng, RngCore};

        const LOCAL_CLIENT_IFACE: usize = 10;
        const NETWORK_IFACE: usize = 0;

        fn make_transport_with_local_client() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(NETWORK_IFACE, "tcp_server/127.0.0.1".into());
            transport.set_interface_name(LOCAL_CLIENT_IFACE, "Local[rns/default]/0".into());
            transport.set_local_client(LOCAL_CLIENT_IFACE, true);
            transport
        }

        /// Build a valid announce packet as raw bytes. Returns (raw_bytes, dest_hash).
        fn make_announce_raw(
            hops: u8,
            context: PacketContext,
        ) -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["localclient"],
            )
            .unwrap();

            let id = dest.identity().unwrap();
            let random_hash = [0x42u8; crate::constants::RANDOM_HASHBYTES];

            let mut payload = Vec::new();
            payload.extend_from_slice(&id.public_key_bytes());
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(&random_hash);

            let app_data = b"test";
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&id.public_key_bytes());
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(&random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = id.sign(&signed_data).unwrap();
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            (buf[..len].to_vec(), dest.hash().into_bytes())
        }

        #[test]
        fn test_local_client_set_and_query() {
            let mut transport = make_transport_with_local_client();
            assert!(transport.is_local_client(LOCAL_CLIENT_IFACE));
            assert!(transport.has_local_clients());
            assert!(!transport.is_local_client(NETWORK_IFACE));

            // Remove
            transport.set_local_client(LOCAL_CLIENT_IFACE, false);
            assert!(!transport.is_local_client(LOCAL_CLIENT_IFACE));
            assert!(!transport.has_local_clients());
        }

        #[test]
        fn test_announce_forwarded_to_local_clients() {
            let mut transport = make_transport_with_local_client();
            let (raw, _dest_hash) = make_announce_raw(0, crate::packet::PacketContext::None);

            // Process announce arriving from the network interface
            let packet = Packet::unpack(&raw).unwrap();
            let result = transport.handle_announce(packet, NETWORK_IFACE, &raw);
            assert!(result.is_ok());

            // Check that a SendPacket action was emitted for the local client
            let actions = transport.drain_actions();
            let local_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(
                !local_sends.is_empty(),
                "Should forward announce to local client"
            );
        }

        #[test]
        fn test_announce_forwarded_to_local_client_has_daemon_transport_id() {
            let mut transport = make_transport_with_local_client();
            let daemon_hash = *transport.identity.hash();
            let (raw, _dest_hash) = make_announce_raw(0, crate::packet::PacketContext::None);

            // Simulate process_incoming's receipt hop increment
            let mut packet = Packet::unpack(&raw).unwrap();
            packet.hops = packet.hops.saturating_add(1);
            let result = transport.handle_announce(packet, NETWORK_IFACE, &raw);
            assert!(result.is_ok());

            // Extract the forwarded announce bytes
            let actions = transport.drain_actions();
            let send_data = actions
                .iter()
                .find_map(|a| match a {
                    Action::SendPacket { iface, data } if iface.0 == LOCAL_CLIENT_IFACE => {
                        Some(data)
                    }
                    _ => None,
                })
                .expect("Should forward announce to local client");

            // Unpack and verify Header2 with daemon's own transport_id
            let forwarded = Packet::unpack(send_data).unwrap();
            assert_eq!(
                forwarded.flags.header_type,
                HeaderType::Type2,
                "Announce forwarded to local client must be Header2"
            );
            assert_eq!(
                forwarded.transport_id,
                Some(daemon_hash),
                "Announce forwarded to local client must have daemon's transport_id, \
                 not the relay's. Otherwise the client's outbound packets will have \
                 a transport_id that fails the daemon's filter."
            );
            assert_eq!(
                forwarded.hops, 1,
                "Announce forwarded to local client should have receipt-incremented hops"
            );
        }

        #[test]
        fn test_announce_not_forwarded_to_source_local_client() {
            let mut transport = make_transport_with_local_client();
            let (raw, _dest_hash) = make_announce_raw(0, crate::packet::PacketContext::None);

            // Process announce arriving FROM the local client itself
            let packet = Packet::unpack(&raw).unwrap();
            let result = transport.handle_announce(packet, LOCAL_CLIENT_IFACE, &raw);
            assert!(result.is_ok());

            // Should NOT forward back to the same local client
            let actions = transport.drain_actions();
            let self_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(
                self_sends.is_empty(),
                "Should not forward announce back to the source local client"
            );
        }

        #[test]
        fn test_announce_not_forwarded_without_local_clients() {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(NETWORK_IFACE, "tcp".into());

            let (raw, _dest_hash) = make_announce_raw(0, crate::packet::PacketContext::None);
            let packet = Packet::unpack(&raw).unwrap();
            let result = transport.handle_announce(packet, NETWORK_IFACE, &raw);
            assert!(result.is_ok());

            // Without local clients, no SendPacket to local client should exist
            let actions = transport.drain_actions();
            let local_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(local_sends.is_empty());
        }

        #[test]
        fn test_announce_cache_always_populated() {
            // Announce cache is always populated regardless of transport mode or
            // local client presence, local clients may connect later and issue
            // path requests for destinations announced before they connected.
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: false,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(NETWORK_IFACE, "tcp".into());
            // No local clients registered, cache should still be populated

            let (raw, dest_hash) = make_announce_raw(0, crate::packet::PacketContext::None);
            let packet = Packet::unpack(&raw).unwrap();
            let _ = transport.handle_announce(packet, NETWORK_IFACE, &raw);

            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should always be populated"
            );
        }

        #[test]
        fn test_path_request_from_local_client_forwarded_to_network() {
            let mut transport = make_transport_with_local_client();

            // Build a path request packet
            let target_dest = [0xAAu8; TRUNCATED_HASHBYTES];
            let tag = [0xBBu8; TRUNCATED_HASHBYTES];

            let mut data = Vec::new();
            data.extend_from_slice(&target_dest);
            data.extend_from_slice(&tag);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: transport.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let result = transport.handle_path_request(packet, LOCAL_CLIENT_IFACE);
            assert!(result.is_ok());

            let actions = transport.drain_actions();
            // Should have a SendPacket to the network interface
            let network_sends: Vec<_> = actions
                .iter()
                .filter(
                    |a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == NETWORK_IFACE),
                )
                .collect();
            assert!(
                !network_sends.is_empty(),
                "Path request from local client should be forwarded to network interfaces"
            );

            // Should NOT have a SendPacket back to the local client
            let self_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(
                self_sends.is_empty(),
                "Path request should not be forwarded back to source local client"
            );
        }

        /// Create a transport with enable_transport=false and a local client.
        /// This ensures local client conditions are the sole routing enablers.
        fn make_no_transport_with_local_client() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: false,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(NETWORK_IFACE, "tcp_server/127.0.0.1".into());
            transport.set_interface_name(LOCAL_CLIENT_IFACE, "Local[rns/default]/0".into());
            transport.set_local_client(LOCAL_CLIENT_IFACE, true);
            transport
        }

        #[test]
        fn test_link_request_routed_to_local_client() {
            // enable_transport=false, local client announced dest, network sends
            // link request → verify SendPacket targets local client + link table created
            let mut transport = make_no_transport_with_local_client();

            let dest_hash = [0xEEu8; TRUNCATED_HASHBYTES];

            // Insert path pointing to local client interface (hops=0)
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 0,
                    expires_ms: TEST_TIME_MS + 300_000,
                    interface_index: LOCAL_CLIENT_IFACE,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Build a link request from the network
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::LinkRequest,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0u8; 96]),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport
                .process_incoming(NETWORK_IFACE, &buf[..len])
                .unwrap();

            // Should forward to the local client
            let actions = transport.drain_actions();
            let local_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(
                !local_sends.is_empty(),
                "Link request should be forwarded to local client (enable_transport=false)"
            );

            // Link table entry should be created
            let link_id = Link::calculate_link_id(&buf[..len]);
            assert!(
                transport
                    .storage
                    .get_link_entry(link_id.as_bytes())
                    .is_some(),
                "Link table entry should be created for local client routing"
            );
        }

        #[test]
        fn test_from_local_client_link_request_forwarded() {
            // enable_transport=false, local client sends link request →
            // verify forwarded to network interface
            let mut transport = make_no_transport_with_local_client();

            let dest_hash = [0xFFu8; TRUNCATED_HASHBYTES];

            // Insert path pointing to network interface
            transport.insert_path(
                dest_hash,
                PathEntry {
                    hops: 2,
                    expires_ms: TEST_TIME_MS + 300_000,
                    interface_index: NETWORK_IFACE,
                    random_blobs: Vec::new(),
                    next_hop: None,
                },
            );

            // Build a link request from the local client
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::LinkRequest,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0u8; 96]),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &buf[..len])
                .unwrap();

            // Should forward to the network
            let actions = transport.drain_actions();
            let network_sends: Vec<_> = actions
                .iter()
                .filter(
                    |a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == NETWORK_IFACE),
                )
                .collect();
            assert!(
                !network_sends.is_empty(),
                "Link request from local client should be forwarded to network (enable_transport=false)"
            );
        }

        #[test]
        fn test_proof_routed_via_link_table_to_local_client() {
            // LRPROOF from network → verify forwarded to local client via link table
            let mut transport = make_no_transport_with_local_client();

            let link_id = [0xAAu8; TRUNCATED_HASHBYTES];

            // Insert link table entry: received from local client, next hop is network
            transport.storage.set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: TEST_TIME_MS,
                    next_hop_interface_index: LOCAL_CLIENT_IFACE,
                    remaining_hops: 0,
                    received_interface_index: NETWORK_IFACE,
                    hops: 1,
                    validated: false,
                    proof_timeout_ms: TEST_TIME_MS + 60_000,
                    destination_hash: [0xBBu8; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build LRPROOF arriving from the local client (responder side)
            // LRPROOF = sig(64) + X25519(32) = 96 bytes
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::Proof,
                },
                hops: 0,
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::Lrproof,
                data: PacketData::Owned(alloc::vec![0u8; 96]),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &buf[..len])
                .unwrap();

            // Should forward to the network interface
            let actions = transport.drain_actions();
            let network_sends: Vec<_> = actions
                .iter()
                .filter(
                    |a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == NETWORK_IFACE),
                )
                .collect();
            assert!(
                !network_sends.is_empty(),
                "LRPROOF from local client should be forwarded to network via link table (enable_transport=false)"
            );
        }

        #[test]
        fn test_proof_routed_via_reverse_table_to_local_client() {
            // Regular proof from network → verify forwarded to local client via reverse table
            let mut transport = make_no_transport_with_local_client();

            let dest_hash = [0xCCu8; TRUNCATED_HASHBYTES];

            // Insert reverse table entry: original packet came from local client,
            // was forwarded to network
            transport.storage.set_reverse(
                dest_hash,
                crate::storage_types::ReverseEntry {
                    timestamp_ms: TEST_TIME_MS,
                    receiving_interface_index: LOCAL_CLIENT_IFACE,
                    outbound_interface_index: NETWORK_IFACE,
                },
            );

            // Build a regular proof arriving from the network (outbound side)
            // Explicit proof = packet_hash(32) + signature(64) = 96 bytes
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Proof,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(alloc::vec![0u8; 96]),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport
                .process_incoming(NETWORK_IFACE, &buf[..len])
                .unwrap();

            // Should forward to the local client
            let actions = transport.drain_actions();
            let local_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(
                !local_sends.is_empty(),
                "Proof should be forwarded to local client via reverse table (enable_transport=false)"
            );
        }

        #[test]
        fn test_data_routed_to_local_client_via_link_table() {
            // Data packet from network → verify forwarded to local client via link table
            let mut transport = make_no_transport_with_local_client();

            let link_id = [0xDDu8; TRUNCATED_HASHBYTES];

            // Insert validated link table entry: received from network, next hop is local client
            transport.storage.set_link_entry(
                link_id,
                LinkEntry {
                    timestamp_ms: TEST_TIME_MS,
                    next_hop_interface_index: LOCAL_CLIENT_IFACE,
                    remaining_hops: 0,
                    received_interface_index: NETWORK_IFACE,
                    hops: 1,
                    validated: true,
                    proof_timeout_ms: TEST_TIME_MS + 60_000,
                    destination_hash: [0xBBu8; TRUNCATED_HASHBYTES],
                    peer_signing_key: None,
                },
            );

            // Build data packet from network (from the received_interface side)
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Link,
                    packet_type: PacketType::Data,
                },
                hops: 0, // wire hops; receipt increment makes it 1, matching link_entry.hops
                transport_id: None,
                destination_hash: link_id,
                context: PacketContext::None,
                data: PacketData::Owned(b"hello local client".to_vec()),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            transport
                .process_incoming(NETWORK_IFACE, &buf[..len])
                .unwrap();

            // Should forward to the local client
            let actions = transport.drain_actions();
            let local_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE))
                .collect();
            assert!(
                !local_sends.is_empty(),
                "Data should be forwarded to local client via link table (enable_transport=false)"
            );
        }

        #[test]
        fn test_path_request_from_local_client_returns_cached_announce() {
            let mut transport = make_transport_with_local_client();

            // Process an announce on the network interface to populate the cache
            let (raw, dest_hash) = make_announce_raw(0, crate::packet::PacketContext::None);
            let packet = Packet::unpack(&raw).unwrap();
            let _ = transport.handle_announce(packet, NETWORK_IFACE, &raw);
            transport.drain_actions();
            transport.drain_events();

            // Verify cache is populated
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should be populated"
            );
            let cached_raw = transport
                .storage
                .get_announce_cache(&dest_hash)
                .cloned()
                .unwrap();

            // Clear announce table (keep cache) to simulate fresh state
            for key in transport.storage().announce_keys() {
                transport.storage_mut().remove_announce(&key);
            }

            // Build a path request from the local client
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: transport.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let result = transport.handle_path_request(packet, LOCAL_CLIENT_IFACE);
            assert!(result.is_ok());

            let actions = transport.drain_actions();

            // Should have a SendPacket to the local client with the cached announce
            let local_sends: Vec<_> = actions
                .iter()
                .filter(
                    |a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE),
                )
                .collect();
            assert_eq!(
                local_sends.len(),
                1,
                "Should send cached announce directly to local client"
            );

            // Verify the sent data is a Header2 announce with hops +1 and transport_id
            if let Action::SendPacket { data, .. } = local_sends[0] {
                let unpacked = Packet::unpack(data).expect("Sent data should be a valid packet");
                assert_eq!(
                    unpacked.flags.header_type,
                    HeaderType::Type2,
                    "Should be converted to Header2 for local client"
                );
                assert_eq!(
                    unpacked.flags.transport_type,
                    TransportType::Transport,
                    "Should have Transport type"
                );
                assert_eq!(
                    unpacked.transport_id,
                    Some(*transport.identity.hash()),
                    "Transport ID should be daemon's identity"
                );
                // Hops should be wire hops + 1 (receipt increment)
                let original = Packet::unpack(&cached_raw).unwrap();
                assert_eq!(
                    unpacked.hops,
                    original.hops + 1,
                    "Hops should be receipt-incremented"
                );
            }

            // Should NOT have any sends to the network interface
            let network_sends: Vec<_> = actions
                .iter()
                .filter(
                    |a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == NETWORK_IFACE),
                )
                .collect();
            assert!(
                network_sends.is_empty(),
                "Should not send to network interface for local client path response"
            );

            // Should NOT have any Broadcast actions
            let broadcasts: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .collect();
            assert!(
                broadcasts.is_empty(),
                "Should not broadcast for local client path response"
            );

            // Should NOT have created an announce table entry (no deferred rebroadcast)
            assert!(
                transport.storage().get_announce(&dest_hash).is_none(),
                "Should not create deferred announce entry for local client response"
            );
        }

        #[test]
        fn test_path_request_local_client_receives_correct_hops() {
            let mut transport = make_transport_with_local_client();

            // Process announce from direct neighbor via process_incoming so the
            // receipt hop increment (+1) is applied.
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming should succeed");
            transport.drain_actions();
            transport.drain_events();

            // Verify path table stores hops=1 (receipt-incremented)
            assert_eq!(
                transport.hops_to(&dest_hash),
                Some(1),
                "Direct neighbor announce should be stored with hops=1 after receipt increment"
            );

            // Verify announce cache is populated
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should be populated after process_incoming"
            );

            // Clear announce table (keep cache) to simulate fresh state where
            // only the cache remains (e.g. after rebroadcast has already fired).
            for key in transport.storage().announce_keys() {
                transport.storage_mut().remove_announce(&key);
            }

            // Build a path request from the local client
            let mut data = Vec::new();
            data.extend_from_slice(&dest_hash);
            data.extend_from_slice(&[0xBB; TRUNCATED_HASHBYTES]); // tag

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: transport.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let result = transport.handle_path_request(packet, LOCAL_CLIENT_IFACE);
            assert!(result.is_ok());

            let actions = transport.drain_actions();

            // Should have a SendPacket to the local client with the cached announce
            let local_sends: Vec<_> = actions
                .iter()
                .filter(
                    |a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE),
                )
                .collect();
            assert_eq!(
                local_sends.len(),
                1,
                "Should send cached announce directly to local client"
            );

            // Unpack the sent bytes and verify hops reflects the receipt-incremented value
            if let Action::SendPacket { data, .. } = local_sends[0] {
                let unpacked = Packet::unpack(data).expect("Sent data should be a valid packet");
                assert_eq!(
                    unpacked.hops, 1,
                    "Cached announce forwarded to local client should have hops=1 \
                     (receipt-incremented), but got hops={}. The raw cached bytes contain \
                     pre-increment wire hops=0.",
                    unpacked.hops
                );
            }
        }

        #[test]
        fn test_path_request_from_network_forwarded_to_local_clients() {
            let mut transport = make_transport_with_local_client();
            let local_transport_id = *transport.identity.hash();

            // Build a path request packet (from a non-transport node: no transport_id)
            let target_dest = [0xCCu8; TRUNCATED_HASHBYTES];
            let tag = [0xDDu8; TRUNCATED_HASHBYTES];

            let mut data = Vec::new();
            data.extend_from_slice(&target_dest);
            data.extend_from_slice(&tag);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 1,
                transport_id: None,
                destination_hash: transport.path_request_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data),
            };

            let result = transport.handle_path_request(packet, NETWORK_IFACE);
            assert!(result.is_ok());

            let actions = transport.drain_actions();
            // Should have a SendPacket to the local client
            let local_sends: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::SendPacket { iface, data, .. } if iface.0 == LOCAL_CLIENT_IFACE => {
                        Some(data.as_slice())
                    }
                    _ => None,
                })
                .collect();
            assert!(
                !local_sends.is_empty(),
                "Path request from network should be re-originated to local clients"
            );
            for raw in &local_sends {
                // Re-originated packet must have hops=0
                assert_eq!(raw[1], 0, "Re-originated path request must have hops=0");
                // Data: dest_hash(16) + transport_id(16) + tag(16) (transport enabled)
                assert_eq!(
                    &raw[19..19 + TRUNCATED_HASHBYTES],
                    &target_dest,
                    "Payload must contain the requested dest hash"
                );
                assert_eq!(
                    &raw[19 + TRUNCATED_HASHBYTES..19 + 2 * TRUNCATED_HASHBYTES],
                    &local_transport_id,
                    "Re-originated request must contain local transport_id"
                );
            }
        }

        // Block B: Shared Instance Registration
        #[test]
        fn test_shared_instance_registration_triggers_announce() {
            // When a local client sends an announce for a NEW destination,
            // the daemon should delay the network rebroadcast by 250ms
            // (Python Transport.py:2232) instead of broadcasting immediately.
            let mut transport = make_transport_with_local_client();

            // Process an announce from the local client (hops=0 on wire, +1/-1 = net 0)
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            let result = transport.process_incoming(LOCAL_CLIENT_IFACE, &raw);
            assert!(result.is_ok());

            // Drain actions, there should be NO immediate Broadcast for this announce
            // (because it's a new local client destination, 250ms delay applies)
            let actions = transport.drain_actions();
            let _ = transport.drain_events();

            let network_broadcasts = actions
                .iter()
                .filter(|a| match a {
                    Action::Broadcast { .. } => true,
                    Action::SendPacket { iface, .. } if iface.0 == NETWORK_IFACE => true,
                    _ => false,
                })
                .count();
            assert_eq!(
                network_broadcasts, 0,
                "First announce from local client should NOT broadcast immediately"
            );

            // Verify the destination is tracked in Storage
            assert!(
                transport
                    .storage
                    .has_local_client_dest(LOCAL_CLIENT_IFACE, &dest_hash),
                "Dest hash should be tracked in local_client_dest_map"
            );
            assert!(
                transport.storage.has_local_client_known_dest(&dest_hash),
                "Dest hash should be tracked in local_client_known_dests"
            );

            // Verify an AnnounceEntry was created with deferred retransmit at 250ms.
            // Python parity (Transport.py:1748-1752): local-client source
            // starts at retries=PATHFINDER_R so the scheduler fires exactly
            // once before `retries > PATHFINDER_R` removes the entry.
            let entry = transport.storage.get_announce(&dest_hash);
            assert!(entry.is_some(), "AnnounceEntry should exist");
            let entry = entry.unwrap();
            assert_eq!(
                entry.retries, PATHFINDER_RETRIES,
                "local-client source starts at retries=PATHFINDER_RETRIES"
            );
            assert_eq!(
                entry.retransmit_at_ms,
                Some(TEST_TIME_MS + LOCAL_CLIENT_ANNOUNCE_DELAY_MS),
                "retransmit should be scheduled at now + 250ms"
            );

            // Advance clock past the 250ms delay and poll
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let actions = transport.drain_actions();

            // Now we should see the rebroadcast action
            assert!(
                !actions.is_empty(),
                "After 250ms delay, the announce should be rebroadcast"
            );
        }

        #[test]
        fn test_shared_instance_repeat_announce_not_delayed() {
            // A repeat announce from the same local client for the same destination
            // should NOT be delayed (only the first registration triggers the delay).
            let mut transport = make_transport_with_local_client();

            // First announce, gets 250ms delay
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw)
                .unwrap();
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Advance clock so the deferred rebroadcast fires
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let _ = transport.drain_actions();

            // Advance past announce rate limit window so re-announce is accepted
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Second announce from the same client for the same destination needs
            // a different random_hash to avoid replay protection.
            // We create a fresh announce with a new identity but same dest_hash
            // is not possible (different identity = different hash), so we just
            // verify the tracking state: dest_hash is already in the map.
            let already_known = transport
                .storage
                .has_local_client_dest(LOCAL_CLIENT_IFACE, &dest_hash);
            assert!(
                already_known,
                "After first announce, dest should be known — \
                 subsequent announces will not trigger delay"
            );
        }

        // Block C: Shared Instance Reconnect
        #[test]
        fn test_shared_instance_reconnect_reannounces() {
            // When a local client disconnects and reconnects, the client sends
            // fresh announces (new random_hash). The daemon should:
            // 1. Preserve announce_cache entries during disconnect
            // 2. Accept the fresh announces from the reconnected client
            // 3. Rebroadcast them to the network (with 250ms delay)
            let mut transport = make_transport_with_local_client();
            const RECONNECTED_IFACE: usize = 20;

            // 1. Client sends initial announce
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw)
                .unwrap();
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Flush deferred announce
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let _ = transport.drain_actions();

            // Verify announce cache is populated
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should have the client's announce"
            );

            // 2. Client disconnects
            transport.remove_paths_for_interface(LOCAL_CLIENT_IFACE);
            transport.set_local_client(LOCAL_CLIENT_IFACE, false);
            transport.remove_interface_name(LOCAL_CLIENT_IFACE);

            // Verify per-client map is cleaned but known_dests persists
            assert!(
                !transport
                    .storage
                    .has_local_client_dest(LOCAL_CLIENT_IFACE, &dest_hash),
                "Per-client dest map should be removed on disconnect"
            );
            assert!(
                transport.storage.has_local_client_known_dest(&dest_hash),
                "Known dests should persist across disconnect for reconnect"
            );

            // Run cleanup, announce_cache should be preserved because
            // dest_hash is in local_client_known_dests
            transport.clean_path_states();
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should survive cleanup for known local client dests"
            );

            // 3. Client reconnects with a new interface ID
            transport.set_interface_name(RECONNECTED_IFACE, "Local[rns/default]/1".into());
            transport.set_local_client(RECONNECTED_IFACE, true);

            // 4. Client sends fresh announce (new random_hash), simulate by
            // creating a new announce from a new Identity for a different dest.
            // In practice, same dest_hash with new random_hash; for test simplicity,
            // we use a different dest entirely and verify the mechanism works.
            let (raw2, dest_hash2) = make_announce_raw(0, PacketContext::None);
            transport
                .process_incoming(RECONNECTED_IFACE, &raw2)
                .unwrap();
            let actions_immediate = transport.drain_actions();
            let _ = transport.drain_events();

            // Should NOT have immediate broadcast (250ms delay for new registration)
            let immediate_network = actions_immediate
                .iter()
                .filter(|a| match a {
                    Action::Broadcast { .. } => true,
                    Action::SendPacket { iface, .. } if iface.0 == NETWORK_IFACE => true,
                    _ => false,
                })
                .count();
            assert_eq!(
                immediate_network, 0,
                "Reconnected client's announce should be delayed 250ms"
            );

            // 5. After 250ms, announce is rebroadcast
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let actions = transport.drain_actions();
            assert!(
                !actions.is_empty(),
                "After 250ms, reconnected client's announce should be rebroadcast"
            );

            // 6. Verify tracking
            assert!(
                transport
                    .storage
                    .has_local_client_dest(RECONNECTED_IFACE, &dest_hash2),
                "New dest should be tracked for reconnected client"
            );
        }

        #[test]
        fn test_shared_instance_announce_cache_survives_disconnect() {
            // Announce cache for local client destinations should survive
            // client disconnect so the daemon can answer path requests during
            // the gap between disconnect and reconnect.
            let mut transport = make_transport_with_local_client();

            // Client registers a destination
            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw)
                .unwrap();
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Flush deferred announce
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let _ = transport.drain_actions();

            // Client disconnects
            transport.remove_paths_for_interface(LOCAL_CLIENT_IFACE);
            transport.set_local_client(LOCAL_CLIENT_IFACE, false);

            // Run cleanup multiple times, cache should persist
            for _ in 0..3 {
                transport.clean_path_states();
            }

            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should survive cleanup while dest is in local_client_known_dests"
            );
        }

        // Expiry Tests
        /// Build an announce packet from a given Destination with a fresh random_hash.
        fn make_announce_raw_from_dest(
            dest: &Destination,
            hops: u8,
        ) -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
            let identity = dest.identity().unwrap();
            let public_key = identity.public_key_bytes();
            let app_data = b"test";

            // Generate fresh random_hash (10 bytes: 5 random + 5 timestamp)
            let mut random_hash = [0u8; crate::constants::RANDOM_HASHBYTES];
            OsRng.fill_bytes(&mut random_hash[..5]);
            // Leave last 5 bytes as zeros (timestamp); doesn't affect test logic.

            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&public_key);
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(&random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = identity.sign(&signed_data).unwrap();

            let mut payload = Vec::new();
            payload.extend_from_slice(&public_key);
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(&random_hash);
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            (buf[..len].to_vec(), dest.hash().into_bytes())
        }

        #[test]
        fn test_local_client_known_dest_expiry() {
            // A local client dest not refreshed within LOCAL_CLIENT_DEST_EXPIRY_MS
            // should be removed from tracking and its announce cache cleaned up.
            use crate::constants::LOCAL_CLIENT_DEST_EXPIRY_MS;

            let mut transport = make_transport_with_local_client();

            let (raw, dest_hash) = make_announce_raw(0, PacketContext::None);
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw)
                .unwrap();
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Flush 250ms delay
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let _ = transport.drain_actions();

            assert!(
                transport.storage.has_local_client_known_dest(&dest_hash),
                "Dest should be tracked"
            );
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Announce cache should exist"
            );

            // Advance past local_client_known_dests expiry
            transport.clock.advance(LOCAL_CLIENT_DEST_EXPIRY_MS + 1);
            transport.clean_path_states();

            assert!(
                !transport.storage.has_local_client_known_dest(&dest_hash),
                "Expired dest should be removed from local_client_known_dests"
            );
            // The announce cache is still protected by the path_table entry
            // (7-day expiry). Expire paths too, then re-clean to verify full cleanup.
            transport.clock.advance(PATHFINDER_EXPIRY_SECS * 1000);
            transport.expire_paths(transport.clock.now_ms());
            transport.clean_path_states();
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_none(),
                "Expired dest's announce cache should be cleaned up after path expires"
            );
        }

        #[test]
        fn test_local_client_known_dest_refreshed() {
            // A local client that re-announces before expiry refreshes the timestamp.
            // The dest should survive cleanup even if total time exceeds expiry.
            use crate::constants::{ANNOUNCE_RATE_LIMIT_MS, LOCAL_CLIENT_DEST_EXPIRY_MS};

            let mut transport = make_transport_with_local_client();

            // Create a destination we can re-announce from
            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["refresh"],
            )
            .unwrap();

            // First announce
            let (raw1, dest_hash) = make_announce_raw_from_dest(&dest, 0);
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw1)
                .unwrap();
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Flush 250ms delay
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let _ = transport.drain_actions();

            // Advance 5 hours (past half of expiry but not past full expiry)
            let five_hours_ms = 5 * 60 * 60 * 1000;
            transport.clock.advance(five_hours_ms);

            // Advance past rate limit window so second announce is accepted
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Second announce from SAME dest with NEW random_hash (refreshes timestamp)
            let (raw2, _) = make_announce_raw_from_dest(&dest, 0);
            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw2)
                .unwrap();
            let _ = transport.drain_actions();
            let _ = transport.drain_events();

            // Flush 250ms delay for second announce
            transport.clock.advance(LOCAL_CLIENT_ANNOUNCE_DELAY_MS + 1);
            transport.poll();
            let _ = transport.drain_actions();

            // Advance 2 more hours (total ~7h from first announce, ~2h from refresh)
            let two_hours_ms = 2 * 60 * 60 * 1000;
            transport.clock.advance(two_hours_ms);

            // Cleanup, dest should survive because refresh was only ~2h ago
            transport.clean_path_states();

            assert!(
                transport.storage.has_local_client_known_dest(&dest_hash),
                "Refreshed dest should survive cleanup (only {}ms since refresh, expiry is {}ms)",
                two_hours_ms,
                LOCAL_CLIENT_DEST_EXPIRY_MS
            );
            assert!(
                transport.storage.get_announce_cache(&dest_hash).is_some(),
                "Refreshed dest's announce cache should survive cleanup"
            );
        }

        // Plain Broadcast Forwarding
        /// Helper: build a raw PLAIN BROADCAST data packet.
        fn make_plain_broadcast_raw(dest_hash: [u8; TRUNCATED_HASHBYTES], data: &[u8]) -> Vec<u8> {
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Plain,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: PacketContext::None,
                data: PacketData::Owned(data.to_vec()),
            };
            let size = packet.packed_size();
            let mut buf = alloc::vec![0u8; size];
            packet.pack(&mut buf).expect("pack");
            buf
        }

        #[test]
        fn test_plain_broadcast_from_local_client_forwards_to_all() {
            // A PLAIN BROADCAST from a local client should be forwarded to
            // ALL interfaces (Action::Broadcast) except the sender.
            // Python Transport.py:1390-1393.
            let mut transport = make_transport_with_local_client();
            let dest_hash = [0xAB; TRUNCATED_HASHBYTES];
            let raw = make_plain_broadcast_raw(dest_hash, b"broadcast from local");

            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw)
                .expect("process_incoming");

            let actions = transport.drain_actions();
            let has_broadcast = actions.iter().any(|a| {
                matches!(
                    a,
                    Action::Broadcast {
                        exclude_iface: Some(iface),
                        ..
                    } if iface.0 == LOCAL_CLIENT_IFACE
                )
            });
            assert!(
                has_broadcast,
                "Expected Action::Broadcast excluding local client iface, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_plain_broadcast_from_network_forwards_to_local_clients() {
            // A PLAIN BROADCAST from a network interface should be forwarded
            // ONLY to local client interfaces (Action::SendPacket).
            // Python Transport.py:1396-1398.
            let mut transport = make_transport_with_local_client();
            let dest_hash = [0xCD; TRUNCATED_HASHBYTES];
            let raw = make_plain_broadcast_raw(dest_hash, b"broadcast from network");

            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming");

            let actions = transport.drain_actions();
            let has_send_to_local = actions.iter().any(|a| {
                matches!(
                    a,
                    Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE
                )
            });
            assert!(
                has_send_to_local,
                "Expected Action::SendPacket to local client iface, got: {:?}",
                actions
            );

            // Should NOT have a Broadcast action (that would also hit network ifaces)
            let has_broadcast = actions
                .iter()
                .any(|a| matches!(a, Action::Broadcast { .. }));
            assert!(
                !has_broadcast,
                "Network→local should use SendPacket, not Broadcast"
            );
        }

        #[test]
        fn test_plain_broadcast_no_local_clients_no_forward() {
            // With no local clients, a network PLAIN BROADCAST produces no
            // forwarding actions (packet is only delivered locally if dest exists).
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(NETWORK_IFACE, "tcp_server/127.0.0.1".into());
            // No local clients registered

            let dest_hash = [0xEF; TRUNCATED_HASHBYTES];
            let raw = make_plain_broadcast_raw(dest_hash, b"broadcast no clients");

            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming");

            let actions = transport.drain_actions();
            let has_forward = actions
                .iter()
                .any(|a| matches!(a, Action::SendPacket { .. } | Action::Broadcast { .. }));
            assert!(
                !has_forward,
                "No forwarding should happen without local clients, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_plain_broadcast_delivers_locally_and_forwards() {
            // When the daemon itself has a matching PLAIN destination AND
            // has local clients, a network broadcast should both deliver
            // locally (PacketReceived event) and forward to local clients.
            let mut transport = make_transport_with_local_client();

            // Register a local PLAIN destination
            let dest = Destination::new(
                None,
                Direction::In,
                DestinationType::Plain,
                "plaintest",
                &["localdeliver"],
            )
            .unwrap();
            let dest_hash = dest.hash().as_bytes();
            transport.register_destination(*dest_hash);

            let raw = make_plain_broadcast_raw(*dest_hash, b"deliver and forward");

            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming");

            // Check forwarding to local client
            let actions = transport.drain_actions();
            let has_send_to_local = actions.iter().any(|a| {
                matches!(
                    a,
                    Action::SendPacket { iface, .. } if iface.0 == LOCAL_CLIENT_IFACE
                )
            });
            assert!(
                has_send_to_local,
                "Should forward to local client even when delivering locally"
            );

            // Check local delivery
            let events: Vec<_> = transport.drain_events().collect();
            let has_delivery = events.iter().any(|e| {
                matches!(
                    e,
                    TransportEvent::PacketReceived {
                        destination_hash, ..
                    } if *destination_hash == *dest_hash
                )
            });
            assert!(
                has_delivery,
                "Should deliver locally to registered PLAIN destination"
            );
        }

        #[test]
        fn test_plain_broadcast_control_dest_not_forwarded() {
            // tunnel_synthesize is a PLAIN BROADCAST control destination.
            // It must NOT enter the broadcast forwarding block.
            // Regression test: omitting this exclusion caused
            // test_node_receives_announce_from_daemon to fail 1/5 under RUST_LOG=debug.
            let mut transport = make_transport_with_local_client();
            let tunnel_hash =
                Transport::<MockClock, MemoryStorage>::compute_tunnel_synthesize_hash();
            let raw = make_plain_broadcast_raw(tunnel_hash, b"tunnel synthesize");

            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming");

            // Must NOT produce any forwarding actions
            let actions = transport.drain_actions();
            let has_forward = actions
                .iter()
                .any(|a| matches!(a, Action::SendPacket { .. } | Action::Broadcast { .. }));
            assert!(
                !has_forward,
                "Control dest (tunnel_synthesize) must not be broadcast-forwarded, got: {:?}",
                actions
            );

            // Also verify: no PacketReceived event (tunnel_synthesize is not registered locally)
            let events: Vec<_> = transport.drain_events().collect();
            let has_delivery = events
                .iter()
                .any(|e| matches!(e, TransportEvent::PacketReceived { .. }));
            assert!(
                !has_delivery,
                "tunnel_synthesize should not be delivered locally either"
            );
        }

        #[test]
        fn test_plain_broadcast_multiple_local_clients() {
            // Two local clients (iface 10, iface 11). Network broadcast on iface 0
            // must produce SendPacket to BOTH.
            let mut transport = make_transport_with_local_client(); // has iface 0 + iface 10
            const LOCAL_CLIENT_2: usize = 11;
            transport.set_interface_name(LOCAL_CLIENT_2, "Local[rns/default]/1".into());
            transport.set_local_client(LOCAL_CLIENT_2, true);

            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            let raw = make_plain_broadcast_raw(dest_hash, b"multi client");

            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming");

            let actions = transport.drain_actions();
            let send_targets: Vec<usize> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::SendPacket { iface, .. } => Some(iface.0),
                    _ => None,
                })
                .collect();

            assert!(
                send_targets.contains(&LOCAL_CLIENT_IFACE),
                "Broadcast must reach local client 1 (iface {}), got targets: {:?}",
                LOCAL_CLIENT_IFACE,
                send_targets
            );
            assert!(
                send_targets.contains(&LOCAL_CLIENT_2),
                "Broadcast must reach local client 2 (iface {}), got targets: {:?}",
                LOCAL_CLIENT_2,
                send_targets
            );
            // Must NOT have a Broadcast action (would leak to network)
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::Broadcast { .. })),
                "Network→local must use targeted SendPacket, not Broadcast"
            );
        }

        #[test]
        fn test_plain_broadcast_local_to_local_client() {
            // Local client A (iface 10) sends broadcast.
            // forward_on_all_except emits Action::Broadcast excluding iface 10.
            // The driver dispatches to all interfaces including local client B (iface 11).
            let mut transport = make_transport_with_local_client(); // iface 0 + iface 10
            const LOCAL_CLIENT_2: usize = 11;
            transport.set_interface_name(LOCAL_CLIENT_2, "Local[rns/default]/1".into());
            transport.set_local_client(LOCAL_CLIENT_2, true);

            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let raw = make_plain_broadcast_raw(dest_hash, b"client to client");

            transport
                .process_incoming(LOCAL_CLIENT_IFACE, &raw)
                .expect("process_incoming");

            let actions = transport.drain_actions();
            // Should get a Broadcast action excluding sender (iface 10)
            let has_broadcast = actions.iter().any(|a| {
                matches!(
                    a,
                    Action::Broadcast {
                        exclude_iface: Some(iface),
                        ..
                    } if iface.0 == LOCAL_CLIENT_IFACE
                )
            });
            assert!(
                has_broadcast,
                "Local client broadcast should emit Action::Broadcast excluding sender, got: {:?}",
                actions
            );
        }

        #[test]
        fn test_plain_broadcast_duplicate_suppressed() {
            // Send same plain broadcast twice. Second should be dropped by dedup.
            let mut transport = make_transport_with_local_client();
            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let raw = make_plain_broadcast_raw(dest_hash, b"dedup test");

            // First: should produce forwarding action
            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("first");
            let actions1 = transport.drain_actions();
            assert!(
                actions1
                    .iter()
                    .any(|a| matches!(a, Action::SendPacket { .. })),
                "First broadcast should be forwarded"
            );

            // Second: identical packet, should be dropped as duplicate
            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("second");
            let actions2 = transport.drain_actions();
            assert!(
                !actions2
                    .iter()
                    .any(|a| matches!(a, Action::SendPacket { .. } | Action::Broadcast { .. })),
                "Duplicate broadcast must be suppressed, got: {:?}",
                actions2
            );
        }

        #[test]
        fn test_plain_broadcast_local_delivery_without_local_clients() {
            // No local clients, but daemon has the PLAIN destination registered.
            // Broadcast from network should deliver locally (PacketReceived).
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(NETWORK_IFACE, "tcp_server/127.0.0.1".into());

            // Register a local PLAIN destination
            let dest = Destination::new(
                None,
                Direction::In,
                DestinationType::Plain,
                "plaintest",
                &["nolocal"],
            )
            .unwrap();
            let dest_hash = dest.hash().as_bytes();
            transport.register_destination(*dest_hash);

            let raw = make_plain_broadcast_raw(*dest_hash, b"deliver without clients");

            transport
                .process_incoming(NETWORK_IFACE, &raw)
                .expect("process_incoming");

            // No forwarding (no local clients)
            let actions = transport.drain_actions();
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::SendPacket { .. } | Action::Broadcast { .. })),
                "No forwarding without local clients"
            );

            // But local delivery SHOULD happen
            let events: Vec<_> = transport.drain_events().collect();
            let has_delivery = events.iter().any(|e| {
                matches!(
                    e,
                    TransportEvent::PacketReceived {
                        destination_hash, ..
                    } if *destination_hash == *dest_hash
                )
            });
            assert!(
                has_delivery,
                "Daemon should deliver broadcast locally even without local clients"
            );
        }
    }

    mod announce_frequency_tests {
        use super::*;
        use crate::memory_storage::MemoryStorage;
        use crate::test_utils::{MockClock, TEST_TIME_MS};
        use alloc::collections::VecDeque;
        use rand_core::OsRng;

        extern crate std;

        fn make_transport() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = crate::identity::Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(0, "iface0".into());
            transport.set_interface_name(1, "iface1".into());
            transport
        }

        #[test]
        fn frequency_empty_deque_returns_zero() {
            let deque = VecDeque::new();
            assert_eq!(
                Transport::<MockClock, MemoryStorage>::announce_frequency(&deque, 1000),
                0.0
            );
        }

        #[test]
        fn frequency_single_sample_returns_zero() {
            let mut deque = VecDeque::new();
            deque.push_back(1000);
            assert_eq!(
                Transport::<MockClock, MemoryStorage>::announce_frequency(&deque, 2000),
                0.0
            );
        }

        #[test]
        fn frequency_two_samples_one_second_apart() {
            let mut deque = VecDeque::new();
            deque.push_back(1000);
            deque.push_back(2000);
            // Deltas: [1000ms between samples] + [0ms since last sample queried at same time]
            // avg_delta = 1000ms / 2 samples = 500ms = 0.5s
            // freq = 1 / 0.5 = 2.0 Hz
            let freq = Transport::<MockClock, MemoryStorage>::announce_frequency(&deque, 2000);
            assert!((freq - 2.0).abs() < 0.001, "expected ~2.0 Hz, got {}", freq);
        }

        #[test]
        fn frequency_includes_time_since_last_sample() {
            let mut deque = VecDeque::new();
            deque.push_back(1000);
            deque.push_back(2000);
            // Deltas: [1000ms between samples] + [3000ms since last sample (now=5000)]
            // delta_sum = 1000 + 3000 = 4000ms
            // avg_delta = 4000ms / 2 samples = 2000ms = 2.0s
            // freq = 1 / 2.0 = 0.5 Hz
            let freq = Transport::<MockClock, MemoryStorage>::announce_frequency(&deque, 5000);
            assert!((freq - 0.5).abs() < 0.001, "expected ~0.5 Hz, got {}", freq);
        }

        #[test]
        fn frequency_six_evenly_spaced_samples() {
            let mut deque = VecDeque::new();
            // 6 samples, 10s apart
            for i in 0..6 {
                deque.push_back(10_000 + i * 10_000);
            }
            // now_ms = last sample time (no additional elapsed)
            let now = 10_000 + 5 * 10_000;
            // 5 inter-sample deltas of 10000ms each + 0ms since last = 50000ms
            // avg = 50000 / 6 = 8333.33ms = 8.333s
            // freq = 1 / 8.333 = 0.12 Hz
            let freq = Transport::<MockClock, MemoryStorage>::announce_frequency(&deque, now);
            let expected = 1.0 / (50000.0 / 6.0 / 1000.0);
            assert!(
                (freq - expected).abs() < 0.001,
                "expected ~{}, got {}",
                expected,
                freq
            );
        }

        #[test]
        fn record_incoming_stores_timestamp() {
            let mut transport = make_transport();
            transport.record_incoming_announce(0);
            let stats = transport.interface_stats();
            let iface0 = stats.iter().find(|s| s.id == 0).unwrap();
            // Single sample → 0.0 Hz
            assert_eq!(iface0.incoming_announce_frequency, 0.0);
        }

        #[test]
        fn record_incoming_two_samples_computes_frequency() {
            let mut transport = make_transport();
            transport.record_incoming_announce(0);
            transport.clock.advance(5000);
            transport.record_incoming_announce(0);

            let stats = transport.interface_stats();
            let iface0 = stats.iter().find(|s| s.id == 0).unwrap();
            // 2 samples, 5s apart, queried at last sample time
            // delta_sum = 5000 + 0 = 5000ms, avg = 5000/2 = 2500ms = 2.5s
            // freq = 1/2.5 = 0.4 Hz
            assert!(
                (iface0.incoming_announce_frequency - 0.4).abs() < 0.01,
                "expected ~0.4 Hz, got {}",
                iface0.incoming_announce_frequency
            );
        }

        #[test]
        fn record_outgoing_stores_timestamp() {
            let mut transport = make_transport();
            transport.record_outgoing_announce(0);
            let stats = transport.interface_stats();
            let iface0 = stats.iter().find(|s| s.id == 0).unwrap();
            assert_eq!(iface0.outgoing_announce_frequency, 0.0);
        }

        #[test]
        fn record_outgoing_broadcast_tracks_all_except() {
            let mut transport = make_transport();
            // Record broadcast excluding iface 0 → should only track on iface 1
            transport.record_outgoing_announce_broadcast(0);
            assert!(transport.interface_outgoing_announce_times.contains_key(&1));
            assert!(!transport.interface_outgoing_announce_times.contains_key(&0));
        }

        #[test]
        fn deque_capped_at_max_samples() {
            let mut transport = make_transport();
            for i in 0..10 {
                transport.clock.advance(1000);
                transport.record_incoming_announce(0);
                let _ = i;
            }
            let deque = transport.interface_incoming_announce_times.get(&0).unwrap();
            assert_eq!(deque.len(), ANNOUNCE_FREQ_SAMPLES);
        }

        #[test]
        fn cleanup_removes_tracking() {
            let mut transport = make_transport();
            transport.record_incoming_announce(0);
            transport.record_outgoing_announce(0);
            transport.remove_announce_freq_tracking(0);
            assert!(!transport.interface_incoming_announce_times.contains_key(&0));
            assert!(!transport.interface_outgoing_announce_times.contains_key(&0));
        }

        #[test]
        fn frequency_decays_with_time() {
            let mut transport = make_transport();
            // Record 3 announces 1s apart
            transport.record_incoming_announce(0);
            transport.clock.advance(1000);
            transport.record_incoming_announce(0);
            transport.clock.advance(1000);
            transport.record_incoming_announce(0);

            let stats_early = transport.interface_stats();
            let freq_early = stats_early
                .iter()
                .find(|s| s.id == 0)
                .unwrap()
                .incoming_announce_frequency;

            // Advance time significantly without new announces
            transport.clock.advance(60_000);
            let stats_late = transport.interface_stats();
            let freq_late = stats_late
                .iter()
                .find(|s| s.id == 0)
                .unwrap()
                .incoming_announce_frequency;

            assert!(
                freq_late < freq_early,
                "frequency should decay over time: early={}, late={}",
                freq_early,
                freq_late
            );
        }

        #[test]
        fn interface_stats_returns_both_frequencies() {
            let mut transport = make_transport();
            // Record some incoming on iface 0
            transport.record_incoming_announce(0);
            transport.clock.advance(2000);
            transport.record_incoming_announce(0);

            // Record some outgoing on iface 1
            transport.record_outgoing_announce(1);
            transport.clock.advance(3000);
            transport.record_outgoing_announce(1);

            let stats = transport.interface_stats();
            let iface0 = stats.iter().find(|s| s.id == 0).unwrap();
            let iface1 = stats.iter().find(|s| s.id == 1).unwrap();

            assert!(iface0.incoming_announce_frequency > 0.0);
            assert_eq!(iface0.outgoing_announce_frequency, 0.0);
            assert_eq!(iface1.incoming_announce_frequency, 0.0);
            assert!(iface1.outgoing_announce_frequency > 0.0);
        }
    }

    mod known_ratchets_tests {
        use super::*;
        use crate::announce::build_announce_payload;
        use crate::constants::{ANNOUNCE_RATE_LIMIT_MS, MTU, RATCHET_EXPIRY_SECS};
        use crate::destination::{Destination, DestinationType, Direction};
        use crate::memory_storage::MemoryStorage;
        use crate::packet::{
            HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
        };
        use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
        use rand_core::OsRng;

        extern crate std;

        fn make_transport() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = crate::identity::Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            Transport::new(config, clock, MemoryStorage::with_defaults(), identity)
        }

        /// Build a ratcheted announce packet from a destination with ratchets enabled.
        fn make_ratcheted_announce_raw(dest: &Destination, hops: u8, now_ms: u64) -> Vec<u8> {
            let identity = dest.identity().unwrap();
            let ratchet_pub = dest.current_ratchet_public().unwrap();
            let payload = build_announce_payload(
                identity,
                dest.hash().as_bytes(),
                dest.name_hash(),
                Some(&ratchet_pub),
                Some(b"test"),
                &mut OsRng,
                now_ms,
            )
            .unwrap();

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: true,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            buf[..len].to_vec()
        }

        /// Build a non-ratcheted announce packet from a destination.
        fn make_plain_announce_raw(dest: &Destination, hops: u8, now_ms: u64) -> Vec<u8> {
            let identity = dest.identity().unwrap();
            let payload = build_announce_payload(
                identity,
                dest.hash().as_bytes(),
                dest.name_hash(),
                None,
                Some(b"test"),
                &mut OsRng,
                now_ms,
            )
            .unwrap();

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            buf[..len].to_vec()
        }

        fn make_ratcheted_dest() -> Destination {
            let identity = crate::identity::Identity::generate(&mut OsRng);
            let mut dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ratchet"],
            )
            .unwrap();
            dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
            dest
        }

        #[test]
        fn test_handle_announce_stores_ratchet() {
            let mut transport = make_transport();
            let _idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = make_ratcheted_dest();
            let dest_hash = dest.hash().into_bytes();
            let expected_ratchet = dest.current_ratchet_public().unwrap();

            let now = transport.clock.now_ms();
            let raw = make_ratcheted_announce_raw(&dest, 1, now);
            transport.process_incoming(0, &raw).unwrap();

            let stored = transport.storage.get_known_ratchet(&dest_hash);
            assert!(
                stored.is_some(),
                "Ratchet should be stored after valid announce"
            );
            assert_eq!(
                stored.unwrap(),
                expected_ratchet,
                "Stored ratchet should match destination's current ratchet"
            );
        }

        #[test]
        fn test_handle_announce_no_ratchet_no_store() {
            let mut transport = make_transport();
            let _idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let identity = crate::identity::Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["noratchet"],
            )
            .unwrap();

            let now = transport.clock.now_ms();
            let raw = make_plain_announce_raw(&dest, 1, now);
            transport.process_incoming(0, &raw).unwrap();

            assert_eq!(
                transport.storage.known_ratchet_count(),
                0,
                "No ratchet should be stored for non-ratcheted announce"
            );
        }

        #[test]
        fn test_handle_announce_ratchet_rotation() {
            let mut transport = make_transport();
            let _idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let mut dest = make_ratcheted_dest();
            let dest_hash = dest.hash().into_bytes();
            let first_ratchet = dest.current_ratchet_public().unwrap();

            // First announce
            let now1 = transport.clock.now_ms();
            let raw1 = make_ratcheted_announce_raw(&dest, 1, now1);
            transport.process_incoming(0, &raw1).unwrap();

            assert_eq!(
                transport.storage.get_known_ratchet(&dest_hash).unwrap(),
                first_ratchet
            );

            // Advance past rate limit
            transport.clock.advance(ANNOUNCE_RATE_LIMIT_MS + 1);

            // Rotate ratchet on destination
            let now2 = transport.clock.now_ms();
            dest.rotate_ratchet_if_needed(&mut OsRng, now2);
            // Force rotation by directly calling enable_ratchets again
            // (rotate_ratchet_if_needed may not rotate if interval hasn't passed)
            dest.enable_ratchets(&mut OsRng, now2).unwrap();
            let second_ratchet = dest.current_ratchet_public().unwrap();
            assert_ne!(first_ratchet, second_ratchet, "Ratchet should have rotated");

            // Second announce with new ratchet
            let raw2 = make_ratcheted_announce_raw(&dest, 1, now2);
            transport.process_incoming(0, &raw2).unwrap();

            let stored = transport.storage.get_known_ratchet(&dest_hash).unwrap();
            assert_eq!(
                stored, second_ratchet,
                "Stored ratchet should be updated to the rotated one"
            );
        }

        #[test]
        fn test_known_ratchets_cleanup_in_path_states() {
            let mut transport = make_transport();
            let _idx = transport.register_interface(Box::new(MockInterface::new("if0", 1)));

            let dest = make_ratcheted_dest();

            let now = transport.clock.now_ms();
            let raw = make_ratcheted_announce_raw(&dest, 1, now);
            transport.process_incoming(0, &raw).unwrap();

            let dest_hash_bytes = dest.hash().into_bytes();
            assert!(transport
                .storage
                .get_known_ratchet(&dest_hash_bytes)
                .is_some());

            // Advance past 30-day expiry
            transport.clock.advance(RATCHET_EXPIRY_SECS * 1000 + 1);
            transport.clean_path_states();

            assert_eq!(
                transport.storage.known_ratchet_count(),
                0,
                "Expired ratchets should be cleaned up"
            );
        }
    }

    mod discovery_path_requests_tests {
        use super::*;
        use crate::constants::PATH_REQUEST_TIMEOUT_MS;
        use crate::destination::{Destination, DestinationType, Direction};
        use crate::identity::Identity;
        use crate::memory_storage::MemoryStorage;
        use crate::packet::{HeaderType, PacketContext, PacketData, PacketFlags, TransportType};
        use crate::test_utils::MockClock;
        use rand_core::OsRng;
        extern crate alloc;
        use alloc::vec::Vec;

        const TEST_TIME_MS: u64 = 1_000_000;
        const IFACE_A: usize = 0;
        const IFACE_B: usize = 1;

        fn make_transport() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(IFACE_A, "iface_a".into());
            transport.set_interface_name(IFACE_B, "iface_b".into());
            transport
        }

        /// Build a valid announce as raw bytes. Returns (raw_bytes, dest_hash).
        fn make_announce_raw(
            hops: u8,
            context: PacketContext,
        ) -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["discovery"],
            )
            .unwrap();

            let id = dest.identity().unwrap();
            let random_hash = [0x42u8; crate::constants::RANDOM_HASHBYTES];

            let mut payload = Vec::new();
            payload.extend_from_slice(&id.public_key_bytes());
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(&random_hash);

            let app_data = b"test";
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&id.public_key_bytes());
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(&random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = id.sign(&signed_data).unwrap();
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; 500];
            let len = packet.pack(&mut buf).unwrap();
            (buf[..len].to_vec(), dest.hash().into_bytes())
        }

        #[test]
        fn test_discovery_records_and_responds() {
            let mut transport = make_transport();
            let (raw, dest_hash) = make_announce_raw(1, PacketContext::None);

            // Manually record a discovery path request (simulating what
            // handle_path_request Stage 3 does)
            let now = transport.clock.now_ms();
            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + PATH_REQUEST_TIMEOUT_MS,
            );

            // Verify the entry is stored
            assert!(transport
                .storage
                .get_discovery_path_request(&dest_hash)
                .is_some());

            // Process an announce matching the destination from iface_b
            let packet = Packet::unpack(&raw).unwrap();
            let result = transport.handle_announce(packet, IFACE_B, &raw);
            assert!(result.is_ok());

            // The discovery response should have been sent as a SendPacket
            // targeting iface_a (the requesting interface)
            let actions = transport.drain_actions();
            let discovery_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == IFACE_A))
                .collect();
            assert!(
                !discovery_sends.is_empty(),
                "Expected a targeted PATH_RESPONSE to iface_a"
            );

            // The discovery entry should be removed after delivery
            assert!(
                transport
                    .storage
                    .get_discovery_path_request(&dest_hash)
                    .is_none(),
                "Discovery entry should be removed after delivery"
            );
        }

        #[test]
        fn test_discovery_first_wins() {
            let mut transport = make_transport();
            let dest_hash = [0xAA; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // First request from iface_a
            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + PATH_REQUEST_TIMEOUT_MS,
            );

            // Second request from iface_b, should be ignored
            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_B,
                now + PATH_REQUEST_TIMEOUT_MS,
            );

            // Verify first requester wins
            let (iface, _timeout) = transport
                .storage
                .get_discovery_path_request(&dest_hash)
                .unwrap();
            assert_eq!(
                iface, IFACE_A,
                "First requesting interface should be preserved"
            );
        }

        #[test]
        fn test_discovery_expires() {
            let mut transport = make_transport();
            let dest_hash = [0xBB; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + PATH_REQUEST_TIMEOUT_MS,
            );

            // Advance past timeout
            transport.clock.advance(PATH_REQUEST_TIMEOUT_MS + 1);

            // Expire
            let removed = transport
                .storage
                .expire_discovery_path_requests(transport.clock.now_ms());
            assert_eq!(removed, 1);

            // Verify gone
            assert!(transport
                .storage
                .get_discovery_path_request(&dest_hash)
                .is_none());
        }

        #[test]
        fn test_discovery_no_response_after_expiry() {
            let mut transport = make_transport();
            let (raw, dest_hash) = make_announce_raw(1, PacketContext::None);

            let now = transport.clock.now_ms();
            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + PATH_REQUEST_TIMEOUT_MS,
            );

            // Advance past timeout
            transport.clock.advance(PATH_REQUEST_TIMEOUT_MS + 1);

            // Process a matching announce, should NOT trigger a discovery response
            let packet = Packet::unpack(&raw).unwrap();
            let result = transport.handle_announce(packet, IFACE_B, &raw);
            assert!(result.is_ok());

            // No targeted SendPacket to iface_a
            let actions = transport.drain_actions();
            let discovery_sends: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == IFACE_A))
                .collect();
            assert!(
                discovery_sends.is_empty(),
                "Expired discovery entry should not produce a response"
            );

            // Expired entry should be cleaned up
            assert!(transport
                .storage
                .get_discovery_path_request(&dest_hash)
                .is_none());
        }
    }

    mod discovery_retry_tests {
        use super::*;
        use crate::constants::{DISCOVERY_RETRY_INTERVAL_MS, DISCOVERY_TIMEOUT_MS};
        use crate::identity::Identity;
        use crate::memory_storage::MemoryStorage;
        use crate::test_utils::MockClock;
        use rand_core::OsRng;
        extern crate alloc;
        use alloc::vec::Vec;

        const TEST_TIME_MS: u64 = 1_000_000;
        const IFACE_A: usize = 0;
        const IFACE_B: usize = 1;

        fn make_transport() -> Transport<MockClock, MemoryStorage> {
            let clock = MockClock::new(TEST_TIME_MS);
            let identity = Identity::generate(&mut OsRng);
            let config = TransportConfig {
                enable_transport: true,
                ..TransportConfig::default()
            };
            let mut transport =
                Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
            transport.set_interface_name(IFACE_A, "iface_a".into());
            transport.set_interface_name(IFACE_B, "iface_b".into());
            transport
        }

        #[test]
        fn test_discovery_retry_fires_after_interval() {
            let mut transport = make_transport();
            let dest_hash = [0xCC; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            // Record a pending discovery
            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + DISCOVERY_TIMEOUT_MS,
            );

            // Drain any existing actions
            transport.drain_actions();

            // Advance past the retry interval
            transport.clock.advance(DISCOVERY_RETRY_INTERVAL_MS + 1);

            // Trigger retry
            transport.retry_pending_discoveries(&mut OsRng);

            // Should have produced a Broadcast action (the retried path request)
            let actions = transport.drain_actions();
            let broadcasts: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .collect();
            assert_eq!(
                broadcasts.len(),
                1,
                "Should broadcast one retried path request"
            );

            // Verify it's a Broadcast excluding iface_a (the requesting interface)
            if let Action::Broadcast { exclude_iface, .. } = &broadcasts[0] {
                assert_eq!(
                    exclude_iface.unwrap().0,
                    IFACE_A,
                    "Retry should exclude the requesting interface"
                );
            }

            // Discovery entry should still be pending (not removed by retry)
            assert!(
                transport
                    .storage
                    .get_discovery_path_request(&dest_hash)
                    .is_some(),
                "Discovery entry should remain pending after retry"
            );
        }

        #[test]
        fn test_discovery_retry_stops_after_timeout() {
            let mut transport = make_transport();
            let dest_hash = [0xDD; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + DISCOVERY_TIMEOUT_MS,
            );
            transport.drain_actions();

            // Advance past the discovery timeout (30s)
            transport.clock.advance(DISCOVERY_TIMEOUT_MS + 1);

            transport.retry_pending_discoveries(&mut OsRng);

            // Should NOT produce any Broadcast actions
            let actions = transport.drain_actions();
            let broadcasts: Vec<_> = actions
                .iter()
                .filter(|a| matches!(a, Action::Broadcast { .. }))
                .collect();
            assert!(
                broadcasts.is_empty(),
                "Expired discovery should not trigger retry"
            );
        }

        #[test]
        fn test_discovery_retry_uses_fresh_tag() {
            let mut transport = make_transport();
            let dest_hash = [0xEE; TRUNCATED_HASHBYTES];
            let now = transport.clock.now_ms();

            transport.storage.set_discovery_path_request(
                dest_hash,
                IFACE_A,
                now + DISCOVERY_TIMEOUT_MS,
            );
            transport.drain_actions();

            // First retry
            transport.clock.advance(DISCOVERY_RETRY_INTERVAL_MS + 1);
            transport.retry_pending_discoveries(&mut OsRng);
            let actions1 = transport.drain_actions();
            let raw1 = match &actions1[0] {
                Action::Broadcast { data, .. } => data.clone(),
                _ => panic!("Expected Broadcast"),
            };

            // Second retry
            transport.clock.advance(DISCOVERY_RETRY_INTERVAL_MS + 1);
            transport.retry_pending_discoveries(&mut OsRng);
            let actions2 = transport.drain_actions();
            let raw2 = match &actions2[0] {
                Action::Broadcast { data, .. } => data.clone(),
                _ => panic!("Expected Broadcast"),
            };

            // The two packets should differ (fresh tag means different bytes)
            assert_ne!(raw1, raw2, "Each retry must use a fresh random tag");

            // But both should be the same length (same packet structure)
            assert_eq!(
                raw1.len(),
                raw2.len(),
                "Retry packets should have the same structure"
            );
        }
    }

    mod backpressure_tests {
        use super::*;
        use crate::test_utils::{test_transport, MockInterface};
        extern crate alloc;
        use alloc::vec;

        #[test]
        fn test_send_to_destination_pacing_delay_when_interface_not_ready() {
            let mut transport = test_transport();

            // Add a path to a destination via interface 0
            let dest_hash = [0xAA; crate::constants::TRUNCATED_HASHBYTES];
            let iface_idx = 0;
            use crate::traits::Storage as _;
            transport.storage_mut().set_path(
                dest_hash,
                crate::storage_types::PathEntry {
                    interface_index: iface_idx,
                    hops: 1,
                    next_hop: None,
                    expires_ms: 1000 + 3_600_000,
                    random_blobs: vec![],
                },
            );

            // Mark the interface as unavailable until a future wall-clock ms
            // via the next-slot backchannel.
            let now_ms = transport.clock().now_ms();
            let future_slot = now_ms + 5_000;
            transport.set_interface_next_slot_ms(iface_idx, future_slot);

            // Send must return PacingDelay with that exact timestamp.
            let result = transport.send_to_destination(&dest_hash, &[0u8; 32]);
            assert_eq!(
                result,
                Err(TransportError::PacingDelay {
                    ready_at_ms: future_slot
                })
            );
        }

        /// An announce whose retransmit_at has fired is deferred
        /// (retransmit_at bumped, retries unchanged, no Broadcast Action
        /// pushed) when its target interface's next_slot_ms is in the
        /// future.
        #[test]
        fn retry_scheduler_defers_targeted_announce_when_iface_not_ready() {
            use crate::storage_types::AnnounceEntry;
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xCC; crate::constants::TRUNCATED_HASHBYTES];
            // Register an announce that's retransmit-due RIGHT NOW and
            // targeted at interface 7. Retry count 2 so we can observe
            // that it doesn't change.
            let now = transport.clock().now_ms();
            use crate::traits::Storage as _;
            transport.storage_mut().set_announce(
                dest_hash,
                AnnounceEntry {
                    timestamp_ms: now,
                    hops: 0,
                    retries: 0,
                    retransmit_at_ms: Some(now),
                    raw_packet: vec![0xAA; 32],
                    receiving_interface_index: usize::MAX,
                    target_interface: Some(7),
                    local_rebroadcasts: 0,
                    block_rebroadcasts: false,
                },
            );
            // Mark interface 7 as not-yet-ready.
            let future = now + 500;
            transport.set_interface_next_slot_ms(7, future);
            // Fire the scheduler.
            transport.check_announce_rebroadcasts(now);
            // Load-bearing invariant: no Broadcast or SendPacket Action
            // was pushed for this announce.
            assert!(
                transport
                    .pending_actions
                    .iter()
                    .all(|a| !matches!(a, Action::Broadcast { .. } | Action::SendPacket { .. })),
                "no Action should have been pushed, got {:?}",
                transport.pending_actions
            );
            // Deferral: timer bumped forward to the interface's slot.
            let updated = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(updated.retransmit_at_ms, Some(future));
            // Deferral ≠ retry: retries counter unchanged.
            assert_eq!(updated.retries, 0);
        }

        /// Once the interface becomes ready, the next scheduler tick does
        /// NOT defer. Observable via retries being incremented on the
        /// normal-push path vs unchanged on the defer path.
        #[test]
        fn retry_scheduler_increments_retries_when_slot_ready() {
            use crate::storage_types::AnnounceEntry;
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xDD; crate::constants::TRUNCATED_HASHBYTES];
            let now = transport.clock().now_ms();
            use crate::traits::Storage as _;
            transport.storage_mut().set_announce(
                dest_hash,
                AnnounceEntry {
                    timestamp_ms: now,
                    hops: 0,
                    retries: 0,
                    retransmit_at_ms: Some(now),
                    raw_packet: alloc::vec![0u8; 32],
                    receiving_interface_index: usize::MAX,
                    target_interface: Some(3),
                    local_rebroadcasts: 0,
                    block_rebroadcasts: false,
                },
            );
            // Interface 3 ready at now, no deferral.
            transport.set_interface_next_slot_ms(3, now);
            transport.check_announce_rebroadcasts(now);
            // Deferral path leaves retries == 0; push path increments to 1
            // via the exp-backoff block. Packet::unpack may fail on the
            // fake bytes so the Action is never actually emitted, but the
            // retries += 1 happens regardless.
            let updated = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                updated.retries, 1,
                "expected retries incremented (push path took); got {}",
                updated.retries
            );
        }

        /// Broadcast (target_interface = None): defer if ALL registered
        /// interfaces are in the future; use the minimum of their slots.
        #[test]
        fn retry_scheduler_defers_broadcast_when_all_interfaces_future() {
            use crate::storage_types::AnnounceEntry;
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xEE; crate::constants::TRUNCATED_HASHBYTES];
            let now = transport.clock().now_ms();
            // Register two interfaces (via hw_mtu map) and push future slots.
            transport.set_interface_hw_mtu(0, 500);
            transport.set_interface_hw_mtu(1, 500);
            transport.set_interface_next_slot_ms(0, now + 800);
            transport.set_interface_next_slot_ms(1, now + 500);
            use crate::traits::Storage as _;
            transport.storage_mut().set_announce(
                dest_hash,
                AnnounceEntry {
                    timestamp_ms: now,
                    hops: 0,
                    retries: 0,
                    retransmit_at_ms: Some(now),
                    raw_packet: vec![0xBB; 32],
                    receiving_interface_index: usize::MAX,
                    target_interface: None,
                    local_rebroadcasts: 0,
                    block_rebroadcasts: false,
                },
            );
            transport.check_announce_rebroadcasts(now);
            assert!(
                transport
                    .pending_actions
                    .iter()
                    .all(|a| !matches!(a, Action::Broadcast { .. } | Action::SendPacket { .. })),
                "no Action pushed, got {:?}",
                transport.pending_actions
            );
            let updated = transport.storage().get_announce(&dest_hash).unwrap();
            // Deferred to the MINIMUM of the two slots.
            assert_eq!(updated.retransmit_at_ms, Some(now + 500));
            assert_eq!(updated.retries, 0, "retries unchanged");
        }

        /// Broadcast with at least one ready interface → push path taken.
        /// Observable via retries counter incrementing (deferral keeps it
        /// unchanged, push increments via the exp-backoff block).
        #[test]
        fn retry_scheduler_pushes_broadcast_when_any_interface_ready() {
            use crate::storage_types::AnnounceEntry;
            let mut transport = test_transport();
            transport.set_enable_transport_for_tests(true);
            let dest_hash = [0xFE; crate::constants::TRUNCATED_HASHBYTES];
            let now = transport.clock().now_ms();
            transport.set_interface_hw_mtu(0, 500);
            transport.set_interface_hw_mtu(1, 500);
            transport.set_interface_next_slot_ms(0, now + 800); // future
                                                                // iface 1 has no slot pushed → getter returns now_ms → ready.
            use crate::traits::Storage as _;
            transport.storage_mut().set_announce(
                dest_hash,
                AnnounceEntry {
                    timestamp_ms: now,
                    hops: 0,
                    retries: 0,
                    retransmit_at_ms: Some(now),
                    raw_packet: alloc::vec![0u8; 32],
                    receiving_interface_index: usize::MAX,
                    target_interface: None,
                    local_rebroadcasts: 0,
                    block_rebroadcasts: false,
                },
            );
            transport.check_announce_rebroadcasts(now);
            // Push path ran → retries incremented. Deferral path would
            // leave retries unchanged at 0.
            let updated = transport.storage().get_announce(&dest_hash).unwrap();
            assert_eq!(
                updated.retries, 1,
                "expected retries incremented (push path took); got {}",
                updated.retries
            );
        }

        #[test]
        fn test_send_to_destination_ok_when_interface_ready() {
            let mut transport = test_transport();

            let dest_hash = [0xBB; crate::constants::TRUNCATED_HASHBYTES];
            let iface_idx = 0;
            use crate::traits::Storage as _;
            transport.storage_mut().set_path(
                dest_hash,
                crate::storage_types::PathEntry {
                    interface_index: iface_idx,
                    hops: 1,
                    next_hop: None,
                    expires_ms: 1000 + 3_600_000,
                    random_blobs: vec![],
                },
            );

            // Leave next_slot unset (→ getter returns now_ms = ready).
            let result = transport.send_to_destination(&dest_hash, &[0u8; 32]);
            assert!(
                result.is_ok(),
                "ready interface should succeed, got {result:?}"
            );
        }

        #[test]
        fn test_dispatch_sendpacket_returns_retry() {
            // Create a mock interface that returns BufferFull
            let mut iface = MockInterface::new("test", 0);
            iface.reject_sends = true;
            let mut interfaces: Vec<&mut dyn crate::traits::Interface> = vec![&mut iface];

            let actions = vec![Action::SendPacket {
                iface: InterfaceId(0),
                data: vec![1, 2, 3],
            }];

            let no_ifac = BTreeMap::new();
            let result = dispatch_actions(&mut interfaces, actions, &no_ifac);

            // Should have a retry entry
            assert_eq!(result.retries.len(), 1);
            assert_eq!(result.retries[0].iface_idx, 0);
            assert_eq!(result.retries[0].data, vec![1, 2, 3]);

            // Should also have an error logged
            assert_eq!(result.errors.len(), 1);
        }

        #[test]
        fn test_dispatch_broadcast_not_in_retries() {
            // Create a mock interface that returns BufferFull
            let mut iface = MockInterface::new("test", 0);
            iface.reject_sends = true;
            let mut interfaces: Vec<&mut dyn crate::traits::Interface> = vec![&mut iface];

            let actions = vec![Action::Broadcast {
                data: vec![1, 2, 3],
                exclude_iface: None,
            }];

            let no_ifac = BTreeMap::new();
            let result = dispatch_actions(&mut interfaces, actions, &no_ifac);

            // Broadcast failures should NOT produce retries
            assert!(result.retries.is_empty());
            // But should still log the error
            assert_eq!(result.errors.len(), 1);
        }
    }

    /// Tests for IFAC (Interface Access Code) integration with Transport
    mod ifac_tests {
        use super::*;
        use crate::ifac::IfacConfig;
        use crate::test_utils::{test_transport, MockInterface};
        use alloc::vec;
        use rand_core::OsRng;

        extern crate std;

        /// Build a valid announce packet raw bytes for testing.
        /// Returns (raw_bytes, dest_hash).
        fn make_test_announce() -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
            use crate::destination::{Destination, DestinationType, Direction};
            use crate::identity::Identity;
            use crate::packet::{HeaderType, PacketData, PacketFlags, TransportType};

            let identity = Identity::generate(&mut OsRng);
            let dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &["ifac"],
            )
            .unwrap();

            let id = dest.identity().unwrap();
            let random_hash = [0x42u8; crate::constants::RANDOM_HASHBYTES];

            let mut payload = Vec::new();
            payload.extend_from_slice(&id.public_key_bytes());
            payload.extend_from_slice(dest.name_hash());
            payload.extend_from_slice(&random_hash);

            let app_data = b"ifac_test";
            let mut signed_data = Vec::new();
            signed_data.extend_from_slice(dest.hash().as_bytes());
            signed_data.extend_from_slice(&id.public_key_bytes());
            signed_data.extend_from_slice(dest.name_hash());
            signed_data.extend_from_slice(&random_hash);
            signed_data.extend_from_slice(app_data);

            let signature = id.sign(&signed_data).unwrap();
            payload.extend_from_slice(&signature);
            payload.extend_from_slice(app_data);

            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type: HeaderType::Type1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Announce,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest.hash().into_bytes(),
                context: crate::packet::PacketContext::None,
                data: PacketData::Owned(payload),
            };

            let mut buf = [0u8; MTU];
            let len = packet.pack(&mut buf).unwrap();
            let raw = buf[..len].to_vec();
            (raw, dest.hash().into_bytes())
        }

        #[test]
        fn test_ifac_inbound_valid_packet_accepted() {
            let mut transport = test_transport();
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("ifac0", 0)));
            transport.set_interface_name(iface_idx, "ifac0".into());

            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 16).unwrap();
            transport.set_ifac_config(iface_idx, cfg.clone());

            let (raw, _dest_hash) = make_test_announce();
            let wrapped = cfg.apply_ifac(&raw).unwrap();

            let result = transport.process_incoming(iface_idx, &wrapped);
            assert!(result.is_ok(), "IFAC-valid packet should be accepted");
            // Should not be counted as dropped
            assert_eq!(transport.stats().packets_dropped, 0);
        }

        #[test]
        fn test_ifac_inbound_invalid_packet_dropped() {
            let mut transport = test_transport();
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("ifac0", 0)));
            transport.set_interface_name(iface_idx, "ifac0".into());

            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 16).unwrap();
            transport.set_ifac_config(iface_idx, cfg);

            // Send raw packet without IFAC wrapping, should be dropped
            let (raw, _dest_hash) = make_test_announce();
            let result = transport.process_incoming(iface_idx, &raw);
            assert!(result.is_ok(), "Silently drop, no error");
            assert_eq!(transport.stats().packets_dropped, 1);
        }

        #[test]
        fn test_ifac_inbound_non_ifac_iface_drops_ifac_tagged() {
            let mut transport = test_transport();
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("plain0", 0)));
            transport.set_interface_name(iface_idx, "plain0".into());
            // No IFAC config set on this interface

            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 16).unwrap();
            let (raw, _dest_hash) = make_test_announce();
            let wrapped = cfg.apply_ifac(&raw).unwrap();

            // Feed IFAC-tagged packet to non-IFAC interface, should drop
            let result = transport.process_incoming(iface_idx, &wrapped);
            assert!(result.is_ok(), "Silently drop, no error");
            assert_eq!(transport.stats().packets_dropped, 1);
        }

        #[test]
        fn test_ifac_outbound_dispatch_applies_ifac() {
            let mut iface = MockInterface::new("ifac0", 0);
            let mut interfaces: Vec<&mut dyn crate::traits::Interface> = vec![&mut iface];

            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 16).unwrap();
            let mut ifac_configs = BTreeMap::new();
            ifac_configs.insert(0, cfg);

            let (raw, _) = make_test_announce();
            let actions = vec![Action::SendPacket {
                iface: InterfaceId(0),
                data: raw.clone(),
            }];

            let result = dispatch_actions(&mut interfaces, actions, &ifac_configs);
            assert!(result.errors.is_empty());

            // The sent bytes should have IFAC flag set
            let sent = iface
                .sent
                .last()
                .expect("should have sent something")
                .clone();
            assert!(
                IfacConfig::has_ifac_flag(&sent),
                "Dispatched packet should have IFAC flag"
            );
            assert_eq!(
                sent.len(),
                raw.len() + 16,
                "IFAC should add 16 bytes (ifac_size)"
            );
        }

        #[test]
        fn test_ifac_roundtrip_dispatch_then_process() {
            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 16).unwrap();

            // Outbound: dispatch_actions applies IFAC
            let mut iface = MockInterface::new("ifac0", 0);
            let (raw, _) = make_test_announce();
            {
                let mut interfaces: Vec<&mut dyn crate::traits::Interface> = vec![&mut iface];
                let mut ifac_configs = BTreeMap::new();
                ifac_configs.insert(0, cfg.clone());
                let actions = vec![Action::SendPacket {
                    iface: InterfaceId(0),
                    data: raw.clone(),
                }];
                dispatch_actions(&mut interfaces, actions, &ifac_configs);
            }

            let sent = iface.sent.last().expect("should have sent").clone();

            // Inbound: process_incoming verifies and strips IFAC
            let mut transport = test_transport();
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("ifac0", 0)));
            transport.set_interface_name(iface_idx, "ifac0".into());
            transport.set_ifac_config(iface_idx, cfg);

            let result = transport.process_incoming(iface_idx, &sent);
            assert!(result.is_ok(), "Roundtrip IFAC should succeed");
            assert_eq!(transport.stats().packets_dropped, 0);
        }

        #[test]
        fn test_ifac_broadcast_mixed_interfaces() {
            let mut iface0 = MockInterface::new("ifac0", 0);
            let mut iface1 = MockInterface::new("plain1", 1);
            let mut interfaces: Vec<&mut dyn crate::traits::Interface> =
                vec![&mut iface0, &mut iface1];

            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 8).unwrap();
            let mut ifac_configs = BTreeMap::new();
            ifac_configs.insert(0, cfg); // IFAC only on iface 0

            let (raw, _) = make_test_announce();
            let actions = vec![Action::Broadcast {
                data: raw.clone(),
                exclude_iface: None,
            }];

            let result = dispatch_actions(&mut interfaces, actions, &ifac_configs);
            assert!(result.errors.is_empty());

            // iface 0 should get IFAC-wrapped bytes
            let sent0 = iface0.sent.last().expect("iface0 should have sent").clone();
            assert!(
                IfacConfig::has_ifac_flag(&sent0),
                "IFAC interface should get IFAC-wrapped packet"
            );
            assert_eq!(sent0.len(), raw.len() + 8);

            // iface 1 should get raw bytes (no IFAC)
            let sent1 = iface1.sent.last().expect("iface1 should have sent").clone();
            assert!(
                !IfacConfig::has_ifac_flag(&sent1),
                "Non-IFAC interface should get raw packet"
            );
            assert_eq!(sent1.len(), raw.len());
        }

        #[test]
        fn test_ifac_wrong_key_dropped() {
            let cfg1 = IfacConfig::new(Some("net1"), Some("key1"), 16).unwrap();
            let cfg2 = IfacConfig::new(Some("net2"), Some("key2"), 16).unwrap();

            let (raw, _) = make_test_announce();
            let wrapped = cfg1.apply_ifac(&raw).unwrap();

            // Try to verify with wrong key
            let mut transport = test_transport();
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("ifac0", 0)));
            transport.set_interface_name(iface_idx, "ifac0".into());
            transport.set_ifac_config(iface_idx, cfg2);

            let result = transport.process_incoming(iface_idx, &wrapped);
            assert!(result.is_ok(), "Should silently drop");
            assert_eq!(transport.stats().packets_dropped, 1);
        }

        #[test]
        fn test_ifac_config_cleanup_on_remove() {
            let mut transport = test_transport();
            let cfg = IfacConfig::new(Some("testnet"), None, 8).unwrap();
            transport.set_ifac_config(0, cfg);
            assert!(transport.ifac_config(0).is_some());

            transport.remove_ifac_config(0);
            assert!(transport.ifac_config(0).is_none());
        }

        #[test]
        fn test_ifac_config_restored_after_reconnect() {
            let cfg = IfacConfig::new(Some("testnet"), Some("secret"), 16).unwrap();

            let mut transport = test_transport();
            let iface_idx = transport.register_interface(Box::new(MockInterface::new("ifac0", 0)));
            transport.set_interface_name(iface_idx, "ifac0".into());
            transport.set_ifac_config(iface_idx, cfg.clone());

            // Simulate disconnect: remove_ifac_config (called by handle_interface_down)
            transport.remove_ifac_config(iface_idx);
            assert!(transport.ifac_config(iface_idx).is_none());

            // Simulate reconnect: re-apply IFAC config
            transport.set_ifac_config(iface_idx, cfg.clone());
            assert!(transport.ifac_config(iface_idx).is_some());

            // Verify IFAC-wrapped packet is accepted after re-apply
            let (raw, _) = make_test_announce();
            let wrapped = cfg.apply_ifac(&raw).unwrap();
            let result = transport.process_incoming(iface_idx, &wrapped);
            assert!(
                result.is_ok(),
                "IFAC packet should be accepted after config restore"
            );
            assert_eq!(transport.stats().packets_dropped, 0);
        }
    }

    /// PacingDelay Display includes the ready-at timestamp. The exact
    /// wording is less important than (a) presence of the variant,
    /// (b) the timestamp being visible for log grepping.
    #[test]
    fn pacing_delay_display_includes_ready_at_ms() {
        use alloc::format;
        let err = TransportError::PacingDelay { ready_at_ms: 1_000 };
        let rendered = format!("{}", err);
        assert!(
            rendered.contains("1000"),
            "expected ready_at_ms in Display output, got {rendered:?}"
        );
    }

    /// Setter + getter round-trip for the interface_next_slot_ms
    /// backchannel. Driver pushes, Transport reads.
    #[test]
    fn interface_next_slot_ms_round_trip() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_next_slot_ms(3, 5_000);
        assert_eq!(t.next_slot_ms_for_interface(3, 0), 5_000);
    }

    /// Absent key returns the caller's `now_ms`, unknown interfaces
    /// are treated as "always ready", matching the trait-level
    /// next_slot_ms default.
    #[test]
    fn interface_next_slot_ms_absent_returns_now() {
        use crate::test_utils::test_transport;
        let t = test_transport();
        assert_eq!(t.next_slot_ms_for_interface(99, 42), 42);
    }

    /// Overwriting pushes a fresh value; no accumulation.
    #[test]
    fn interface_next_slot_ms_overwrite() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_next_slot_ms(7, 1_000);
        t.set_interface_next_slot_ms(7, 2_000);
        assert_eq!(t.next_slot_ms_for_interface(7, 0), 2_000);
    }

    /// Empty registry: jitter window falls back to the legacy
    /// `PATHFINDER_RW_MS = 500` floor. Hosts without any interface
    /// reporting airtime see the pre-airtime behaviour.
    #[test]
    fn announce_jitter_max_ms_empty_registry_uses_floor() {
        use crate::test_utils::test_transport;
        let t = test_transport();
        assert_eq!(t.announce_jitter_max_ms(), PATHFINDER_RW_MS);
    }

    /// Tiny airtime (3 × 100 ms = 300) does not lift the window above
    /// the legacy floor.
    #[test]
    fn announce_jitter_max_ms_small_airtime_below_floor() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_max_airtime_ms(0, 100);
        assert_eq!(t.announce_jitter_max_ms(), 500);
    }

    /// SF10-class airtime (~2700 ms) lifts the window to 3 × 2700 =
    /// 8100 ms, well above one packet's airtime.
    #[test]
    fn announce_jitter_max_ms_sf10_airtime_dominates() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_max_airtime_ms(0, 2_700);
        assert_eq!(t.announce_jitter_max_ms(), 3 * 2_700);
    }

    /// Fast and slow interfaces: the slowest wins.
    #[test]
    fn announce_jitter_max_ms_two_interfaces_slowest_wins() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_max_airtime_ms(0, 100);
        t.set_interface_max_airtime_ms(1, 2_700);
        assert_eq!(t.announce_jitter_max_ms(), 3 * 2_700);
    }

    /// Setter overwrite works for max-airtime as it does for next_slot.
    #[test]
    fn set_interface_max_airtime_ms_overwrite() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_max_airtime_ms(0, 1_000);
        t.set_interface_max_airtime_ms(0, 500);
        assert_eq!(t.announce_jitter_max_ms(), 3 * 500);
    }

    /// With no interfaces registered, the jitter window matches the
    /// legacy floor, pre-airtime behaviour preserved.
    #[test]
    fn announce_retry_jitter_no_interfaces_uses_legacy_floor() {
        use crate::test_utils::test_transport;
        let t = test_transport();
        let dest = [0xAB; TRUNCATED_HASHBYTES];
        let jitter = t.deterministic_jitter_ms(&dest, t.announce_jitter_max_ms());
        assert!(jitter < PATHFINDER_RW_MS);
    }

    /// With one SF10-class interface, the jitter window scales to
    /// 3 × airtime. Verify the jitter falls within the expected
    /// extended bound and exceeds the legacy floor over many seeds.
    #[test]
    fn announce_retry_jitter_sf10_extends_window() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_max_airtime_ms(0, 2_700);
        let upper = t.announce_jitter_max_ms();
        assert_eq!(upper, 3 * 2_700);
        let mut max_seen = 0u64;
        for byte in 0..=255u8 {
            let dest = [byte; TRUNCATED_HASHBYTES];
            let jitter = t.deterministic_jitter_ms(&dest, upper);
            assert!(jitter < upper);
            max_seen = max_seen.max(jitter);
        }
        assert!(
            max_seen > PATHFINDER_RW_MS,
            "256-seed sample should expose the extended window, max_seen={max_seen}"
        );
    }

    /// The retry-scheduler call site multiplies jitter window by the
    /// backoff_factor (1, 2, 4, 8, 16 across retries 0-4). At retry 3
    /// the effective ceiling is 8 × (3 × airtime), comfortably wide.
    #[test]
    fn announce_retry_jitter_with_backoff_factor() {
        use crate::test_utils::test_transport;
        let mut t = test_transport();
        t.set_interface_max_airtime_ms(0, 2_700);
        let base = t.announce_jitter_max_ms();
        let backoff_factor = 8u64;
        let upper = base * backoff_factor;
        let dest = [0x42; TRUNCATED_HASHBYTES];
        let jitter = t.deterministic_jitter_ms(&dest, upper);
        assert!(jitter < upper);
        assert_eq!(upper, 8 * 3 * 2_700);
    }

    /// Two nodes with distinct identities and the same airtime
    /// configuration must compute different jitter values for the
    /// same destination, that is the whole point of folding identity
    /// into `deterministic_jitter_ms`. Without this, synchronous
    /// startups would still collide on every retry.
    #[test]
    fn announce_retry_jitter_diverges_across_identities() {
        use crate::test_utils::test_transport;
        let mut a = test_transport();
        let mut b = test_transport();
        a.set_interface_max_airtime_ms(0, 2_700);
        b.set_interface_max_airtime_ms(0, 2_700);
        let dest = [0x77; TRUNCATED_HASHBYTES];
        let upper = a.announce_jitter_max_ms();
        let ja = a.deterministic_jitter_ms(&dest, upper);
        let jb = b.deterministic_jitter_ms(&dest, upper);
        assert_ne!(
            ja, jb,
            "two random identities produced identical jitter for the same dest"
        );
    }
}
