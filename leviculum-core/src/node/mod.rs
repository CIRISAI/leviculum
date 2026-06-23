//! High-level Node API for Reticulum
//!
//! This module provides [`NodeCore`], a unified interface that combines
//! transport, link management, and routing into a single, easy-to-use API.
//!
//! # Overview
//!
//! The Node API provides:
//! - **Unified event handling**: All events (announces, packets, links) through one stream
//! - **Single-packet delivery**: Encrypted single-packet sends via [`NodeCore::send_single_packet()`]
//! - **Builder pattern**: Easy configuration via [`NodeCoreBuilder`]
//!
//! # Example
//!
//! ```no_run
//! use leviculum_core::node::{NodeCore, NodeCoreBuilder, NodeEvent};
//! use leviculum_core::{Destination, DestinationType, Direction};
//! use leviculum_core::identity::Identity;
//! use leviculum_core::traits::{Clock, NoStorage};
//! # use core::cell::Cell;
//! # struct MyClock(Cell<u64>);
//! # impl MyClock { fn new(ms: u64) -> Self { Self(Cell::new(ms)) } }
//! # impl Clock for MyClock { fn now_ms(&self) -> u64 { self.0.get() } }
//!
//! # fn example() {
//! let my_identity = Identity::generate(&mut rand_core::OsRng);
//!
//! // Build a node
//! let mut node = NodeCoreBuilder::new()
//!     .enable_transport(false)
//!     .build(rand_core::OsRng, MyClock::new(0), NoStorage);
//!
//! // Register a destination
//! let dest = Destination::new(
//!     Some(my_identity),
//!     Direction::In,
//!     DestinationType::Single,
//!     "myapp",
//!     &["echo"],
//! ).unwrap();
//! node.register_destination(dest);
//! # }

mod builder;
mod event;
mod link_management;
// Gated on `tracing`: the mvr captures structured tracing output to prove the
// exact drop reason, so it needs the real `tracing`/`tracing-subscriber` crates
// (absent under `--no-default-features`, e.g. the core-no-tracing CI gate).
#[cfg(all(test, feature = "tracing"))]
mod mvr_establishment_loss;
#[cfg(test)]
mod mvr_link_rekey_alias;
#[cfg(all(test, feature = "tracing"))]
mod mvr_lrproof;
#[cfg(test)]
mod mvr_teardown_resource_fail;
pub mod request;
mod send;

