//! Node events - unified event system for all node operations
//!
//! This module provides a unified [`NodeEvent`] enum that combines events from
//! transport, link management, and channels into a single stream, simplifying
//! event handling for application developers.

use alloc::string::String;
use alloc::vec::Vec;

use crate::announce::ReceivedAnnounce;
use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::DestinationHash;
use crate::link::{LinkCloseReason, LinkId, PeerKeys};

/// Unified event enum for all node operations
///
/// This combines events from transport, link management, and channels into a
/// single stream that applications can handle uniformly.
#[derive(Debug)]
#[non_exhaustive]
pub enum NodeEvent {
    // Path Discovery Events
    /// A new announce was received and validated
    AnnounceReceived {
        /// Parsed announce data
        announce: ReceivedAnnounce,
        /// Interface it arrived on
        interface_index: usize,
    },

    /// Path to a destination was found (from announce)
    PathFound {
        /// Destination hash
        destination_hash: DestinationHash,
        /// Number of hops
        hops: u8,
        /// Interface index
        interface_index: usize,
    },

    /// A remote node requested the path to one of our local destinations
    ///
    /// This is informational, the transport layer already handles
    /// auto-re-announce internally (Transport.py:1843-1853).
    /// No application action is required.
    PathRequestReceived {
        /// The destination hash that was requested
        destination_hash: DestinationHash,
    },

    /// Path to a destination expired
    PathLost {
        /// Destination hash
        destination_hash: DestinationHash,
    },

    // Single-Packet Events
    /// Incoming single-packet data (not via a Link)
    PacketReceived {
        /// The destination hash that received this packet
        destination: DestinationHash,
        /// The decrypted data
        data: Vec<u8>,
        /// Interface it arrived on
        interface_index: usize,
    },

    /// Delivery confirmation for a sent packet
    PacketDeliveryConfirmed {
        /// The packet hash identifying the sent packet
        packet_hash: [u8; TRUNCATED_HASHBYTES],
    },

    /// Delivery failed for a sent packet
    DeliveryFailed {
        /// The packet hash identifying the sent packet
        packet_hash: [u8; TRUNCATED_HASHBYTES],
        /// The error that occurred
        error: DeliveryError,
    },

    // Link Events
    /// Incoming link request (Link establishment request)
    LinkRequest {
        /// The link ID
        link_id: LinkId,
        /// The destination that received the request
        destination_hash: DestinationHash,
        /// Peer's public keys
        peer_keys: PeerKeys,
    },

    /// Link established (handshake completed)
    LinkEstablished {
        /// The link ID
        link_id: LinkId,
        /// Whether we initiated this link
        is_initiator: bool,
    },

    /// Message received on a link via the Channel multiplexer
    ///
    /// Emitted when the peer sends a channel message (with `PacketContext::Channel`).
    /// Most link-based applications use this variant for structured message exchange.
    MessageReceived {
        /// The link ID
        link_id: LinkId,
        /// Message type identifier
        msgtype: u16,
        /// Message sequence number
        sequence: u16,
        /// The message data
        data: Vec<u8>,
    },

    /// Raw data received on a link without Channel framing
    ///
    /// Emitted when the peer sends a plain link data packet (not via Channel).
    /// This is the lower-level variant, use [`MessageReceived`](NodeEvent::MessageReceived)
    /// for channel-multiplexed messaging.
    LinkDataReceived {
        /// The link ID
        link_id: LinkId,
        /// The decrypted data
        data: Vec<u8>,
    },

    /// Link became stale (no activity for too long)
    LinkStale {
        /// The link ID
        link_id: LinkId,
    },

    /// Link recovered from stale state (traffic resumed)
    LinkRecovered {
        /// The link ID
        link_id: LinkId,
    },

    /// Observability event, a channel message was retransmitted due to timeout.
    ///
    /// No application action is required. Useful for logging and diagnostics.
    ChannelRetransmit {
        /// The link ID
        link_id: LinkId,
        /// Message sequence number
        sequence: u16,
        /// Retry attempt number (2 = first retry, etc.)
        tries: u8,
    },

    /// The remote peer has proven their identity on a link.
    ///
    /// Emitted on the responder (non-initiator) side when the initiator sends
    /// a valid LINKIDENTIFY packet. The full identity can be queried via
    /// `get_remote_identity(link_id)`.
    LinkIdentified {
        /// The link on which identification occurred
        link_id: LinkId,
        /// Truncated hash of the identified identity (16 bytes)
        identity_hash: [u8; TRUNCATED_HASHBYTES],
    },

