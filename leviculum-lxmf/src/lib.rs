//! LXMF wire format, delivery, and anti-spam primitives.
//!
//! This crate is `no_std + alloc` and contains no platform I/O. Its transport
//! adapters drive `leviculum-core` and return actions for the application to
//! dispatch.
//!
//! Propagation support is client-side: [`PropagationUpload`] implements the
//! Python-compatible origin upload envelope, [`MessageGetRequest`] implements
//! the `/get` list, download, and acknowledgement exchange, and
//! [`PropagationNodeAnnounce`] decodes propagation-node discovery data. The
//! crate does not host propagation nodes or implement the peer `/offer` path.

#![no_std]

extern crate alloc;

pub mod announce;
pub mod attachments;
pub mod constants;
pub mod message;
pub mod msgpack;
pub mod node;
pub mod paper;
pub mod propagation;
pub mod propagation_client;
pub mod router;
pub mod stamp;
pub mod storage;
pub mod ticket;

pub use attachments::{
    AttachmentError, AudioAttachment, FileAttachment, ImageAttachment, MessageAttachments,
};
pub use message::{DeliveryMethod, Field, Message, MessageError, Verification};
pub use node::{
    DeliveryFailure, DeliveryRepresentation, DirectLinkState, InboundRejection,
    IncomingResourceTransfer, LxmfNode, LxmfNodeConfig, LxmfNodeError, LxmfNodeEvent,
    LxmfNodeOutput, SubmissionId,
};
pub use paper::{PaperError, PaperMessage};
pub use propagation::{
    MessageGetRequest, MessageGetResponse, MessageListResponse, MetadataEntry, PeerError,
    PropagationError, PropagationNodeAnnounce, PropagationSignal, PropagationUpload, TransferLimit,
    TransientId, MESSAGE_GET_PATH,
};
pub use propagation_client::{
    KnownPropagationNode, PropagationRequestKind, PropagationTransport, PropagationTransportError,
    PropagationTransportEvent, PropagationTransportOutput, PropagationUploadFailure,
    PropagationUploadRepresentation, PROPAGATION_ASPECT,
};
pub use router::{DeliveryStampRequest, InboundStampRequest, PropagationStampRequest};
#[cfg(feature = "pow")]
pub use stamp::{CooperativeStamper, CooperativeYield, StampError, StampExecutor, Yield};