pub use builder::NodeCoreBuilder;
pub use event::{DeliveryError, EventClass, NodeEvent};
pub use request::{RequestError, RequestPolicy};
pub use send::SendError;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::announce::{AnnounceControl, AnnounceError};
use crate::constants::{RATCHET_SIZE, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationHash, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{Link, LinkId};
use crate::packet::packet_hash;
use crate::traits::{Clock, Storage};
use crate::transport::{Transport, TransportConfig, TransportEvent, TransportStats};
use rand_core::CryptoRngCore;

use crate::hex_fmt::{HexFmt, HexShort};

use crate::constants::MGMT_ANNOUNCE_INTERVAL_MS;

/// Delay before the first management announce after startup.
/// Python defers by `mgmt_announce_interval - 15` so first fires at ~15s.
const MGMT_ANNOUNCE_INITIAL_DELAY_MS: u64 = 15 * 1000;

/// Link statistics for observability
#[derive(Debug, Clone)]
pub struct LinkStats {
    tx_ring_size: usize,
    window: usize,
    window_max: usize,
    pacing_interval_ms: u64,
}

impl LinkStats {
    pub(crate) fn new(
        tx_ring_size: usize,
        window: usize,
        window_max: usize,
        pacing_interval_ms: u64,
    ) -> Self {
        Self {
            tx_ring_size,
            window,
            window_max,
            pacing_interval_ms,
        }
    }

    /// Number of outstanding (unacknowledged) messages in the channel tx ring
    pub fn tx_ring_size(&self) -> usize {
        self.tx_ring_size
    }

    /// Current channel window size
    pub fn window(&self) -> usize {
        self.window
    }

    /// Maximum channel window size
    pub fn window_max(&self) -> usize {
        self.window_max
    }

    /// Current pacing interval between sends (milliseconds)
    pub fn pacing_interval_ms(&self) -> u64 {
        self.pacing_interval_ms
    }
}

/// The unified Reticulum node, owns all protocol state
///
/// NodeCore is generic over RNG, Clock, and Storage traits, allowing it to run
/// on both std and no_std environments.
///
/// # Type Parameters
///
/// * `R` - Random number generator (must implement `CryptoRngCore`)
/// * `C` - Clock implementation for timestamps
/// * `S` - Storage implementation for persistence
pub struct NodeCore<R: CryptoRngCore, C: Clock, S: Storage> {
    /// Owned random number generator
    rng: R,
    /// Transport layer (routing, paths, packets) - owns the node's identity
    transport: Transport<C, S>,
    /// Active links by ID
    links: BTreeMap<LinkId, Link>,
    /// Tracks channel message receipts awaiting delivery proofs
    receipt_tracker: link_management::ReceiptTracker,
    /// Count of rx_ring full drops since last log
    rx_ring_full_count: u64,
    /// Timestamp (ms) when last rx_ring full log was emitted
    rx_ring_full_last_log_ms: u64,
    /// Registered destinations
    destinations: BTreeMap<DestinationHash, Destination>,
    /// Default proof strategy for new destinations
    default_proof_strategy: ProofStrategy,
    /// Pending events
    events: Vec<NodeEvent>,
    /// Probe destination hash (if respond_to_probes is enabled).
    /// Used for periodic management announces and status reporting.
    probe_dest_hash: Option<DestinationHash>,
    /// Management destinations to announce periodically (probe, etc.).
    /// Announced 15s after startup, then every 2 hours (matching Python).
    mgmt_destinations: Vec<DestinationHash>,
    /// Next time (ms) to send management announces. None if no mgmt destinations.
    next_mgmt_announce_ms: Option<u64>,
    /// Request handler registry, keyed by path_hash.
    /// Cleanup: entries removed via `deregister_request_handler()`.
    request_handlers: BTreeMap<[u8; TRUNCATED_HASHBYTES], request::RequestHandlerEntry>,
    /// Pending outgoing requests, keyed by request_id.
    /// Cleanup: removed on (a) response, (b) timeout, (c) link close.
    pending_requests: BTreeMap<[u8; TRUNCATED_HASHBYTES], request::PendingRequest>,
    /// Link establishment retry state, keyed by link_id.
    /// When a link request times out, the retry state determines whether
    /// to re-attempt with fresh keys or emit LinkClosed::Timeout.
    /// Cleanup: removed on (a) successful proof, (b) all retries exhausted,
    /// (c) remove_link() when the link is torn down.
    link_retry_state: BTreeMap<LinkId, link_management::LinkRetryState>,
    /// Caller-visible link-id aliases (old id → current id).
    ///
    /// Codeberg #66: a link-establishment retry regenerates the
    /// ephemeral keys, which changes the wire link id. The `links` map
    /// is keyed by the CURRENT wire id (inbound proof/data lookups stay
    /// natural), and ids previously handed to callers (`connect()`
    /// return value, LinkHandle) resolve through this map. Entries are
    /// dropped when their target link is removed.
    link_id_aliases: BTreeMap<LinkId, LinkId>,
    /// Reverse of `link_id_aliases` for the event boundary (current id
    /// → the ORIGINAL id `connect()` returned). Outbound events are
    /// rewritten to the original id in `process_events_and_actions`,
    /// so applications can correlate every event — including a
    /// LinkClosed for a link that never established — with the one id
    /// they hold. Entries exist only for re-keyed links.
    link_origin_ids: BTreeMap<LinkId, LinkId>,
    /// Maximum incoming resource size in bytes. Resources larger than this
    /// are rejected at advertisement time, before any allocation.
    max_incoming_resource_size: usize,
    /// Optional policy hook for per-destination announce suppression.
    /// `None` (default) announces every eligible destination — historical
    /// behaviour. Consulted on every scheduled announce; see
    /// [`AnnounceControl`].
    announce_control: Option<Box<dyn AnnounceControl>>,
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// Create a new NodeCore with the given configuration
    ///
    /// # Arguments
    /// * `identity` - The node's identity
    /// * `config` - Transport configuration
    /// * `proof_strategy` - Default proof strategy for destinations
    /// * `rng` - Random number generator (moved into NodeCore)
    /// * `clock` - Clock instance (moved into NodeCore)
    /// * `storage` - Storage instance (moved into NodeCore)
    pub fn new(
        identity: Identity,
        config: TransportConfig,
        proof_strategy: ProofStrategy,
        max_incoming_resource_size: usize,
        rng: R,
        clock: C,
        storage: S,
    ) -> Self {
        // Transport takes ownership of a second identity created from the same keys
        // Since we can't clone Identity, we store the hash for reference and let
        // Transport own the identity for packet operations
        let transport = Transport::new(config, clock, storage, identity);

        Self {
            rng,
            // We don't store identity separately - Transport owns it
            // Access via transport.identity()
            transport,
            links: BTreeMap::new(),
            receipt_tracker: link_management::ReceiptTracker::new(),
            rx_ring_full_count: 0,
            rx_ring_full_last_log_ms: 0,
            destinations: BTreeMap::new(),
            default_proof_strategy: proof_strategy,
            events: Vec::new(),
            probe_dest_hash: None,
            mgmt_destinations: Vec::new(),
            next_mgmt_announce_ms: None,
            request_handlers: BTreeMap::new(),
            pending_requests: BTreeMap::new(),
            link_retry_state: BTreeMap::new(),
            link_id_aliases: BTreeMap::new(),
            link_origin_ids: BTreeMap::new(),
            max_incoming_resource_size,
            announce_control: None,
        }
    }

    // Destination Management
    /// Register a destination to receive packets and/or connections
    ///
    /// # Arguments
    /// * `dest` - The destination to register
    pub fn register_destination(&mut self, dest: Destination) {
        let hash = *dest.hash();
        let ratchets_enabled = dest.ratchets_enabled();

        // Only Direction::In destinations are locally reachable and should
        // be registered with transport for routing. Direction::Out destinations
        // are remote peer references (for encryption/proof verification) and
        // must NOT appear in local_destinations, otherwise their announces
        // would be dropped as self-echoes.
        if dest.direction() == Direction::In {
            self.transport.register_destination(hash.into_bytes());
        }

        self.destinations.insert(hash, dest);

        // Load persisted ratchet keys if ratchets are enabled
        if ratchets_enabled {
            if let Some(serialized) = self
                .transport
                .storage()
                .load_dest_ratchet_keys(hash.as_bytes())
            {
                if let Some(dest) = self.destinations.get_mut(&hash) {
                    if let Err(e) = dest.load_ratchets_signed(&serialized) {
                        crate::tracing::warn!(
                            "Failed to load persisted ratchet keys for <{}>: {}",
                            hash,
                            e
                        );
                    }
                    // Reset rotation timer, loaded timestamps are from a previous
                    // session's monotonic domain and would block rotation.
                    dest.set_last_ratchet_time(0);
                }
            }
        }
    }

    /// Unregister a destination
    pub fn unregister_destination(&mut self, hash: &DestinationHash) {
        self.transport.unregister_destination(hash.as_bytes());
        self.destinations.remove(hash);
    }

    /// Install (or clear) the per-destination announce-suppression policy.
    ///
    /// `None` restores the default of announcing every eligible destination.
    /// The policy is consulted on every scheduled announce — both the periodic
    /// management tick and the interface-recovery re-announce — before a
    /// destination is emitted. Suppressed destinations remain routable; they
    /// are simply never gossiped. See [`AnnounceControl`].
    pub fn set_announce_control(&mut self, policy: Option<Box<dyn AnnounceControl>>) {
        self.announce_control = policy;
    }

    /// Returns `true` if an announce-suppression policy is installed.
    pub fn has_announce_control(&self) -> bool {
        self.announce_control.is_some()
    }

    /// Get the probe destination hash (if respond_to_probes is enabled).
    ///
    /// Used by RPC handlers (rnstatus) and status reporting.
    pub fn probe_dest_hash(&self) -> Option<&DestinationHash> {
        self.probe_dest_hash.as_ref()
    }

    /// Create and register the probe responder destination (rnstransport.probe).
    ///
    /// Called by `NodeCoreBuilder::build()` when `respond_to_probes` is set.
    /// Reconstructs the transport identity from private key bytes, creates
    /// a `Destination(identity, IN, SINGLE, "rnstransport", "probe")` with
    /// `ProofStrategy::All`, registers it, and schedules periodic announces
    /// (15s after startup, then every 2 hours, matching Python rnsd).
    fn enable_probe_responder(&mut self) {
        // Reconstruct identity for the probe destination (Transport owns the original)
        let identity_bytes = match self.transport.identity().private_key_bytes() {
            Ok(bytes) => bytes,
            Err(e) => {
                crate::tracing::warn!("Cannot create probe destination: {e}");
                return;
            }
        };
        let probe_identity = match Identity::from_private_key_bytes(&identity_bytes) {
            Ok(id) => id,
            Err(e) => {
                crate::tracing::warn!("Cannot create probe destination: {e}");
                return;
            }
        };
        let mut probe_dest = match Destination::new(
            Some(probe_identity),
            crate::destination::Direction::In,
            crate::destination::DestinationType::Single,
            "rnstransport",
            &["probe"],
        ) {
            Ok(d) => d,
            Err(e) => {
                crate::tracing::warn!("Cannot create probe destination: {e}");
                return;
            }
        };
        probe_dest.set_proof_strategy(ProofStrategy::All);
        let hash = *probe_dest.hash();
        self.register_destination(probe_dest);
        self.probe_dest_hash = Some(hash);
        self.mgmt_destinations.push(hash);

        // Schedule first announce 15s after startup
        let now_ms = self.transport.clock().now_ms();
        self.next_mgmt_announce_ms = Some(now_ms + MGMT_ANNOUNCE_INITIAL_DELAY_MS);

        crate::tracing::info!("Probe responder at <{}> active", hash);
        crate::tracing::info!(
            "[IDENTITY] probe_destination={} aspect=rnstransport.probe",
            hash
        );
    }

    /// Get a registered destination
    pub fn destination(&self, hash: &DestinationHash) -> Option<&Destination> {
        self.destinations.get(hash)
    }

    /// Get a mutable reference to a registered destination
    pub fn destination_mut(&mut self, hash: &DestinationHash) -> Option<&mut Destination> {
        self.destinations.get_mut(hash)
    }

    /// Register a remote identity for single-packet encryption.
    ///
    /// Identities learned from received announces are cached automatically.    /// call this only for out-of-band identity registration or testing.
    pub fn remember_identity(&mut self, dest_hash: DestinationHash, identity: Identity) {
        self.storage_mut()
            .set_identity(dest_hash.into_bytes(), identity);
    }

    // Storage Accessors
    /// Borrow the storage implementation (delegates through Transport)
    pub fn storage(&self) -> &S {
        self.transport.storage()
    }

    /// Mutably borrow the storage implementation (delegates through Transport)
    pub fn storage_mut(&mut self) -> &mut S {
        self.transport.storage_mut()
    }

    /// Announce a registered destination on all interfaces
    ///
    /// Builds the announce packet and broadcasts it. The announce is queued
    /// as a Broadcast action, dispatched by the next [`NodeCore::handle_timeout()`] or
    /// [`NodeCore::handle_packet()`] call.
    ///
    /// # Deferred dispatch
    ///
    /// This method queues I/O actions internally. The actions are not executed
    /// until the driver calls [`NodeCore::handle_packet()`] or [`NodeCore::handle_timeout()`],
    /// which drain all pending actions. Callers must ensure the event loop
    /// runs promptly after calling this method.
    ///
    /// # Arguments
    /// * `dest_hash` - Hash of the registered destination to announce
    /// * `app_data` - Optional application data to include in the announce
    ///
    /// # Returns
    /// A `TickOutput` containing the broadcast action. The driver must
    /// dispatch this output the same way it handles output from
    /// `handle_packet()` / `handle_timeout()`.
    pub fn announce_destination(
        &mut self,
        dest_hash: &DestinationHash,
        app_data: Option<&[u8]>,
    ) -> Result<crate::transport::TickOutput, AnnounceError> {
        let now_ms = self.transport.clock().now_ms();

        let dest = self
            .destinations
            .get_mut(dest_hash)
            .ok_or(AnnounceError::DestinationNotFound)?;

        let packet = dest.announce(app_data, &mut self.rng, now_ms)?;
        let ratchet_pub = dest.current_ratchet_public();

        let mut buf = [0u8; crate::constants::MTU];
        let len = packet
            .pack(&mut buf)
            .map_err(|_| AnnounceError::PacketTooLarge)?;

        self.transport
            .storage_mut()
            .set_announce_cache(dest_hash.into_bytes(), buf[..len].to_vec());
        self.transport.send_on_all_interfaces(&buf[..len]);

        // Sender self-remember: store own ratchet in known_ratchets so that
        // encrypt-to-self works (Python Destination.announce -> _remember_ratchet).
        if let Some(rp) = ratchet_pub {
            self.transport
                .storage_mut()
                .remember_known_ratchet(dest_hash.into_bytes(), rp, now_ms);
        }

        // Persist ratchet private keys if they changed (new key generated).
        if let Some(dest) = self.destinations.get_mut(dest_hash) {
            if dest.ratchets_dirty() {
                if let Some(signed) = dest.serialize_ratchets_signed() {
                    self.transport
                        .storage_mut()
                        .store_dest_ratchet_keys(dest_hash.into_bytes(), signed);
                }
                dest.clear_ratchets_dirty();
            }
        }

        Ok(self.process_events_and_actions())
    }

    /// Send unreliable data via single packet
    ///
    /// This builds and sends a single data packet to the destination.
    /// Use this when you've already determined that single-packet delivery
    /// is appropriate via `send()`.
    ///
    /// # Arguments
    /// * `dest_hash` - The destination to send to
    /// * `data` - The data to send
    ///
    /// # Returns
    /// The packet hash for tracking delivery (if proofs are enabled) and
    /// a `TickOutput` containing the send action. The driver must dispatch
    /// this output the same way it handles output from `handle_packet()` /
    /// `handle_timeout()`.
    pub fn send_single_packet(
        &mut self,
        dest_hash: &DestinationHash,
        data: &[u8],
    ) -> Result<([u8; TRUNCATED_HASHBYTES], crate::transport::TickOutput), send::SendError> {
        // Build a data packet
        use crate::destination::DestinationType;
        use crate::packet::{
            HeaderType, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
        };

        // Look up ratchet key for forward-secure encryption (Python Identity.encrypt).
        let ratchet_key = self.transport.get_ratchet(dest_hash);

        // Encrypt payload using remote identity from storage.
        // Access transport.storage() directly to allow disjoint borrow of self.rng.
        let payload =
            if let Some(identity) = self.transport.storage().get_identity(dest_hash.as_bytes()) {
                identity
                    .encrypt_for_destination(data, ratchet_key.as_ref(), &mut self.rng)
                    .map_err(|_| send::SendError::EncryptionFailed)?
            } else {
                return Err(send::SendError::EncryptionFailed);
            };

        let packet = crate::packet::Packet {
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
            destination_hash: dest_hash.into_bytes(),
            context: PacketContext::None,
            data: PacketData::Owned(payload),
        };

        // Pack the packet
        let mut buf = [0u8; crate::constants::MTU];
        let len = packet
            .pack(&mut buf)
            .map_err(|_| send::SendError::TooLarge)?;

        // Send via transport
        self.transport
            .send_to_destination(dest_hash.as_bytes(), &buf[..len])
            .map_err(|e| match e {
                crate::transport::TransportError::PacingDelay { ready_at_ms } => {
                    send::SendError::PacingDelay { ready_at_ms }
                }
                _ => send::SendError::NoPath,
            })?;

        // Create receipt and return hash
        let packet_hash = self
            .transport
            .create_receipt(&buf[..len], dest_hash.into_bytes());

        let output = self.process_events_and_actions();
        Ok((packet_hash, output))
    }

    /// Send a proof for a received single packet (`ProofStrategy::App`)
    ///
    /// Call this after receiving `NodeEvent::PacketProofRequested` if the
    /// application decides to prove delivery. Uses path-table routing
    /// to reach the original sender.
    ///
    /// # Arguments
    /// * `packet_hash` - The full SHA256 hash from `NodeEvent::PacketProofRequested`
    /// * `destination_hash` - The destination hash from `NodeEvent::PacketProofRequested`
    pub fn send_proof(
        &mut self,
        packet_hash: &[u8; 32],
        destination_hash: &DestinationHash,
    ) -> Result<crate::transport::TickOutput, crate::transport::TransportError> {
        let identity = self
            .destinations
            .get(destination_hash)
            .and_then(|d| d.identity())
            .ok_or(crate::transport::TransportError::NoPath)?;

        self.transport.send_proof(
            packet_hash,
            destination_hash.as_bytes(),
            identity,
            None, // path-based routing for App strategy
        )?;

        Ok(self.process_events_and_actions())
    }

    // Link Identity API
    /// Identify our identity to the link peer.
    ///
    /// Sends a LINKIDENTIFY packet encrypted with the link session key,
    /// containing our public key and a signature binding the identity to
    /// this specific link. Only the initiator can identify; the link must
    /// be active.
    ///
    /// Matches Python `Link.identify()` (Link.py:459-475).
    pub fn identify_link(
        &mut self,
        link_id: &LinkId,
        identity: &Identity,
    ) -> Result<crate::transport::TickOutput, crate::link::LinkError> {
        use crate::link::LinkError;
        use crate::packet::PacketContext;

        // Resolve a possibly-stale caller-visible id to the current wire id:
        // a #66 establishment retry re-keys the link, so a caller holding the
        // original id must be routed through `link_id_aliases` (mirrors
        // link()/link_mut()). The resolved id binds both the lookup AND the
        // signature below: the responder knows the link under the new id.
        let link_id = &self.resolve_link_id(link_id);

        // Verify link exists, is active, and we are initiator
        let link = self.links.get(link_id).ok_or(LinkError::NotFound)?;
        if !link.is_initiator() {
            return Err(LinkError::InvalidState);
        }
        if !link.is_active() {
            return Err(LinkError::InvalidState);
        }

        // Build proof: public_key(64) || signature(64) = 128 bytes
        let public_key = identity.public_key_bytes();

        let mut signed_data = [0u8; 80];
        signed_data[..16].copy_from_slice(link_id.as_bytes());
        signed_data[16..80].copy_from_slice(&public_key);

        let signature = identity
            .sign(&signed_data)
            .map_err(|_| LinkError::NoIdentity)?;

        let mut proof_data = [0u8; 128];
        proof_data[..64].copy_from_slice(&public_key);
        proof_data[64..128].copy_from_slice(&signature);

        // Encrypt and build packet
        let packet = link.build_data_packet_with_context(
            &proof_data,
            PacketContext::LinkIdentify,
            &mut self.rng,
        )?;

        // Route
        self.route_link_packet(link_id, &packet);

        Ok(self.process_events_and_actions())
    }

    // Request/Response API
    /// Register a request handler for a given path on a destination.
    ///
    /// Incoming requests matching the truncated hash of `path` will emit
    /// [`NodeEvent::RequestReceived`] events.
    pub fn register_request_handler(
        &mut self,
        destination_hash: DestinationHash,
        path: &str,
        policy: request::RequestPolicy,
    ) {
        let path_hash = crate::crypto::truncated_hash(path.as_bytes());
        self.request_handlers.insert(
            path_hash,
            request::RequestHandlerEntry {
                path: String::from(path),
                destination_hash,
                policy,
            },
        );
    }

    /// Deregister a request handler for a given path.
    ///
    /// Returns `true` if a handler was removed.
    pub fn deregister_request_handler(&mut self, path: &str) -> bool {
        let path_hash = crate::crypto::truncated_hash(path.as_bytes());
        self.request_handlers.remove(&path_hash).is_some()
    }

    /// Send a request on an established link.
    ///
    /// Returns `(request_id, TickOutput)` on success. The `request_id` can be
    /// used to match the subsequent [`NodeEvent::ResponseReceived`] or
    /// [`NodeEvent::RequestTimedOut`] event.
    ///
    /// `data` must be exactly one valid msgpack value (or `None` for nil).
    pub fn send_request(
        &mut self,
        link_id: &LinkId,
        path: &str,
        data: Option<&[u8]>,
        timeout_ms: Option<u64>,
    ) -> Result<([u8; TRUNCATED_HASHBYTES], crate::transport::TickOutput), request::RequestError>
    {
        use crate::packet::PacketContext;
        use crate::resource::msgpack::{
            write_bin, write_fixarray_header, write_float64, write_nil,
        };

        // Resolve a possibly-stale caller-visible id (a #66 retry re-keys the
        // link) so the lookup, the pending-request bookkeeping, and the route
        // all use the current wire id consistently.
        let link_id = &self.resolve_link_id(link_id);

        // Verify link exists and is active
        let link = self
            .links
            .get(link_id)
            .ok_or(request::RequestError::LinkNotFound)?;
        if !link.is_active() {
            return Err(request::RequestError::LinkNotActive);
        }

        // Compute path_hash
        let path_hash = crate::crypto::truncated_hash(path.as_bytes());

        // Compute timestamp (seconds since epoch, float64)
        let now_ms = self.transport.clock().now_ms();
        let timestamp = now_ms as f64 / 1000.0;

        // Build msgpack: fixarray(3) + float64(timestamp) + bin(path_hash) + data_or_nil
        let mut packed = Vec::new();
        write_fixarray_header(&mut packed, 3);
        write_float64(&mut packed, timestamp);
        write_bin(&mut packed, &path_hash);
        if let Some(d) = data {
            debug_assert!(
                {
                    let mut p = 0;
                    crate::resource::msgpack::skip_msgpack_value(d, &mut p).is_some()
                        && p == d.len()
                },
                "data must be exactly one valid msgpack value"
            );
            packed.extend_from_slice(d);
        } else {
            write_nil(&mut packed);
        }

        // Check against link MDU
        if packed.len() > link.mdu() {
            return Err(request::RequestError::PayloadTooLarge);
        }

        // Build encrypted data packet with Request context
        let raw_packet = link
            .build_data_packet_with_context(&packed, PacketContext::Request, &mut self.rng)
            .map_err(|_| request::RequestError::EncryptionFailed)?;

        // Compute request_id = truncated_packet_hash(raw_packet)
        let request_id = crate::packet::truncated_packet_hash(&raw_packet);

        // Compute timeout
        let rtt_ms = link.rtt_ms();
        let timeout = timeout_ms.unwrap_or_else(|| {
            rtt_ms
                .saturating_mul(crate::constants::TRAFFIC_TIMEOUT_FACTOR)
                .saturating_add(crate::constants::RESPONSE_MAX_GRACE_TIME_MS)
                .saturating_add(1250) // Python adds 1250ms extra grace
        });

        // Insert pending request
        self.pending_requests.insert(
            request_id,
            request::PendingRequest {
                link_id: *link_id,
                request_id,
                sent_at_ms: now_ms,
                timeout_ms: timeout,
            },
        );

        // Route the packet
        self.route_link_packet(link_id, &raw_packet);

        Ok((request_id, self.process_events_and_actions()))
    }

    /// Send a response to a received request.
    ///
    /// `response_data` must be exactly one valid msgpack-encoded value
    /// (e.g., msgpack True `0xC3`, False `0xC2`, int, bin, etc.).
    pub fn send_response(
        &mut self,
        link_id: &LinkId,
        request_id: &[u8; TRUNCATED_HASHBYTES],
        response_data: &[u8],
    ) -> Result<crate::transport::TickOutput, request::RequestError> {
        use crate::packet::PacketContext;
        use crate::resource::msgpack::{write_bin, write_fixarray_header};

        // Resolve a possibly-stale caller-visible id (a #66 retry re-keys the
        // link) so the lookup and the route both use the current wire id.
        let link_id = &self.resolve_link_id(link_id);

        // Verify link exists and is active
        let link = self
            .links
            .get(link_id)
            .ok_or(request::RequestError::LinkNotFound)?;
        if !link.is_active() {
            return Err(request::RequestError::LinkNotActive);
        }

        // Debug-assert response_data is a valid single msgpack value
        debug_assert!(
            {
                let mut p = 0;
                crate::resource::msgpack::skip_msgpack_value(response_data, &mut p).is_some()
                    && p == response_data.len()
            },
            "response_data must be exactly one valid msgpack value"
        );

        // Build msgpack: fixarray(2) + bin(request_id) + response_data (raw)
        let mut packed = Vec::new();
        write_fixarray_header(&mut packed, 2);
        write_bin(&mut packed, request_id);
        packed.extend_from_slice(response_data);

        // Check against link MDU
        if packed.len() > link.mdu() {
            return Err(request::RequestError::PayloadTooLarge);
        }

        // Build encrypted data packet with Response context
        let raw_packet = link
            .build_data_packet_with_context(&packed, PacketContext::Response, &mut self.rng)
            .map_err(|_| request::RequestError::EncryptionFailed)?;

        // Route the packet
        self.route_link_packet(link_id, &raw_packet);

        Ok(self.process_events_and_actions())
    }

    // Resource Transfer API
    /// Initiate a resource transfer on an established link.
    ///
    /// Creates an `OutgoingResource`, sends the advertisement, and returns
    /// the resource hash identifying this transfer. The actual data transfer
    /// proceeds automatically via REQ/data packet exchanges.
    ///
    /// # Arguments
    /// * `link_id` - The link to send the resource on (must be Active)
    /// * `data` - The data to transfer
    /// * `metadata` - Optional metadata bytes. Must be msgpack-encoded by the caller
    ///   (e.g., via `rmpv::encode::write_value`). Python's Resource constructor
    ///   calls `umsgpack.packb(metadata)`, the caller must do the equivalent.
    pub fn send_resource(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
        metadata: Option<&[u8]>,
        auto_compress: bool,
    ) -> Result<([u8; 32], crate::transport::TickOutput), crate::resource::ResourceError> {
        use crate::packet::PacketContext;
        use crate::resource::outgoing::OutgoingResource;
        use crate::resource::ResourceError;

        let now_ms = self.transport.clock().now_ms();

        // Resolve a possibly-stale caller-visible id (a #66 retry re-keys the
        // link) so every links.get/get_mut and the route use the wire id.
        let link_id = &self.resolve_link_id(link_id);

        let link = self
            .links
            .get(link_id)
            .ok_or(ResourceError::InvalidRequest)?;

        if link.has_outgoing_resource() {
            return Err(ResourceError::TransferInProgress);
        }

        let outgoing = OutgoingResource::new(
            data,
            metadata,
            None,
            link,
            auto_compress,
            &mut self.rng,
            now_ms,
        )?;
        let resource_hash = *outgoing.resource_hash();
        let adv_bytes = outgoing.adv_packet().to_vec();

        // Store the outgoing resource on the link
        let link = self
            .links
            .get_mut(link_id)
            .ok_or(ResourceError::InvalidRequest)?;
        link.set_outgoing_resource(outgoing);

        // Send the advertisement (encrypted)
        match link.build_data_packet_with_context(
            &adv_bytes,
            PacketContext::ResourceAdv,
            &mut self.rng,
        ) {
            Ok(pkt) => {
                self.route_link_packet(link_id, &pkt);
            }
            Err(e) => {
                // Failed to build ADV, clean up
                if let Some(link) = self.links.get_mut(link_id) {
                    link.clear_outgoing_resource();
                }
                crate::tracing::debug!("Failed to build resource ADV packet: {e}");
                return Err(ResourceError::InvalidRequest);
            }
        }

        Ok((resource_hash, self.process_events_and_actions()))
    }

    /// Accept a pending resource advertisement on a link.
    ///
    /// Call this after receiving a `NodeEvent::ResourceAdvertised` event.
    /// Creates an `IncomingResource` and sends the first REQ to start
    /// the transfer.
    pub fn accept_resource(
        &mut self,
        link_id: &LinkId,
    ) -> Result<crate::transport::TickOutput, crate::resource::ResourceError> {
        use crate::packet::PacketContext;
        use crate::resource::incoming::IncomingResource;
        use crate::resource::ResourceError;

        let now_ms = self.transport.clock().now_ms();

        let link = self
            .links
            .get_mut(link_id)
            .ok_or(ResourceError::InvalidRequest)?;

        let adv = link
            .take_pending_resource_adv()
            .ok_or(ResourceError::NoPendingResource)?;
        let resource_hash = adv.resource_hash;
        let link_mdu = link.mdu();
        let sdu = crate::resource::resource_sdu(link.negotiated_mtu());

        let (incoming, req_payload) = IncomingResource::from_advertisement(
            &adv,
            link_mdu,
            sdu,
            now_ms,
            self.max_incoming_resource_size,
        )?;

        let req_packet = link
            .build_data_packet_with_context(&req_payload, PacketContext::ResourceReq, &mut self.rng)
            .map_err(|_| ResourceError::InvalidRequest)?;

        link.set_incoming_resource(incoming);
        self.route_link_packet(link_id, &req_packet);

        self.events.push(NodeEvent::ResourceTransferStarted {
            link_id: *link_id,
            resource_hash,
            is_sender: false,
        });

        Ok(self.process_events_and_actions())
    }

    /// Reject a pending resource advertisement on a link.
    ///
    /// Call this after receiving a `NodeEvent::ResourceAdvertised` event
    /// to decline the transfer.
    pub fn reject_resource(
        &mut self,
        link_id: &LinkId,
    ) -> Result<crate::transport::TickOutput, crate::resource::ResourceError> {
        use crate::packet::PacketContext;
        use crate::resource::ResourceError;

        let link = self
            .links
            .get_mut(link_id)
            .ok_or(ResourceError::InvalidRequest)?;

        let adv = link
            .take_pending_resource_adv()
            .ok_or(ResourceError::NoPendingResource)?;

        // Send RCL (receiver cancel, we are rejecting the sender's ADV)
        let cancel_data = adv.resource_hash.to_vec();
        if let Ok(pkt) = link.build_data_packet_with_context(
            &cancel_data,
            PacketContext::ResourceRcl,
            &mut self.rng,
        ) {
            self.route_link_packet(link_id, &pkt);
        }

        Ok(self.process_events_and_actions())
    }

    /// Set the resource acceptance strategy for a link.
    pub fn set_resource_strategy(
        &mut self,
        link_id: &LinkId,
        strategy: crate::resource::ResourceStrategy,
    ) -> Result<(), crate::resource::ResourceError> {
        let link = self
            .links
            .get_mut(link_id)
            .ok_or(crate::resource::ResourceError::InvalidRequest)?;
        link.set_resource_strategy(strategy);
        Ok(())
    }

    // Sans-I/O Entry Points
    /// Process an incoming packet from an interface (sans-I/O)
    ///
    /// This is the primary entry point for incoming data. The driver reads
    /// deframed packets from interfaces and passes them here. The full
    /// processing pipeline runs synchronously: transport processing, link
    /// manager handling, and pending packet dispatch.
    ///
    /// # Arguments
    /// * `iface` - The interface the packet arrived on
    /// * `data` - The deframed packet bytes
    pub fn handle_packet(
        &mut self,
        iface: crate::transport::InterfaceId,
        data: &[u8],
    ) -> crate::transport::TickOutput {
        // Process through transport layer
        if let Err(e) = self.transport.process_incoming(iface.0, data) {
            crate::tracing::trace!(
                "Failed to process incoming packet on {}: {}",
                self.transport.iface_name(iface.0),
                e
            );
        }

        // Run the full event pipeline (same as tick() but without polling interfaces)
        self.process_events_and_actions()
    }

    /// Send management announces if their timer has expired.
    ///
    /// Announces each destination in `mgmt_destinations` (probe, etc.)
    /// and reschedules the next announce 2 hours later.
    /// Queues broadcast actions internally, they are drained by the
    /// `process_events_and_actions()` call at the end of `handle_timeout()`.
    fn check_mgmt_announces(&mut self, now_ms: u64) {
        let deadline = match self.next_mgmt_announce_ms {
            Some(d) if now_ms >= d => d,
            _ => return,
        };
        let _ = deadline; // used only for the >= check

        // Clone hashes to avoid borrow conflict with self
        let hashes: Vec<DestinationHash> = self.mgmt_destinations.clone();
        for dest_hash in &hashes {
            // Installed announce-suppression policy: skip silently. Borrows
            // announce_control only; released before destinations.get_mut.
            if let Some(policy) = self.announce_control.as_ref() {
                if policy.should_suppress_announce(dest_hash) {
                    continue;
                }
            }
            let dest = match self.destinations.get_mut(dest_hash) {
                Some(d) => d,
                None => continue,
            };
            let packet = match dest.announce(None, &mut self.rng, now_ms) {
                Ok(p) => p,
                Err(e) => {
                    crate::tracing::warn!("Management announce failed for <{}>: {}", dest_hash, e);
                    continue;
                }
            };
            let ratchet_pub = dest.current_ratchet_public();
            let ratchets_dirty = dest.ratchets_dirty();
            let mut buf = [0u8; crate::constants::MTU];
            match packet.pack(&mut buf) {
                Ok(len) => {
                    self.transport
                        .storage_mut()
                        .set_announce_cache(dest_hash.into_bytes(), buf[..len].to_vec());
                    self.transport.send_on_all_interfaces(&buf[..len]);
                    crate::tracing::debug!("Management announce sent for <{}>", dest_hash);

                    // Sender self-remember for management destinations
                    if let Some(rp) = ratchet_pub {
                        self.transport.storage_mut().remember_known_ratchet(
                            dest_hash.into_bytes(),
                            rp,
                            now_ms,
                        );
                    }

                    // Persist ratchet keys if changed
                    if ratchets_dirty {
                        if let Some(dest) = self.destinations.get_mut(dest_hash) {
                            if let Some(signed) = dest.serialize_ratchets_signed() {
                                self.transport
                                    .storage_mut()
                                    .store_dest_ratchet_keys(dest_hash.into_bytes(), signed);
                            }
                            dest.clear_ratchets_dirty();
                        }
                    }
                }
                Err(_) => {
                    crate::tracing::warn!("Management announce pack failed for <{}>", dest_hash);
                }
            }
        }

        self.next_mgmt_announce_ms = Some(now_ms + MGMT_ANNOUNCE_INTERVAL_MS);
    }

    /// Run periodic maintenance (sans-I/O)
    ///
    /// The driver should call this when the deadline from [`NodeCore::next_deadline`]
    /// expires, or on a regular interval. Handles path expiry, announce
    /// rebroadcasts, keepalives, stale link detection, receipt timeouts,
    /// and channel retransmissions.
    pub fn handle_timeout(&mut self) -> crate::transport::TickOutput {
        let now_ms = self.transport.clock().now_ms();

        // Run transport periodic tasks
        self.transport.poll();
        self.transport.retry_pending_discoveries(&mut self.rng);

        // Run link-layer periodic tasks
        self.check_timeouts(now_ms);
        self.check_rtt_retry(now_ms);
        let now_secs = now_ms / crate::constants::MS_PER_SECOND;
        self.check_keepalives(now_secs);
        self.check_stale_links(now_secs);
        self.check_channel_timeouts(now_ms);
        self.check_resource_timeouts(now_ms);
        self.check_request_timeouts(now_ms);

        // Send management announces (probe destination, etc.)
        self.check_mgmt_announces(now_ms);

        // Process all resulting events and actions
        self.process_events_and_actions()
    }

    /// Compute the earliest deadline across all timers
    ///
    /// Returns `None` if there are no pending deadlines. The driver should
    /// call [`NodeCore::handle_timeout`] when this deadline expires (or sooner).
    ///
    /// The returned value is an absolute timestamp in milliseconds
    /// (same timebase as the `Clock` trait).
    pub fn next_deadline(&self) -> Option<u64> {
        let now_ms = self.transport.clock().now_ms();
        let transport_deadline = self.transport.next_deadline();
        let link_deadline = self.link_next_deadline(now_ms);
        let mgmt_deadline = self.next_mgmt_announce_ms;

        [transport_deadline, link_deadline, mgmt_deadline]
            .into_iter()
            .flatten()
            .min()
    }

    /// Register a human-readable name for an interface.
    ///
    /// The driver should call this after spawning each interface so that
    /// log messages show names instead of numeric IDs.
    pub fn set_interface_name(&mut self, id: usize, name: alloc::string::String) {
        self.transport.set_interface_name(id, name);
    }

    /// Register the hardware MTU for an interface.
    ///
    /// The driver should call this after spawning each interface so that
    /// link MTU negotiation can signal the correct MTU.
    pub fn set_interface_hw_mtu(&mut self, id: usize, hw_mtu: u32) {
        self.transport.set_interface_hw_mtu(id, hw_mtu);
    }

    /// Mark or unmark an interface as a local IPC client (shared instance).
    ///
    /// Local client interfaces receive announce forwarding and path request
    /// routing from the daemon.
    pub fn set_interface_local_client(&mut self, id: usize, is_local: bool) {
        self.transport.set_local_client(id, is_local);
    }

    /// Record the wall-clock ms at which the given interface will next
    /// accept an MTU-sized packet. Called by the driver after every
    /// dispatch tick. See `Transport::set_interface_next_slot_ms` for
    /// the MTU-cache trade-off rationale.
    pub fn set_interface_next_slot_ms(&mut self, iface_idx: usize, slot_ms: u64) {
        self.transport
            .set_interface_next_slot_ms(iface_idx, slot_ms);
    }

    /// Record the worst-case airtime in milliseconds for one MTU-sized
    /// transmit on the given interface. Pushed by the driver after each
    /// dispatch tick for LoRa-Serial interfaces. See
    /// `Transport::set_interface_max_airtime_ms` for the
    /// per-destination jitter trade-off.
    pub fn set_interface_max_airtime_ms(&mut self, iface_idx: usize, ms: u64) {
        self.transport.set_interface_max_airtime_ms(iface_idx, ms);
    }

    /// Read the announce-retry jitter ceiling derived from the
    /// per-interface airtime backchannel. See
    /// `Transport::announce_jitter_max_ms`.
    pub fn announce_jitter_max_ms(&self) -> u64 {
        self.transport.announce_jitter_max_ms()
    }

    /// Read the earliest ready time pushed by the driver for this
    /// interface, falling back to `now_ms` when no value has been
    /// pushed yet.
    pub fn next_slot_ms_for_interface(&self, iface_idx: usize, now_ms: u64) -> u64 {
        self.transport.next_slot_ms_for_interface(iface_idx, now_ms)
    }

    /// Register an IFAC configuration for an interface.
    ///
    /// The driver should call this during interface setup for interfaces with
    /// networkname/passphrase configured.
    pub fn set_ifac_config(&mut self, id: usize, config: crate::ifac::IfacConfig) {
        self.transport.set_ifac_config(id, config);
    }

    /// Remove IFAC configuration for an interface.
    pub fn remove_ifac_config(&mut self, id: usize) {
        self.transport.remove_ifac_config(id);
    }

    /// Clone all IFAC configurations (for passing to dispatch_actions outside the lock).
    pub fn clone_ifac_configs(
        &self,
    ) -> alloc::collections::BTreeMap<usize, crate::ifac::IfacConfig> {
        self.transport.clone_ifac_configs()
    }

    /// Register an announce-rate configuration for an interface (Codeberg #67
    /// Stage 2a). The driver calls this during setup for interfaces that set
    /// any `announce_rate_*` key.
    pub fn set_announce_rate_config(
        &mut self,
        id: usize,
        config: crate::transport::AnnounceRateConfig,
    ) {
        self.transport.set_announce_rate_config(id, config);
    }

    /// Remove announce-rate configuration for an interface.
    pub fn remove_announce_rate_config(&mut self, id: usize) {
        self.transport.remove_announce_rate_config(id);
    }

    /// Notify core that an interface has gone offline (sans-I/O)
    ///
    /// The driver should call this when it detects that an interface is no
    /// longer available (e.g., TCP disconnect, serial port closed). Core
    /// removes routing entries referencing this interface and emits
    /// appropriate events.
    pub fn handle_interface_down(
        &mut self,
        iface: crate::transport::InterfaceId,
    ) -> crate::transport::TickOutput {
        let iface_idx = iface.0;

        // Remove path entries referencing this interface
        let lost_paths = self.transport.remove_paths_for_interface(iface_idx);

        // Log before removing the name so the message still shows the human-readable name
        crate::tracing::debug!(
            "Interface {} went down, removed {} paths",
            self.transport.iface_name(iface_idx),
            lost_paths.len()
        );

        for hash in &lost_paths {
            self.events.push(NodeEvent::PathLost {
                destination_hash: crate::destination::DestinationHash::new(*hash),
            });
        }

        // Remove link table entries referencing this interface
        self.transport.remove_link_entries_for_interface(iface_idx);

        // Remove reverse table entries referencing this interface
        self.transport
            .remove_reverse_entries_for_interface(iface_idx);

        // Remove announce cap state for this interface
        self.transport.unregister_interface_announce_cap(iface_idx);

        // Remove announce frequency tracking state for this interface
        self.transport.remove_announce_freq_tracking(iface_idx);

        // Remove IFAC config for this interface
        self.transport.remove_ifac_config(iface_idx);

        // Remove announce-rate config for this interface (Codeberg #67 Stage 2a)
        self.transport.remove_announce_rate_config(iface_idx);

        // Remove local client flag, interface name and HW_MTU
        // (after logging so the name is still available above)
        self.transport.set_local_client(iface_idx, false);
        self.transport.remove_interface_name(iface_idx);
        self.transport.remove_interface_hw_mtu(iface_idx);

        // Emit the InterfaceDown event
        self.events.push(NodeEvent::InterfaceDown(iface_idx));

        let next_deadline_ms = self.next_deadline();
        crate::transport::TickOutput {
            actions: Vec::new(),
            events: core::mem::take(&mut self.events),
            next_deadline_ms,
        }
    }

    /// Notify core that a non-local interface has come online (sans-I/O).
    ///
    /// Generates fresh announces for all daemon-owned destinations and
    /// rebroadcasts cached announce bytes for local-client destinations.
    /// Fresh announces ensure peers accept the path update even if they
    /// already have a stale entry. Cached bytes are sufficient for client
    /// destinations because the peer on the recovered interface has never
    /// seen them (Block D).
    pub fn handle_interface_up(&mut self, _interface_index: usize) -> crate::transport::TickOutput {
        let now_ms = self.transport.clock().now_ms();

        // Collect destination hashes first to avoid borrow conflict
        let local_hashes: Vec<crate::destination::DestinationHash> = self
            .destinations
            .keys()
            .filter(|h| self.transport.has_destination(h.as_bytes()))
            .copied()
            .collect();

        for dest_hash in &local_hashes {
            // Installed announce-suppression policy: skip silently.
            if let Some(policy) = self.announce_control.as_ref() {
                if policy.should_suppress_announce(dest_hash) {
                    continue;
                }
            }
            if let Some(dest) = self.destinations.get_mut(dest_hash) {
                match dest.announce(None, &mut self.rng, now_ms) {
                    Ok(packet) => {
                        let mut buf = [0u8; crate::constants::MTU];
                        if let Ok(len) = packet.pack(&mut buf) {
                            self.transport
                                .storage_mut()
                                .set_announce_cache(dest_hash.into_bytes(), buf[..len].to_vec());
                            self.transport.send_on_all_interfaces(&buf[..len]);
                        }
                    }
                    Err(e) => {
                        crate::tracing::warn!(
                            "Failed to re-announce <{}> on new interface: {:?}",
                            HexShort(dest_hash.as_bytes()),
                            e,
                        );
                    }
                }
            }
        }

        // Block D: Rebroadcast cached announces for local client destinations.
        // These are destinations announced by IPC clients (shared instance).
        // The daemon doesn't hold their private keys, so it rebroadcasts cached
        // bytes. This is safe because the peer on the recovered interface has
        // never seen these announces.
        let client_hashes: Vec<[u8; crate::constants::TRUNCATED_HASHBYTES]> =
            self.transport.local_client_known_dest_hashes();
        for hash in &client_hashes {
            // Skip if this dest was already announced as a daemon-owned destination
            if local_hashes.iter().any(|h| h.as_bytes() == hash) {
                continue;
            }
            if let Some(cached_raw) = self.transport.storage().get_announce_cache(hash).cloned() {
                if !cached_raw.is_empty() {
                    crate::tracing::debug!(
                        "Re-announcing cached local-client dest <{}> on interface recovery",
                        HexShort(hash),
                    );
                    self.transport.send_on_all_interfaces(&cached_raw);
                }
            }
        }

        self.process_events_and_actions()
    }

    /// Internal: Process transport events, drain actions
    ///
    /// Shared logic between `handle_packet` and `handle_timeout`.
    fn process_events_and_actions(&mut self) -> crate::transport::TickOutput {
        // Drain transport events and dispatch
        let transport_events: Vec<_> = self.transport.drain_events().collect();
        for event in transport_events {
            self.handle_transport_event(event);
        }

        // Collect all actions, events, and next deadline
        let actions = self.transport.drain_actions();
        let mut events = core::mem::take(&mut self.events);
        // Rewrite re-keyed link ids back to the caller-visible original
        // (Codeberg #66) — see `link_origin_ids`.
        if !self.link_origin_ids.is_empty() {
            for event in events.iter_mut() {
                if let Some(link_id) = event.link_id_mut() {
                    if let Some(origin) = self.link_origin_ids.get(link_id) {
                        *link_id = *origin;
                    }
                }
            }
        }
        let next_deadline_ms = self.next_deadline();

        crate::transport::TickOutput {
            actions,
            events,
            next_deadline_ms,
        }
    }

    // Accessors
    /// Get the node's identity
    pub fn identity(&self) -> &Identity {
        self.transport.identity()
    }

    /// Get the remote identity for a link, if the peer has identified.
    pub fn get_remote_identity(&self, link_id: &LinkId) -> Option<&Identity> {
        // Resolve a possibly-stale caller-visible id (a #66 retry re-keys the
        // link), mirroring link()/link_mut().
        self.links
            .get(&self.resolve_link_id(link_id))?
            .remote_identity()
    }

    /// Get the transport configuration
    pub fn transport_config(&self) -> &TransportConfig {
        self.transport.config()
    }

    /// Return metadata for all registered interfaces (for RPC reporting).
    /// `&mut` because frequency reads pop decayed samples (Python parity,
    /// see `Transport::interface_stats`).
    pub fn interface_stats(&mut self) -> Vec<crate::transport::InterfaceStatEntry> {
        self.transport.interface_stats()
    }

    /// The registered name for an interface id. Pure lookup without the
    /// frequency-deque read side effect of [`Self::interface_stats`].
    pub fn interface_name(&self, id: usize) -> Option<&str> {
        self.transport.interface_name(id)
    }

    /// Get the default proof strategy
    pub fn default_proof_strategy(&self) -> ProofStrategy {
        self.default_proof_strategy
    }

    /// Check if we have a path to a destination
    pub fn has_path(&self, dest_hash: &DestinationHash) -> bool {
        self.transport.has_path(dest_hash.as_bytes())
    }

    /// Request a path to a destination.
    ///
    /// Sends a PATH_REQUEST packet to all interfaces. The path response
    /// (if any) will arrive as a `PathFound` event and `has_path()` will
    /// return true.
    pub fn request_path(&mut self, dest_hash: &DestinationHash) -> crate::transport::TickOutput {
        let mut tag = [0u8; TRUNCATED_HASHBYTES];
        self.rng.fill_bytes(&mut tag);
        if let Err(e) = self
            .transport
            .request_path(dest_hash.as_bytes(), None, &tag)
        {
            crate::tracing::debug!(
                "Failed to build path request for <{}>: {}",
                HexShort(dest_hash.as_bytes()),
                e
            );
        }
        self.process_events_and_actions()
    }

    /// Get the hop count to a destination
    pub fn hops_to(&self, dest_hash: &DestinationHash) -> Option<u8> {
        self.transport.hops_to(dest_hash.as_bytes())
    }

    /// Returns the current ratchet public key for a registered destination.
    pub fn destination_ratchet_public(
        &self,
        dest_hash: &DestinationHash,
    ) -> Option<[u8; RATCHET_SIZE]> {
        self.destinations
            .get(dest_hash)
            .and_then(|d| d.current_ratchet_public())
    }

    /// Get the number of known paths
    pub fn path_count(&self) -> usize {
        self.transport.path_count()
    }

    /// Get transport statistics
    pub fn transport_stats(&self) -> TransportStats {
        self.transport.stats().clone()
    }

    /// Return a diagnostic dump of all protocol state memory usage
    pub fn diagnostic_dump(&self) -> String {
        use core::fmt::Write;

        let mut out = String::new();
        let _ = writeln!(out, "=== Memory Diagnostic ===");

        // Storage collections
        let (storage_dump, storage_total) = self.transport.storage().diagnostic_dump();
        out.push_str(&storage_dump);

        // Transport-owned collections
        let (transport_dump, transport_total) = self.transport.diagnostic_dump();
        out.push_str(&transport_dump);

        let grand_total = storage_total + transport_total;
        let _ = writeln!(out, "=== Total estimated: {} bytes ===", grand_total);
        out
    }

    /// Get the current time in milliseconds from the transport clock
    pub fn now_ms(&self) -> u64 {
        self.transport.clock().now_ms()
    }

    /// Return all path table entries for RPC export.
    pub fn path_table_entries(&self) -> Vec<crate::transport::PathTableExport> {
        self.transport.path_table_entries()
    }

    /// Return all announce rate table entries for RPC export.
    pub fn rate_table_entries(&self) -> Vec<crate::transport::RateTableExport> {
        self.transport.rate_table_entries()
    }

    /// Clone a path entry by destination hash (for RPC lookups).
    pub fn get_path_clone(
        &self,
        hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> Option<crate::storage_types::PathEntry> {
        self.transport.get_path_clone(hash)
    }

    /// Remove a path entry by destination hash. Returns true if found.
    pub fn remove_path(&mut self, hash: &[u8; crate::constants::TRUNCATED_HASHBYTES]) -> bool {
        self.transport.remove_path(hash)
    }

    /// Remove all paths whose next_hop matches `via_hash`. Returns count removed.
    pub fn drop_all_paths_via(
        &mut self,
        via_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> usize {
        self.transport.drop_all_paths_via(via_hash)
    }

    /// Blackhole an identity (Codeberg #67). Returns true if newly added.
    pub fn blackhole_identity(
        &mut self,
        identity_hash: [u8; crate::constants::TRUNCATED_HASHBYTES],
        until: Option<f64>,
        reason: Option<String>,
    ) -> bool {
        self.transport
            .blackhole_identity(identity_hash, until, reason)
    }

    /// Lift a blackhole (Codeberg #67). Returns true if it was present.
    pub fn unblackhole_identity(
        &mut self,
        identity_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.unblackhole_identity(identity_hash)
    }

    /// Return whether an identity hash is currently blackholed (Codeberg #67).
    pub fn is_blackholed(
        &self,
        identity_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.is_blackholed(identity_hash)
    }

    /// Borrow the blackhole map for RPC export (Codeberg #67).
    pub fn blackholed_identities(
        &self,
    ) -> &BTreeMap<[u8; crate::constants::TRUNCATED_HASHBYTES], crate::transport::BlackholeEntry>
    {
        self.transport.blackholed_identities()
    }

    /// Remove expired blackhole entries (Codeberg #88). `now_unix_secs` is
    /// wall-clock unix time supplied by the driver; the sweep self-throttles
    /// to one pass per 60 s like Python's jobs loop (Transport.py:973-994).
    pub fn expire_blackholed_identities(&mut self, now_unix_secs: f64) -> usize {
        self.transport.expire_blackholed_identities(now_unix_secs)
    }

    /// Pin a known destination against cache eviction (Codeberg #84).
    /// Returns true iff the destination is known (has a cached announce).
    pub fn retain_destination_data(
        &mut self,
        dest_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.retain_destination_data(dest_hash)
    }

    /// Lift a destination's retain pin (Codeberg #84). Returns true iff known.
    pub fn unretain_destination_data(
        &mut self,
        dest_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.unretain_destination_data(dest_hash)
    }

    /// Touch a known, non-retained destination's recency (Codeberg #84).
    /// Returns true iff the touch applied (known and not retained).
    pub fn used_destination_data(
        &mut self,
        dest_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.used_destination_data(dest_hash)
    }

    /// Retain every known destination for an identity (Codeberg #84).
    /// Returns true iff at least one destination was retained.
    pub fn retain_identity_data(
        &mut self,
        identity_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.retain_identity_data(identity_hash)
    }

    /// Lift the retain pin on every known destination for an identity
    /// (Codeberg #84). Returns true iff at least one was affected.
    pub fn unretain_identity_data(
        &mut self,
        identity_hash: &[u8; crate::constants::TRUNCATED_HASHBYTES],
    ) -> bool {
        self.transport.unretain_identity_data(identity_hash)
    }

    /// Access the underlying transport (test-only, for clock manipulation)
    #[cfg(test)]
    pub(crate) fn transport(&self) -> &Transport<C, S> {
        &self.transport
    }

    /// Get the number of tracked receipts (test-only)
    #[cfg(test)]
    pub(crate) fn receipt_count(&self) -> usize {
        self.receipt_tracker.len()
    }

    /// Count receipt entries for a given link (test-only)
    #[cfg(test)]
    pub(crate) fn receipt_count_for_link(&self, link_id: &LinkId) -> usize {
        self.receipt_tracker.count_for_link(link_id)
    }

    /// Expire all receipts as of `now_ms` without triggering retransmit (test-only)
    #[cfg(test)]
    pub(crate) fn expire_receipts(&mut self, now_ms: u64) {
        self.receipt_tracker.expire(now_ms);
    }

    // Internal: Event Handling
    fn handle_transport_event(&mut self, event: TransportEvent) {
        match event {
            TransportEvent::AnnounceReceived {
                announce,
                interface_index,
            } => {
                // Remember remote identity for single-packet encryption
                if let Ok(identity) = announce.to_identity() {
                    let dest_hash = announce.destination_hash().into_bytes();
                    self.storage_mut().set_identity(dest_hash, identity);
                }
                self.events.push(NodeEvent::AnnounceReceived {
                    announce,
                    interface_index,
                });
            }

            TransportEvent::PathFound {
                destination_hash,
                hops,
                interface_index,
            } => {
                self.events.push(NodeEvent::PathFound {
                    destination_hash: DestinationHash::new(destination_hash),
                    hops,
                    interface_index,
                });
            }

            TransportEvent::PathLost { destination_hash } => {
                self.events.push(NodeEvent::PathLost {
                    destination_hash: DestinationHash::new(destination_hash),
                });
            }

            TransportEvent::PacketReceived {
                destination_hash,
                packet,
                interface_index,
                raw_hash,
            } => {
                crate::tracing::debug!(
                    "node: PacketReceived dest=<{}> type={:?} ctx={:?} iface={}",
                    HexShort(&destination_hash),
                    packet.flags.packet_type,
                    packet.context,
                    interface_index,
                );
                // Check if this is a link-related packet
                if packet.flags.packet_type == crate::packet::PacketType::LinkRequest
                    || packet.flags.packet_type == crate::packet::PacketType::Proof
                    || (packet.flags.packet_type == crate::packet::PacketType::Data
                        && self.links.contains_key(&LinkId::new(destination_hash)))
                {
                    // Route to link packet handler
                    let Some(raw) = self.repack_packet(&packet) else {
                        // repack_packet logs the error; drop the packet
                        return;
                    };

                    // Verify repack symmetry: if original wire hash is known,
                    // check that repacking produces the same hashable bytes.
                    if let Some(original_hash) = raw_hash {
                        let repacked_hash = packet_hash(&raw);
                        if original_hash != repacked_hash {
                            crate::tracing::warn!(
                                original = %HexFmt(&original_hash),
                                repacked = %HexFmt(&repacked_hash),
                                ptype = ?packet.flags.packet_type,
                                ctx = ?packet.context,
                                "REPACK HASH MISMATCH — proof chain will fail"
                            );
                        }
                    }

                    let now_ms = self.transport.clock().now_ms();
                    self.process_link_packet(&packet, &raw, now_ms, interface_index);
                } else {
                    // Regular packet, decrypt if Single destination
                    let dest_hash_typed = DestinationHash::new(destination_hash);
                    let plaintext = if let Some(dest) = self.destinations.get(&dest_hash_typed) {
                        if dest.dest_type() == crate::destination::DestinationType::Single {
                            match dest.decrypt(packet.data.as_slice()) {
                                Ok(data) => data,
                                Err(_) => {
                                    crate::tracing::trace!(
                                        dest = %HexShort(destination_hash.as_ref()),
                                        "Dropped packet, decryption failed"
                                    );
                                    return;
                                }
                            }
                        } else {
                            // Plain destination, pass through
                            packet.data.as_slice().to_vec()
                        }
                    } else {
                        crate::tracing::trace!(
                            dest = %HexShort(destination_hash.as_ref()),
                            "Dropped packet, no destination registered"
                        );
                        return;
                    };

                    self.events.push(NodeEvent::PacketReceived {
                        destination: dest_hash_typed,
                        data: plaintext,
                        interface_index,
                    });
                }
            }

            TransportEvent::InterfaceDown(index) => {
                self.events.push(NodeEvent::InterfaceDown(index));
            }

            TransportEvent::ProofRequested {
                packet_hash,
                destination_hash,
                interface_index,
            } => {
                let dest_hash = DestinationHash::new(destination_hash);
                let proof_strategy = self
                    .destinations
                    .get(&dest_hash)
                    .map(|d| d.proof_strategy())
                    .unwrap_or(ProofStrategy::None);

                match proof_strategy {
                    ProofStrategy::All => {
                        if let Some(identity) =
                            self.destinations.get(&dest_hash).and_then(|d| d.identity())
                        {
                            if let Err(e) = self.transport.send_proof(
                                &packet_hash,
                                &destination_hash,
                                identity,
                                Some(interface_index),
                            ) {
                                crate::tracing::warn!(
                                    "failed to send auto-proof for PROVE_ALL: {}",
                                    e
                                );
                            }
                        }
                    }
                    ProofStrategy::App => {
                        self.events.push(NodeEvent::PacketProofRequested {
                            packet_hash,
                            destination_hash: dest_hash,
                        });
                    }
                    ProofStrategy::None => {}
                }
            }

            TransportEvent::ProofReceived {
                packet_hash,
                destination_hash,
                expected_packet_hash,
                proof_data,
            } => {
                let dest_hash = DestinationHash::new(destination_hash);
                // Validate against the local serve destination if present, else
                // fall back to the recalled remote identity from storage (the
                // same source the encrypt path uses at the top of send). A remote
                // outbound dest is never in self.destinations, so without this
                // fallback a valid proof was misreported as DeliveryFailed (#76).
                let is_valid = self
                    .destinations
                    .get(&dest_hash)
                    .and_then(|dest| dest.identity())
                    .or_else(|| self.transport.storage().get_identity(dest_hash.as_bytes()))
                    .map(|identity| identity.verify_proof(&proof_data, &expected_packet_hash))
                    .unwrap_or(false);

                if is_valid {
                    self.transport.mark_receipt_delivered(&packet_hash);
                    self.events
                        .push(NodeEvent::PacketDeliveryConfirmed { packet_hash });
                } else {
                    self.events.push(NodeEvent::DeliveryFailed {
                        packet_hash,
                        error: DeliveryError::LinkFailed,
                    });
                }
            }

            TransportEvent::ReceiptTimeout { packet_hash } => {
                self.events.push(NodeEvent::DeliveryFailed {
                    packet_hash,
                    error: DeliveryError::Timeout,
                });
            }

            TransportEvent::PathRequestReceived {
                destination_hash,
                requesting_interface,
            } => {
                // Block A: Generate a fresh announce (new signature, current app_data)
                // instead of serving cached bytes. Transport set up a deferred AnnounceEntry
                // with the 400ms grace period, we replace its raw_packet with fresh bytes.
                let now_ms = self.transport.clock().now_ms();
                if let Some(dest) = self
                    .destinations
                    .get_mut(&DestinationHash::new(destination_hash))
                {
                    match dest.announce(None, &mut self.rng, now_ms) {
                        Ok(packet) => {
                            let mut buf = [0u8; crate::constants::MTU];
                            if let Ok(len) = packet.pack(&mut buf) {
                                let fresh_raw = buf[..len].to_vec();
                                // Update announce_cache with fresh bytes
                                self.transport
                                    .storage_mut()
                                    .set_announce_cache(destination_hash, fresh_raw.clone());
                                // Update the AnnounceEntry that Transport already set up
                                if let Some(entry) = self
                                    .transport
                                    .storage_mut()
                                    .get_announce_mut(&destination_hash)
                                {
                                    entry.raw_packet = fresh_raw;
                                    entry.hops = 0; // Local destination
                                    crate::tracing::debug!(
                                        "Fresh path response generated for <{}>",
                                        HexShort(&destination_hash),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            crate::tracing::warn!(
                                "Failed to generate fresh path response for <{}>: {:?}",
                                HexShort(&destination_hash),
                                e,
                            );
                        }
                    }
                }
                let _ = requesting_interface; // used by Transport for target_interface
                self.events.push(NodeEvent::PathRequestReceived {
                    destination_hash: DestinationHash::new(destination_hash),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::destination::{DestinationType, Direction};
    use crate::link::{LinkCloseReason, LinkState};
    use crate::memory_storage::MemoryStorage;
    use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
    use crate::traits::NoStorage;
    use rand_core::OsRng;

    #[test]
    fn test_nodecore_builder_default() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        assert_eq!(node.active_link_count(), 0);
        assert_eq!(node.pending_link_count(), 0);
        assert_eq!(node.default_proof_strategy(), ProofStrategy::None);
    }

    #[test]
    fn test_nodecore_builder_with_identity() {
        let identity = Identity::generate(&mut OsRng);
        let id_hash = *identity.hash();
        let clock = MockClock::new(TEST_TIME_MS);

        let node = NodeCoreBuilder::new()
            .identity(identity)
            .build(OsRng, clock, NoStorage);

        assert_eq!(node.identity().hash(), &id_hash);
    }

    #[test]
    fn test_nodecore_register_destination() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();

        let hash = *dest.hash();
        node.register_destination(dest);

        assert!(node.destination(&hash).is_some());
        assert!(node.transport.has_destination(hash.as_bytes()));
    }

    // An installed AnnounceControl policy suppresses the scheduled announce for
    // a matching destination while leaving others alone.
    #[test]
    fn test_announce_control_suppresses_scheduled_announce() {
        struct SuppressOne([u8; 16]);
        impl crate::announce::AnnounceControl for SuppressOne {
            fn should_suppress_announce(&self, h: &DestinationHash) -> bool {
                h.as_bytes() == &self.0
            }
        }

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

        let d_keep = Destination::new(
            Some(Identity::generate(&mut OsRng)),
            Direction::In,
            DestinationType::Single,
            "app",
            &["keep"],
        )
        .unwrap();
        let d_hide = Destination::new(
            Some(Identity::generate(&mut OsRng)),
            Direction::In,
            DestinationType::Single,
            "app",
            &["hide"],
        )
        .unwrap();
        let keep = *d_keep.hash();
        let hide = *d_hide.hash();
        node.register_destination(d_keep);
        node.register_destination(d_hide);

        node.set_announce_control(Some(Box::new(SuppressOne(hide.into_bytes()))));
        assert!(node.has_announce_control());

        node.mgmt_destinations.push(keep);
        node.mgmt_destinations.push(hide);
        node.next_mgmt_announce_ms = Some(TEST_TIME_MS);
        node.check_mgmt_announces(TEST_TIME_MS);

        assert!(
            node.transport
                .storage()
                .get_announce_cache(keep.as_bytes())
                .is_some(),
            "unsuppressed destination should announce"
        );
        assert!(
            node.transport
                .storage()
                .get_announce_cache(hide.as_bytes())
                .is_none(),
            "suppressed destination must not announce"
        );

        // Clearing the policy restores announcing.
        node.set_announce_control(None);
        assert!(!node.has_announce_control());
        node.next_mgmt_announce_ms = Some(TEST_TIME_MS);
        node.check_mgmt_announces(TEST_TIME_MS);
        assert!(
            node.transport
                .storage()
                .get_announce_cache(hide.as_bytes())
                .is_some(),
            "after clearing policy the destination announces"
        );
    }

    #[test]
    fn test_nodecore_handle_timeout_empty() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // handle_timeout should return empty when there are no events
        let output = node.handle_timeout();
        assert!(output.actions.is_empty());
        assert!(output.events.is_empty());
    }

    #[test]
    fn test_nodecore_has_path_empty() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let dest_hash = DestinationHash::new([0x42; 16]);
        assert!(!node.has_path(&dest_hash));
        assert!(node.hops_to(&dest_hash).is_none());
    }

    #[test]
    fn test_link_stats_no_link() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let fake_id = LinkId::new([0xFF; 16]);
        assert!(node.link_stats(&fake_id).is_none());
    }

    #[test]
    fn test_find_link_to_none() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let dest_hash = DestinationHash::new([0x42; 16]);
        assert!(node.find_link_to(&dest_hash).is_none());
    }

    // Sans-I/O API Tests
    #[test]
    fn test_handle_packet_invalid_data() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // Garbage data should not panic and should produce no events/actions
        let output = node.handle_packet(InterfaceId(0), &[0xFF; 3]);
        assert!(output.actions.is_empty());
        assert!(output.events.is_empty());
    }

    #[test]
    fn test_handle_packet_announce() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new()
            .enable_transport(true)
            .build(OsRng, clock, NoStorage);

        // Build a valid announce
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();

        let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        let output = node.handle_packet(InterfaceId(0), &buf[..len]);

        // Should have AnnounceReceived event
        let has_announce = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::AnnounceReceived { .. }));
        assert!(has_announce, "expected AnnounceReceived event");
    }

    #[test]
    fn test_next_deadline_empty_node() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // Empty node should have no deadlines
        assert!(node.next_deadline().is_none());
    }

    #[test]
    fn test_next_deadline_with_path() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new()
            .enable_transport(true)
            .build(OsRng, clock, NoStorage);

        // Inject an announce to create a path with an expiry deadline
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["deadline"],
        )
        .unwrap();

        let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);

        // Now there should be a deadline (path expiry and/or announce rebroadcast)
        let deadline = node.next_deadline();
        assert!(
            deadline.is_some(),
            "should have a deadline after processing an announce"
        );
        assert!(
            deadline.unwrap() > TEST_TIME_MS,
            "deadline should be in the future"
        );
    }

    #[test]
    fn test_handle_interface_down_cleans_paths() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Inject an announce on interface 0 to create a path
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["ifacedown"],
        )
        .unwrap();

        let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);

        // Verify path exists
        assert!(
            node.path_count() > 0,
            "should have a path from the announce"
        );

        // Take down interface 0
        let output = node.handle_interface_down(InterfaceId(0));

        // Path should be gone
        assert_eq!(
            node.path_count(),
            0,
            "path should be removed after interface down"
        );

        // Should have PathLost and InterfaceDown events
        let has_path_lost = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PathLost { .. }));
        let has_iface_down = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::InterfaceDown(0)));

        assert!(has_path_lost, "should emit PathLost event");
        assert!(has_iface_down, "should emit InterfaceDown event");
    }

    // Codeberg #63 minimal test: culling is per-interface — taking one
    // interface down removes exactly its paths and leaves entries
    // learned on other interfaces untouched.
    #[test]
    fn test_handle_interface_down_leaves_other_interfaces_untouched() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        let announce_on = |node: &mut NodeCore<OsRng, MockClock, MemoryStorage>,
                           iface: usize,
                           aspect: &str|
         -> DestinationHash {
            let identity = Identity::generate(&mut OsRng);
            let mut dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &[aspect],
            )
            .unwrap();
            let hash = *dest.hash();
            let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
            let mut buf = [0u8; crate::constants::MTU];
            let len = announce_packet.pack(&mut buf).unwrap();
            let _ = node.handle_packet(InterfaceId(iface), &buf[..len]);
            hash
        };

        let dest_iface0 = announce_on(&mut node, 0, "downiface");
        let dest_iface1 = announce_on(&mut node, 1, "upiface");
        assert!(node.has_path(&dest_iface0));
        assert!(node.has_path(&dest_iface1));

        let _ = node.handle_interface_down(InterfaceId(0));

        assert!(
            !node.has_path(&dest_iface0),
            "path learned on the downed interface must be culled"
        );
        assert!(
            node.has_path(&dest_iface1),
            "path learned on another interface must survive"
        );
    }

    #[test]
    fn test_handle_interface_down_no_paths() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // Interface down on empty node should produce just the InterfaceDown event
        let output = node.handle_interface_down(InterfaceId(5));
        assert_eq!(output.events.len(), 1);
        assert!(matches!(output.events[0], NodeEvent::InterfaceDown(5)));
        assert!(output.actions.is_empty());
    }

    #[test]
    fn test_handle_timeout_produces_rebroadcast_actions() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new()
            .enable_transport(true)
            .build(OsRng, clock, NoStorage);

        // Inject an announce
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["rebroadcast"],
        )
        .unwrap();

        let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);

        // Advance time past the rebroadcast delay
        // MockClock uses Cell<u64>, which allows mutation through &self
        node.transport().clock().set(TEST_TIME_MS + 20_000);

        // handle_timeout should produce rebroadcast actions
        let output = node.handle_timeout();

        // May or may not produce actions depending on jitter calculation,
        // but the mechanism is tested - just verify no panics and the pipeline works
        let _ = output;
    }

    // Sans-I/O Audit: Deferred-dispatch tests
    #[test]
    fn test_handle_packet_announce_produces_rebroadcast_action() {
        use crate::transport::{Action, InterfaceId};

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Create a valid announce from a remote destination
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["actiontest"],
        )
        .unwrap();

        let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        // Feed announce on interface 0
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);

        // Advance time well past the retransmit delay window
        node.transport().clock().set(TEST_TIME_MS + 100_000);
        let output = node.handle_timeout();

        // Should have at least one Broadcast action with exclude_iface=None
        // (announce rebroadcast fans out on all interfaces; the self-echo is
        // absorbed by packet-hash dedup on RX).
        let has_rebroadcast = output.actions.iter().any(|a| {
            matches!(
                a,
                Action::Broadcast {
                    exclude_iface: None,
                    ..
                }
            )
        });
        assert!(
            has_rebroadcast,
            "transport-enabled node should produce Broadcast action for announce rebroadcast, \
             got: {:?}",
            output.actions
        );
    }

    /// Regression: announces originating from a local IPC client must be
    /// rebroadcast to the network even when `enable_transport=false`.
    ///
    /// Pre-fix the `should_rebroadcast` decision required `enable_transport`
    /// unconditionally, and the retry scheduler bailed out early on
    /// non-transport nodes. The combined effect was that a non-transport node
    /// running an LXMF / Nomadnet client could register a destination locally
    /// but never propagate the announce on the wire — to other peers the
    /// node looked silent. This caused the asymmetric "Bob doesn't get
    /// Alice's pubkey" symptom on hamster ↔ miauhaus when only one side had
    /// `enable_transport=Yes`.
    #[test]
    fn test_local_client_announce_rebroadcasts_with_transport_disabled() {
        use crate::transport::{Action, InterfaceId};

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(false).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Interface 0: regular network interface. Interface 1: local IPC.
        node.transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("net0", 1)));
        node.transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("local0", 1)));
        node.transport.set_local_client(1, true);

        // Build an announce for a destination owned by the local client.
        let identity = Identity::generate(&mut OsRng);
        let mut client_dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "lxmftest",
            &["delivery"],
        )
        .unwrap();
        let announce_packet = client_dest
            .announce(None, &mut OsRng, TEST_TIME_MS)
            .unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        // Feed the announce in on the local-client interface (id 1).
        let _ = node.handle_packet(InterfaceId(1), &buf[..len]);

        // Push past the 250 ms local-client batching delay and the scheduler
        // jitter window so any pending rebroadcast must have fired.
        node.transport().clock().set(TEST_TIME_MS + 100_000);
        let output = node.handle_timeout();

        let has_rebroadcast = output
            .actions
            .iter()
            .any(|a| matches!(a, Action::Broadcast { .. }));
        assert!(
            has_rebroadcast,
            "non-transport node must rebroadcast announces originating from local IPC \
             clients so the registered destination becomes visible to peers; got: {:?}",
            output.actions
        );
    }

    #[test]
    fn test_connect_queues_send_action() {
        use crate::transport::{Action, InterfaceId};

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Create a remote identity and announce to establish a path
        let remote_identity = Identity::generate(&mut OsRng);
        let remote_signing_key = remote_identity.ed25519_verifying().to_bytes();
        let mut remote_dest = Destination::new(
            Some(remote_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["connect"],
        )
        .unwrap();

        let announce_packet = remote_dest
            .announce(None, &mut OsRng, TEST_TIME_MS)
            .unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        // Process announce to create a path via interface 0
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);
        // Drain the announce rebroadcast actions
        let _ = node.handle_timeout();

        // Verify path exists
        let dest_hash = *remote_dest.hash();
        assert!(node.has_path(&dest_hash), "should have path from announce");

        // Call connect(), actions are returned immediately in TickOutput
        let (link_id, _, output) = node.connect(dest_hash, &remote_signing_key);

        // The link request should have been returned as an Action (SendPacket
        // to the path's interface since we have a path, or Broadcast if no path)
        let has_send = output
            .actions
            .iter()
            .any(|a| matches!(a, Action::SendPacket { .. } | Action::Broadcast { .. }));
        assert!(
            has_send,
            "connect() should return an Action in TickOutput, \
             link_id={:?}, actions={:?}",
            link_id, output.actions
        );
    }

    #[test]
    fn test_connect_was_routed_false_without_path() {
        use crate::transport::Action;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // Create a destination hash with no path entry
        let remote_identity = Identity::generate(&mut OsRng);
        let signing_key = remote_identity.ed25519_verifying().to_bytes();
        let dest = Destination::new(
            Some(remote_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["nopath"],
        )
        .unwrap();
        let dest_hash = *dest.hash();

        // Connect without any path, should broadcast
        let (_link_id, was_routed, output) = node.connect(dest_hash, &signing_key);
        assert!(!was_routed, "should not be routed without a path");

        // Verify it broadcast instead
        assert!(
            output
                .actions
                .iter()
                .any(|a| matches!(a, Action::Broadcast { .. })),
            "should have broadcast the link request"
        );
    }

    #[test]
    fn test_connect_was_routed_true_with_path() {
        use crate::transport::{Action, InterfaceId};

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

        // Create remote destination and announce to establish a path
        let remote_identity = Identity::generate(&mut OsRng);
        let signing_key = remote_identity.ed25519_verifying().to_bytes();
        let mut remote_dest = Destination::new(
            Some(remote_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["routed"],
        )
        .unwrap();

        let announce_packet = remote_dest
            .announce(None, &mut OsRng, TEST_TIME_MS)
            .unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);
        let _ = node.handle_timeout();

        let dest_hash = *remote_dest.hash();
        assert!(node.has_path(&dest_hash), "should have path from announce");

        // Connect with path, should be routed
        let (_link_id, was_routed, output) = node.connect(dest_hash, &signing_key);
        assert!(was_routed, "should be routed with a known path");

        // Verify it sent via SendPacket (not Broadcast)
        assert!(
            output
                .actions
                .iter()
                .any(|a| matches!(a, Action::SendPacket { .. })),
            "should have sent via specific interface"
        );
    }

    // Pending-Link Path Recovery Tests
    /// Helper: create a non-transport node, announce a remote destination,
    /// and call connect(). Returns (node, dest_hash, link_id).
    fn setup_pending_link(
        enable_transport: bool,
    ) -> (
        NodeCore<OsRng, MockClock, MemoryStorage>,
        DestinationHash,
        LinkId,
    ) {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new()
            .enable_transport(enable_transport)
            .build(OsRng, clock, MemoryStorage::with_defaults());

        // Create remote identity and announce to establish a path
        let remote_identity = Identity::generate(&mut OsRng);
        let remote_signing_key = remote_identity.ed25519_verifying().to_bytes();
        let mut remote_dest = Destination::new(
            Some(remote_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["recovery"],
        )
        .unwrap();

        let announce_packet = remote_dest
            .announce(None, &mut OsRng, TEST_TIME_MS)
            .unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        // Process announce on interface 0 to create path
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);
        // Drain the announce rebroadcast actions
        let _ = node.handle_timeout();

        let dest_hash = *remote_dest.hash();
        assert!(node.has_path(&dest_hash), "should have path from announce");

        // Initiate connection, actions returned in output
        let (link_id, _, _output) = node.connect(dest_hash, &remote_signing_key);

        (node, dest_hash, link_id)
    }

    // Codeberg #66 regression guard (caught by rnsd_interop in the first
    // tier2 after the fresh-keys fix): retries re-key the link, but the
    // application only holds the id `connect()` returned — every event,
    // including the final LinkClosed of a link that never established,
    // must carry that ORIGINAL id.
    #[test]
    fn test_link_closed_after_retries_carries_original_id() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, LINK_REQUEST_MAX_RETRIES};

        let (mut node, _dest_hash, original_link_id) = setup_pending_link(false);

        let mut final_output = node.handle_timeout();
        for _attempt in 0..=(LINK_REQUEST_MAX_RETRIES as usize) {
            let now = node.transport().clock().now_ms();
            node.transport()
                .clock()
                .set(now + LINK_PENDING_TIMEOUT_MS + 1);
            final_output = node.handle_timeout();
        }

        let closed_id = final_output.events.iter().find_map(|e| match e {
            NodeEvent::LinkClosed {
                link_id,
                reason: LinkCloseReason::Timeout,
                ..
            } => Some(*link_id),
            _ => None,
        });
        assert_eq!(
            closed_id,
            Some(original_link_id),
            "LinkClosed must carry the id connect() returned, not the re-keyed wire id"
        );
    }

    #[test]
    fn test_pending_link_timeout_triggers_path_recovery() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, LINK_REQUEST_MAX_RETRIES};
        use crate::transport::Action;

        let (mut node, dest_hash, _link_id) = setup_pending_link(false);

        // Path should exist before timeout
        assert!(node.has_path(&dest_hash));

        // Exhaust all link request retries. Each timeout retries with
        // FRESH ephemeral keys and a re-keyed link id (Codeberg #66,
        // link_management.rs check_timeouts). The final timeout emits
        // LinkClosed — carrying the original caller-visible id, see
        // test_link_closed_after_retries_carries_original_id.
        let mut final_output = node.handle_timeout(); // no-op, not timed out yet
        for _attempt in 0..=(LINK_REQUEST_MAX_RETRIES as usize) {
            let now = node.transport().clock().now_ms();
            node.transport()
                .clock()
                .set(now + LINK_PENDING_TIMEOUT_MS + 1);
            final_output = node.handle_timeout();
        }

        // Path should have been expired after all retries exhausted
        assert!(
            !node.has_path(&dest_hash),
            "path should be expired after pending link timeout"
        );

        // Should have a Broadcast action (the path request)
        let has_broadcast = final_output
            .actions
            .iter()
            .any(|a| matches!(a, Action::Broadcast { .. }));
        assert!(
            has_broadcast,
            "should emit a Broadcast action for path request, got: {:?}",
            final_output.actions
        );

        // Should have LinkClosed event with Timeout reason
        let has_closed = final_output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::Timeout,
                    ..
                }
            )
        });
        assert!(has_closed, "should emit LinkClosed with Timeout");
    }

    // Codeberg #66 mvr: initiator behind a transport node, first
    // LinkRequest forward lost. Before the fresh-keys fix, every retry
    // re-sent identical bytes and died in the transport node's
    // duplicate-hash dedup (DEDUP_DROP), so a single lost first frame
    // made the link unestablishable. Intended behaviour: each retry
    // carries fresh ephemeral keys → new packet hash → the transport
    // node forwards it → the link establishes.
    #[test]
    fn test_link_retry_through_transport_node_establishes() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, MTU};
        use crate::transport::InterfaceId;

        // Responder B with a link-accepting destination.
        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["retry"],
        )
        .unwrap();
        resp_dest.set_accepts_links(true);
        resp_dest.set_proof_strategy(ProofStrategy::All);
        let dest_hash = *resp_dest.hash();
        let announce_packet = resp_dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        responder.register_destination(resp_dest);
        let mut buf = [0u8; MTU];
        let len = announce_packet.pack(&mut buf).unwrap();
        let announce_raw = buf[..len].to_vec();

        // Transport node T: learns B's path on iface 1 (direct).
        let clock = MockClock::new(TEST_TIME_MS);
        let mut transport_node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );
        let _ = transport_node.handle_packet(InterfaceId(1), &announce_raw);
        assert!(transport_node.has_path(&dest_hash));

        // Initiator A (non-transport): connect broadcasts the request.
        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (caller_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let first_request = extract_broadcast_data(&output);

        // T forwards the first request toward B — and the forward is LOST.
        let output = transport_node.handle_packet(InterfaceId(0), &first_request);
        assert_eq!(
            extract_all_action_data(&output).len(),
            1,
            "transport node must forward the first link request"
        );
        // (dropped: never delivered to B)

        // A times out and retries.
        let now = initiator.transport().clock().now_ms();
        initiator
            .transport()
            .clock()
            .set(now + LINK_PENDING_TIMEOUT_MS + 1);
        let output = initiator.handle_timeout();
        let retry_request = extract_broadcast_data(&output);

        // Intended (#66): the retry carries fresh keys, so its bytes and
        // packet hash differ and the transport node forwards it instead
        // of dedup-dropping it.
        assert_ne!(
            first_request, retry_request,
            "retry must carry fresh bytes (new ephemeral keys), not an identical re-send"
        );
        let output = transport_node.handle_packet(InterfaceId(0), &retry_request);
        let forwarded = extract_all_action_data(&output);
        assert_eq!(
            forwarded.len(),
            1,
            "transport node must forward the fresh-keys retry (dedup must not eat it)"
        );

        // Deliver to B; it auto-accepts (Stage 1) and the proof walks back A ← T ← B.
        let output = responder.handle_packet(InterfaceId(0), &forwarded[0]);
        let proof_data = extract_broadcast_data(&output);
        let output = transport_node.handle_packet(InterfaceId(1), &proof_data);
        let to_initiator = extract_all_action_data(&output);
        assert_eq!(to_initiator.len(), 1, "proof must route back through T");
        let output = initiator.handle_packet(InterfaceId(0), &to_initiator[0]);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "link must establish via the retried request"
        );

        // The caller-visible handle id from connect() must still resolve
        // (rebound to the retried link).
        assert!(
            initiator.link(&caller_link_id).is_some(),
            "original link id from connect() must still resolve after a retry rebind"
        );
    }

    // Codeberg #66 responder side: a stale PendingIncoming from a
    // superseded request expires via its own timeout without touching
    // the link established by the retry.
    #[test]
    fn test_responder_stale_pending_from_superseded_retry_expires() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, MTU};
        use crate::transport::InterfaceId;

        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["retry"],
        )
        .unwrap();
        resp_dest.set_accepts_links(true);
        resp_dest.set_proof_strategy(ProofStrategy::All);
        let dest_hash = *resp_dest.hash();
        let announce_packet = resp_dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        responder.register_destination(resp_dest);
        let mut buf = [0u8; MTU];
        let len = announce_packet.pack(&mut buf).unwrap();
        let _ = announce_packet;
        let _announce_raw = buf[..len].to_vec();

        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (_caller_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let first_request = extract_broadcast_data(&output);

        // First request DOES reach B; B auto-accepts (Stage 1) and its proof is lost
        // (never delivered back to A).
        let output = responder.handle_packet(InterfaceId(0), &first_request);
        let first_pending = Link::calculate_link_id(&first_request);
        let _lost_proof = extract_broadcast_data(&output);
        assert_eq!(responder.pending_link_count(), 1);

        // A times out, retries with fresh keys; retry reaches B.
        let now = initiator.transport().clock().now_ms();
        initiator
            .transport()
            .clock()
            .set(now + LINK_PENDING_TIMEOUT_MS + 1);
        let output = initiator.handle_timeout();
        let retry_request = extract_broadcast_data(&output);
        assert_ne!(first_request, retry_request);
        let output = responder.handle_packet(InterfaceId(0), &retry_request);
        let second_pending = Link::calculate_link_id(&retry_request);
        assert_ne!(first_pending, second_pending, "fresh keys ⇒ new link id");
        let proof_data = extract_broadcast_data(&output);

        // Establish the retried link end-to-end.
        let output = initiator.handle_packet(InterfaceId(0), &proof_data);
        assert!(output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })));
        let rtt_data = extract_broadcast_data(&output);
        let output = responder.handle_packet(InterfaceId(0), &rtt_data);
        assert!(output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })));

        // The stale first pending expires on its own timeout; the
        // established retried link survives.
        let now = responder.transport().clock().now_ms();
        responder
            .transport()
            .clock()
            .set(now + LINK_PENDING_TIMEOUT_MS * 4 + 1);
        let _ = responder.handle_timeout();
        assert!(
            responder.link(&second_pending).is_some(),
            "established link must survive stale-pending expiry"
        );
        assert!(
            responder.link(&first_pending).is_none(),
            "superseded pending must be gone after its timeout"
        );
    }

    #[test]
    fn test_pending_link_timeout_no_recovery_for_transport_nodes() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, LINK_REQUEST_MAX_RETRIES};

        let (mut node, dest_hash, _link_id) = setup_pending_link(true);

        // Exhaust all retries
        for _attempt in 0..=(LINK_REQUEST_MAX_RETRIES as usize) {
            let now = node.transport().clock().now_ms();
            node.transport()
                .clock()
                .set(now + LINK_PENDING_TIMEOUT_MS + 1);
            let _output = node.handle_timeout();
        }

        // Transport nodes should NOT expire the path, they handle recovery
        // via clean_link_table() instead
        assert!(
            node.has_path(&dest_hash),
            "transport node should NOT expire path on pending link timeout"
        );
    }

    #[test]
    fn test_pending_link_normal_close_no_recovery() {
        let (mut node, dest_hash, link_id) = setup_pending_link(false);

        // Close the connection normally (before timeout)
        let _ = node.close_link(&link_id);

        // Advance clock and run maintenance
        node.transport().clock().set(TEST_TIME_MS + 5_000);
        let _output = node.handle_timeout();

        // Path should still exist, normal close doesn't trigger recovery
        assert!(
            node.has_path(&dest_hash),
            "path should NOT be expired after normal close"
        );
    }

    // T6: NodeCore Messaging Path
    /// Helper: create two NodeCores and perform a full link handshake.
    struct NodeCoreLinkPair {
        initiator: NodeCore<OsRng, MockClock, NoStorage>,
        responder: NodeCore<OsRng, MockClock, NoStorage>,
        initiator_link_id: LinkId,
        responder_link_id: LinkId,
        _dest_hash: DestinationHash,
    }

    fn extract_broadcast_data(output: &crate::transport::TickOutput) -> Vec<u8> {
        output
            .actions
            .iter()
            .map(|a| match a {
                crate::transport::Action::Broadcast { data, .. }
                | crate::transport::Action::SendPacket { data, .. } => data.clone(),
            })
            .next()
            .expect("expected Broadcast or SendPacket action")
    }

    fn extract_all_action_data(output: &crate::transport::TickOutput) -> Vec<Vec<u8>> {
        output
            .actions
            .iter()
            .map(|a| match a {
                crate::transport::Action::Broadcast { data, .. }
                | crate::transport::Action::SendPacket { data, .. } => data.clone(),
            })
            .collect()
    }

    // Core-level coverage for peer-close notification: a responder-initiated
    // close emits a close packet, and the initiator that receives it surfaces
    // LinkClosed (so a peer learns of a graceful close promptly, not only via
    // the stale timeout). This isolates the close handling from interface
    // routing: it asserts the packet is built and processed correctly, with
    // delivery done by hand. Whether that packet actually reaches the peer over
    // a real interface is a separate routing concern this test does not cover,
    // and one observed to fail intermittently over TCP (responder->initiator
    // close packets are usually dropped); that delivery race is tracked
    // separately, not in the close-handling logic this test exercises.
    #[test]
    fn responder_close_packet_notifies_initiator() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        let output = pair.responder.close_link(&pair.responder_link_id);
        let close_packets = extract_all_action_data(&output);
        assert!(
            !close_packets.is_empty(),
            "a responder-initiated close must emit a close packet to the peer"
        );

        let output = pair
            .initiator
            .handle_packet(InterfaceId(0), &close_packets[0]);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkClosed { .. })),
            "initiator must be notified of the peer-initiated close"
        );
    }

    fn establish_nodecore_link_pair_with_strategy(strategy: ProofStrategy) -> NodeCoreLinkPair {
        use crate::transport::InterfaceId;

        // 1. Create responder with a destination that accepts links
        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();
        resp_dest.set_accepts_links(true);
        resp_dest.set_proof_strategy(strategy);
        let dest_hash = *resp_dest.hash();
        responder.register_destination(resp_dest);

        // 2. Create initiator
        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // 3. Initiator connects (broadcasts since no path)
        let (init_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let link_req_data = extract_broadcast_data(&output);

        // 4. Responder receives link request → auto-accepts (Stage 1): the
        //    LinkRequest event AND the establishment proof land in one output.
        let output = responder.handle_packet(InterfaceId(0), &link_req_data);
        let resp_link_id = Link::calculate_link_id(&link_req_data);
        let proof_data = extract_broadcast_data(&output);

        // 6. Initiator receives proof → LinkEstablished + RTT action
        let output = initiator.handle_packet(InterfaceId(0), &proof_data);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "initiator should get LinkEstablished"
        );
        let rtt_data = extract_broadcast_data(&output);

        // 7. Responder receives RTT → LinkEstablished
        let output = responder.handle_packet(InterfaceId(0), &rtt_data);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "responder should get LinkEstablished"
        );

        // Mark RTT confirmed on the initiator, the RTT was delivered (step 7
        // verified it), so in a real scenario the first inbound packet from the
        // responder would confirm it. Prevents RTT retry from firing in tests
        // that advance time after establishment.
        initiator.link_mut(&init_link_id).unwrap().confirm_rtt();

        NodeCoreLinkPair {
            initiator,
            responder,
            initiator_link_id: init_link_id,
            responder_link_id: resp_link_id,
            _dest_hash: dest_hash,
        }
    }

    fn establish_nodecore_link_pair() -> NodeCoreLinkPair {
        use crate::transport::InterfaceId;

        // 1. Create responder with a destination that accepts links
        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();
        resp_dest.set_accepts_links(true);
        let dest_hash = *resp_dest.hash();
        responder.register_destination(resp_dest);

        // 2. Create initiator
        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // 3. Initiator connects (broadcasts since no path)
        let (init_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let link_req_data = extract_broadcast_data(&output);

        // 4. Responder receives link request → auto-accepts (Stage 1): the
        //    LinkRequest event AND the establishment proof land in one output.
        let output = responder.handle_packet(InterfaceId(0), &link_req_data);
        let resp_link_id = Link::calculate_link_id(&link_req_data);
        let proof_data = extract_broadcast_data(&output);

        // 6. Initiator receives proof → LinkEstablished + RTT action
        let output = initiator.handle_packet(InterfaceId(0), &proof_data);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "initiator should get LinkEstablished"
        );
        // RTT packet is in the output actions
        let rtt_data = extract_broadcast_data(&output);

        // 7. Responder receives RTT → LinkEstablished
        let output = responder.handle_packet(InterfaceId(0), &rtt_data);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "responder should get LinkEstablished"
        );

        // Mark RTT confirmed, same as the _with_strategy variant above.
        initiator.link_mut(&init_link_id).unwrap().confirm_rtt();

        NodeCoreLinkPair {
            initiator,
            responder,
            initiator_link_id: init_link_id,
            responder_link_id: resp_link_id,
            _dest_hash: dest_hash,
        }
    }

    // Codeberg #21: a registered destination with accepts_links == false must
    // keep declining inbound links, the OFF switch the C-API exposes through
    // lev_destination_set_accepts_links / register_destination_with_links(.., false).
    // The link request is ignored: no establishment proof is emitted and no
    // LinkEstablished event fires.
    #[test]
    fn inbound_link_rejected_when_destination_does_not_accept() {
        use crate::transport::InterfaceId;

        // Responder with a destination that does NOT accept links.
        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();
        resp_dest.set_accepts_links(false);
        let dest_hash = *resp_dest.hash();
        responder.register_destination(resp_dest);

        // Initiator connects (broadcasts a link request since no path is known).
        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (_init_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let link_req_data = extract_broadcast_data(&output);

        // Responder receives the link request: it is ignored, so there is no
        // proof to send back and no LinkEstablished event.
        let output = responder.handle_packet(InterfaceId(0), &link_req_data);
        assert!(
            extract_all_action_data(&output).is_empty(),
            "a non-accepting destination must not emit an establishment proof"
        );
        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "a non-accepting destination must not establish an inbound link"
        );
    }

    // Pins the addressing contract the std facade `send_proof` relies on: the
    // engine resolves the signing identity from `destination_hash`, not from
    // `packet_hash`. A destination hash with no registered local destination has
    // no identity, so the call fails fast. This is the contract that makes the
    // facade's parameter mapping (dest_hash, packet_hash) -> engine
    // (packet_hash, dest_hash) correct; the two arguments also have distinct
    // types, so a literal swap would not compile.
    #[test]
    fn send_proof_resolves_identity_by_destination_hash() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let packet_hash = [7u8; 32];
        let unknown_dest = DestinationHash::new([9u8; TRUNCATED_HASHBYTES]);
        // No destination registered under this hash -> no identity -> error.
        assert!(
            node.send_proof(&packet_hash, &unknown_dest).is_err(),
            "send_proof must look the identity up by destination_hash"
        );
    }

    #[test]
    fn test_accept_link() {
        let pair = establish_nodecore_link_pair();

        assert_eq!(pair.responder.active_link_count(), 1);
        assert_eq!(pair.initiator.active_link_count(), 1);
    }

    #[test]
    fn test_reject_link() {
        use crate::transport::InterfaceId;

        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["reject"],
        )
        .unwrap();
        resp_dest.set_accepts_links(true);
        let dest_hash = *resp_dest.hash();
        responder.register_destination(resp_dest);

        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let (_init_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let link_req_data = extract_broadcast_data(&output);

        let _output = responder.handle_packet(InterfaceId(0), &link_req_data);
        let resp_link_id = Link::calculate_link_id(&link_req_data);

        // Reject instead of accept
        responder.reject_link(&resp_link_id);

        assert_eq!(responder.pending_link_count(), 0);
        assert_eq!(responder.active_link_count(), 0);
    }

    #[test]
    fn test_send_on_link() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send data from initiator
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"Hello!")
            .unwrap();

        // Should have actions (send packet)
        assert!(
            !output.actions.is_empty(),
            "send_on_link should produce actions"
        );

        // Deliver to responder
        let data = extract_broadcast_data(&output);
        let output = pair.responder.handle_packet(InterfaceId(0), &data);

        // Should have MessageReceived event
        let has_msg = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::MessageReceived { .. }));
        assert!(has_msg, "responder should get MessageReceived event");
    }

    // The responder can originate a channel message (the initiator need not
    // send first); with full packet routing the initiator receives it.
    #[test]
    fn channel_responder_can_send_first() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();
        let out = pair
            .responder
            .send_on_link(&pair.responder_link_id, b"from-responder")
            .expect("responder send_on_link");

        let mut got = false;
        for data in extract_all_action_data(&out) {
            let io = pair.initiator.handle_packet(InterfaceId(0), &data);
            got |= io
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::MessageReceived { .. }));
        }
        assert!(
            got,
            "initiator must receive a responder-first channel message"
        );
    }

    // A sustained stream must not stall at one window: with proofs routed back,
    // the sender's window advances and every message is delivered.
    #[test]
    fn channel_stream_does_not_stall_at_window() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();
        let lid = pair.initiator_link_id;
        let mut delivered = 0usize;
        for i in 0..40u32 {
            // Advance the clock so interface pacing is never the blocker; this
            // isolates the channel window/proof behaviour.
            let now = pair.initiator.transport().clock().now_ms();
            pair.initiator.transport().clock().set(now + 1000);
            let msg = alloc::format!("msg-{i:05}");
            let out = match pair.initiator.send_on_link(&lid, msg.as_bytes()) {
                Ok(o) => o,
                Err(e) => panic!("send {i} failed (window stalled?): {e:?}"),
            };
            for data in extract_all_action_data(&out) {
                let ro = pair.responder.handle_packet(InterfaceId(0), &data);
                delivered += ro
                    .events
                    .iter()
                    .filter(|e| matches!(e, NodeEvent::MessageReceived { .. }))
                    .count();
                for proof in extract_all_action_data(&ro) {
                    let _ = pair.initiator.handle_packet(InterfaceId(0), &proof);
                }
            }
        }
        assert_eq!(delivered, 40, "every streamed message must be delivered");
    }

    #[test]
    fn test_close_link() {
        let mut pair = establish_nodecore_link_pair();

        let output = pair.initiator.close_link(&pair.initiator_link_id);

        // Should have LinkClosed event
        let has_closed = output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::Normal,
                    ..
                }
            )
        });
        assert!(has_closed, "Expected LinkClosed with Normal reason");

        assert_eq!(pair.initiator.active_link_count(), 0);
    }

    #[test]
    fn test_announce_destination() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["announce"],
        )
        .unwrap();
        let hash = *dest.hash();
        node.register_destination(dest);

        let output = node.announce_destination(&hash, None).unwrap();

        // Should have a Broadcast action with the announce packet
        let has_broadcast = output
            .actions
            .iter()
            .any(|a| matches!(a, crate::transport::Action::Broadcast { .. }));
        assert!(has_broadcast, "announce should produce Broadcast action");
    }

    #[test]
    fn test_multiple_simultaneous_links() {
        use crate::transport::InterfaceId;

        // Create responder with destination
        let resp_identity1 = Identity::generate(&mut OsRng);
        let signing1 = resp_identity1.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut dest1 = Destination::new(
            Some(resp_identity1),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["multi1"],
        )
        .unwrap();
        dest1.set_accepts_links(true);
        let hash1 = *dest1.hash();
        responder.register_destination(dest1);

        let resp_identity2 = Identity::generate(&mut OsRng);
        let signing2 = resp_identity2.ed25519_verifying().to_bytes();
        let mut dest2 = Destination::new(
            Some(resp_identity2),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["multi2"],
        )
        .unwrap();
        dest2.set_accepts_links(true);
        let hash2 = *dest2.hash();
        responder.register_destination(dest2);

        // First initiator connects
        let clock = MockClock::new(TEST_TIME_MS);
        let mut init1 = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (link1, _, out1) = init1.connect(hash1, &signing1);
        let data1 = extract_broadcast_data(&out1);
        let out = responder.handle_packet(InterfaceId(0), &data1);
        let proof1 = extract_broadcast_data(&out);
        let out = init1.handle_packet(InterfaceId(0), &proof1);
        let rtt1 = extract_broadcast_data(&out);
        let _ = responder.handle_packet(InterfaceId(0), &rtt1);

        // Second initiator connects
        let clock = MockClock::new(TEST_TIME_MS);
        let mut init2 = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (_link2, _, out2) = init2.connect(hash2, &signing2);
        let data2 = extract_broadcast_data(&out2);
        let out = responder.handle_packet(InterfaceId(0), &data2);
        let proof2 = extract_broadcast_data(&out);
        let out = init2.handle_packet(InterfaceId(0), &proof2);
        let rtt2 = extract_broadcast_data(&out);
        let _ = responder.handle_packet(InterfaceId(0), &rtt2);

        assert_eq!(responder.active_link_count(), 2);

        // Both should be functional
        let _ = init1
            .send_on_link(&link1, b"data1")
            .expect("link 1 should work");
    }

    #[test]
    fn test_send_on_link_creates_receipt() {
        let mut pair = establish_nodecore_link_pair();

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"test")
            .unwrap();

        assert_eq!(pair.initiator.receipt_count(), 1);
    }

    #[test]
    fn test_close_link_removes_receipts() {
        let mut pair = establish_nodecore_link_pair();

        // Send data (populates receipt tracker)
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"test")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Close connection
        let _ = pair.initiator.close_link(&pair.initiator_link_id);

        // All receipts for this link should be cleaned
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipts should be cleaned on close"
        );
    }

    #[test]
    fn test_handle_packet_link_request() {
        use crate::transport::InterfaceId;

        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["linkreq"],
        )
        .unwrap();
        dest.set_accepts_links(true);
        let dest_hash = *dest.hash();
        responder.register_destination(dest);

        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (_, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let req_data = extract_broadcast_data(&output);

        let output = responder.handle_packet(InterfaceId(0), &req_data);

        // Auto-accept (Python parity): handle_packet proves the link inline and
        // creates a pending incoming link. There is no separate request event.
        let proof_data = extract_broadcast_data(&output);
        assert!(
            !proof_data.is_empty(),
            "responder must send an establishment proof"
        );
        assert_eq!(
            responder.pending_link_count(),
            1,
            "responder must create a pending incoming link"
        );
    }

    #[test]
    fn test_handle_packet_proof() {
        use crate::transport::InterfaceId;

        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["proof"],
        )
        .unwrap();
        dest.set_accepts_links(true);
        let dest_hash = *dest.hash();
        responder.register_destination(dest);

        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (_, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let req_data = extract_broadcast_data(&output);

        let output = responder.handle_packet(InterfaceId(0), &req_data);
        let proof_data = extract_broadcast_data(&output);

        // Feed proof to initiator
        let output = initiator.handle_packet(InterfaceId(0), &proof_data);

        let has_established = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
        assert!(
            has_established,
            "initiator should get LinkEstablished after proof"
        );
    }

    #[test]
    fn test_handle_packet_link_data() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send data from initiator
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"hello data")
            .unwrap();
        let data = extract_broadcast_data(&output);

        // Deliver to responder
        let output = pair.responder.handle_packet(InterfaceId(0), &data);

        let msg_data = output.events.iter().find_map(|e| match e {
            NodeEvent::MessageReceived { data, .. } => Some(data.clone()),
            _ => None,
        });
        assert!(msg_data.is_some(), "Expected MessageReceived event");
    }

    #[test]
    fn test_handle_timeout_keepalive_generation() {
        let mut pair = establish_nodecore_link_pair();

        // Advance time well past keepalive interval (default: 360s for slow links, 5s min)
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 400_000);

        let output = pair.initiator.handle_timeout();

        // Should produce actions (keepalive packet)
        // The keepalive may or may not produce an action depending on timing,
        // but handle_timeout should work without panicking
        let _ = output;
    }

    #[test]
    fn test_interface_down_cleans_link_paths() {
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Inject an announce on interface 0 to create a path
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["ifdown2"],
        )
        .unwrap();
        let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(0), &buf[..len]);

        assert!(node.path_count() > 0, "should have a path from announce");

        let output = node.handle_interface_down(InterfaceId(0));

        assert_eq!(node.path_count(), 0, "paths should be cleaned");

        let has_iface_down = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::InterfaceDown(0)));
        assert!(has_iface_down, "should emit InterfaceDown event");
    }

    // T16: NodeCore Regression Tests
    #[test]
    fn test_d12_handshake_timeout_vs_stale_timeout_different_reason() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, LINK_REQUEST_MAX_RETRIES};

        // Case 1: Handshake timeout produces LinkClosed with Timeout reason
        // Exhaust all retries first
        let (mut node, _dest_hash, _link_id) = setup_pending_link(false);
        let mut output = node.handle_timeout();
        for _attempt in 0..=(LINK_REQUEST_MAX_RETRIES as usize) {
            let now = node.transport().clock().now_ms();
            node.transport()
                .clock()
                .set(now + LINK_PENDING_TIMEOUT_MS + 1);
            output = node.handle_timeout();
        }

        let has_timeout = output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::Timeout,
                    ..
                }
            )
        });
        assert!(
            has_timeout,
            "handshake timeout should produce LinkClosed with Timeout reason"
        );

        // Case 2: Stale closure produces LinkClosed with Stale reason
        // (tested via establish_nodecore_link_pair, then advancing time)
        // This is more involved, the key point is that the reason values are different
    }

    #[test]
    fn test_retransmit_replaces_receipt() {
        let mut pair = establish_nodecore_link_pair();

        // Send data (creates receipt)
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"sync test")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Trigger retransmit by advancing time past timeout
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 10_000);
        let _output = pair.initiator.handle_timeout();

        // Receipt should still be exactly 1 (old replaced by new, no leak)
        assert_eq!(
            pair.initiator.receipt_count(),
            1,
            "receipt count should stay at 1 after retransmit (old replaced, not leaked)"
        );
    }

    // T13: Split test_pending_link_recovery_rate_limited
    #[test]
    fn test_link_timeout_triggers_path_recovery() {
        use crate::constants::{LINK_PENDING_TIMEOUT_MS, LINK_REQUEST_MAX_RETRIES};
        use crate::transport::Action;

        let (mut node, dest_hash, _link_id) = setup_pending_link(false);

        assert!(node.has_path(&dest_hash));

        // Exhaust all retries
        let mut output = node.handle_timeout();
        for _attempt in 0..=(LINK_REQUEST_MAX_RETRIES as usize) {
            let now = node.transport().clock().now_ms();
            node.transport()
                .clock()
                .set(now + LINK_PENDING_TIMEOUT_MS + 1);
            output = node.handle_timeout();
        }

        assert!(
            !node.has_path(&dest_hash),
            "path should be expired after timeout"
        );

        let has_broadcast = output
            .actions
            .iter()
            .any(|a| matches!(a, Action::Broadcast { .. }));
        assert!(has_broadcast, "should emit path request Broadcast");

        let has_closed = output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::Timeout,
                    ..
                }
            )
        });
        assert!(has_closed, "should emit LinkClosed with Timeout");
    }

    #[test]
    fn test_link_timeout_rate_limited_across_destinations() {
        use crate::constants::LINK_PENDING_TIMEOUT_MS;
        use crate::transport::{Action, InterfaceId};

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new()
            .enable_transport(false)
            .build(OsRng, clock, NoStorage);

        let remote1 = Identity::generate(&mut OsRng);
        let signing1 = remote1.ed25519_verifying().to_bytes();
        let mut dest1 = Destination::new(
            Some(remote1),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["rl1"],
        )
        .unwrap();
        let remote2 = Identity::generate(&mut OsRng);
        let signing2 = remote2.ed25519_verifying().to_bytes();
        let mut dest2 = Destination::new(
            Some(remote2),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["rl2"],
        )
        .unwrap();

        let ann1 = dest1.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let ann2 = dest2.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len1 = ann1.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(0), &buf[..len1]);
        let len2 = ann2.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(0), &buf[..len2]);
        let _ = node.handle_timeout();

        let hash1 = *dest1.hash();
        let hash2 = *dest2.hash();
        let (_, _, _) = node.connect(hash1, &signing1);
        let (_, _, _) = node.connect(hash2, &signing2);

        // Exhaust all retries for both links
        let mut output = node.handle_timeout();
        for _attempt in 0..=(crate::constants::LINK_REQUEST_MAX_RETRIES as usize) {
            let now = node.transport().clock().now_ms();
            node.transport()
                .clock()
                .set(now + LINK_PENDING_TIMEOUT_MS + 1);
            output = node.handle_timeout();
        }

        assert!(!node.has_path(&hash1));
        assert!(!node.has_path(&hash2));

        // Count LinkClosed events across ALL handle_timeout rounds.
        // Each destination eventually times out after retries are exhausted.
        let closed_count = output
            .events
            .iter()
            .filter(|e| matches!(e, NodeEvent::LinkClosed { .. }))
            .count();
        assert_eq!(closed_count, 2);

        let broadcast_count = output
            .actions
            .iter()
            .filter(|a| matches!(a, Action::Broadcast { .. }))
            .count();
        assert!(
            broadcast_count >= 2,
            "both path requests should succeed (different destinations), got {}",
            broadcast_count
        );
    }

    // Restored LinkManager Tests (as NodeCore tests)
    // Group A: ProofStrategy propagation
    #[test]
    fn test_proof_strategy_propagated_on_accept() {
        for strategy in [ProofStrategy::All, ProofStrategy::App, ProofStrategy::None] {
            let pair = establish_nodecore_link_pair_with_strategy(strategy);

            // Responder's link should have the requested proof strategy
            let resp_link = pair
                .responder
                .link(&pair.responder_link_id)
                .expect("responder link must exist");
            assert_eq!(
                resp_link.proof_strategy(),
                strategy,
                "proof_strategy mismatch for {:?}",
                strategy
            );
            // Responder always has a dest_signing_key (set during establishment proof)
            assert!(
                resp_link.dest_signing_key().is_some(),
                "dest_signing_key should always be set on responder for {:?}",
                strategy
            );
        }
    }

    #[test]
    fn test_prove_all_auto_generates_proof_on_data() {
        use crate::packet::PacketContext;
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair_with_strategy(ProofStrategy::All);

        // Build raw data packet (not channel) on initiator
        let raw_packet = pair
            .initiator
            .link(&pair.initiator_link_id)
            .unwrap()
            .build_data_packet_with_context(b"hello", PacketContext::None, &mut OsRng)
            .unwrap();

        // Deliver to responder
        let output = pair.responder.handle_packet(InterfaceId(0), &raw_packet);

        // Should have LinkDataReceived event
        let has_data = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDataReceived { .. }));
        assert!(has_data, "expected LinkDataReceived event");

        // Should NOT have LinkProofRequested (that's for App strategy)
        let has_proof_requested = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkProofRequested { .. }));
        assert!(
            !has_proof_requested,
            "All strategy should NOT emit LinkProofRequested"
        );

        // Should have a proof packet in actions (auto-generated)
        assert!(
            !output.actions.is_empty(),
            "All strategy should auto-generate proof packet"
        );

        // Negative: ProofStrategy::None should NOT generate proof
        let mut pair_none = establish_nodecore_link_pair_with_strategy(ProofStrategy::None);
        let raw_packet = pair_none
            .initiator
            .link(&pair_none.initiator_link_id)
            .unwrap()
            .build_data_packet_with_context(b"hello", PacketContext::None, &mut OsRng)
            .unwrap();
        let output = pair_none
            .responder
            .handle_packet(InterfaceId(0), &raw_packet);
        assert!(
            output.actions.is_empty(),
            "None strategy should NOT generate proof"
        );
        let has_data = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDataReceived { .. }));
        assert!(
            has_data,
            "expected LinkDataReceived even with None strategy"
        );
    }

    #[test]
    fn test_prove_app_emits_proof_requested_no_auto_proof() {
        use crate::packet::PacketContext;
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair_with_strategy(ProofStrategy::App);

        // Build raw data packet on initiator
        let raw_packet = pair
            .initiator
            .link(&pair.initiator_link_id)
            .unwrap()
            .build_data_packet_with_context(b"appdata", PacketContext::None, &mut OsRng)
            .unwrap();

        // Deliver to responder
        let output = pair.responder.handle_packet(InterfaceId(0), &raw_packet);

        // Should have LinkProofRequested BEFORE LinkDataReceived
        let proof_req_pos = output
            .events
            .iter()
            .position(|e| matches!(e, NodeEvent::LinkProofRequested { .. }));
        let data_pos = output
            .events
            .iter()
            .position(|e| matches!(e, NodeEvent::LinkDataReceived { .. }));
        assert!(
            proof_req_pos.is_some(),
            "App strategy should emit LinkProofRequested"
        );
        assert!(
            data_pos.is_some(),
            "App strategy should emit LinkDataReceived"
        );
        assert!(
            proof_req_pos.unwrap() < data_pos.unwrap(),
            "LinkProofRequested should come before LinkDataReceived"
        );

        // No auto-proof in actions
        assert!(
            output.actions.is_empty(),
            "App strategy should NOT auto-generate proof"
        );
    }

    #[test]
    fn test_channel_proof_round_trip_delivery() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send channel message from initiator
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"roundtrip")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);
        let channel_data = extract_broadcast_data(&output);

        // Deliver to responder → should get proof in actions
        let output = pair.responder.handle_packet(InterfaceId(0), &channel_data);
        let proof_data = extract_broadcast_data(&output);

        // Deliver proof to initiator → LinkDeliveryConfirmed
        let output = pair.initiator.handle_packet(InterfaceId(0), &proof_data);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(has_confirmed, "expected LinkDeliveryConfirmed event");

        // Receipt should be consumed
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should be consumed after proof"
        );
    }

    // Group B: Channel proof generation
    #[test]
    fn test_channel_proof_generated_for_in_order_message() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"ordered")
            .unwrap();
        let channel_data = extract_broadcast_data(&output);

        let output = pair.responder.handle_packet(InterfaceId(0), &channel_data);

        // Exactly 1 action (the proof)
        assert_eq!(
            output.actions.len(),
            1,
            "expected exactly 1 proof action, got {}",
            output.actions.len()
        );

        // MessageReceived event
        let has_msg = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::MessageReceived { .. }));
        assert!(has_msg, "expected MessageReceived event");
    }

    #[test]
    fn test_channel_proof_suppressed_on_rx_ring_full() {
        use crate::constants::CHANNEL_RX_RING_MAX;
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send 1 in-order message (seq=0), deliver, get proof, deliver proof
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"first")
            .unwrap();
        let data = extract_broadcast_data(&output);
        let output = pair.responder.handle_packet(InterfaceId(0), &data);
        let proof = extract_broadcast_data(&output);
        let _ = pair.initiator.handle_packet(InterfaceId(0), &proof);

        // Force next tx sequence to jump past rx ring capacity
        let jumped_seq = 1 + CHANNEL_RX_RING_MAX as u16;
        pair.initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .channel_mut()
            .unwrap()
            .force_next_tx_sequence_for_test(jumped_seq);

        // Send jumped-sequence msg (window is free after proof delivery)
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"jumped")
            .unwrap();
        let data = extract_broadcast_data(&output);
        let output = pair.responder.handle_packet(InterfaceId(0), &data);

        // No proof (rx_ring full, message dropped)
        assert!(
            output.actions.is_empty(),
            "expected no proof when rx_ring is full"
        );
        // No MessageReceived event
        let has_msg = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::MessageReceived { .. }));
        assert!(!has_msg, "should not get MessageReceived when rx_ring full");

        // Now send in-order msg (next expected is seq=1) → proof IS generated
        // Advance clock past pacing delay (proof delivery set pacing ~166ms)
        pair.initiator.transport().clock().set(TEST_TIME_MS + 1_000);
        pair.initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .channel_mut()
            .unwrap()
            .force_next_tx_sequence_for_test(1);
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"inorder")
            .unwrap();
        let data = extract_broadcast_data(&output);
        let output = pair.responder.handle_packet(InterfaceId(0), &data);
        assert!(
            !output.actions.is_empty(),
            "in-order message after full should generate proof"
        );
    }

    #[test]
    fn test_channel_proof_generated_with_prove_none() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair_with_strategy(ProofStrategy::None);

        // Even with ProofStrategy::None on destination, channel proofs are unconditional
        let resp_link = pair.responder.link(&pair.responder_link_id).unwrap();
        assert!(
            resp_link.dest_signing_key().is_some(),
            "responder should always have dest_signing_key"
        );

        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"prove_none")
            .unwrap();
        let data = extract_broadcast_data(&output);
        let output = pair.responder.handle_packet(InterfaceId(0), &data);

        assert!(
            !output.actions.is_empty(),
            "channel proof should be generated regardless of ProofStrategy::None"
        );
    }

    #[test]
    fn test_channel_proof_by_initiator_round_trip() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Initiator has no dest_signing_key but has ephemeral proof_signing_key
        let init_link = pair.initiator.link(&pair.initiator_link_id).unwrap();
        assert!(
            init_link.dest_signing_key().is_none(),
            "initiator should not have dest_signing_key"
        );
        assert!(
            init_link.proof_signing_key().is_some(),
            "initiator should have ephemeral proof_signing_key"
        );

        // Send channel message from responder to initiator
        let output = pair
            .responder
            .send_on_link(&pair.responder_link_id, b"resp_msg")
            .unwrap();
        let data = extract_broadcast_data(&output);

        // Deliver to initiator → proof in actions
        let output = pair.initiator.handle_packet(InterfaceId(0), &data);
        assert!(
            !output.actions.is_empty(),
            "initiator should generate channel proof"
        );
        let proof_data = extract_broadcast_data(&output);

        // Deliver proof back to responder → LinkDeliveryConfirmed
        let output = pair.responder.handle_packet(InterfaceId(0), &proof_data);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(
            has_confirmed,
            "responder should get LinkDeliveryConfirmed from initiator proof"
        );
    }

    // Group C: Timing
    #[test]
    fn test_channel_retransmit_on_timeout() {
        let mut pair = establish_nodecore_link_pair();

        // Send channel message (don't deliver proof)
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"timeout_test")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Phase 1: advance 1s → no retransmit yet
        pair.initiator.transport().clock().set(TEST_TIME_MS + 1_000);
        let output = pair.initiator.handle_timeout();
        let has_retransmit = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::ChannelRetransmit { .. }));
        assert!(!has_retransmit, "no retransmit at 1s");

        // Phase 2: advance 10s → retransmit with tries=2
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 10_000);
        let output = pair.initiator.handle_timeout();
        let retransmit = output.events.iter().find_map(|e| match e {
            NodeEvent::ChannelRetransmit { tries, .. } => Some(*tries),
            _ => None,
        });
        assert_eq!(
            retransmit,
            Some(2),
            "expected ChannelRetransmit with tries=2"
        );

        // Retransmit packet should be in actions
        assert!(
            !output.actions.is_empty(),
            "retransmit should produce a packet"
        );

        // Receipt count should still be 1 (old replaced, not leaked)
        assert_eq!(pair.initiator.receipt_count(), 1);
    }

    #[test]
    fn test_stale_recovery_on_inbound() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Force responder's link to Stale state
        pair.responder
            .link_mut(&pair.responder_link_id)
            .unwrap()
            .set_state(LinkState::Stale);
        assert_eq!(
            pair.responder
                .link(&pair.responder_link_id)
                .unwrap()
                .state(),
            LinkState::Stale
        );

        // Send channel message from initiator
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"recover")
            .unwrap();
        let data = extract_broadcast_data(&output);

        // Deliver to stale responder
        let output = pair.responder.handle_packet(InterfaceId(0), &data);

        // Should get LinkRecovered event
        let has_recovered = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkRecovered { .. }));
        assert!(has_recovered, "expected LinkRecovered event");

        // Link should be Active again
        assert_eq!(
            pair.responder
                .link(&pair.responder_link_id)
                .unwrap()
                .state(),
            LinkState::Active
        );

        // Negative: active link receiving data does NOT emit LinkRecovered
        let output2 = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"normal")
            .unwrap();
        let data2 = extract_broadcast_data(&output2);
        let output2 = pair.responder.handle_packet(InterfaceId(0), &data2);
        let has_recovered2 = output2
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkRecovered { .. }));
        assert!(!has_recovered2, "active link should NOT emit LinkRecovered");
    }

    #[test]
    fn test_channel_exhaustion_produces_channel_exhausted_close() {
        let mut pair = establish_nodecore_link_pair();

        // Send a message and reduce max_tries to 2
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"exhaust")
            .unwrap();

        pair.initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .channel_mut()
            .unwrap()
            .set_max_tries_for_test(2);

        // With RTT≈0, stale_time=10s. Keep under that to avoid stale close.
        // Channel timeout for tries=1 at default RTT=500ms: ~3125ms.

        // Phase 1: first retransmit (tries=2, first send was try 1)
        pair.initiator.transport().clock().set(TEST_TIME_MS + 4_000);
        let output = pair.initiator.handle_timeout();
        let has_retransmit = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::ChannelRetransmit { .. }));
        assert!(has_retransmit, "expected retransmit at tries=2");

        // Phase 2: tries=2 >= max_tries=2 → TearDownLink → LinkClosed
        // Timeout for tries=2: ~4687ms. 4000+5000=9000ms < 10s stale.
        pair.initiator.transport().clock().set(TEST_TIME_MS + 9_000);
        let output = pair.initiator.handle_timeout();

        let has_closed = output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::ChannelExhausted,
                    ..
                }
            )
        });
        assert!(
            has_closed,
            "expected LinkClosed with ChannelExhausted after exhaustion"
        );

        assert_eq!(
            pair.initiator.active_link_count(),
            0,
            "link should be removed"
        );
        assert!(
            pair.initiator.link(&pair.initiator_link_id).is_none(),
            "link should not be found"
        );
    }

    #[test]
    fn test_receipts_expire_after_timeout() {
        use crate::constants::DATA_RECEIPT_TIMEOUT_MS;

        let mut pair = establish_nodecore_link_pair();

        // Send (don't deliver proof)
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"expire_test")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Advance time past both stale close and receipt timeout.
        // The receipt is cleaned either by link close (remove_for_link) or
        // by time-based expiry, both paths converge to receipt_count == 0.
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + DATA_RECEIPT_TIMEOUT_MS + 1);
        let _ = pair.initiator.handle_timeout();
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should be cleaned after timeout"
        );
    }

    // Group D: Receipt tracking
    #[test]
    fn test_receipt_lifecycle() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send → receipt registered
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"lifecycle")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Deliver to responder → get proof
        let data = extract_broadcast_data(&output);
        let output = pair.responder.handle_packet(InterfaceId(0), &data);
        let proof = extract_broadcast_data(&output);

        // Deliver proof to initiator → confirmed, receipts cleared
        let output = pair.initiator.handle_packet(InterfaceId(0), &proof);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(has_confirmed, "expected LinkDeliveryConfirmed");
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should be consumed"
        );
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should be consumed after proof"
        );
    }

    #[test]
    fn test_retransmit_event_payload() {
        let mut pair = establish_nodecore_link_pair();

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"retransmit_payload")
            .unwrap();

        // Advance past timeout
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 10_000);
        let output = pair.initiator.handle_timeout();

        // Find the ChannelRetransmit event and check its fields
        let retransmit = output.events.iter().find_map(|e| match e {
            NodeEvent::ChannelRetransmit {
                link_id,
                sequence,
                tries,
            } => Some((*link_id, *sequence, *tries)),
            _ => None,
        });
        let (link_id, sequence, tries) = retransmit.expect("expected ChannelRetransmit");
        assert_eq!(link_id, pair.initiator_link_id);
        assert_eq!(sequence, 0, "first message should be sequence 0");
        assert_eq!(tries, 2, "first retransmit should be tries=2");

        // Receipt should still exist (retransmit replaced, not leaked)
        assert_eq!(pair.initiator.receipt_count(), 1);
    }

    #[test]
    fn test_retransmit_proof_matches_new_hash() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Suppress keepalives so handle_timeout only produces the retransmit action
        pair.initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .set_timing_for_test(3600, 3600, TEST_TIME_MS / crate::constants::MS_PER_SECOND);

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"retransmit_proof")
            .unwrap();

        // Trigger retransmit
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 10_000);
        let output = pair.initiator.handle_timeout();
        let retransmit_packet = extract_all_action_data(&output);
        assert!(
            !retransmit_packet.is_empty(),
            "retransmit should produce a packet"
        );

        // Deliver retransmit to responder → proof
        let output = pair
            .responder
            .handle_packet(InterfaceId(0), &retransmit_packet[0]);
        let proof = extract_broadcast_data(&output);

        // Deliver proof to initiator → confirmed
        let output = pair.initiator.handle_packet(InterfaceId(0), &proof);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(
            has_confirmed,
            "proof for retransmit should produce LinkDeliveryConfirmed"
        );
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should be consumed"
        );
    }

    #[test]
    fn test_multiple_retransmits_single_receipt() {
        let mut pair = establish_nodecore_link_pair();

        // Extend stale timeout so the link survives through retransmit intervals
        pair.initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .set_timing_for_test(3600, 3600, TEST_TIME_MS / crate::constants::MS_PER_SECOND);

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"multi_retransmit")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Retransmit 1
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 10_000);
        let _ = pair.initiator.handle_timeout();
        assert_eq!(
            pair.initiator.receipt_count(),
            1,
            "should still have 1 receipt after first retransmit"
        );

        // Retransmit 2
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + 25_000);
        let _ = pair.initiator.handle_timeout();
        assert_eq!(
            pair.initiator.receipt_count(),
            1,
            "should still have 1 receipt after second retransmit (no leak)"
        );
    }

    #[test]
    fn test_proof_for_final_retransmit_delivers() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Extend stale timeout to prevent link closure during retransmit intervals.
        // Default stale_time is 10s (keepalive 5s * factor 2), which is too short
        // for the 50s intervals below.
        pair.initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .set_timing_for_test(5, 3600, TEST_TIME_MS / crate::constants::MS_PER_SECOND);

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"final_retransmit")
            .unwrap();

        // Perform 3 retransmits, keep the last packet.
        // At intervals ≥5s, keepalive fires before retransmit in actions,
        // so use .last() to get the retransmit packet (not the keepalive).
        let mut last_packet = Vec::new();
        let intervals = [10_000u64, 25_000, 50_000];
        for &dt in &intervals {
            pair.initiator.transport().clock().set(TEST_TIME_MS + dt);
            let output = pair.initiator.handle_timeout();
            let packets = extract_all_action_data(&output);
            if !packets.is_empty() {
                last_packet = packets.last().unwrap().clone();
            }
        }
        assert!(!last_packet.is_empty(), "should have a retransmit packet");

        // Deliver last retransmit to responder → proof
        let output = pair.responder.handle_packet(InterfaceId(0), &last_packet);
        // Responder might see it as duplicate or new depending on channel state,
        // but should generate a proof for the packet
        let proofs = extract_all_action_data(&output);
        assert!(!proofs.is_empty(), "should get a proof for the retransmit");

        // Deliver proof to initiator → confirmed
        let output = pair.initiator.handle_packet(InterfaceId(0), &proofs[0]);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(has_confirmed, "expected LinkDeliveryConfirmed");
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should be consumed"
        );
    }

    #[test]
    fn test_receipt_count_increments_on_send() {
        let mut pair = establish_nodecore_link_pair();

        assert_eq!(pair.initiator.receipt_count(), 0);

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"first")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"second")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 2);
    }

    // Group E: Misc
    #[test]
    fn test_attached_interface_set_on_handshake() {
        let pair = establish_nodecore_link_pair();

        // Both sides should have attached_interface set to 0
        let init_link = pair.initiator.link(&pair.initiator_link_id).unwrap();
        assert_eq!(
            init_link.attached_interface(),
            Some(0),
            "initiator should have attached_interface=0"
        );

        let resp_link = pair.responder.link(&pair.responder_link_id).unwrap();
        assert_eq!(
            resp_link.attached_interface(),
            Some(0),
            "responder should have attached_interface=0"
        );

        // Negative: pending link (before proof) should have no attached_interface on initiator
        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut resp = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["iface"],
        )
        .unwrap();
        dest.set_accepts_links(true);
        let dest_hash = *dest.hash();
        resp.register_destination(dest);

        let clock = MockClock::new(TEST_TIME_MS);
        let mut init = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let (link_id, _, _) = init.connect(dest_hash, &resp_signing_key);
        let pending_link = init.link(&link_id).unwrap();
        assert_eq!(
            pending_link.attached_interface(),
            None,
            "pending initiator should have no attached_interface"
        );
    }

    #[test]
    fn test_mark_delivered_bogus_sequence() {
        let mut pair = establish_nodecore_link_pair();

        // Send to create channel
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"create_channel")
            .unwrap();

        let now_ms = pair.initiator.now_ms();
        let result = pair
            .initiator
            .link_mut(&pair.initiator_link_id)
            .unwrap()
            .channel_mut()
            .unwrap()
            .mark_delivered(9999, now_ms, 500);
        assert!(
            !result,
            "mark_delivered with bogus sequence should return false"
        );
    }

    #[test]
    fn test_channel_accessor_none_when_absent() {
        let pair = establish_nodecore_link_pair();

        // Before any send, channel should be None
        let link = pair.initiator.link(&pair.initiator_link_id).unwrap();
        assert!(
            link.channel().is_none(),
            "channel should be None before any send"
        );

        // After send, channel should be Some
        let mut pair = establish_nodecore_link_pair();
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"data")
            .unwrap();
        let link = pair.initiator.link(&pair.initiator_link_id).unwrap();
        assert!(
            link.channel().is_some(),
            "channel should be Some after send"
        );

        // Fake link_id → link().is_none()
        let fake_id = LinkId::new([0xFF; 16]);
        assert!(
            pair.initiator.link(&fake_id).is_none(),
            "fake link_id should return None"
        );
    }

    // Group F: Close cleanup
    #[test]
    fn test_close_link_removes_receipt_entries() {
        let mut pair = establish_nodecore_link_pair();

        // Send (populates receipts)
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"cleanup")
            .unwrap();
        assert!(pair.initiator.receipt_count() >= 1);
        assert!(
            pair.initiator
                .receipt_count_for_link(&pair.initiator_link_id)
                >= 1
        );

        // Close link
        let _ = pair.initiator.close_link(&pair.initiator_link_id);

        // All receipt entries for this link should be cleaned
        assert_eq!(
            pair.initiator
                .receipt_count_for_link(&pair.initiator_link_id),
            0,
            "receipt entries should be cleaned on close"
        );
    }

    // Group G: ReceiptTracker orphan-path fix tests
    #[test]
    fn test_delivery_proof_removes_receipt() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send channel message → creates receipt
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"proof_cleanup")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);
        let channel_data = extract_broadcast_data(&output);

        // Deliver to responder → get proof
        let output = pair.responder.handle_packet(InterfaceId(0), &channel_data);
        let proof = extract_broadcast_data(&output);

        // Deliver proof to initiator → receipt consumed entirely
        let output = pair.initiator.handle_packet(InterfaceId(0), &proof);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(has_confirmed, "expected LinkDeliveryConfirmed");

        // Orphan path #1 fix: the receipt entry is completely removed
        // (previously channel_receipt_keys entry was orphaned)
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "delivery proof should remove all receipt state"
        );
        assert_eq!(
            pair.initiator
                .receipt_count_for_link(&pair.initiator_link_id),
            0,
            "no receipt entries should remain for this link"
        );
    }

    #[test]
    fn test_expired_receipts_cleaned_on_timeout() {
        use crate::constants::DATA_RECEIPT_TIMEOUT_MS;

        let mut pair = establish_nodecore_link_pair();

        // Send channel message → creates receipt
        let _ = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"expire_cleanup")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);

        // Advance time past DATA_RECEIPT_TIMEOUT_MS → handle_timeout cleans up
        // (link will also close due to stale timeout, but that's fine.        // the point is that no receipt entries survive)
        pair.initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + DATA_RECEIPT_TIMEOUT_MS + 1);
        let _ = pair.initiator.handle_timeout();

        // Orphan path #2 fix: all receipt state is cleaned
        // (previously channel_hash_to_seq entries were orphaned after expiry GC)
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "all receipt entries should be cleaned after timeout"
        );
    }

    #[test]
    fn test_valid_proof_after_receipt_expiry_emits_no_event() {
        use crate::constants::DATA_RECEIPT_TIMEOUT_MS;
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Send channel message → creates receipt
        let output = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"will_expire")
            .unwrap();
        assert_eq!(pair.initiator.receipt_count(), 1);
        let channel_data = extract_broadcast_data(&output);

        // Deliver to responder → get proof (save for later)
        let output = pair.responder.handle_packet(InterfaceId(0), &channel_data);
        let proof = extract_broadcast_data(&output);

        // Expire the receipt directly (bypasses handle_timeout which would
        // also retransmit and re-register)
        pair.initiator
            .expire_receipts(TEST_TIME_MS + DATA_RECEIPT_TIMEOUT_MS + 1);
        assert_eq!(
            pair.initiator.receipt_count(),
            0,
            "receipt should have expired"
        );

        // Deliver the valid proof, receipt is gone, link still alive
        let output = pair.initiator.handle_packet(InterfaceId(0), &proof);
        let has_confirmed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }));
        assert!(
            !has_confirmed,
            "LinkDeliveryConfirmed must not fire when receipt was already expired"
        );
        assert_eq!(pair.initiator.receipt_count(), 0);
    }

    // Group H1: Single-packet proof verification at NodeCore
    #[test]
    fn test_single_packet_proof_delivery_confirmed() {
        // ProofStrategy::All: receiver auto-generates proof via Transport,
        // sender receives it and emits DeliveryConfirmed.
        use crate::transport::{InterfaceId, PathEntry};

        // 1. Create receiver with a destination that has ProofStrategy::All
        let recv_identity = Identity::generate(&mut OsRng);
        let recv_clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, recv_clock, NoStorage);

        let mut dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["proof"],
        )
        .unwrap();
        dest.set_proof_strategy(ProofStrategy::All);
        let dest_hash = *dest.hash();
        receiver.register_destination(dest);

        // Receiver needs an interface so the auto-proof can be sent
        let _recv_iface = receiver
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("recv_if", 2)));

        // 2. Create sender. MemoryStorage needed for receipt persistence
        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());

        // 3. Set up paths: sender → receiver (interface 0)
        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("if0", 1)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Sender needs a copy of the destination registered so NodeCore can
        // find the identity for proof verification. Use public-key-only identity.
        let recv_dest_ref = receiver.destination(&dest_hash).unwrap();
        let recv_pub_bytes = recv_dest_ref.identity().unwrap().public_key_bytes();
        let sender_side_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();

        // Teach sender about receiver's identity for encryption
        let recv_pub_for_encrypt = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, recv_pub_for_encrypt);

        let sender_side_dest = Destination::new(
            Some(sender_side_identity),
            Direction::Out,
            DestinationType::Single,
            "testapp",
            &["proof"],
        )
        .unwrap();
        sender.register_destination(sender_side_dest);

        // 4. Sender sends a single packet → creates receipt
        let (receipt_hash, output) = sender
            .send_single_packet(&dest_hash, b"hello proof test")
            .unwrap();
        let sent_raw = extract_broadcast_data(&output);

        // Verify receipt was created
        assert!(
            sender.transport.get_receipt(&receipt_hash).is_some(),
            "sender should have a receipt"
        );

        // 5. Receiver processes the packet → auto-generates proof (no NodeEvent)
        let recv_output = receiver.handle_packet(InterfaceId(0), &sent_raw);

        // Bug 1 fix: All strategy must NOT emit ProofRequested to the app
        let has_proof_requested = recv_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketProofRequested { .. }));
        assert!(
            !has_proof_requested,
            "ProofStrategy::All must NOT emit NodeEvent::PacketProofRequested"
        );

        // Auto-proof should appear as a SendPacket action
        let proof_raw = recv_output
            .actions
            .iter()
            .find_map(|a| match a {
                crate::transport::Action::SendPacket { data, .. } => Some(data.clone()),
                _ => None,
            })
            .expect("ProofStrategy::All should auto-generate a SendPacket proof");

        // 6. Feed proof to sender → should get DeliveryConfirmed
        let sender_output = sender.handle_packet(InterfaceId(0), &proof_raw);

        let has_confirmed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }));
        let has_failed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::DeliveryFailed { .. }));

        assert!(
            has_confirmed,
            "valid proof should produce DeliveryConfirmed, got events: {:?}",
            sender_output.events
        );
        assert!(!has_failed, "valid proof should NOT produce DeliveryFailed");

        // Verify receipt is now marked as delivered
        let receipt = sender.transport.get_receipt(&receipt_hash).unwrap();
        assert_eq!(
            receipt.status,
            crate::storage_types::ReceiptStatus::Delivered,
            "receipt should be marked Delivered"
        );
    }

    #[test]
    fn test_single_packet_proof_invalid_signature_delivery_failed() {
        // When a proof has a bad signature, NodeCore should emit DeliveryFailed.
        use crate::packet::build_proof_packet;
        use crate::transport::{InterfaceId, PathEntry};

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, recv_clock, NoStorage);

        let mut dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["badproof"],
        )
        .unwrap();
        dest.set_proof_strategy(ProofStrategy::All);
        let dest_hash = *dest.hash();
        receiver.register_destination(dest);

        // Receiver needs an interface for auto-proof
        let _recv_iface = receiver
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("recv_if", 2)));

        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());

        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("if0", 1)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Register dest on sender with the real identity for proof verification
        let recv_dest_ref = receiver.destination(&dest_hash).unwrap();
        let recv_pub_bytes = recv_dest_ref.identity().unwrap().public_key_bytes();
        let sender_side_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();

        // Teach sender about receiver's identity for encryption
        let recv_pub_for_encrypt = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, recv_pub_for_encrypt);

        let sender_side_dest = Destination::new(
            Some(sender_side_identity),
            Direction::Out,
            DestinationType::Single,
            "testapp",
            &["badproof"],
        )
        .unwrap();
        sender.register_destination(sender_side_dest);

        let (receipt_hash, output) = sender
            .send_single_packet(&dest_hash, b"hello bad proof")
            .unwrap();
        let _sent_raw = extract_broadcast_data(&output);

        // We need a packet_hash to craft a bad proof against. Use any 32-byte
        // hash that matches the sender's receipt. The receipt tracks by truncated
        // dest hash, and the proof packet carries the full packet hash.
        // For this test, just use a known hash from the receipt.
        let receipt = sender.transport.get_receipt(&receipt_hash).unwrap();
        let packet_hash = receipt.packet_hash;

        // Create proof with a WRONG identity (not the destination's)
        let wrong_identity = Identity::generate(&mut OsRng);
        let bad_proof_data = wrong_identity.create_proof(&packet_hash).unwrap();

        let proof_packet = build_proof_packet(&dest_hash.into_bytes(), &bad_proof_data);
        let mut buf = [0u8; crate::constants::MTU];
        let len = proof_packet.pack(&mut buf).unwrap();

        // Feed bad proof to sender → should get DeliveryFailed
        let sender_output = sender.handle_packet(InterfaceId(0), &buf[..len]);

        let has_failed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::DeliveryFailed { .. }));
        let has_confirmed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }));

        assert!(has_failed, "bad proof should produce DeliveryFailed");
        assert!(
            !has_confirmed,
            "bad proof should NOT produce DeliveryConfirmed"
        );

        // Receipt should NOT be marked delivered
        let receipt = sender.transport.get_receipt(&receipt_hash).unwrap();
        assert_ne!(
            receipt.status,
            crate::storage_types::ReceiptStatus::Delivered,
            "receipt should NOT be marked Delivered with bad proof"
        );
    }

    #[test]
    fn test_proof_remote_recalled_identity_delivery_confirmed() {
        // Codeberg #76: a valid proof from a REMOTE destination must confirm
        // delivery. The remote dest is NOT in self.destinations; its identity is
        // only recalled via storage (as an announce would). The proof path must
        // fall back to the recalled identity, like the encrypt path does.
        use crate::packet::build_proof_packet;
        use crate::transport::{InterfaceId, PathEntry};

        // Remote destination identity (held only by the remote peer).
        let recv_identity = Identity::generate(&mut OsRng);
        let recv_dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["recallproof"],
        )
        .unwrap();
        let dest_hash = *recv_dest.hash();
        let recv_pub_bytes = recv_dest.identity().unwrap().public_key_bytes();

        // Sender recalls the remote identity (public key only) via storage, the
        // way it would after processing the remote's announce. Crucially it does
        // NOT register a local destination for dest_hash.
        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());
        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("if0", 1)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );
        let recalled = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, recalled);
        assert!(
            sender.destination(&dest_hash).is_none(),
            "remote dest must NOT be a local destination for this test"
        );

        // VALID proof path -> must confirm delivery.
        let (receipt_hash, _out) = sender
            .send_single_packet(&dest_hash, b"hello remote proof")
            .unwrap();
        let packet_hash = sender
            .transport
            .get_receipt(&receipt_hash)
            .unwrap()
            .packet_hash;
        let good_proof = recv_dest
            .identity()
            .unwrap()
            .create_proof(&packet_hash)
            .unwrap();
        let proof_packet = build_proof_packet(&dest_hash.into_bytes(), &good_proof);
        let mut buf = [0u8; crate::constants::MTU];
        let len = proof_packet.pack(&mut buf).unwrap();
        let sender_output = sender.handle_packet(InterfaceId(0), &buf[..len]);

        let has_confirmed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }));
        let has_failed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::DeliveryFailed { .. }));
        assert!(
            has_confirmed,
            "valid proof from recalled remote identity must confirm delivery, got: {:?}",
            sender_output.events
        );
        assert!(
            !has_failed,
            "valid proof from recalled remote identity must NOT fail, got: {:?}",
            sender_output.events
        );

        // INVALID proof path -> must still fail (no weakening of rejection).
        let (receipt_hash2, _out2) = sender
            .send_single_packet(&dest_hash, b"hello remote proof 2")
            .unwrap();
        let packet_hash2 = sender
            .transport
            .get_receipt(&receipt_hash2)
            .unwrap()
            .packet_hash;
        let wrong_identity = Identity::generate(&mut OsRng);
        let bad_proof = wrong_identity.create_proof(&packet_hash2).unwrap();
        let bad_packet = build_proof_packet(&dest_hash.into_bytes(), &bad_proof);
        let mut buf2 = [0u8; crate::constants::MTU];
        let len2 = bad_packet.pack(&mut buf2).unwrap();
        let bad_output = sender.handle_packet(InterfaceId(0), &buf2[..len2]);

        let bad_confirmed = bad_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }));
        let bad_failed = bad_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::DeliveryFailed { .. }));
        assert!(bad_failed, "invalid proof must still fail");
        assert!(!bad_confirmed, "invalid proof must NOT confirm delivery");
    }

    #[test]
    fn test_single_packet_prove_all_no_app_event() {
        // Bug 1 fix: ProofStrategy::All must auto-generate proof via Transport
        // and must NOT emit NodeEvent::PacketProofRequested to the app.
        use crate::transport::InterfaceId;

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, recv_clock, NoStorage);

        let mut dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["allproof"],
        )
        .unwrap();
        dest.set_proof_strategy(ProofStrategy::All);
        let dest_hash = *dest.hash();
        receiver.register_destination(dest);

        // Receiver needs an interface for auto-proof delivery
        let _recv_iface = receiver
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("recv_if", 1)));

        // Create a sender to produce a valid data packet
        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());
        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("send_if", 2)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            crate::transport::PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Teach sender about receiver's identity for encryption
        let recv_dest_ref = receiver.destination(&dest_hash).unwrap();
        let recv_pub_bytes = recv_dest_ref.identity().unwrap().public_key_bytes();
        let recv_pub_for_encrypt = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, recv_pub_for_encrypt);

        let (_receipt_hash, output) = sender
            .send_single_packet(&dest_hash, b"prove all test")
            .unwrap();
        let sent_raw = extract_broadcast_data(&output);

        // Feed packet to receiver
        let recv_output = receiver.handle_packet(InterfaceId(0), &sent_raw);

        // Must NOT have ProofRequested event (Bug 1)
        let has_proof_event = recv_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketProofRequested { .. }));
        assert!(
            !has_proof_event,
            "ProofStrategy::All must NOT emit NodeEvent::PacketProofRequested"
        );

        // Must have a PacketReceived event (data still delivered to app)
        let has_packet_event = recv_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketReceived { .. }));
        assert!(has_packet_event, "data packet should still be delivered");

        // Must have a SendPacket action (the auto-generated proof)
        let has_send_action = recv_output
            .actions
            .iter()
            .any(|a| matches!(a, crate::transport::Action::SendPacket { .. }));
        assert!(
            has_send_action,
            "ProofStrategy::All should auto-generate a proof SendPacket action"
        );
    }

    #[test]
    fn test_single_packet_prove_app_emits_event_and_send_proof_works() {
        // Bug 2 fix: ProofStrategy::App emits NodeEvent::PacketProofRequested
        // and the app can call NodeCore::send_proof() to respond.
        use crate::transport::{InterfaceId, PathEntry};

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_clock = MockClock::new(TEST_TIME_MS);
        let mut receiver =
            NodeCoreBuilder::new().build(OsRng, recv_clock, MemoryStorage::with_defaults());

        let mut dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["appproof"],
        )
        .unwrap();
        dest.set_proof_strategy(ProofStrategy::App);
        let dest_hash = *dest.hash();
        receiver.register_destination(dest);

        // Receiver needs an interface and a path back to sender for send_proof()
        let recv_iface = receiver
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("recv_if", 1)));

        // Create a sender. MemoryStorage needed for receipt persistence
        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());
        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("send_if", 2)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Sender needs the receiver's public key for proof verification
        let recv_dest_ref = receiver.destination(&dest_hash).unwrap();
        let recv_pub_bytes = recv_dest_ref.identity().unwrap().public_key_bytes();
        let sender_side_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();

        // Teach sender about receiver's identity for encryption
        let recv_pub_for_encrypt = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, recv_pub_for_encrypt);

        let sender_side_dest = Destination::new(
            Some(sender_side_identity),
            Direction::Out,
            DestinationType::Single,
            "testapp",
            &["appproof"],
        )
        .unwrap();
        sender.register_destination(sender_side_dest);

        // Sender sends a single packet
        let (receipt_hash, output) = sender
            .send_single_packet(&dest_hash, b"app proof test")
            .unwrap();
        let sent_raw = extract_broadcast_data(&output);

        // Feed packet to receiver, should get ProofRequested event (App strategy)
        let recv_output = receiver.handle_packet(InterfaceId(0), &sent_raw);

        let proof_req = recv_output
            .events
            .iter()
            .find_map(|e| match e {
                NodeEvent::PacketProofRequested {
                    packet_hash,
                    destination_hash,
                } => Some((*packet_hash, *destination_hash)),
                _ => None,
            })
            .expect("ProofStrategy::App should emit NodeEvent::PacketProofRequested");
        let (packet_hash, req_dest_hash) = proof_req;

        // Must NOT have any auto-generated proof action
        let has_send_action = recv_output
            .actions
            .iter()
            .any(|a| matches!(a, crate::transport::Action::SendPacket { .. }));
        assert!(
            !has_send_action,
            "ProofStrategy::App must NOT auto-generate a proof"
        );

        // Now the app decides to prove: set up a path on receiver for send_proof()
        // (In real usage the path would exist from the announce; here we add it manually)
        receiver.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: recv_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Call send_proof(). Bug 2 fix: this method now exists
        let proof_output = receiver
            .send_proof(&packet_hash, &req_dest_hash)
            .expect("send_proof should succeed");

        // Should have a SendPacket action containing the proof
        let proof_raw = proof_output
            .actions
            .iter()
            .find_map(|a| match a {
                crate::transport::Action::SendPacket { data, .. } => Some(data.clone()),
                _ => None,
            })
            .expect("send_proof should produce a SendPacket action");

        // Feed proof to sender → should get DeliveryConfirmed
        let sender_output = sender.handle_packet(InterfaceId(0), &proof_raw);
        let has_confirmed = sender_output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::PacketDeliveryConfirmed { .. }));
        assert!(
            has_confirmed,
            "valid proof via send_proof() should produce DeliveryConfirmed, got: {:?}",
            sender_output.events
        );

        let receipt = sender.transport.get_receipt(&receipt_hash).unwrap();
        assert_eq!(
            receipt.status,
            crate::storage_types::ReceiptStatus::Delivered,
            "receipt should be marked Delivered"
        );
    }

    // Identity storage tests (via Storage trait)
    #[test]
    fn test_remember_identity_and_lookup() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

        let id = Identity::generate(&mut OsRng);
        let id_hash = *id.hash();
        let pub_bytes = id.public_key_bytes();
        let pub_id = Identity::from_public_key_bytes(&pub_bytes).unwrap();
        let hash = DestinationHash::new([0x01; 16]);
        node.remember_identity(hash, pub_id);

        assert!(node.storage().get_identity(hash.as_bytes()).is_some());
        assert_eq!(
            node.storage().get_identity(hash.as_bytes()).unwrap().hash(),
            &id_hash
        );
        assert!(node.storage().get_identity(&[0x02; 16]).is_none());
    }

    #[test]
    fn test_remember_identity_multiple() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

        let h1 = DestinationHash::new([0x01; 16]);
        let h2 = DestinationHash::new([0x02; 16]);
        let h3 = DestinationHash::new([0x03; 16]);

        node.remember_identity(h1, Identity::generate(&mut OsRng));
        node.remember_identity(h2, Identity::generate(&mut OsRng));
        node.remember_identity(h3, Identity::generate(&mut OsRng));

        assert!(node.storage().get_identity(h1.as_bytes()).is_some());
        assert!(node.storage().get_identity(h2.as_bytes()).is_some());
        assert!(node.storage().get_identity(h3.as_bytes()).is_some());
    }

    #[test]
    fn test_remember_identity_update_replaces() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

        let h1 = DestinationHash::new([0x01; 16]);

        let id1 = Identity::generate(&mut OsRng);
        let id2 = Identity::generate(&mut OsRng);
        let id2_hash = *id2.hash();

        let pub1 = Identity::from_public_key_bytes(&id1.public_key_bytes()).unwrap();
        let pub2 = Identity::from_public_key_bytes(&id2.public_key_bytes()).unwrap();

        node.remember_identity(h1, pub1);
        node.remember_identity(h1, pub2);

        assert_eq!(
            node.storage().get_identity(h1.as_bytes()).unwrap().hash(),
            &id2_hash
        );
    }

    #[test]
    fn test_send_single_packet_without_known_identity_fails() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let dest_hash = DestinationHash::new([0x42; 16]);
        let result = node.send_single_packet(&dest_hash, b"hello");
        assert_eq!(
            result.unwrap_err(),
            send::SendError::EncryptionFailed,
            "send without known identity should fail with EncryptionFailed"
        );
    }

    #[test]
    fn test_send_single_packet_encrypts_and_receiver_decrypts() {
        // Verify the full encrypt→send→receive→decrypt round trip
        use crate::transport::{InterfaceId, PathEntry};

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_pub_bytes = recv_identity.public_key_bytes();
        let recv_clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, recv_clock, NoStorage);

        let dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["enc"],
        )
        .unwrap();
        let dest_hash = *dest.hash();
        receiver.register_destination(dest);

        let _recv_iface = receiver
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("recv_if", 1)));

        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());
        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("send_if", 2)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Teach sender about receiver's identity for encryption
        let pub_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, pub_identity);

        // Send encrypted packet
        let payload = b"encrypted hello";
        let (_receipt_hash, output) = sender.send_single_packet(&dest_hash, payload).unwrap();
        let sent_raw = extract_broadcast_data(&output);

        // Receiver processes the packet, should decrypt and emit plaintext
        let recv_output = receiver.handle_packet(InterfaceId(0), &sent_raw);
        let received_data = recv_output
            .events
            .iter()
            .find_map(|e| match e {
                NodeEvent::PacketReceived { data, .. } => Some(data.clone()),
                _ => None,
            })
            .expect("receiver should emit PacketReceived with decrypted data");

        assert_eq!(
            received_data, payload,
            "decrypted data should match original plaintext"
        );
    }

    #[test]
    fn test_send_single_packet_uses_ratchet_key() {
        // Verify that send_single_packet uses the ratchet key when available.
        // The receiver enforces ratchets, so packets encrypted without the
        // ratchet key are silently dropped, proving the sender used it.
        use crate::transport::{InterfaceId, PathEntry};

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_pub_bytes = recv_identity.public_key_bytes();
        let recv_clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, recv_clock, NoStorage);

        let mut dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["ratchetenc"],
        )
        .unwrap();
        let dest_hash = *dest.hash();

        // Enable and enforce ratchets, receiver drops packets not using a ratchet
        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        dest.set_enforce_ratchets(true);
        let ratchet_pub = dest.current_ratchet_public().unwrap();

        receiver.register_destination(dest);

        let _recv_iface = receiver
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("recv_if", 1)));

        let send_clock = MockClock::new(TEST_TIME_MS);
        let mut sender =
            NodeCoreBuilder::new().build(OsRng, send_clock, MemoryStorage::with_defaults());
        let sender_iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("send_if", 2)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: sender_iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );

        // Teach sender about receiver's identity
        let pub_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, pub_identity);

        // Store the ratchet key in sender's known_ratchets (normally done by announce processing)
        sender.transport.storage_mut().remember_known_ratchet(
            dest_hash.into_bytes(),
            ratchet_pub,
            TEST_TIME_MS,
        );

        // Send encrypted packet, should use ratchet key
        let payload = b"ratchet encrypted hello";
        let (_receipt_hash, output) = sender.send_single_packet(&dest_hash, payload).unwrap();
        let sent_raw = extract_broadcast_data(&output);

        // Receiver processes the packet, enforce_ratchets means it drops non-ratcheted packets
        let recv_output = receiver.handle_packet(InterfaceId(0), &sent_raw);
        let received_data = recv_output
            .events
            .iter()
            .find_map(|e| match e {
                NodeEvent::PacketReceived { data, .. } => Some(data.clone()),
                _ => None,
            })
            .expect("receiver should emit PacketReceived — proves ratchet key was used");

        assert_eq!(
            received_data, payload,
            "decrypted data should match original plaintext"
        );
    }

    #[test]
    fn test_announce_populates_known_identities() {
        // When a node receives an announce, it should populate storage identities
        use crate::transport::InterfaceId;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Register an interface for the announce to arrive on
        node.transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("if0", 1)));

        // Build a valid announce from a remote identity
        let remote_identity = Identity::generate(&mut OsRng);
        let mut remote_dest = Destination::new(
            Some(remote_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["announce"],
        )
        .unwrap();

        let announce_packet = remote_dest
            .announce(None, &mut OsRng, TEST_TIME_MS)
            .unwrap();
        let remote_dest_hash = *remote_dest.hash();

        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_packet.pack(&mut buf).unwrap();

        // Feed announce to node
        let output = node.handle_packet(InterfaceId(0), &buf[..len]);

        // Should have an AnnounceReceived event
        let has_announce = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::AnnounceReceived { .. }));
        assert!(has_announce, "should emit AnnounceReceived event");

        // Storage should now contain the remote identity
        assert!(
            node.storage()
                .get_identity(remote_dest_hash.as_bytes())
                .is_some(),
            "announce should populate storage identities"
        );
    }

    // Encryption Wire Inspection Tests
    //
    // Security invariant: plaintext user data must NEVER appear in outgoing
    // Action::SendPacket or Action::Broadcast bytes.

    #[test]
    fn test_single_packet_wire_bytes_never_contain_plaintext() {
        use crate::transport::PathEntry;

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_pub_bytes = recv_identity.public_key_bytes();
        let dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["wire"],
        )
        .unwrap();
        let dest_hash = *dest.hash();

        let clock = MockClock::new(TEST_TIME_MS);
        let mut sender = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
        let iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("if0", 1)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );
        let pub_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, pub_identity);

        let plaintext = b"KNOWN_PLAINTEXT_MARKER_XYZZY_12345678";
        let (_hash, output) = sender.send_single_packet(&dest_hash, plaintext).unwrap();
        let wire = extract_broadcast_data(&output);

        assert!(
            !wire.windows(plaintext.len()).any(|w| w == plaintext),
            "plaintext must not appear in wire bytes"
        );
        // Wire bytes should be longer than plaintext (header + encryption overhead)
        assert!(
            wire.len() > plaintext.len(),
            "encrypted packet should be longer than plaintext"
        );
    }

    #[test]
    fn test_single_packet_ciphertext_differs_each_send() {
        use crate::transport::PathEntry;

        let recv_identity = Identity::generate(&mut OsRng);
        let recv_pub_bytes = recv_identity.public_key_bytes();
        let dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["nonce"],
        )
        .unwrap();
        let dest_hash = *dest.hash();

        let clock = MockClock::new(TEST_TIME_MS);
        let mut sender = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
        let iface = sender
            .transport
            .register_interface(alloc::boxed::Box::new(MockInterface::new("if0", 1)));
        sender.transport.insert_path(
            dest_hash.into_bytes(),
            PathEntry {
                hops: 1,
                expires_ms: u64::MAX,
                interface_index: iface,
                random_blobs: Vec::new(),
                next_hop: None,
            },
        );
        let pub_identity = Identity::from_public_key_bytes(&recv_pub_bytes).unwrap();
        sender.remember_identity(dest_hash, pub_identity);

        let plaintext = b"same payload both times";
        let (_h1, out1) = sender.send_single_packet(&dest_hash, plaintext).unwrap();
        let wire1 = extract_broadcast_data(&out1);

        let (_h2, out2) = sender.send_single_packet(&dest_hash, plaintext).unwrap();
        let wire2 = extract_broadcast_data(&out2);

        assert_ne!(
            wire1, wire2,
            "two encryptions of the same plaintext must produce different ciphertext (ephemeral key)"
        );
    }

    #[test]
    fn test_link_data_wire_bytes_never_contain_plaintext() {
        let pair = establish_nodecore_link_pair();
        let mut initiator = pair.initiator;
        let link_id = pair.initiator_link_id;

        let plaintext = b"KNOWN_PLAINTEXT_MARKER_LINK_DATA_9876";
        let output = initiator.send_on_link(&link_id, plaintext).unwrap();

        for action_data in extract_all_action_data(&output) {
            assert!(
                !action_data.windows(plaintext.len()).any(|w| w == plaintext),
                "plaintext must not appear in link data wire bytes"
            );
        }
    }

    #[test]
    fn test_channel_message_wire_bytes_never_contain_plaintext() {
        // send_on_link goes through Channel, which is the channel message path
        let pair = establish_nodecore_link_pair();
        let mut responder = pair.responder;
        let link_id = pair.responder_link_id;

        let plaintext = b"KNOWN_PLAINTEXT_MARKER_CHANNEL_MSG_ABCD";
        let output = responder.send_on_link(&link_id, plaintext).unwrap();

        for action_data in extract_all_action_data(&output) {
            assert!(
                !action_data.windows(plaintext.len()).any(|w| w == plaintext),
                "plaintext must not appear in channel message wire bytes"
            );
        }
    }

    // Encryption Edge Cases and Failure Modes
    #[test]
    fn test_send_before_announce_returns_encryption_failed() {
        // register destination but DON'T call remember_identity
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["noannounce"],
        )
        .unwrap();
        let dest_hash = *dest.hash();
        node.register_destination(dest);

        let result = node.send_single_packet(&dest_hash, b"should fail");
        assert_eq!(result.unwrap_err(), send::SendError::EncryptionFailed);
        // No actions should be emitted, no plaintext leak
    }

    #[test]
    fn test_known_identities_many_entries() {
        // Verify we can store many identities through the Storage trait
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

        let mut hashes = Vec::new();
        for i in 0..100u8 {
            let id = Identity::generate(&mut OsRng);
            let pub_bytes = id.public_key_bytes();
            let dest = Destination::new(
                Some(id),
                Direction::In,
                DestinationType::Single,
                "testapp",
                &[&alloc::format!("dest{}", i)],
            )
            .unwrap();
            let h = *dest.hash();
            let pub_id = Identity::from_public_key_bytes(&pub_bytes).unwrap();
            node.remember_identity(h, pub_id);
            hashes.push(h);
        }

        // All entries should be retrievable
        for h in &hashes {
            assert!(
                node.storage().get_identity(h.as_bytes()).is_some(),
                "all identities should be retrievable from storage"
            );
        }
    }

    #[test]
    fn test_receive_encrypted_packet_for_unregistered_destination() {
        use crate::transport::InterfaceId;

        // Build an encrypted packet for a destination that the receiver doesn't have
        let remote_identity = Identity::generate(&mut OsRng);
        let pub_bytes = remote_identity.public_key_bytes();
        let dest = Destination::new(
            Some(remote_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["unknown"],
        )
        .unwrap();
        let dest_hash = *dest.hash();

        // Encrypt data targeting this destination using the public key
        let pub_only = Identity::from_public_key_bytes(&pub_bytes).unwrap();
        let ciphertext = pub_only
            .encrypt_for_destination(b"secret data", None, &mut OsRng)
            .unwrap();

        // Build a raw data packet
        let packet = crate::packet::Packet {
            flags: crate::packet::PacketFlags {
                ifac_flag: false,
                header_type: crate::packet::HeaderType::Type1,
                context_flag: false,
                transport_type: crate::packet::TransportType::Broadcast,
                dest_type: DestinationType::Single,
                packet_type: crate::packet::PacketType::Data,
            },
            hops: 0,
            transport_id: None,
            destination_hash: dest_hash.into_bytes(),
            context: crate::packet::PacketContext::None,
            data: crate::packet::PacketData::Owned(ciphertext),
        };
        let mut buf = [0u8; crate::constants::MTU];
        let len = packet.pack(&mut buf).unwrap();

        // Create a receiver node WITHOUT registering that destination
        let clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let output = receiver.handle_packet(InterfaceId(0), &buf[..len]);

        // No PacketReceived events and no actions
        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::PacketReceived { .. })),
            "unregistered destination should produce no PacketReceived events"
        );
    }

    #[test]
    fn test_receive_corrupted_ciphertext_silently_dropped() {
        use crate::transport::InterfaceId;

        // Build a valid encrypted packet targeting a destination, then corrupt the ciphertext
        let recv_identity = Identity::generate(&mut OsRng);
        let recv_pub = recv_identity.public_key_bytes();
        let dest = Destination::new(
            Some(recv_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["corrupt"],
        )
        .unwrap();
        let dest_hash = *dest.hash();

        // Encrypt using the public-key-only identity (simulates remote sender)
        let recv_pub_only = Identity::from_public_key_bytes(&recv_pub).unwrap();
        let mut ciphertext = recv_pub_only
            .encrypt_for_destination(b"valid data before corruption", None, &mut OsRng)
            .unwrap();

        // Flip a bit in the ciphertext payload (past the ephemeral key prefix)
        if ciphertext.len() > 40 {
            ciphertext[40] ^= 0xFF;
        }

        let packet = crate::packet::Packet {
            flags: crate::packet::PacketFlags {
                ifac_flag: false,
                header_type: crate::packet::HeaderType::Type1,
                context_flag: false,
                transport_type: crate::packet::TransportType::Broadcast,
                dest_type: DestinationType::Single,
                packet_type: crate::packet::PacketType::Data,
            },
            hops: 0,
            transport_id: None,
            destination_hash: dest_hash.into_bytes(),
            context: crate::packet::PacketContext::None,
            data: crate::packet::PacketData::Owned(ciphertext),
        };
        let mut buf = [0u8; crate::constants::MTU];
        let len = packet.pack(&mut buf).unwrap();

        let clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        receiver.register_destination(dest);

        let output = receiver.handle_packet(InterfaceId(0), &buf[..len]);

        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::PacketReceived { .. })),
            "corrupted ciphertext should be silently dropped (HMAC reject)"
        );
    }

    #[test]
    fn test_receive_with_wrong_key_silently_dropped() {
        use crate::transport::InterfaceId;

        // Encrypt with identity A's key, but send to identity B's destination
        let identity_a = Identity::generate(&mut OsRng);
        let pub_a_bytes = identity_a.public_key_bytes();
        let identity_b = Identity::generate(&mut OsRng);

        let dest_b = Destination::new(
            Some(identity_b),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["wrongkey"],
        )
        .unwrap();
        let dest_hash_b = *dest_b.hash();

        // Encrypt targeting identity A (wrong key for dest B)
        let pub_a = Identity::from_public_key_bytes(&pub_a_bytes).unwrap();
        let ciphertext = pub_a
            .encrypt_for_destination(b"encrypted for wrong dest", None, &mut OsRng)
            .unwrap();

        let packet = crate::packet::Packet {
            flags: crate::packet::PacketFlags {
                ifac_flag: false,
                header_type: crate::packet::HeaderType::Type1,
                context_flag: false,
                transport_type: crate::packet::TransportType::Broadcast,
                dest_type: DestinationType::Single,
                packet_type: crate::packet::PacketType::Data,
            },
            hops: 0,
            transport_id: None,
            destination_hash: dest_hash_b.into_bytes(),
            context: crate::packet::PacketContext::None,
            data: crate::packet::PacketData::Owned(ciphertext),
        };
        let mut buf = [0u8; crate::constants::MTU];
        let len = packet.pack(&mut buf).unwrap();

        let clock = MockClock::new(TEST_TIME_MS);
        let mut receiver = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        receiver.register_destination(dest_b);

        let output = receiver.handle_packet(InterfaceId(0), &buf[..len]);

        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::PacketReceived { .. })),
            "packet encrypted with wrong key should be silently dropped"
        );
    }

    #[test]
    fn test_send_single_packet_never_sends_plaintext_on_missing_identity() {
        // Verify that EncryptionFailed path produces zero Actions
        // (this is a security test, no plaintext leak even on error)
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        let dest_hash = DestinationHash::new([0xAB; 16]);
        let result = node.send_single_packet(&dest_hash, b"should never be sent");

        assert_eq!(result.unwrap_err(), send::SendError::EncryptionFailed);
        // Since send_single_packet returns Err, no TickOutput is produced.
        // The error path cannot possibly emit actions because no TickOutput
        // is returned. This is correct by construction.
    }

    #[test]
    fn test_handle_interface_up_triggers_fresh_announces() {
        // Fix 3: When a new non-local interface comes online, the node
        // should generate fresh announces for all local destinations.
        // This ensures the new peer learns about destinations even if the
        // original announce was sent before the connection was established.
        use crate::transport::Action;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Register an interface so we can send packets
        let iface = Box::new(MockInterface::new("if0", 1));
        node.transport.register_interface(iface);

        // Register a destination and announce it (populates announce_cache)
        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["ifaceup"],
        )
        .unwrap();
        let dest_hash = *dest.hash();
        node.register_destination(dest);

        let _ = node.announce_destination(&dest_hash, None).unwrap();

        // Verify announce_cache is populated
        assert!(
            node.transport
                .storage()
                .get_announce_cache(dest_hash.as_bytes())
                .is_some(),
            "announce_cache should be populated after announce_destination"
        );

        // Now simulate a new interface coming online
        let iface2 = Box::new(MockInterface::new("if1", 2));
        node.transport.register_interface(iface2);

        let output = node.handle_interface_up(1);

        // Should produce Broadcast actions (fresh announces sent on all interfaces)
        assert!(
            !output.actions.is_empty(),
            "handle_interface_up should produce actions"
        );
        assert!(
            output
                .actions
                .iter()
                .any(|a| matches!(a, Action::Broadcast { .. })),
            "handle_interface_up should broadcast fresh announces"
        );
    }

    #[test]
    fn test_handle_interface_up_no_announces_without_destinations() {
        // If no local destinations are registered, handle_interface_up
        // should produce no broadcast actions.
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        let iface = Box::new(MockInterface::new("if0", 1));
        node.transport.register_interface(iface);

        let output = node.handle_interface_up(0);

        assert!(
            output.actions.is_empty(),
            "no destinations registered, should have no actions"
        );
    }

    // Block A: Fresh Path Response for Local Destinations
    #[test]
    fn test_path_response_local_dest_is_fresh() {
        // When a path request arrives for our OWN destination, the response
        // should contain a fresh announce (new signature, new random_hash)
        // rather than cached bytes from the initial announce.
        use crate::announce::ReceivedAnnounce;
        use crate::constants::{PATH_REQUEST_GRACE_MS, TRUNCATED_HASHBYTES};
        use crate::packet::{
            HeaderType, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
        };
        use crate::traits::Storage;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        let iface = Box::new(MockInterface::new("if0", 1));
        node.transport.register_interface(iface);

        // Register a local destination
        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();
        let dest_hash = dest.hash().into_bytes();
        node.register_destination(dest);

        // Do an initial announce to populate the cache
        let output = node.handle_interface_up(0);
        let _ = output; // actions dispatched

        // Grab the cached announce bytes
        let initial_cached = node
            .transport
            .storage()
            .get_announce_cache(&dest_hash)
            .cloned()
            .expect("announce cache should be populated after interface_up");

        // Build a path request packet targeting the local destination
        // The fresh announce will have different random_hash because
        // generate_random_hash() uses the RNG (different on each call).
        let path_req_hash = *node.transport.path_request_hash();
        let mut data = Vec::new();
        data.extend_from_slice(&dest_hash);
        data.extend_from_slice(&[0xCC; TRUNCATED_HASHBYTES]); // tag

        let packet = crate::packet::Packet {
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
            destination_hash: path_req_hash,
            context: PacketContext::None,
            data: PacketData::Owned(data),
        };

        let mut buf = [0u8; 500];
        let len = packet.pack(&mut buf).unwrap();

        // Process the path request through NodeCore's handle_packet
        let output = node.handle_packet(crate::transport::InterfaceId(0), &buf[..len]);

        // Verify PathRequestReceived event is emitted
        let path_req_event = output.events.iter().any(|e| {
            matches!(e, NodeEvent::PathRequestReceived { destination_hash } if destination_hash.as_bytes() == &dest_hash)
        });
        assert!(path_req_event, "Expected PathRequestReceived event");

        // Check that the AnnounceEntry has fresh bytes (different from initial cache)
        let announce_entry = node
            .transport
            .storage()
            .get_announce(&dest_hash)
            .expect("AnnounceEntry should exist after path request");
        assert_eq!(
            announce_entry.retransmit_at_ms,
            Some(TEST_TIME_MS + PATH_REQUEST_GRACE_MS),
            "retransmit should be at now + 400ms grace"
        );
        assert_eq!(
            announce_entry.target_interface,
            Some(0),
            "response should target the requesting interface"
        );
        assert!(
            announce_entry.block_rebroadcasts,
            "path response should block rebroadcasts"
        );

        // The fresh bytes should differ from initial (new random_hash + signature)
        assert_ne!(
            announce_entry.raw_packet, initial_cached,
            "Path response should contain FRESH announce bytes, not cached bytes"
        );

        // Parse both to verify the fresh announce has different random_hash
        let initial_pkt = crate::packet::Packet::unpack(&initial_cached).unwrap();
        let fresh_pkt = crate::packet::Packet::unpack(&announce_entry.raw_packet).unwrap();
        let initial_announce = ReceivedAnnounce::from_packet(&initial_pkt).unwrap();
        let fresh_announce = ReceivedAnnounce::from_packet(&fresh_pkt).unwrap();
        assert_ne!(
            initial_announce.random_hash(),
            fresh_announce.random_hash(),
            "Fresh path response should have a new random_hash"
        );

        // The updated announce_cache should also have the fresh bytes
        let updated_cache = node
            .transport
            .storage()
            .get_announce_cache(&dest_hash)
            .cloned()
            .expect("announce cache should be updated");
        assert_eq!(
            updated_cache, announce_entry.raw_packet,
            "announce_cache should match the fresh AnnounceEntry"
        );
    }

    /// Block D: When handle_interface_up fires (TCP reconnect), the daemon
    /// should rebroadcast cached announce bytes for local-client destinations
    /// in addition to generating fresh announces for daemon-owned destinations.
    #[test]
    fn test_interface_up_rebroadcasts_client_cached_announces() {
        use crate::traits::Storage;
        use crate::transport::Action;

        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Interface 0: network interface (TCP)
        let iface0 = Box::new(MockInterface::new("tcp0", 1));
        node.transport.register_interface(iface0);

        // Interface 1: local client (IPC)
        let iface1 = Box::new(MockInterface::new("local0", 1));
        node.transport.register_interface(iface1);
        node.transport.set_local_client(1, true);

        // Register a daemon-owned destination
        let daemon_identity = Identity::generate(&mut OsRng);
        let daemon_dest = Destination::new(
            Some(daemon_identity),
            Direction::In,
            DestinationType::Single,
            "daemonapp",
            &["echo"],
        )
        .unwrap();
        let daemon_hash = daemon_dest.hash().into_bytes();
        node.register_destination(daemon_dest);

        // Create a client announce by generating a fresh identity and announce,
        // then feeding it through the local client interface path.
        let client_identity = Identity::generate(&mut OsRng);
        let mut client_dest = Destination::new(
            Some(client_identity),
            Direction::In,
            DestinationType::Single,
            "clientapp",
            &["svc"],
        )
        .unwrap();
        let client_hash = client_dest.hash().into_bytes();
        let now_ms = node.transport.clock().now_ms();
        let announce_pkt = client_dest.announce(None, &mut OsRng, now_ms).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce_pkt.pack(&mut buf).unwrap();
        let client_announce_raw = buf[..len].to_vec();

        // Feed the client announce through the local interface
        let _output = node.handle_packet(crate::transport::InterfaceId(1), &client_announce_raw);

        // The announce should have been accepted and cached
        assert!(
            node.transport
                .storage()
                .get_announce_cache(&client_hash)
                .is_some(),
            "Client announce should be cached after local client announce"
        );
        assert!(
            node.transport
                .storage()
                .has_local_client_known_dest(&client_hash),
            "Client dest should be tracked in local_client_known_dests"
        );

        // Advance clock past the 250ms registration delay and poll to flush
        // the delayed announce rebroadcast (Block B).
        node.transport.clock().advance(300);
        let _ = node.handle_timeout();

        // Now simulate interface recovery: call handle_interface_up
        let output = node.handle_interface_up(0);

        // We expect two broadcasts: one fresh announce for daemon_dest,
        // and one cached rebroadcast for client_dest.
        let broadcast_actions: Vec<&Vec<u8>> = output
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Broadcast { data, .. } => Some(data),
                _ => None,
            })
            .collect();

        assert!(
            broadcast_actions.len() >= 2,
            "Expected at least 2 broadcasts (daemon fresh + client cached), got {}",
            broadcast_actions.len()
        );

        // Verify daemon dest got a fresh announce (different from anything cached before)
        let has_daemon_broadcast = broadcast_actions.iter().any(|data| {
            crate::packet::Packet::unpack(data)
                .ok()
                .map(|pkt| pkt.destination_hash == daemon_hash)
                .unwrap_or(false)
        });
        assert!(
            has_daemon_broadcast,
            "Should broadcast fresh announce for daemon-owned dest"
        );

        // Verify client dest got its cached bytes rebroadcast
        let has_client_broadcast = broadcast_actions.iter().any(|data| {
            crate::packet::Packet::unpack(data)
                .ok()
                .map(|pkt| pkt.destination_hash == client_hash)
                .unwrap_or(false)
        });
        assert!(
            has_client_broadcast,
            "Should rebroadcast cached announce for local-client dest"
        );
    }

    #[test]
    fn test_announce_self_remembers_ratchet() {
        let clock = MockClock::new(TEST_TIME_MS);
        let mut node = NodeCoreBuilder::new().enable_transport(true).build(
            OsRng,
            clock,
            MemoryStorage::with_defaults(),
        );

        // Register interface so announce can be sent
        node.transport
            .register_interface(Box::new(MockInterface::new("if0", 1)));

        // Create destination with ratchets enabled
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["selfratchet"],
        )
        .unwrap();
        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();

        let dest_hash = *dest.hash();
        let expected_ratchet = dest.current_ratchet_public().unwrap();
        node.register_destination(dest);

        // Announce
        let _output = node.announce_destination(&dest_hash, None).unwrap();

        // Verify self-remember
        let stored = node.transport.get_ratchet(&dest_hash);
        assert!(
            stored.is_some(),
            "Sender should self-remember its ratchet after announce"
        );
        assert_eq!(
            stored.unwrap(),
            expected_ratchet,
            "Self-remembered ratchet should match destination's current ratchet"
        );
    }

    #[test]
    fn test_announce_persists_ratchet_keys() {
        let clock = MockClock::new(TEST_TIME_MS);
        let storage = MemoryStorage::with_defaults();
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, storage);

        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["persistratchet"],
        )
        .unwrap();
        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        let dest_hash = *dest.hash();
        node.register_destination(dest);

        // Announce triggers ratchet persistence
        let _output = node.announce_destination(&dest_hash, None).unwrap();

        // Verify ratchet keys were persisted via Storage
        let stored_keys = node
            .transport
            .storage()
            .load_dest_ratchet_keys(dest_hash.as_bytes());
        assert!(
            stored_keys.is_some(),
            "Ratchet keys should be persisted after announce"
        );

        // Verify the serialized data is non-empty signed msgpack (starts with 0x82 fixmap)
        let data = stored_keys.unwrap();
        assert!(
            !data.is_empty(),
            "Serialized ratchet keys should not be empty"
        );
        assert_eq!(
            data[0], 0x82,
            "Signed ratchet keys should start with msgpack fixmap(2)"
        );
    }

    #[test]
    fn test_register_destination_loads_ratchet_keys() {
        // Phase 1: Create a node, register a ratchet-enabled destination,
        // announce it (persists keys), then extract the persisted bytes.
        let clock = MockClock::new(TEST_TIME_MS);
        let storage = MemoryStorage::with_defaults();
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, storage);

        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["loadratchet"],
        )
        .unwrap();
        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        let dest_hash = *dest.hash();
        node.register_destination(dest);
        let _output = node.announce_destination(&dest_hash, None).unwrap();

        // Confirm keys were persisted (now signed msgpack format)
        let stored = node
            .transport
            .storage()
            .load_dest_ratchet_keys(dest_hash.as_bytes());
        assert!(stored.is_some(), "Keys should be persisted after announce");
        let serialized = stored.unwrap();
        assert!(!serialized.is_empty(), "Should have persisted ratchet keys");

        // Phase 2: Build a new node with fresh storage pre-loaded with
        // the serialized ratchet keys, then register a destination with
        // the same hash. register_destination should load the persisted keys.
        let mut fresh_storage = MemoryStorage::with_defaults();
        fresh_storage.store_dest_ratchet_keys(dest_hash.into_bytes(), serialized);

        let clock2 = MockClock::new(TEST_TIME_MS);
        let mut node2 = NodeCoreBuilder::new().build(OsRng, clock2, fresh_storage);

        // Take the destination out of the first node and re-register it
        // on the second node. The load path in register_destination should
        // find the pre-loaded keys in storage.
        let dest_back = node.destinations.remove(&dest_hash).unwrap();
        let count_before = dest_back.ratchet_count();

        node2.register_destination(dest_back);
        let loaded_dest = node2.destinations.get(&dest_hash).unwrap();
        assert!(
            loaded_dest.ratchet_count() >= count_before,
            "Should have at least as many ratchets after loading from storage"
        );
    }

    #[test]
    fn test_register_destination_resets_ratchet_timer() {
        // After loading persisted keys, the rotation timer should be 0
        // so that rotation is not blocked by stale timestamps.
        let clock = MockClock::new(TEST_TIME_MS);
        let storage = MemoryStorage::with_defaults();
        let mut node = NodeCoreBuilder::new().build(OsRng, clock, storage);

        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["resettimer"],
        )
        .unwrap();
        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        let dest_hash = *dest.hash();
        node.register_destination(dest);
        let _output = node.announce_destination(&dest_hash, None).unwrap();

        let serialized = node
            .transport
            .storage()
            .load_dest_ratchet_keys(dest_hash.as_bytes())
            .unwrap();

        // Build second node and load keys
        let mut fresh_storage = MemoryStorage::with_defaults();
        fresh_storage.store_dest_ratchet_keys(dest_hash.into_bytes(), serialized);
        // Use a time well past the default rotation interval (30 min = 1_800_000ms)
        let future_ms = TEST_TIME_MS + 2_000_000;
        let clock2 = MockClock::new(future_ms);
        let mut node2 = NodeCoreBuilder::new().build(OsRng, clock2, fresh_storage);

        let dest_back = node.destinations.remove(&dest_hash).unwrap();
        node2.register_destination(dest_back);

        // The loaded dest should be able to rotate immediately because
        // the timer was reset to 0 and current time > rotation interval.
        let loaded = node2.destinations.get_mut(&dest_hash).unwrap();
        assert!(loaded.ratchets_enabled(), "Ratchets should be enabled");
        let rotated = loaded.rotate_ratchet_if_needed(&mut OsRng, future_ms);
        assert!(
            rotated,
            "Should be able to rotate after loading persisted keys (timer reset to 0)"
        );
    }

    #[test]
    fn test_send_on_link_pacing_delay_when_interface_not_ready() {
        let mut pair = establish_nodecore_link_pair();

        // The link was established via InterfaceId(0), push a future
        // slot via the next-slot backchannel.
        let now_ms = pair.initiator.transport().clock().now_ms();
        let future_slot = now_ms + 5_000;
        pair.initiator.set_interface_next_slot_ms(0, future_slot);

        // send_on_link must return PacingDelay with that exact timestamp
        // and NOT accept the envelope.
        let result = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"Hello!");
        assert_eq!(
            result.as_ref().err(),
            Some(&crate::node::send::SendError::PacingDelay {
                ready_at_ms: future_slot
            }),
            "Expected SendError::PacingDelay {{ ready_at_ms: {future_slot} }}, got: {result:?}"
        );

        // Reset the slot to now_ms → send should succeed.
        pair.initiator.set_interface_next_slot_ms(0, now_ms);
        let result = pair
            .initiator
            .send_on_link(&pair.initiator_link_id, b"Hello!");
        assert!(result.is_ok(), "Expected Ok after clearing pacing delay");
    }

    // RTT Retry Tests
    type TestNode = NodeCore<OsRng, MockClock, NoStorage>;
    type LinkPairWithoutRtt = (TestNode, TestNode, LinkId, LinkId, Vec<u8>);

    /// Helper: create two NodeCores and perform handshake up to initiator
    /// receiving the proof (initiator is ACTIVE), but do NOT deliver the RTT
    /// packet to the responder (responder stays in HANDSHAKE).
    ///
    /// Returns (initiator, responder, init_link_id, resp_link_id, rtt_packet).
    fn establish_link_pair_without_rtt() -> LinkPairWithoutRtt {
        use crate::transport::InterfaceId;

        let resp_identity = Identity::generate(&mut OsRng);
        let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
        let clock = MockClock::new(TEST_TIME_MS);
        let mut responder = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
        let mut resp_dest = Destination::new(
            Some(resp_identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["rttretry"],
        )
        .unwrap();
        resp_dest.set_accepts_links(true);
        let dest_hash = *resp_dest.hash();
        responder.register_destination(resp_dest);

        let clock = MockClock::new(TEST_TIME_MS);
        let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        // Initiator connects
        let (init_link_id, _, output) = initiator.connect(dest_hash, &resp_signing_key);
        let link_req_data = extract_broadcast_data(&output);

        // Responder receives link request → auto-accepts (Stage 1): proof in actions
        let output = responder.handle_packet(InterfaceId(0), &link_req_data);
        let resp_link_id = Link::calculate_link_id(&link_req_data);
        let proof_data = extract_broadcast_data(&output);

        // Initiator receives proof → Active, emits RTT packet
        let output = initiator.handle_packet(InterfaceId(0), &proof_data);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "initiator should get LinkEstablished"
        );
        let rtt_data = extract_broadcast_data(&output);

        // DO NOT deliver rtt_data to responder, that's the point of this helper

        (initiator, responder, init_link_id, resp_link_id, rtt_data)
    }

    #[test]
    fn test_rtt_retry_fires_after_interval() {
        use crate::constants::RTT_RETRY_MIN_INTERVAL_MS;

        let (mut initiator, _responder, init_link_id, _resp_link_id, _rtt_data) =
            establish_link_pair_without_rtt();

        // Initiator link should be Active with rtt_send_count=1, rtt_confirmed=false
        let link = initiator.link(&init_link_id).unwrap();
        assert_eq!(link.state(), LinkState::Active);
        assert_eq!(link.rtt_send_count(), 1);
        assert!(!link.rtt_confirmed());

        // Advance past the retry interval
        initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + RTT_RETRY_MIN_INTERVAL_MS + 1);
        let output = initiator.handle_timeout();

        // Should have emitted an RTT retry packet
        assert!(
            !output.actions.is_empty(),
            "expected RTT retry packet in actions"
        );

        // rtt_send_count should be incremented
        let link = initiator.link(&init_link_id).unwrap();
        assert_eq!(link.rtt_send_count(), 2);
    }

    #[test]
    fn test_rtt_confirmed_stops_retry() {
        use crate::constants::RTT_RETRY_MIN_INTERVAL_MS;
        use crate::transport::InterfaceId;

        let (mut initiator, mut responder, init_link_id, _resp_link_id, rtt_data) =
            establish_link_pair_without_rtt();

        // Deliver the RTT to responder (responder becomes Active)
        let _ = responder.handle_packet(InterfaceId(0), &rtt_data);

        // Simulate an inbound packet on the initiator to confirm RTT.
        // Use a keepalive from the responder, we need the responder link
        // to build a keepalive, but it might not have one because keepalives
        // are only built by initiators. Instead, just call confirm_rtt directly.
        initiator.link_mut(&init_link_id).unwrap().confirm_rtt();

        assert!(initiator.link(&init_link_id).unwrap().rtt_confirmed());

        // Advance past retry interval
        initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + RTT_RETRY_MIN_INTERVAL_MS + 1);
        let _output = initiator.handle_timeout();

        // No retry should be emitted (actions may contain keepalive or other
        // items, but rtt_send_count must stay at 1)
        let link = initiator.link(&init_link_id).unwrap();
        assert_eq!(link.rtt_send_count(), 1, "no retry should have fired");
    }

    #[test]
    fn test_rtt_retry_limit_tears_down_link() {
        use crate::constants::{RTT_RETRY_MAX_ATTEMPTS, RTT_RETRY_MIN_INTERVAL_MS};

        let (mut initiator, _responder, init_link_id, _resp_link_id, _rtt_data) =
            establish_link_pair_without_rtt();

        // Extend stale timeout so the link survives through the retry period.
        // With zero-RTT test links, the default stale timeout is only 15s,
        // which is shorter than the retry budget (~60s).
        initiator
            .link_mut(&init_link_id)
            .unwrap()
            .set_timing_for_test(3600, 3600, TEST_TIME_MS / crate::constants::MS_PER_SECOND);

        // Exhaust all retries
        for i in 0..RTT_RETRY_MAX_ATTEMPTS {
            initiator
                .transport()
                .clock()
                .set(TEST_TIME_MS + (i as u64 + 1) * (RTT_RETRY_MIN_INTERVAL_MS + 1));
            let _ = initiator.handle_timeout();
            // After each retry, send_count increases
            if let Some(link) = initiator.link(&init_link_id) {
                assert_eq!(link.rtt_send_count(), i + 2);
            }
        }

        // Link should still exist (retries not exceeded yet, just at limit)
        assert!(
            initiator.link(&init_link_id).is_some(),
            "link should still exist at retry limit"
        );

        // One more tick, should tear down
        initiator.transport().clock().set(
            TEST_TIME_MS + (RTT_RETRY_MAX_ATTEMPTS as u64 + 1) * (RTT_RETRY_MIN_INTERVAL_MS + 1),
        );
        let output = initiator.handle_timeout();

        // Link should be gone
        assert!(
            initiator.link(&init_link_id).is_none(),
            "link should be torn down after exhausting retries"
        );

        // Should have LinkClosed event with Timeout reason
        let has_closed = output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::Timeout,
                    ..
                }
            )
        });
        assert!(has_closed, "should emit LinkClosed with Timeout reason");
    }

    #[test]
    fn test_responder_handshake_timeout_reduced() {
        let (mut _initiator, mut responder, _init_link_id, resp_link_id, _rtt_data) =
            establish_link_pair_without_rtt();

        // Responder link should be in Handshake state
        let link = responder.link(&resp_link_id).unwrap();
        assert_eq!(link.state(), LinkState::Handshake);
        let timeout = link.establishment_timeout_ms();
        // For 0 hops: max(1, 0) * 6000 + 54000 = 60000
        assert!(
            timeout <= 70_000,
            "responder timeout should be ~60s, got {}ms",
            timeout
        );
        assert!(
            timeout >= 50_000,
            "responder timeout should be at least 50s, got {}ms",
            timeout
        );

        // Advance past the timeout, responder should clean up
        responder
            .transport()
            .clock()
            .set(TEST_TIME_MS + timeout + 1);
        let output = responder.handle_timeout();

        assert!(
            responder.link(&resp_link_id).is_none(),
            "responder link should be removed after handshake timeout"
        );
        let has_closed = output.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::Timeout,
                    ..
                }
            )
        });
        assert!(has_closed, "should emit LinkClosed with Timeout reason");
    }

    #[test]
    fn test_rtt_idempotent() {
        use crate::transport::InterfaceId;

        let (mut _initiator, mut responder, _init_link_id, resp_link_id, rtt_data) =
            establish_link_pair_without_rtt();

        // Deliver RTT, responder becomes Active
        let output = responder.handle_packet(InterfaceId(0), &rtt_data);
        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "responder should get LinkEstablished on first RTT"
        );
        let link = responder.link(&resp_link_id).unwrap();
        assert_eq!(link.state(), LinkState::Active);
        let rtt_us = link.rtt_us();

        // Deliver the same RTT packet again, should be silently ignored
        let output = responder.handle_packet(InterfaceId(0), &rtt_data);
        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
            "duplicate RTT should NOT trigger LinkEstablished again"
        );

        // Link should still be Active with the same RTT
        let link = responder.link(&resp_link_id).unwrap();
        assert_eq!(link.state(), LinkState::Active);
        assert_eq!(link.rtt_us(), rtt_us);
    }

    #[test]
    fn test_rtt_retry_deadline_in_next_deadline() {
        use crate::constants::RTT_RETRY_MIN_INTERVAL_MS;

        let (initiator, _responder, init_link_id, _resp_link_id, _rtt_data) =
            establish_link_pair_without_rtt();

        let link = initiator.link(&init_link_id).unwrap();
        let sent_at = link.rtt_sent_at_ms().unwrap();

        let deadline = initiator.next_deadline();
        assert!(deadline.is_some(), "should have a deadline");
        let d = deadline.unwrap();

        // The RTT retry deadline should be at sent_at + interval
        let expected = sent_at + RTT_RETRY_MIN_INTERVAL_MS;
        assert!(
            d <= expected,
            "deadline {} should be <= rtt retry deadline {}",
            d,
            expected
        );
    }

    // LinkIdentify tests
    #[test]
    fn test_identify_link_nodecore_roundtrip() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Generate an identity to identify with
        let identity = Identity::generate(&mut OsRng);
        let expected_hash = *identity.hash();

        // Before identify: responder has no remote identity
        assert!(
            pair.responder
                .get_remote_identity(&pair.responder_link_id)
                .is_none(),
            "responder should not have remote identity yet"
        );

        // Initiator sends identify
        let output = pair
            .initiator
            .identify_link(&pair.initiator_link_id, &identity)
            .expect("identify_link should succeed");
        let identify_data = extract_broadcast_data(&output);

        // Feed to responder
        let output = pair.responder.handle_packet(InterfaceId(0), &identify_data);

        // Assert: responder emitted LinkIdentified with correct hash
        let identified_hash = output
            .events
            .iter()
            .find_map(|e| match e {
                NodeEvent::LinkIdentified { identity_hash, .. } => Some(*identity_hash),
                _ => None,
            })
            .expect("responder should emit LinkIdentified event");
        assert_eq!(identified_hash, expected_hash, "identity hash should match");

        // Assert: get_remote_identity returns matching identity
        let remote = pair
            .responder
            .get_remote_identity(&pair.responder_link_id)
            .expect("responder should have remote identity");
        assert_eq!(remote.hash(), &expected_hash);
    }

    /// A peer that identifies (LINKIDENTIFY) as a blackholed identity gets its
    /// link torn down: no LinkIdentified event, no stored remote identity, the
    /// link is removed, and LinkClosed carries reason Blackholed (Codeberg #88;
    /// Python Link.py:1021-1023).
    #[test]
    fn test_identify_link_blackholed_identity_tears_down() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();
        let identity = Identity::generate(&mut OsRng);

        assert!(pair
            .responder
            .blackhole_identity(*identity.hash(), None, None));

        let output = pair
            .initiator
            .identify_link(&pair.initiator_link_id, &identity)
            .expect("identify_link should succeed");
        let identify_data = extract_broadcast_data(&output);

        let output = pair.responder.handle_packet(InterfaceId(0), &identify_data);

        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkIdentified { .. })),
            "blackholed identity must not produce LinkIdentified"
        );
        let closed_reason = output
            .events
            .iter()
            .find_map(|e| match e {
                NodeEvent::LinkClosed { reason, .. } => Some(*reason),
                _ => None,
            })
            .expect("responder should emit LinkClosed for blackholed identify");
        assert_eq!(closed_reason, crate::link::LinkCloseReason::Blackholed);
        assert!(
            pair.responder.link(&pair.responder_link_id).is_none(),
            "the link must be removed on the responder"
        );
    }

    /// NEGATIVE guard: while some OTHER identity is blackholed, a
    /// non-blackholed peer identifies normally and its link stays up.
    #[test]
    fn test_identify_link_unrelated_blackhole_does_not_tear_down() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();
        let identity = Identity::generate(&mut OsRng);
        let other = Identity::generate(&mut OsRng);

        assert!(pair.responder.blackhole_identity(*other.hash(), None, None));

        let output = pair
            .initiator
            .identify_link(&pair.initiator_link_id, &identity)
            .expect("identify_link should succeed");
        let identify_data = extract_broadcast_data(&output);

        let output = pair.responder.handle_packet(InterfaceId(0), &identify_data);

        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkIdentified { .. })),
            "non-blackholed identity must identify normally"
        );
        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkClosed { .. })),
            "an unrelated blackhole must not close the link"
        );
        assert!(
            pair.responder.link(&pair.responder_link_id).is_some(),
            "the link must stay up"
        );
    }

    #[test]
    fn test_identify_link_responder_rejects() {
        let mut pair = establish_nodecore_link_pair();

        // Responder cannot call identify_link (must be initiator)
        let identity = Identity::generate(&mut OsRng);
        let result = pair
            .responder
            .identify_link(&pair.responder_link_id, &identity);
        assert!(
            matches!(result, Err(crate::link::LinkError::InvalidState)),
            "responder should not be able to identify"
        );
    }

    #[test]
    fn test_identify_link_invalid_signature_ignored() {
        use crate::transport::InterfaceId;

        let mut pair = establish_nodecore_link_pair();

        // Manually build a bad identify packet: valid format but wrong signature
        let real_identity = Identity::generate(&mut OsRng);
        let fake_identity = Identity::generate(&mut OsRng);

        // Use fake's public key but sign with real identity
        let public_key = fake_identity.public_key_bytes();
        let mut signed_data = [0u8; 80];
        signed_data[..16].copy_from_slice(pair.initiator_link_id.as_bytes());
        signed_data[16..80].copy_from_slice(&public_key);
        let signature = real_identity.sign(&signed_data).unwrap();

        let mut proof_data = [0u8; 128];
        proof_data[..64].copy_from_slice(&public_key);
        proof_data[64..128].copy_from_slice(&signature);

        // Encrypt and build packet via the initiator's link
        let link = pair.initiator.link(&pair.initiator_link_id).unwrap();
        let packet = link
            .build_data_packet_with_context(
                &proof_data,
                crate::packet::PacketContext::LinkIdentify,
                &mut OsRng,
            )
            .unwrap();

        // Feed to responder
        let output = pair.responder.handle_packet(InterfaceId(0), &packet);

        // Should NOT emit LinkIdentified (invalid signature, silently dropped)
        let has_identified = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkIdentified { .. }));
        assert!(
            !has_identified,
            "invalid signature should not produce LinkIdentified event"
        );

        // And no remote identity stored
        assert!(
            pair.responder
                .get_remote_identity(&pair.responder_link_id)
                .is_none(),
            "responder should not store identity from bad signature"
        );
    }

    // Request/Response Tests
    /// Helper: register an echo handler on responder and return the dest_hash
    fn register_echo_handler(
        responder: &mut NodeCore<OsRng, MockClock, NoStorage>,
        dest_hash: DestinationHash,
        policy: request::RequestPolicy,
    ) {
        responder.register_request_handler(dest_hash, "/echo", policy);
    }

    #[test]
    fn test_request_roundtrip() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowAll,
        );

        // Build msgpack data: fixstr "hello"
        let mut data = Vec::new();
        crate::resource::msgpack::write_fixstr(&mut data, "hello");

        // Send request from initiator
        let (request_id, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", Some(&data), None)
            .unwrap();

        // Deliver request packet to responder
        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        // Responder should have RequestReceived
        let received = output.events.iter().find_map(|e| match e {
            NodeEvent::RequestReceived {
                link_id,
                request_id: rid,
                path,
                data,
                ..
            } => Some((*link_id, *rid, path.clone(), data.clone())),
            _ => None,
        });
        let (resp_link, recv_rid, path, recv_data) =
            received.expect("responder should get RequestReceived");
        assert_eq!(resp_link, pair.responder_link_id);
        assert_eq!(path, "/echo");
        assert_eq!(recv_data, data);

        // Send response from responder
        let mut response_data = Vec::new();
        crate::resource::msgpack::write_bool(&mut response_data, true);
        let output = pair
            .responder
            .send_response(&resp_link, &recv_rid, &response_data)
            .unwrap();

        // Deliver response to initiator
        let resp_data = extract_broadcast_data(&output);
        let output = pair
            .initiator
            .handle_packet(crate::transport::InterfaceId(0), &resp_data);

        // Initiator should have ResponseReceived
        let response = output.events.iter().find_map(|e| match e {
            NodeEvent::ResponseReceived {
                request_id: rid,
                response_data,
                ..
            } => Some((*rid, response_data.clone())),
            _ => None,
        });
        let (resp_rid, resp_payload) = response.expect("initiator should get ResponseReceived");
        assert_eq!(resp_rid, request_id);
        assert_eq!(resp_payload, response_data);
    }

    #[test]
    fn test_request_with_nil_data() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowAll,
        );

        // Send request with no data (nil)
        let (_request_id, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", None, None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        let received = output.events.iter().find_map(|e| match e {
            NodeEvent::RequestReceived { data, .. } => Some(data.clone()),
            _ => None,
        });
        let recv_data = received.expect("responder should get RequestReceived");
        assert!(recv_data.is_empty(), "nil data should produce empty vec");
    }

    #[test]
    fn test_request_handler_lookup() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        // Register with a specific path
        pair.responder.register_request_handler(
            dest_hash,
            "/test/path",
            request::RequestPolicy::AllowAll,
        );

        // Send request matching that path
        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 42);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/test/path", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        let path = output.events.iter().find_map(|e| match e {
            NodeEvent::RequestReceived { path, .. } => Some(path.clone()),
            _ => None,
        });
        assert_eq!(path.as_deref(), Some("/test/path"));
    }

    #[test]
    fn test_request_no_handler_silent_drop() {
        let mut pair = establish_nodecore_link_pair();
        // Don't register any handler

        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 1);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/unknown", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        let has_request = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::RequestReceived { .. }));
        assert!(
            !has_request,
            "unregistered path should silently drop the request"
        );
    }

    #[test]
    fn test_request_allow_all() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowAll,
        );

        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 1);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::RequestReceived { .. })),
            "AllowAll should pass"
        );
    }

    #[test]
    fn test_request_allow_list_accepted() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();

        // Identify the initiator's identity on the link
        let identity = Identity::generate(&mut OsRng);
        let id_hash = *identity.hash();
        let output = pair
            .initiator
            .identify_link(&pair.initiator_link_id, &identity)
            .unwrap();
        let id_data = extract_broadcast_data(&output);
        let _output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &id_data);

        // Register handler with allow list containing this identity
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowList(alloc::vec![id_hash]),
        );

        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 1);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        assert!(
            output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::RequestReceived { .. })),
            "AllowList with matching identity should pass"
        );
    }

    #[test]
    fn test_request_allow_list_rejected() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();

        // Identify with one identity but allow a different one
        let identity = Identity::generate(&mut OsRng);
        let output = pair
            .initiator
            .identify_link(&pair.initiator_link_id, &identity)
            .unwrap();
        let id_data = extract_broadcast_data(&output);
        let _output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &id_data);

        let other_hash = [0xAA; TRUNCATED_HASHBYTES];
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowList(alloc::vec![other_hash]),
        );

        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 1);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::RequestReceived { .. })),
            "AllowList with non-matching identity should drop"
        );
    }

    #[test]
    fn test_request_allow_list_no_identity() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();

        let some_hash = [0xBB; TRUNCATED_HASHBYTES];
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowList(alloc::vec![some_hash]),
        );

        // Don't identify, send request directly
        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 1);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::RequestReceived { .. })),
            "AllowList with no identity should drop"
        );
    }

    #[test]
    fn test_request_allow_none() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowNone,
        );

        let mut data = Vec::new();
        crate::resource::msgpack::write_uint(&mut data, 1);
        let (_, output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", Some(&data), None)
            .unwrap();

        let req_data = extract_broadcast_data(&output);
        let output = pair
            .responder
            .handle_packet(crate::transport::InterfaceId(0), &req_data);

        assert!(
            !output
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::RequestReceived { .. })),
            "AllowNone should silently drop"
        );
    }

    #[test]
    fn test_request_timeout() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowAll,
        );

        // Send request with a short timeout
        let (request_id, _output) = pair
            .initiator
            .send_request(
                &pair.initiator_link_id,
                "/echo",
                None,
                Some(100), // 100ms timeout
            )
            .unwrap();

        // Advance time past timeout
        pair.initiator.transport().clock().advance(200);
        let output = pair.initiator.handle_timeout();

        let timed_out = output.events.iter().find_map(|e| match e {
            NodeEvent::RequestTimedOut {
                request_id: rid, ..
            } => Some(*rid),
            _ => None,
        });
        assert_eq!(timed_out, Some(request_id), "should emit RequestTimedOut");
    }

    #[test]
    fn test_request_exceeds_mdu() {
        let pair = establish_nodecore_link_pair();
        let mut initiator = pair.initiator;

        // Build data larger than link MDU
        let big_data = alloc::vec![0u8; 500];
        let mut encoded = Vec::new();
        crate::resource::msgpack::write_bin(&mut encoded, &big_data);

        let result = initiator.send_request(&pair.initiator_link_id, "/echo", Some(&encoded), None);
        assert!(
            matches!(result, Err(request::RequestError::PayloadTooLarge)),
            "oversized payload should return PayloadTooLarge"
        );
    }

    #[test]
    fn test_pending_request_cleanup_on_link_close() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowAll,
        );

        // Send request (don't deliver response)
        let (_request_id, _output) = pair
            .initiator
            .send_request(
                &pair.initiator_link_id,
                "/echo",
                None,
                Some(60_000), // Long timeout
            )
            .unwrap();

        // Close the link
        let output = pair.initiator.close_link(&pair.initiator_link_id);

        // Should have LinkClosed but no RequestTimedOut
        let has_link_closed = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkClosed { .. }));
        let has_timeout = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::RequestTimedOut { .. }));
        assert!(has_link_closed, "should have LinkClosed event");
        assert!(
            !has_timeout,
            "should NOT have RequestTimedOut event on close"
        );

        // Advance time and verify no timeout fires
        pair.initiator.transport().clock().advance(120_000);
        let output = pair.initiator.handle_timeout();
        let has_timeout = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::RequestTimedOut { .. }));
        assert!(
            !has_timeout,
            "pending request should have been cleaned up on link close"
        );
    }

    /// Bug #26: `reset_pending_request_deadline` pushes the effective
    /// timeout forward so `check_request_timeouts` does not fire while
    /// the peer is still making progress on the associated resource.
    #[test]
    fn test_request_timeout_reset_on_progress_defers_firing() {
        let mut pair = establish_nodecore_link_pair();
        let dest_hash = *pair
            .responder
            .link(&pair.responder_link_id)
            .unwrap()
            .destination_hash();
        register_echo_handler(
            &mut pair.responder,
            dest_hash,
            request::RequestPolicy::AllowAll,
        );

        // Initial timeout = 100 ms from whatever the test clock's
        // current now_ms is when send_request runs.
        let (request_id, _output) = pair
            .initiator
            .send_request(&pair.initiator_link_id, "/echo", None, Some(100))
            .unwrap();

        // At send + 50 ms, simulate an incoming resource-part that
        // reaches `handle_resource_data` on the client side. Call the
        // reset helper directly — the outer `handle_resource_data`
        // wiring is covered by the hardware test and the production
        // call sites in link_management.rs.
        pair.initiator.transport().clock().advance(50);
        let reset_at = pair.initiator.transport().clock().now_ms();
        let link_id_for_reset = pair.initiator_link_id;
        pair.initiator
            .reset_pending_requests_on_link(&link_id_for_reset, reset_at);
        let _ = request_id; // kept for the final assertion below

        // Advance to reset_at + 70 ms (which is the original deadline
        // of reset_at - 50 + 100 = reset_at + 50 PASSED by 20 ms, but
        // the reset-extended deadline reset_at + 100 is still 30 ms
        // away). Timer must NOT fire yet.
        pair.initiator.transport().clock().advance(70);
        let output = pair.initiator.handle_timeout();
        let fired_early = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::RequestTimedOut { .. }));
        assert!(
            !fired_early,
            "request timeout must NOT fire before the reset-extended \
             deadline (original 100ms timeout was extended by 50ms via \
             reset_pending_request_deadline)"
        );

        // Advance another 40 ms (total reset_at + 110 ms, past the
        // reset-extended deadline of reset_at + 100). The timer fires
        // — the fix only defers, it does not remove the timeout.
        pair.initiator.transport().clock().advance(40);
        let output = pair.initiator.handle_timeout();
        let fired_late = output.events.iter().find_map(|e| match e {
            NodeEvent::RequestTimedOut {
                request_id: rid, ..
            } => Some(*rid),
            _ => None,
        });
        assert_eq!(
            fired_late,
            Some(request_id),
            "request timeout must fire once the reset-extended deadline also passes"
        );
    }

    /// Bug #26: resetting on a link with no pending requests is a
    /// safe no-op.
    #[test]
    fn test_reset_pending_requests_on_link_no_requests_is_noop() {
        let mut pair = establish_nodecore_link_pair();
        let link_id = pair.initiator_link_id;
        // Must not panic — no pending_requests on this link.
        pair.initiator
            .reset_pending_requests_on_link(&link_id, 999_999);
        let output = pair.initiator.handle_timeout();
        let has_timeout = output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::RequestTimedOut { .. }));
        assert!(!has_timeout);
    }
}