    /// Link closed
    LinkClosed {
        /// The link ID
        link_id: LinkId,
        /// Why the link was closed
        reason: LinkCloseReason,
        /// Whether we initiated this link
        is_initiator: bool,
        /// The destination hash this link was to
        destination_hash: DestinationHash,
    },

    // Proof Events
    /// Application should decide whether to prove this packet
    ///
    /// Emitted when a packet is received at a destination with `ProofStrategy::App`.
    /// Call `NodeCore::send_proof()` if the application decides to prove delivery.
    /// Not emitted for `ProofStrategy::All` (handled automatically by the library).
    PacketProofRequested {
        /// Full SHA256 hash of the packet to potentially prove
        packet_hash: [u8; 32],
        /// Destination that received the packet
        destination_hash: DestinationHash,
    },

    /// Application should decide whether to prove this link data packet
    ///
    /// Emitted when data is received on a link whose destination has
    /// `ProofStrategy::App`. Call `send_data_proof()` to confirm delivery.
    LinkProofRequested {
        /// The link that received the data
        link_id: LinkId,
        /// Full SHA256 hash of the packet to potentially prove
        packet_hash: [u8; 32],
    },

    /// Delivery confirmation for a link data packet (PROVE_ALL)
    ///
    /// Emitted when a proof is received for a data packet sent on a link.
    /// This confirms the peer received and decrypted the data.
    LinkDeliveryConfirmed {
        /// The link that sent the data
        link_id: LinkId,
        /// Full SHA256 hash of the delivered packet
        packet_hash: [u8; 32],
    },

    // Resource Events
    /// Resource advertisement received (for AcceptApp strategy).
    /// Application should call `accept_resource()` or `reject_resource()`.
    ResourceAdvertised {
        /// The link that received the advertisement
        link_id: LinkId,
        /// Hash identifying this resource
        resource_hash: [u8; 32],
        /// Total encrypted transfer size
        transfer_size: u64,
        /// Original uncompressed data size
        data_size: u64,
    },

    /// Resource transfer started (receiver accepted, first REQ sent).
    ResourceTransferStarted {
        /// The link carrying the transfer
        link_id: LinkId,
        /// Hash identifying this resource
        resource_hash: [u8; 32],
        /// True if we are the sender, false if receiver
        is_sender: bool,
    },

    /// Progress update during resource transfer.
    /// Emitted each time a part is sent (sender) or a REQ is sent (receiver).
    ResourceProgress {
        /// The link carrying the transfer
        link_id: LinkId,
        /// Hash identifying this resource
        resource_hash: [u8; 32],
        /// Progress as a fraction 0.0..1.0
        progress: f32,
        /// Total encrypted transfer size in bytes
        transfer_size: u64,
        /// Original uncompressed data size in bytes
        data_size: u64,
        /// True if we are the sender
        is_sender: bool,
    },

    /// Resource transfer completed successfully.
    ///
    /// For multi-segment resources, this fires once per segment.
    /// `segment_index` and `total_segments` indicate position within
    /// the overall transfer. Metadata is only present in segment 1.
    ResourceCompleted {
        /// The link that carried the transfer
        link_id: LinkId,
        /// Hash identifying this resource
        resource_hash: [u8; 32],
        /// Assembled data (receiver only; empty Vec for sender)
        data: Vec<u8>,
        /// Extracted metadata (receiver only; None for sender).
        /// Contains raw msgpack-encoded bytes as received on the wire.
        /// Decode with a msgpack library to obtain the original value.
        /// Only present in segment 1 of multi-segment transfers.
        metadata: Option<Vec<u8>>,
        /// True if we were the sender
        is_sender: bool,
        /// Segment index (1-based)
        segment_index: u32,
        /// Total number of segments
        total_segments: u32,
    },

    /// Resource transfer failed.
    ResourceFailed {
        /// The link that carried the transfer
        link_id: LinkId,
        /// Hash identifying this resource
        resource_hash: [u8; 32],
        /// The error that caused the failure
        error: crate::resource::ResourceError,
        /// True if we were the sender
        is_sender: bool,
    },

    // Request/Response Events
    /// Request received on a link for a registered handler.
    /// Call `send_response()` with the request_id to reply.
    RequestReceived {
        /// The link that received the request
        link_id: LinkId,
        /// Unique request identifier (truncated packet hash)
        request_id: [u8; TRUNCATED_HASHBYTES],
        /// The request path string
        path: String,
        /// Truncated hash of the path
        path_hash: [u8; TRUNCATED_HASHBYTES],
        /// Raw msgpack-encoded request data (or empty for nil)
        data: Vec<u8>,
        /// Timestamp from requester (seconds since epoch)
        requested_at: f64,
    },

