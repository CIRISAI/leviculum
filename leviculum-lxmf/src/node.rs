//! Sans-I/O integration between LXMF delivery and [`leviculum_core::NodeCore`].
//!
//! LXMF deliberately uses raw Reticulum packets on both SINGLE destinations
//! and Links. It does not use the Link Channel multiplexer. Small direct
//! messages therefore go through `NodeCore::send_packet_on_link`, while large
//! direct messages use the Resource engine.
//!
//! This adapter owns only LXMF-specific correlation state. Network I/O remains
//! represented by [`TickOutput`], exactly as it is in `leviculum-core`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_core::resource::{ResourceError, ResourceStrategy, RESOURCE_MAX_EFFICIENT_SIZE};
use leviculum_core::{
    Clock, DeliveryError, Destination, DestinationError, DestinationHash, DestinationType,
    Direction, Identity, LinkCloseReason, LinkId, NodeCore, NodeEvent, ProofStrategy, SendError,
    Storage, TickOutput,
};
use rand_core::CryptoRngCore;

use crate::constants::{
    DESTINATION_LENGTH, ENCRYPTED_PACKET_MAX_CONTENT, LINK_PACKET_MAX_CONTENT, LXMF_OVERHEAD,
};
use crate::{DeliveryMethod, Message, MessageError};

/// LXMF Reticulum application name.
pub const APP_NAME: &str = "lxmf";

/// Aspect used for an LXMF delivery destination.
pub const DELIVERY_ASPECT: &str = "delivery";

/// Largest opportunistic on-air plaintext accepted by Python LXMF.
pub const OPPORTUNISTIC_PACKET_MDU: usize =
    ENCRYPTED_PACKET_MAX_CONTENT + LXMF_OVERHEAD - DESTINATION_LENGTH;

/// Largest fully packed LXMF message sent as one raw Link packet.
pub const DIRECT_PACKET_MDU: usize = LINK_PACKET_MAX_CONTENT + LXMF_OVERHEAD;

/// Runtime policy for the NodeCore adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LxmfNodeConfig {
    /// Maximum uncompressed size of one incoming LXMF Resource.
    ///
    /// `None` preserves Python's unlimited default. Embedded applications
    /// should set this as well as NodeCore's global incoming Resource limit.
    pub max_incoming_resource_size: Option<u64>,
    /// Ask Reticulum's Resource engine to use BZ2 when beneficial.
    pub auto_compress_resources: bool,
}

impl Default for LxmfNodeConfig {
    fn default() -> Self {
        Self {
            max_incoming_resource_size: None,
            auto_compress_resources: true,
        }
    }
}

/// Reticulum representation selected for an LXMF message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryRepresentation {
    OpportunisticPacket,
    DirectPacket,
    DirectResource,
}

/// Stable identifier returned by the underlying Reticulum send operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionId {
    /// Truncated packet receipt hash used by SINGLE packet delivery.
    Packet([u8; 16]),
    /// Full packet hash used by raw Link data proofs.
    LinkPacket([u8; 32]),
    /// Resource hash returned when the transfer is created.
    Resource([u8; 32]),
}

/// State returned while preparing a direct link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectLinkState {
    Ready(LinkId),
    /// A link request was started by this call.
    Started(LinkId),
    /// A link request started by an earlier call is still pending.
    Connecting(LinkId),
    PathRequested,
}

/// Why an outbound message did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryFailure {
    Opportunistic(DeliveryError),
    DirectPacketTimeout,
    LinkClosed(LinkCloseReason),
    Resource(ResourceError),
}

/// Why an inbound transport payload was not emitted as an LXMF message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundRejection {
    Message(MessageError),
    WrongDestination,
    ResourceSequence,
    ResourceTooLarge,
    Resource(ResourceError),
}

/// LXMF-level events derived from [`NodeEvent`] values.
#[derive(Debug, Clone, PartialEq)]
pub enum LxmfNodeEvent {
    PeerAnnounced {
        destination: DestinationHash,
    },
    PathRequested {
        destination: DestinationHash,
    },
    DirectLinkEstablished {
        destination: Option<DestinationHash>,
        link_id: LinkId,
        is_initiator: bool,
    },
    Submitted {
        message_id: [u8; 32],
        method: DeliveryMethod,
        representation: DeliveryRepresentation,
        submission: SubmissionId,
    },
    Progress {
        message_id: [u8; 32],
        progress: f32,
    },
    Delivered {
        message_id: [u8; 32],
    },
    DeliveryFailed {
        message_id: [u8; 32],
        reason: DeliveryFailure,
    },
    MessageReceived(Message),
    InboundRejected {
        method: DeliveryMethod,
        reason: InboundRejection,
    },
}

/// Combined output of one LXMF adapter operation.
#[derive(Debug, Default)]
#[must_use = "LXMF output contains NodeCore actions and application events"]
pub struct LxmfNodeOutput {
    pub core: TickOutput,
    pub events: Vec<LxmfNodeEvent>,
}

impl LxmfNodeOutput {
    pub fn merge(&mut self, other: Self) {
        self.core.merge(other.core);
        self.events.extend(other.events);
    }
}

/// Synchronous adapter errors. Inbound wire errors are reported as
/// [`LxmfNodeEvent::InboundRejected`] instead, so malformed network traffic
/// never aborts an event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LxmfNodeError {
    InvalidDeliveryDestination,
    Destination(DestinationError),
    Message(MessageError),
    UnsupportedMethod,
    UnknownPeer,
    DirectLinkUnavailable,
    Send(SendError),
    Resource(ResourceError),
    ProofFailed,
    IdentityUnavailable,
}

impl From<DestinationError> for LxmfNodeError {
    fn from(value: DestinationError) -> Self {
        Self::Destination(value)
    }
}

impl From<MessageError> for LxmfNodeError {
    fn from(value: MessageError) -> Self {
        Self::Message(value)
    }
}

impl From<SendError> for LxmfNodeError {
    fn from(value: SendError) -> Self {
        Self::Send(value)
    }
}

