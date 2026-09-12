//! LXMF wire format, delivery, and anti-spam primitives.
//!
//! This crate is `no_std + alloc` and contains no platform I/O. Its transport
//! adapters drive `leviculum-core` and return actions for the application to
//! dispatch.
//!
//! # Propagation
//!
//! Propagation support covers both ends of the *client ↔ node* exchange.
//! [`PropagationUpload`] encodes the Python-compatible origin upload envelope
//! and decodes it again on the receiving node, [`MessageGetRequest`] and the
//! [`MessageListResponse`] / [`MessageGetResponse`] pair carry the `/get`
//! list, download, and acknowledgement exchange in both directions,
//! [`PropagationSignal`] carries a node's refusal of an upload, and
//! [`PropagationNodeAnnounce`] both encodes a node's own discovery data and
//! decodes another node's. The node side of the same exchange is
//! [`propagation_node::PropagationNode`] (leviculum#384 part 1): the role
//! that stores uploads and answers `/get`, over the
//! [`propagation_store::PropagationStore`] boundary a host directory or the
//! boards' record log implements. A host built on these still owns links,
//! resources and stamp-validation scheduling: this crate performs no I/O.
//!
//! The *node ↔ node* direction (leviculum#384 part 2) lives in
//! [`peering`]: the peer table with its cursor-per-peer replacement for the
//! reference's per-message sets, the `/offer` codec and inbound gate, and
//! the pure decisions of an outbound sync round. A Python propagation node
//! syncs with its peers over `/offer`, whose payload is the client upload's
//! wire shape carrying more than one message, admitted only against a
//! validated peering key (`LXMRouter.propagation_resource_concluded`,
//! reference/LXMF/LXMF/LXMRouter.py:2336-2345, and the peering-key branch
//! at :2381-2389). [`PropagationUpload::decode`] still accepts the
//! singleton envelope only and answers the multi-message form with
//! [`PropagationError::MultipleMessages`]; the peer-sync form is decoded by
//! [`peering::PeerSyncEnvelope`] where the key gate can be applied.

#![no_std]
#[cfg(test)]
extern crate std;

extern crate alloc;

pub mod announce;
pub mod attachments;
pub mod constants;
pub mod control;
pub mod message;
pub mod msgpack;
pub mod node;
pub mod paper;
pub mod peering;
pub mod propagation;
pub mod propagation_client;
pub mod propagation_node;
pub mod propagation_store;
pub mod router;
pub mod stamp;
pub mod storage;
pub mod telemetry;
pub mod ticket;

pub use attachments::{
    AttachmentError, AudioAttachment, FileAttachment, ImageAttachment, MessageAttachments,
};
pub use control::{
    encode_control_ack, encode_control_error, ControlNodeStats, ControlPeerStats, ControlResponse,
    CONTROL_ASPECTS, HOPS_UNKNOWN, STATS_GET_PATH, SYNC_REQUEST_PATH, UNPEER_REQUEST_PATH,
};
pub use message::{DeliveryMethod, Field, Message, MessageError, Verification};
pub use node::{
    DeliveryFailure, DeliveryRepresentation, DirectLinkState, InboundRejection,
    IncomingResourceTransfer, LxmfNode, LxmfNodeConfig, LxmfNodeError, LxmfNodeEvent,
    LxmfNodeOutput, LxmfResourceSendParams, PreparedLxmfSend, SubmissionId,
};
pub use paper::{PaperError, PaperMessage};
pub use peering::{
    answer_offer, build_offer, peering_key_material, response_action, DeclineReason, DropReason,
    InboundGate, MemoryPeerStore, OfferPlan, OfferResponse, Peer, PeerChange, PeerOffer,
    PeerRecord, PeerStore, PeerSyncEnvelope, PeerTable, PeeringConfig, ResponseAction, SyncPhase,
    OFFER_REQUEST_PATH,
};
pub use propagation::{
    MessageGetRequest, MessageGetResponse, MessageListResponse, MetadataEntry, PeerError,
    PropagatedMessage, PropagationError, PropagationNodeAnnounce, PropagationSignal,
    PropagationUpload, TransferLimit, TransientId, MESSAGE_GET_PATH,
};
pub use propagation_client::{
    KnownPropagationNode, PreparedUpload, PropagationRequestKind, PropagationTransport,
    PropagationTransportError, PropagationTransportEvent, PropagationTransportOutput,
    PropagationUploadFailure, PropagationUploadRepresentation, UploadSendParams,
    PROPAGATION_ASPECT,
};
pub use propagation_node::{
    Eviction, EvictionReason, GetError, GetOutcome, PropagationNode, PropagationNodeConfig,
    UploadOutcome, MESSAGE_EXPIRY_SECS, PN_META_NAME,
};
pub use propagation_store::{MemoryPropagationStore, PropagationStore, StoredMessage};
pub use router::{
    BuiltResource, DeliveryStampRequest, InboundStampRequest, PendingResourceBuild,
    PropagationStampRequest,
};
#[cfg(feature = "pow")]
pub use stamp::{CooperativeStamper, CooperativeYield, StampError, StampExecutor, Yield};