    /// Response received for a previously sent request.
    ResponseReceived {
        /// The link that received the response
        link_id: LinkId,
        /// The request identifier matching the original request
        request_id: [u8; TRUNCATED_HASHBYTES],
        /// Raw msgpack-encoded response data
        response_data: Vec<u8>,
    },

    /// Pending request timed out without receiving a response.
    RequestTimedOut {
        /// The link the request was sent on
        link_id: LinkId,
        /// The request identifier that timed out
        request_id: [u8; TRUNCATED_HASHBYTES],
    },

    // Interface Events
    /// An interface went offline
    InterfaceDown(usize),
}

impl NodeEvent {
    /// Stable string identifier for the event variant.
    ///
    /// Used as a scalar token in structured tracing fields where a full
    /// `Debug` rendering would break Stage-6 event-log line tokenisation
    /// (whitespace) or expose volatile internal data. The returned string
    /// has no whitespace and is stable across releases — adding a new
    /// `NodeEvent` variant requires extending the match.
    pub fn variant_name(&self) -> &'static str {
        match self {
            NodeEvent::AnnounceReceived { .. } => "AnnounceReceived",
            NodeEvent::PathFound { .. } => "PathFound",
            NodeEvent::PathRequestReceived { .. } => "PathRequestReceived",
            NodeEvent::PathLost { .. } => "PathLost",
            NodeEvent::PacketReceived { .. } => "PacketReceived",
            NodeEvent::PacketDeliveryConfirmed { .. } => "PacketDeliveryConfirmed",
            NodeEvent::DeliveryFailed { .. } => "DeliveryFailed",
            NodeEvent::LinkRequest { .. } => "LinkRequest",
            NodeEvent::LinkEstablished { .. } => "LinkEstablished",
            NodeEvent::MessageReceived { .. } => "MessageReceived",
            NodeEvent::LinkDataReceived { .. } => "LinkDataReceived",
            NodeEvent::LinkStale { .. } => "LinkStale",
            NodeEvent::LinkRecovered { .. } => "LinkRecovered",
            NodeEvent::ChannelRetransmit { .. } => "ChannelRetransmit",
            NodeEvent::LinkIdentified { .. } => "LinkIdentified",
            NodeEvent::LinkClosed { .. } => "LinkClosed",
            NodeEvent::PacketProofRequested { .. } => "PacketProofRequested",
            NodeEvent::LinkProofRequested { .. } => "LinkProofRequested",
            NodeEvent::LinkDeliveryConfirmed { .. } => "LinkDeliveryConfirmed",
            NodeEvent::ResourceAdvertised { .. } => "ResourceAdvertised",
            NodeEvent::ResourceTransferStarted { .. } => "ResourceTransferStarted",
            NodeEvent::ResourceProgress { .. } => "ResourceProgress",
            NodeEvent::ResourceCompleted { .. } => "ResourceCompleted",
            NodeEvent::ResourceFailed { .. } => "ResourceFailed",
            NodeEvent::RequestReceived { .. } => "RequestReceived",
            NodeEvent::ResponseReceived { .. } => "ResponseReceived",
            NodeEvent::RequestTimedOut { .. } => "RequestTimedOut",
            NodeEvent::InterfaceDown(_) => "InterfaceDown",
        }
    }
}

/// Reason why a delivery failed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeliveryError {
    /// Delivery timed out without proof
    Timeout,
    /// Link failed during delivery
    LinkFailed,
}

impl core::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DeliveryError::Timeout => write!(f, "delivery timed out"),
            DeliveryError::LinkFailed => write!(f, "link failed during delivery"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delivery_error_variants() {
        // Ensure all variants are copyable
        let err = DeliveryError::Timeout;
        let err2 = err;
        assert_eq!(err, err2);
    }

    #[test]
    fn variant_name_no_whitespace() {
        // Stage-6 event-log fields tokenise on whitespace; the
        // dropped_event_type field carries this string verbatim, so no
        // variant_name may contain whitespace, '=', or non-printable
        // bytes. A constructable sample of every variant would need
        // every payload type — instead we sanity-check the obvious
        // ones plus the contract on a unit-payload variant that is
        // always constructable.
        let e = NodeEvent::InterfaceDown(0);
        let name = e.variant_name();
        assert_eq!(name, "InterfaceDown");
        assert!(!name.contains(char::is_whitespace));
        assert!(!name.contains('='));
        assert!(name.chars().all(|c| !c.is_control()));
    }
}