impl From<ResourceError> for LxmfNodeError {
    fn from(value: ResourceError) -> Self {
        Self::Resource(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingLinkPacket {
    message_id: [u8; 32],
    link_id: LinkId,
}

#[derive(Debug, Clone, Copy)]
struct PendingResource {
    message_id: [u8; 32],
    current_resource_hash: [u8; 32],
    completed_size: u64,
    current_size: u64,
    total_size: u64,
}

#[derive(Debug)]
struct IncomingResourceAssembly {
    next_segment: u32,
    total_segments: u32,
    data: Vec<u8>,
}

/// LXMF delivery state layered directly on a [`NodeCore`].
pub struct LxmfNode {
    delivery_destination: DestinationHash,
    config: LxmfNodeConfig,
    wanted_links: BTreeSet<DestinationHash>,
    pending_links: BTreeMap<DestinationHash, LinkId>,
    link_destinations: BTreeMap<LinkId, DestinationHash>,
    direct_links: BTreeMap<DestinationHash, LinkId>,
    lxmf_links: BTreeSet<LinkId>,
    identified_links: BTreeSet<LinkId>,
    packet_receipts: BTreeMap<[u8; 16], [u8; 32]>,
    link_packet_receipts: BTreeMap<[u8; 32], PendingLinkPacket>,
    resources: BTreeMap<LinkId, PendingResource>,
    incoming_resources: BTreeMap<LinkId, IncomingResourceAssembly>,
}

impl LxmfNode {
    /// Construct a correctly named delivery destination. The caller can enable
    /// ratchets on the returned destination before registering it.
    pub fn delivery_destination(identity: Identity) -> Result<Destination, DestinationError> {
        let mut destination = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            APP_NAME,
            &[DELIVERY_ASPECT],
        )?;
        destination.set_proof_strategy(ProofStrategy::All);
        destination.set_accepts_links(true);
        Ok(destination)
    }

    /// Validate, configure and register an LXMF delivery destination.
    pub fn register<R, C, S>(
        node: &mut NodeCore<R, C, S>,
        mut destination: Destination,
        config: LxmfNodeConfig,
    ) -> Result<Self, LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if destination.direction() != Direction::In
            || destination.dest_type() != DestinationType::Single
            || destination.name_hash()
                != &Destination::compute_name_hash(APP_NAME, &[DELIVERY_ASPECT])
        {
            return Err(LxmfNodeError::InvalidDeliveryDestination);
        }

        destination.set_proof_strategy(ProofStrategy::All);
        destination.set_accepts_links(true);
        let delivery_destination = *destination.hash();
        node.register_destination(destination);
        Ok(Self::new(delivery_destination, config))
    }

    /// Attach to an already registered LXMF delivery destination.
    pub fn attach<R, C, S>(
        node: &mut NodeCore<R, C, S>,
        delivery_destination: DestinationHash,
        config: LxmfNodeConfig,
    ) -> Result<Self, LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let destination = node
            .destination_mut(&delivery_destination)
            .ok_or(LxmfNodeError::InvalidDeliveryDestination)?;
        if destination.direction() != Direction::In
            || destination.dest_type() != DestinationType::Single
            || destination.name_hash()
                != &Destination::compute_name_hash(APP_NAME, &[DELIVERY_ASPECT])
        {
            return Err(LxmfNodeError::InvalidDeliveryDestination);
        }
        destination.set_proof_strategy(ProofStrategy::All);
        destination.set_accepts_links(true);
        Ok(Self::new(delivery_destination, config))
    }

    fn new(delivery_destination: DestinationHash, config: LxmfNodeConfig) -> Self {
        Self {
            delivery_destination,
            config,
            wanted_links: BTreeSet::new(),
            pending_links: BTreeMap::new(),
            link_destinations: BTreeMap::new(),
            direct_links: BTreeMap::new(),
            lxmf_links: BTreeSet::new(),
            identified_links: BTreeSet::new(),
            packet_receipts: BTreeMap::new(),
            link_packet_receipts: BTreeMap::new(),
            resources: BTreeMap::new(),
            incoming_resources: BTreeMap::new(),
        }
    }

    pub const fn delivery_destination_hash(&self) -> DestinationHash {
        self.delivery_destination
    }

    /// Return the active direct or identified backchannel Link for a peer.
    pub fn direct_link(&self, destination: &DestinationHash) -> Option<LinkId> {
        self.direct_links.get(destination).copied()
    }

    /// Return whether an established Link is owned by LXMF delivery.
    ///
    /// This becomes `true` when [`NodeEvent::LinkEstablished`] associates the
    /// Link with the local delivery destination or an outgoing direct-delivery
    /// attempt, and becomes `false` again when the Link closes.
    pub fn owns_link(&self, link_id: &LinkId) -> bool {
        self.lxmf_links.contains(link_id)
    }

    /// Select the exact Python-compatible delivery representation.
    pub fn representation(message: &Message) -> Result<DeliveryRepresentation, LxmfNodeError> {
        let packed_len = message.pack().len();
        match message.method {
            DeliveryMethod::Opportunistic
                if packed_len.saturating_sub(DESTINATION_LENGTH) <= OPPORTUNISTIC_PACKET_MDU =>
            {
                Ok(DeliveryRepresentation::OpportunisticPacket)
            }
            DeliveryMethod::Opportunistic | DeliveryMethod::Direct
                if packed_len <= DIRECT_PACKET_MDU =>
            {
                Ok(DeliveryRepresentation::DirectPacket)
            }
            DeliveryMethod::Opportunistic | DeliveryMethod::Direct => {
                Ok(DeliveryRepresentation::DirectResource)
            }
            DeliveryMethod::Propagated | DeliveryMethod::Paper => {
                Err(LxmfNodeError::UnsupportedMethod)
            }
        }
    }

