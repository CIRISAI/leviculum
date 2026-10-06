//! Direct-link upgrade (+ciris, leviculum#70): NAT traversal for two peers
//! that already share a link through a transport node.
//!
//! Reticulum's answer to NAT is to dial out to a public transport node and
//! let it relay. That works, but every packet between two NAT'd peers then
//! costs the relay a hop and the peers its latency. This module lets the two
//! ends of an established link learn their reflexive (public) UDP addresses
//! from a facilitator, swap them over the link itself, and punch a direct UDP
//! path through both NATs. When the punch works the link moves onto a new
//! point-to-point interface; when it does not (symmetric NAT, which is common
//! on cellular) nothing changes and the relayed link carries on.
//!
//! The wire protocol is rns-rs's "DirectLink" (`docs/direct-link-protocol.md`
//! in lelloman/rns-rs), implemented here independently so a leviculum node
//! can upgrade a link to an rns-rs node and use an rns-rs facilitator. The
//! signals are channel system messages (MSGTYPE `0xFE00..=0xFE04`), so they
//! change no Reticulum packet format.
//!
//! **Only propose to a peer known to speak this.** A Python RNS peer proves
//! the channel packet carrying a REQUEST and then fails to construct the
//! unknown message type, so its receive sequence never advances past it and
//! every later channel message on that link stalls behind it. That is why an
//! upgrade is only ever started by an explicit call, never automatically.
//!
//! [`wire`] holds the byte formats, [`session`] one upgrade's state machine.
//! The node-level bookkeeping lives in `node::direct_link`; sockets are the
//! driver's.

/// The MTU a link is lowered to when it moves onto a direct interface: what
/// fits one unfragmented UDP datagram on common internet paths. The same
/// value rns-rs uses, so a mixed pair agrees on the link MDU.
pub const DIRECT_LINK_MTU: u32 = 1400;

pub mod session;
pub mod wire;

pub use session::{Failure, Phase, Role, Session, Step};
pub use wire::{ProbeProtocol, SessionId, Signal};