    /// Ensure that a direct link exists, requesting a path first when needed.
    pub fn ensure_direct_link<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        destination: DestinationHash,
    ) -> Result<(DirectLinkState, LxmfNodeOutput), LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if let Some(link_id) = self.direct_links.get(&destination).copied() {
            if node.link(&link_id).is_some_and(|link| link.is_active()) {
                return Ok((DirectLinkState::Ready(link_id), LxmfNodeOutput::default()));
            }
            self.direct_links.remove(&destination);
        }
        if let Some(link_id) = self.pending_links.get(&destination).copied() {
            return Ok((
                DirectLinkState::Connecting(link_id),
                LxmfNodeOutput::default(),
            ));
        }
        if node
            .storage()
            .get_identity(destination.as_bytes())
            .is_none()
        {
            return Err(LxmfNodeError::UnknownPeer);
        }

        self.wanted_links.insert(destination);
        if !node.has_path(&destination) {
            let mut output = LxmfNodeOutput {
                core: node.request_path(&destination),
                events: Vec::new(),
            };
            output
                .events
                .push(LxmfNodeEvent::PathRequested { destination });
            return Ok((DirectLinkState::PathRequested, output));
        }

        let (link_id, output) = self.start_direct_link(node, destination)?;
        Ok((DirectLinkState::Started(link_id), output))
    }

    fn start_direct_link<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        destination: DestinationHash,
    ) -> Result<(LinkId, LxmfNodeOutput), LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let signing_key = node
            .storage()
            .get_identity(destination.as_bytes())
            .map(|identity| identity.ed25519_verifying().to_bytes())
            .ok_or(LxmfNodeError::UnknownPeer)?;
        let (link_id, _routed, core) = node.connect(destination, &signing_key);
        self.pending_links.insert(destination, link_id);
        self.link_destinations.insert(link_id, destination);
        let output = LxmfNodeOutput {
            core,
            events: Vec::new(),
        };
        Ok((link_id, output))
    }

    /// Submit one opportunistic or direct message to NodeCore.
    ///
    /// Oversized opportunistic messages fall back to direct delivery, matching
    /// Python `LXMessage.pack()`. Call [`ensure_direct_link`](Self::ensure_direct_link)
    /// first when this method returns [`LxmfNodeError::DirectLinkUnavailable`].
    pub fn send<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        message: &Message,
    ) -> Result<LxmfNodeOutput, LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let representation = Self::representation(message)?;
        let destination = DestinationHash::new(message.destination_hash);

        match representation {
            DeliveryRepresentation::OpportunisticPacket => {
                let data = message.on_air().map_err(LxmfNodeError::Message)?;
                let (packet_hash, core) = node.send_single_packet(&destination, &data)?;
                self.packet_receipts.insert(packet_hash, message.message_id);
                let output = LxmfNodeOutput {
                    core,
                    events: vec![LxmfNodeEvent::Submitted {
                        message_id: message.message_id,
                        method: DeliveryMethod::Opportunistic,
                        representation,
                        submission: SubmissionId::Packet(packet_hash),
                    }],
                };
                Ok(output)
            }
            DeliveryRepresentation::DirectPacket => {
                let link_id = self.active_direct_link(node, &destination)?;
                let packed = message.pack();
                let (packet_hash, core) = node.send_packet_on_link(&link_id, &packed)?;
                self.link_packet_receipts.insert(
                    packet_hash,
                    PendingLinkPacket {
                        message_id: message.message_id,
                        link_id,
                    },
                );
                let output = LxmfNodeOutput {
                    core,
                    events: vec![LxmfNodeEvent::Submitted {
                        message_id: message.message_id,
                        method: DeliveryMethod::Direct,
                        representation,
                        submission: SubmissionId::LinkPacket(packet_hash),
                    }],
                };
                Ok(output)
            }
            DeliveryRepresentation::DirectResource => {
                let link_id = self.active_direct_link(node, &destination)?;
                let packed = message.pack();
                let (resource_hash, core) = node.send_resource(
                    &link_id,
                    &packed,
                    None,
                    self.config.auto_compress_resources,
                )?;
                self.resources.insert(
                    link_id,
                    PendingResource {
                        message_id: message.message_id,
                        current_resource_hash: resource_hash,
                        completed_size: 0,
                        current_size: packed.len().min(RESOURCE_MAX_EFFICIENT_SIZE) as u64,
                        total_size: packed.len() as u64,
                    },
                );
                let output = LxmfNodeOutput {
                    core,
                    events: vec![LxmfNodeEvent::Submitted {
                        message_id: message.message_id,
                        method: DeliveryMethod::Direct,
                        representation,
                        submission: SubmissionId::Resource(resource_hash),
                    }],
                };
                Ok(output)
            }
        }
    }

    /// Stop tracking an outbound LXMF submission and abort an active Resource.
    ///
    /// Packets that have already left an interface cannot be recalled, but
    /// their proof correlation is removed immediately. A Resource is aborted
    /// by closing its Link, which makes the core emit the normal cancellation
    /// frames and prevents the remaining parts from being transmitted.
    pub fn cancel_outbound<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        message_id: &[u8; 32],
    ) -> TickOutput
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        self.packet_receipts
            .retain(|_, pending| pending != message_id);
        self.link_packet_receipts
            .retain(|_, pending| &pending.message_id != message_id);

        let resource_links: Vec<LinkId> = self
            .resources
            .iter()
            .filter_map(|(link_id, pending)| {
                (&pending.message_id == message_id).then_some(*link_id)
            })
            .collect();
        let mut output = TickOutput::default();
        for link_id in resource_links {
            self.resources.remove(&link_id);
            output.merge(node.close_link(&link_id));
        }
        output
    }

    fn active_direct_link<R, C, S>(
        &mut self,
        node: &NodeCore<R, C, S>,
        destination: &DestinationHash,
    ) -> Result<LinkId, LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let link_id = self
            .direct_links
            .get(destination)
            .copied()
            .ok_or(LxmfNodeError::DirectLinkUnavailable)?;
        if node.link(&link_id).is_some_and(|link| link.is_active()) {
            Ok(link_id)
        } else {
            self.direct_links.remove(destination);
            Err(LxmfNodeError::DirectLinkUnavailable)
        }
    }

    /// Consume one NodeCore event and return any follow-up NodeCore actions and
    /// LXMF-level events.
    pub fn handle_event<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        event: &NodeEvent,
    ) -> Result<LxmfNodeOutput, LxmfNodeError>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let mut output = LxmfNodeOutput::default();
        match event {
            NodeEvent::AnnounceReceived { announce, .. }
                if announce.name_hash()
                    == &Destination::compute_name_hash(APP_NAME, &[DELIVERY_ASPECT]) =>
            {
                let destination = *announce.destination_hash();
                output
                    .events
                    .push(LxmfNodeEvent::PeerAnnounced { destination });

                if self.wanted_links.contains(&destination)
                    && !self.pending_links.contains_key(&destination)
                    && node.has_path(&destination)
                {
                    let (_, follow_up) = self.start_direct_link(node, destination)?;
                    output.merge(follow_up);
                }
            }
            NodeEvent::PathFound {
                destination_hash, ..
            } if self.wanted_links.contains(destination_hash)
                && !self.pending_links.contains_key(destination_hash) =>
            {
                let (_, follow_up) = self.start_direct_link(node, *destination_hash)?;
                output.merge(follow_up);
            }
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator,
                ..
            } => {
                let link_destination = node.link(link_id).map(|link| *link.destination_hash());
                let tracked_destination = self.link_destinations.get(link_id).copied();
                let is_delivery_link = link_destination == Some(self.delivery_destination)
                    || tracked_destination.is_some();
                if is_delivery_link {
                    self.lxmf_links.insert(*link_id);
                    node.set_resource_strategy(link_id, ResourceStrategy::AcceptApp)?;

                    let destination = if *is_initiator {
                        tracked_destination.or(link_destination)
                    } else {
                        None
                    };
                    if let Some(destination) = destination {
                        self.pending_links.remove(&destination);
                        self.direct_links.insert(destination, *link_id);
                        self.link_destinations.insert(*link_id, destination);
                    }
                    output.events.push(LxmfNodeEvent::DirectLinkEstablished {
                        destination,
                        link_id: *link_id,
                        is_initiator: *is_initiator,
                    });
                }
            }
            NodeEvent::LinkIdentified {
                link_id,
                identity_hash,
            } if self.lxmf_links.contains(link_id) => {
                let destination = Destination::compute_destination_hash(
                    &Destination::compute_name_hash(APP_NAME, &[DELIVERY_ASPECT]),
                    identity_hash,
                );
                self.direct_links.insert(destination, *link_id);
                self.link_destinations.insert(*link_id, destination);
                output.events.push(LxmfNodeEvent::DirectLinkEstablished {
                    destination: Some(destination),
                    link_id: *link_id,
                    is_initiator: false,
                });
            }
            NodeEvent::PacketProofRequested {
                packet_hash,
                destination_hash,
                interface_index,
            } if *destination_hash == self.delivery_destination => {
                output.core.merge(
                    node.send_proof_on_interface(packet_hash, destination_hash, *interface_index)
                        .map_err(|_| LxmfNodeError::ProofFailed)?,
                );
            }
            NodeEvent::LinkProofRequested {
                link_id,
                packet_hash,
            } if self.lxmf_links.contains(link_id) => {
                output.core.merge(
                    node.send_data_proof(link_id, packet_hash)
                        .map_err(|_| LxmfNodeError::ProofFailed)?,
                );
            }
            NodeEvent::PacketReceived {
                destination, data, ..
            } if *destination == self.delivery_destination => {
                self.emit_inbound(
                    node.storage(),
                    data,
                    DeliveryMethod::Opportunistic,
                    Some(destination.into_bytes()),
                    &mut output.events,
                );
            }
            NodeEvent::LinkDataReceived { link_id, data } if self.lxmf_links.contains(link_id) => {
                self.emit_inbound(
                    node.storage(),
                    data,
                    DeliveryMethod::Direct,
                    None,
                    &mut output.events,
                );
            }
            NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } if self.lxmf_links.contains(link_id) => {
                let accept = self
                    .config
                    .max_incoming_resource_size
                    .is_none_or(|limit| *data_size <= limit);
                if accept {
                    output.core.merge(node.accept_resource(link_id)?);
                } else {
                    output.core.merge(node.reject_resource(link_id)?);
                    output.events.push(LxmfNodeEvent::InboundRejected {
                        method: DeliveryMethod::Direct,
                        reason: InboundRejection::ResourceTooLarge,
                    });
                }
            }
            NodeEvent::ResourceProgress {
                link_id,
                resource_hash,
                progress,
                data_size,
                is_sender: true,
                ..
            } => {
                if let Some(pending) = self.resources.get_mut(link_id) {
                    if pending.current_resource_hash != *resource_hash {
                        pending.completed_size =
                            pending.completed_size.saturating_add(pending.current_size);
                        pending.current_resource_hash = *resource_hash;
                        pending.current_size = *data_size;
                    }
                    let transferred = pending.completed_size as f64
                        + f64::from(*progress) * pending.current_size as f64;
                    let aggregate_progress = if pending.total_size == 0 {
                        0.0
                    } else {
                        (transferred / pending.total_size as f64).clamp(0.0, 1.0) as f32
                    };
                    output.events.push(LxmfNodeEvent::Progress {
                        message_id: pending.message_id,
                        progress: aggregate_progress,
                    });
                }
            }
            NodeEvent::ResourceCompleted {
                link_id,
                is_sender: true,
                ..
            } => {
                if let Some(pending) = self.resources.remove(link_id) {
                    output.events.push(LxmfNodeEvent::Delivered {
                        message_id: pending.message_id,
                    });
                    self.identify_backchannel(node, *link_id, &mut output.core);
                }
            }
            NodeEvent::ResourceCompleted {
                link_id,
                data,
                is_sender: false,
                segment_index,
                total_segments,
                ..
            } if self.lxmf_links.contains(link_id) => {
                self.handle_resource_segment(
                    node.storage(),
                    *link_id,
                    data,
                    *segment_index,
                    *total_segments,
                    &mut output.events,
                );
            }
            NodeEvent::ResourceFailed {
                link_id,
                error,
                is_sender,
                ..
            } => {
                if *is_sender {
                    if let Some(pending) = self.resources.remove(link_id) {
                        output.events.push(LxmfNodeEvent::DeliveryFailed {
                            message_id: pending.message_id,
                            reason: DeliveryFailure::Resource(*error),
                        });
                    }
                } else if self.lxmf_links.contains(link_id) {
                    self.incoming_resources.remove(link_id);
                    output.events.push(LxmfNodeEvent::InboundRejected {
                        method: DeliveryMethod::Direct,
                        reason: InboundRejection::Resource(*error),
                    });
                }
            }
            NodeEvent::PacketDeliveryConfirmed { packet_hash } => {
                if let Some(message_id) = self.packet_receipts.remove(packet_hash) {
                    output.events.push(LxmfNodeEvent::Delivered { message_id });
                }
            }
            NodeEvent::DeliveryFailed { packet_hash, error } => {
                if let Some(message_id) = self.packet_receipts.remove(packet_hash) {
                    output.events.push(LxmfNodeEvent::DeliveryFailed {
                        message_id,
                        reason: DeliveryFailure::Opportunistic(*error),
                    });
                }
            }
            NodeEvent::LinkDeliveryConfirmed {
                link_id,
                packet_hash,
            } => {
                if let Some(pending) = self.link_packet_receipts.remove(packet_hash) {
                    output.events.push(LxmfNodeEvent::Delivered {
                        message_id: pending.message_id,
                    });
                    self.identify_backchannel(node, *link_id, &mut output.core);
                }
            }
            NodeEvent::LinkDeliveryFailed {
                link_id,
                packet_hash,
            } => {
                if let Some(pending) = self.link_packet_receipts.remove(packet_hash) {
                    output.events.push(LxmfNodeEvent::DeliveryFailed {
                        message_id: pending.message_id,
                        reason: DeliveryFailure::DirectPacketTimeout,
                    });
                    // Python LXMessage.__link_packet_timed_out() tears down
                    // the Link after the PacketReceipt timeout callback.
                    output.core.merge(node.close_link(link_id));
                }
            }
            NodeEvent::LinkClosed {
                link_id, reason, ..
            } => {
                self.handle_link_closed(*link_id, *reason, &mut output.events);
            }
            _ => {}
        }
        Ok(output)
    }

    fn emit_inbound<S: Storage>(
        &self,
        storage: &S,
        data: &[u8],
        method: DeliveryMethod,
        inferred_destination: Option<[u8; 16]>,
        events: &mut Vec<LxmfNodeEvent>,
    ) {
        let source_offset = if method == DeliveryMethod::Opportunistic {
            0
        } else {
            DESTINATION_LENGTH
        };
        let source_hash = data
            .get(source_offset..source_offset + DESTINATION_LENGTH)
            .and_then(|source| <[u8; 16]>::try_from(source).ok());
        let source_identity = source_hash
            .as_ref()
            .and_then(|source| storage.get_identity(source));

        match Message::unpack(data, inferred_destination, source_identity, method) {
            Ok(message) if message.destination_hash == self.delivery_destination.into_bytes() => {
                events.push(LxmfNodeEvent::MessageReceived(message));
            }
            Ok(_) => events.push(LxmfNodeEvent::InboundRejected {
                method,
                reason: InboundRejection::WrongDestination,
            }),
            Err(error) => events.push(LxmfNodeEvent::InboundRejected {
                method,
                reason: InboundRejection::Message(error),
            }),
        }
    }

    fn handle_resource_segment<S: Storage>(
        &mut self,
        storage: &S,
        link_id: LinkId,
        data: &[u8],
        segment_index: u32,
        total_segments: u32,
        events: &mut Vec<LxmfNodeEvent>,
    ) {
        if segment_index == 0 || total_segments == 0 || segment_index > total_segments {
            self.incoming_resources.remove(&link_id);
            events.push(LxmfNodeEvent::InboundRejected {
                method: DeliveryMethod::Direct,
                reason: InboundRejection::ResourceSequence,
            });
            return;
        }

        if total_segments == 1 {
            self.emit_inbound(storage, data, DeliveryMethod::Direct, None, events);
            return;
        }

        if segment_index == 1 {
            self.incoming_resources.insert(
                link_id,
                IncomingResourceAssembly {
                    next_segment: 2,
                    total_segments,
                    data: data.to_vec(),
                },
            );
            return;
        }

        let complete = match self.incoming_resources.get_mut(&link_id) {
            Some(assembly)
                if assembly.next_segment == segment_index
                    && assembly.total_segments == total_segments =>
            {
                assembly.data.extend_from_slice(data);
                assembly.next_segment = assembly.next_segment.saturating_add(1);
                segment_index == total_segments
            }
            _ => {
                self.incoming_resources.remove(&link_id);
                events.push(LxmfNodeEvent::InboundRejected {
                    method: DeliveryMethod::Direct,
                    reason: InboundRejection::ResourceSequence,
                });
                return;
            }
        };

        if complete {
            if let Some(assembly) = self.incoming_resources.remove(&link_id) {
                self.emit_inbound(
                    storage,
                    &assembly.data,
                    DeliveryMethod::Direct,
                    None,
                    events,
                );
            }
        }
    }

    fn handle_link_closed(
        &mut self,
        link_id: LinkId,
        reason: LinkCloseReason,
        events: &mut Vec<LxmfNodeEvent>,
    ) {
        self.lxmf_links.remove(&link_id);
        self.identified_links.remove(&link_id);
        self.incoming_resources.remove(&link_id);
        self.link_destinations.remove(&link_id);
        self.pending_links.retain(|_, id| *id != link_id);
        self.direct_links.retain(|_, id| *id != link_id);

        let packet_hashes: Vec<[u8; 32]> = self
            .link_packet_receipts
            .iter()
            .filter_map(|(hash, pending)| (pending.link_id == link_id).then_some(*hash))
            .collect();
        for hash in packet_hashes {
            if let Some(pending) = self.link_packet_receipts.remove(&hash) {
                events.push(LxmfNodeEvent::DeliveryFailed {
                    message_id: pending.message_id,
                    reason: DeliveryFailure::LinkClosed(reason),
                });
            }
        }
        if let Some(pending) = self.resources.remove(&link_id) {
            events.push(LxmfNodeEvent::DeliveryFailed {
                message_id: pending.message_id,
                reason: DeliveryFailure::LinkClosed(reason),
            });
        }
    }

    fn identify_backchannel<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        link_id: LinkId,
        output: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if self.identified_links.contains(&link_id)
            || !node.link(&link_id).is_some_and(|link| link.is_initiator())
        {
            return;
        }
        let private_key = node
            .destination(&self.delivery_destination)
            .and_then(|destination| destination.identity())
            .and_then(|identity| identity.private_key_bytes().ok());
        let Some(private_key) = private_key else {
            return;
        };
        let Ok(identity) = Identity::from_private_key_bytes(&private_key) else {
            return;
        };
        if let Ok(follow_up) = node.identify_link(&link_id, &identity) {
            self.identified_links.insert(link_id);
            output.merge(follow_up);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Verification;
    use alloc::collections::VecDeque;
    use alloc::vec;
    use core::cell::Cell;
    use leviculum_core::{Action, InterfaceId, MemoryStorage, NodeCoreBuilder};
    use rand_core::OsRng;

    struct TestClock(Cell<u64>);

    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
    }

    fn test_node() -> NodeCore<OsRng, TestClock, MemoryStorage> {
        NodeCoreBuilder::new().build(
            OsRng,
            TestClock(Cell::new(1_000)),
            MemoryStorage::with_defaults(),
        )
    }

    fn identity_from(byte: u8) -> Identity {
        let mut private = [0u8; 64];
        for (index, value) in private.iter_mut().enumerate() {
            *value = byte.wrapping_add(index as u8);
        }
        Identity::from_private_key_bytes(&private).expect("valid deterministic identity")
    }

    fn setup() -> (
        NodeCore<OsRng, TestClock, MemoryStorage>,
        LxmfNode,
        Identity,
        DestinationHash,
    ) {
        let mut node = test_node();
        let local = identity_from(1);
        let local_private = local.private_key_bytes().expect("private key");
        let destination = LxmfNode::delivery_destination(local).expect("delivery destination");
        let local_hash = *destination.hash();
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("register LXMF");

        let source = identity_from(101);
        let source_hash = Destination::compute_destination_hash(
            &Destination::compute_name_hash(APP_NAME, &[DELIVERY_ASPECT]),
            source.hash(),
        );
        let source_private = source.private_key_bytes().expect("source key");
        node.remember_identity(
            source_hash,
            Identity::from_private_key_bytes(&source_private).expect("source copy"),
        );

        // Prove the registered destination still owns the intended identity.
        assert_eq!(
            node.destination(&local_hash)
                .and_then(|d| d.identity())
                .and_then(|i| i.private_key_bytes().ok()),
            Some(local_private)
        );
        (node, lxmf, source, source_hash)
    }

    fn message(
        destination: DestinationHash,
        source: &Identity,
        source_hash: DestinationHash,
        method: DeliveryMethod,
        content_len: usize,
    ) -> Message {
        Message::create(
            destination.into_bytes(),
            source_hash.into_bytes(),
            source,
            1_700_000_000.0,
            b"title".to_vec(),
            vec![0x5a; content_len],
            Vec::new(),
            method,
        )
        .expect("message")
    }

    #[test]
    fn opportunistic_node_event_decodes_and_verifies() {
        let (mut node, mut lxmf, source, source_hash) = setup();
        let msg = message(
            lxmf.delivery_destination_hash(),
            &source,
            source_hash,
            DeliveryMethod::Opportunistic,
            5,
        );
        let event = NodeEvent::PacketReceived {
            destination: lxmf.delivery_destination_hash(),
            data: msg.on_air().expect("opportunistic on-air"),
            interface_index: 0,
        };
        let output = lxmf.handle_event(&mut node, &event).expect("handle");
        assert!(matches!(
            output.events.as_slice(),
            [LxmfNodeEvent::MessageReceived(received)]
                if received.message_id == msg.message_id
                    && received.method == DeliveryMethod::Opportunistic
                    && received.verification == Verification::Valid
        ));
    }

    #[test]
    fn direct_raw_link_event_decodes_full_packed_message() {
        let (mut node, mut lxmf, source, source_hash) = setup();
        let link_id = LinkId::new([0x44; 16]);
        lxmf.lxmf_links.insert(link_id);
        let msg = message(
            lxmf.delivery_destination_hash(),
            &source,
            source_hash,
            DeliveryMethod::Direct,
            5,
        );
        let event = NodeEvent::LinkDataReceived {
            link_id,
            data: msg.pack(),
        };
        let output = lxmf.handle_event(&mut node, &event).expect("handle");
        assert!(matches!(
            output.events.as_slice(),
            [LxmfNodeEvent::MessageReceived(received)]
                if received.message_id == msg.message_id
                    && received.method == DeliveryMethod::Direct
                    && received.verification == Verification::Valid
        ));
    }

    #[test]
    fn link_ownership_is_removed_when_the_link_closes() {
        let (_, mut lxmf, _, _) = setup();
        let link_id = LinkId::new([0x45; 16]);

        assert!(!lxmf.owns_link(&link_id));
        lxmf.lxmf_links.insert(link_id);
        assert!(lxmf.owns_link(&link_id));

        lxmf.handle_link_closed(link_id, LinkCloseReason::Normal, &mut Vec::new());
        assert!(!lxmf.owns_link(&link_id));
    }

    #[test]
    fn split_resource_is_reassembled_before_decode() {
        let (mut node, mut lxmf, source, source_hash) = setup();
        let link_id = LinkId::new([0x55; 16]);
        lxmf.lxmf_links.insert(link_id);
        let msg = message(
            lxmf.delivery_destination_hash(),
            &source,
            source_hash,
            DeliveryMethod::Direct,
            700,
        );
        let packed = msg.pack();
        let split = packed.len() / 2;
        let first = NodeEvent::ResourceCompleted {
            link_id,
            resource_hash: [1; 32],
            data: packed[..split].to_vec(),
            metadata: None,
            is_sender: false,
            segment_index: 1,
            total_segments: 2,
        };
        let second = NodeEvent::ResourceCompleted {
            link_id,
            resource_hash: [2; 32],
            data: packed[split..].to_vec(),
            metadata: None,
            is_sender: false,
            segment_index: 2,
            total_segments: 2,
        };

        let first_output = lxmf.handle_event(&mut node, &first).expect("first segment");
        assert!(first_output.events.is_empty());
        let second_output = lxmf
            .handle_event(&mut node, &second)
            .expect("second segment");
        assert!(matches!(
            second_output.events.as_slice(),
            [LxmfNodeEvent::MessageReceived(received)] if received.message_id == msg.message_id
        ));
    }

    #[test]
    fn representation_matches_python_thresholds_and_fallback() {
        let (_, lxmf, source, source_hash) = setup();
        let small = message(
            lxmf.delivery_destination_hash(),
            &source,
            source_hash,
            DeliveryMethod::Opportunistic,
            1,
        );
        assert_eq!(
            LxmfNode::representation(&small).expect("small"),
            DeliveryRepresentation::OpportunisticPacket
        );

        let fallback = message(
            lxmf.delivery_destination_hash(),
            &source,
            source_hash,
            DeliveryMethod::Opportunistic,
            310,
        );
        assert_eq!(
            LxmfNode::representation(&fallback).expect("fallback"),
            DeliveryRepresentation::DirectPacket
        );

        let large = message(
            lxmf.delivery_destination_hash(),
            &source,
            source_hash,
            DeliveryMethod::Direct,
            700,
        );
        assert_eq!(
            LxmfNode::representation(&large).expect("large"),
            DeliveryRepresentation::DirectResource
        );
    }

    #[test]
    fn out_of_order_resource_is_rejected_and_discarded() {
        let (mut node, mut lxmf, _, _) = setup();
        let link_id = LinkId::new([0x66; 16]);
        lxmf.lxmf_links.insert(link_id);
        let event = NodeEvent::ResourceCompleted {
            link_id,
            resource_hash: [3; 32],
            data: vec![1, 2, 3],
            metadata: None,
            is_sender: false,
            segment_index: 2,
            total_segments: 3,
        };
        let output = lxmf.handle_event(&mut node, &event).expect("handle");
        assert!(matches!(
            output.events.as_slice(),
            [LxmfNodeEvent::InboundRejected {
                method: DeliveryMethod::Direct,
                reason: InboundRejection::ResourceSequence,
            }]
        ));
    }

    #[test]
    fn core_raw_packet_timeout_fails_direct_delivery_once() {
        let (mut node, mut lxmf, _, _) = setup();
        let link_id = LinkId::new([0x77; 16]);
        let message_id = [0xa5; 32];
        let packet_hash = [0x33; 32];
        lxmf.link_packet_receipts.insert(
            packet_hash,
            PendingLinkPacket {
                message_id,
                link_id,
            },
        );
        let event = NodeEvent::LinkDeliveryFailed {
            link_id,
            packet_hash,
        };
        let output = lxmf.handle_event(&mut node, &event).expect("handle");
        assert!(matches!(
            output.events.as_slice(),
            [LxmfNodeEvent::DeliveryFailed {
                message_id: id,
                reason: DeliveryFailure::DirectPacketTimeout,
            }] if id == &message_id
        ));
        assert!(lxmf
            .handle_event(&mut node, &event)
            .expect("repeat handle")
            .events
            .is_empty());
    }

    #[test]
    fn split_resource_sender_progress_is_aggregated_across_segments() {
        let (mut node, mut lxmf, _, _) = setup();
        let link_id = LinkId::new([0x44; 16]);
        let message_id = [0x55; 32];
        let first_hash = [0x66; 32];
        let second_hash = [0x77; 32];
        let segment_size = RESOURCE_MAX_EFFICIENT_SIZE as u64;
        lxmf.resources.insert(
            link_id,
            PendingResource {
                message_id,
                current_resource_hash: first_hash,
                completed_size: 0,
                current_size: segment_size,
                total_size: segment_size * 2,
            },
        );

        let first = lxmf
            .handle_event(
                &mut node,
                &NodeEvent::ResourceProgress {
                    link_id,
                    resource_hash: first_hash,
                    progress: 0.8,
                    transfer_size: segment_size,
                    data_size: segment_size,
                    is_sender: true,
                },
            )
            .expect("first segment progress");
        assert!(matches!(
            first.events.as_slice(),
            [LxmfNodeEvent::Progress { message_id: id, progress }]
                if *id == message_id && (*progress - 0.4).abs() < 0.0001
        ));

        let second = lxmf
            .handle_event(
                &mut node,
                &NodeEvent::ResourceProgress {
                    link_id,
                    resource_hash: second_hash,
                    progress: 0.2,
                    transfer_size: segment_size,
                    data_size: segment_size,
                    is_sender: true,
                },
            )
            .expect("second segment progress");
        assert!(matches!(
            second.events.as_slice(),
            [LxmfNodeEvent::Progress { message_id: id, progress }]
                if *id == message_id && (*progress - 0.6).abs() < 0.0001
        ));
    }

    struct Peer {
        node: NodeCore<OsRng, TestClock, MemoryStorage>,
        lxmf: LxmfNode,
        signing_identity: Identity,
        destination: DestinationHash,
    }

    fn peer(seed: u8) -> Peer {
        let mut node = test_node();
        let identity = identity_from(seed);
        let private = identity.private_key_bytes().expect("private delivery key");
        let destination = LxmfNode::delivery_destination(identity).expect("delivery destination");
        let destination_hash = *destination.hash();
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("register delivery destination");
        Peer {
            node,
            lxmf,
            signing_identity: Identity::from_private_key_bytes(&private)
                .expect("signing identity copy"),
            destination: destination_hash,
        }
    }

    fn take_packets(actions: Vec<Action>) -> Vec<Vec<u8>> {
        actions
            .into_iter()
            .map(|action| match action {
                Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
            })
            .collect()
    }

    fn absorb_core(
        peer: &mut Peer,
        core: TickOutput,
        app_events: &mut Vec<LxmfNodeEvent>,
    ) -> Vec<Vec<u8>> {
        let mut actions = core.actions;
        let mut events: VecDeque<NodeEvent> = core.events.into();
        while let Some(event) = events.pop_front() {
            let follow_up = peer
                .lxmf
                .handle_event(&mut peer.node, &event)
                .expect("handle NodeCore event");
            app_events.extend(follow_up.events);
            actions.extend(follow_up.core.actions);
            events.extend(follow_up.core.events);
        }
        take_packets(actions)
    }

    fn receive(
        peer: &mut Peer,
        packets: Vec<Vec<u8>>,
        app_events: &mut Vec<LxmfNodeEvent>,
    ) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = peer.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(absorb_core(peer, core, app_events));
        }
        outbound
    }

    fn pump(
        a: &mut Peer,
        b: &mut Peer,
        mut to_b: Vec<Vec<u8>>,
        a_events: &mut Vec<LxmfNodeEvent>,
        b_events: &mut Vec<LxmfNodeEvent>,
    ) {
        let mut to_a = Vec::new();
        for _ in 0..256 {
            if !to_b.is_empty() {
                to_a.extend(receive(b, core::mem::take(&mut to_b), b_events));
            }
            if !to_a.is_empty() {
                to_b.extend(receive(a, core::mem::take(&mut to_a), a_events));
            }
            if to_a.is_empty() && to_b.is_empty() {
                return;
            }
        }
        panic!("NodeCore exchange did not quiesce");
    }

    fn pump_bidirectional(
        a: &mut Peer,
        b: &mut Peer,
        mut to_b: Vec<Vec<u8>>,
        mut to_a: Vec<Vec<u8>>,
        a_events: &mut Vec<LxmfNodeEvent>,
        b_events: &mut Vec<LxmfNodeEvent>,
    ) {
        for _ in 0..512 {
            if !to_b.is_empty() {
                to_a.extend(receive(b, core::mem::take(&mut to_b), b_events));
            }
            if !to_a.is_empty() {
                to_b.extend(receive(a, core::mem::take(&mut to_a), a_events));
            }
            if to_a.is_empty() && to_b.is_empty() {
                return;
            }
        }
        panic!("bidirectional NodeCore exchange did not quiesce");
    }

    fn exchange_announces(a: &mut Peer, b: &mut Peer) {
        let mut a_events = Vec::new();
        let mut b_events = Vec::new();
        let a_announce = a
            .node
            .announce_destination(&a.destination, None)
            .expect("announce a");
        let b_announce = b
            .node
            .announce_destination(&b.destination, None)
            .expect("announce b");
        let to_b = absorb_core(a, a_announce, &mut a_events);
        let to_a = absorb_core(b, b_announce, &mut b_events);
        let replies_to_a = receive(b, to_b, &mut b_events);
        let replies_to_b = receive(a, to_a, &mut a_events);
        pump(a, b, replies_to_b, &mut a_events, &mut b_events);
        pump(b, a, replies_to_a, &mut b_events, &mut a_events);
    }

    #[test]
    fn nodecore_round_trip_opportunistic_direct_packet_and_resource() {
        let mut sender = peer(10);
        let mut receiver = peer(90);
        exchange_announces(&mut sender, &mut receiver);

        let mut sender_events = Vec::new();
        let mut receiver_events = Vec::new();

        let opportunistic = message(
            receiver.destination,
            &sender.signing_identity,
            sender.destination,
            DeliveryMethod::Opportunistic,
            12,
        );
        let sent = sender
            .lxmf
            .send(&mut sender.node, &opportunistic)
            .expect("send opportunistic");
        sender_events.extend(sent.events);
        pump(
            &mut sender,
            &mut receiver,
            take_packets(sent.core.actions),
            &mut sender_events,
            &mut receiver_events,
        );
        assert!(sender_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::Delivered { message_id } if *message_id == opportunistic.message_id
        )));
        assert!(receiver_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::MessageReceived(received)
                if received.message_id == opportunistic.message_id
                    && received.verification == Verification::Valid
        )));

        let (link_state, link_output) = sender
            .lxmf
            .ensure_direct_link(&mut sender.node, receiver.destination)
            .expect("ensure direct link");
        assert!(matches!(link_state, DirectLinkState::Started(_)));
        sender_events.extend(link_output.events);
        pump(
            &mut sender,
            &mut receiver,
            take_packets(link_output.core.actions),
            &mut sender_events,
            &mut receiver_events,
        );
        assert!(sender.lxmf.direct_link(&receiver.destination).is_some());

        let direct = message(
            receiver.destination,
            &sender.signing_identity,
            sender.destination,
            DeliveryMethod::Direct,
            12,
        );
        let sent = sender
            .lxmf
            .send(&mut sender.node, &direct)
            .expect("send direct packet");
        sender_events.extend(sent.events);
        pump(
            &mut sender,
            &mut receiver,
            take_packets(sent.core.actions),
            &mut sender_events,
            &mut receiver_events,
        );
        assert!(sender_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::Delivered { message_id } if *message_id == direct.message_id
        )));
        assert!(receiver_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::MessageReceived(received) if received.message_id == direct.message_id
        )));

        let resource = message(
            receiver.destination,
            &sender.signing_identity,
            sender.destination,
            DeliveryMethod::Direct,
            1_200,
        );
        assert_eq!(
            LxmfNode::representation(&resource).expect("representation"),
            DeliveryRepresentation::DirectResource
        );
        let sent = sender
            .lxmf
            .send(&mut sender.node, &resource)
            .expect("send direct resource");
        sender_events.extend(sent.events);
        pump(
            &mut sender,
            &mut receiver,
            take_packets(sent.core.actions),
            &mut sender_events,
            &mut receiver_events,
        );
        assert!(sender_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::Delivered { message_id } if *message_id == resource.message_id
        )));
        assert!(receiver_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::MessageReceived(received)
                if received.message_id == resource.message_id
                    && received.content.len() == 1_200
        )));
    }

    #[test]
    fn direct_link_uses_identity_cached_by_core_without_lxmf_peer_state() {
        let mut sender = peer(30);
        let mut receiver = peer(120);
        let announce = receiver
            .node
            .announce_destination(&receiver.destination, None)
            .expect("announce receiver");

        // Feed the verified announce only to Core. LXMF deliberately does not
        // observe its event, proving link setup has no separate peer-key cache.
        for packet in take_packets(announce.actions) {
            let _ = sender.node.handle_packet(InterfaceId(0), &packet);
        }

        assert!(sender
            .node
            .storage()
            .get_identity(receiver.destination.as_bytes())
            .is_some());
        let (state, _) = sender
            .lxmf
            .ensure_direct_link(&mut sender.node, receiver.destination)
            .expect("start direct link from Core identity");
        assert!(matches!(state, DirectLinkState::Started(_)));
    }

    #[test]
    fn nodecore_direct_resources_are_full_duplex_on_one_link() {
        let mut a = peer(20);
        let mut b = peer(110);
        exchange_announces(&mut a, &mut b);

        let mut a_events = Vec::new();
        let mut b_events = Vec::new();
        let (_, link_output) = a
            .lxmf
            .ensure_direct_link(&mut a.node, b.destination)
            .expect("establish direct link");
        pump(
            &mut a,
            &mut b,
            take_packets(link_output.core.actions),
            &mut a_events,
            &mut b_events,
        );

        // Delivering one packet identifies the initiator over the backchannel,
        // allowing both peers to address the same established link.
        let introduction = message(
            b.destination,
            &a.signing_identity,
            a.destination,
            DeliveryMethod::Direct,
            12,
        );
        let sent = a
            .lxmf
            .send(&mut a.node, &introduction)
            .expect("send introduction");
        pump(
            &mut a,
            &mut b,
            take_packets(sent.core.actions),
            &mut a_events,
            &mut b_events,
        );
        assert_eq!(
            a.lxmf.direct_link(&b.destination),
            b.lxmf.direct_link(&a.destination)
        );

        let large = message(
            b.destination,
            &a.signing_identity,
            a.destination,
            DeliveryMethod::Direct,
            12_000,
        );
        let small = message(
            a.destination,
            &b.signing_identity,
            b.destination,
            DeliveryMethod::Direct,
            700,
        );
        assert_eq!(
            LxmfNode::representation(&large).expect("large representation"),
            DeliveryRepresentation::DirectResource
        );
        assert_eq!(
            LxmfNode::representation(&small).expect("small representation"),
            DeliveryRepresentation::DirectResource
        );

        let large_output = a
            .lxmf
            .send(&mut a.node, &large)
            .expect("send large resource");
        let small_output = b
            .lxmf
            .send(&mut b.node, &small)
            .expect("send reverse resource");
        pump_bidirectional(
            &mut a,
            &mut b,
            take_packets(large_output.core.actions),
            take_packets(small_output.core.actions),
            &mut a_events,
            &mut b_events,
        );

        assert!(a_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::Delivered { message_id } if *message_id == large.message_id
        )));
        assert!(b_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::MessageReceived(received)
                if received.message_id == large.message_id
                    && received.content.len() == 12_000
        )));
        assert!(b_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::Delivered { message_id } if *message_id == small.message_id
        )));
        assert!(a_events.iter().any(|event| matches!(
            event,
            LxmfNodeEvent::MessageReceived(received)
                if received.message_id == small.message_id
                    && received.content.len() == 700
        )));
    }
}
