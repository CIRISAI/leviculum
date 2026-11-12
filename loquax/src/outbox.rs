//! The seam between the frontend and the message engine.
//!
//! Decision 1 of the design record (`docs/src/concepts/loquax.md`, 2026-08-08)
//! is "C, built as A first": one process now, **with the router and the store
//! behind an interface** so that daemon mode later is a wiring change and not
//! a rewrite. This module is that interface.
//!
//! It is deliberately a command/event seam rather than a set of methods that
//! return answers. Two reasons, and they are the same two the architecture
//! record gives for the shape of the whole program:
//!
//! * The engine runs inside the driver's tick, under a non-reentrant core
//!   mutex, and everything crossing that boundary is already a non-blocking
//!   queue push (`docs/src/concepts/loquax-architecture.md` §3). A seam shaped
//!   like the traffic that actually crosses it needs no adapter.
//! * A daemon-mode implementation is a socket, and a socket is a command
//!   stream and an event stream. An interface built from request/response
//!   methods would have to be rewritten into this shape the day `loquaxd`
//!   exists, which is exactly what the decision forbids.
//!
//! Nothing here mentions `LxmfRouter`, `ReticulumNode` or a channel type, so
//! [`crate::send`] can be driven by a fake with no network anywhere.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use leviculum_lxmf::router::{AttemptFailure, MessageState, PropagationPathAttempts};
use leviculum_lxmf::Field;

use crate::mailbox_state::{Decision, Route, Stored};

/// How a message should reach its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// Straight to the peer over the mesh, the delivery method chosen by
    /// body size (a small body goes as one opportunistic packet).
    Direct,
    /// Straight to the peer, but always over a delivery link. This is the
    /// first leg of `--via auto`: a link either comes up — the peer is
    /// alive — or it does not, which is the crisp failure the fallback
    /// decision needs. An opportunistic packet cannot fail that way; it is
    /// `Sent` the moment it leaves, reachable peer or not.
    Link,
    /// Through a propagation node's mailbox. Requires a node to have been
    /// selected with [`Command::SelectPn`] first.
    Propagated,
}

impl Via {
    /// The word the event log and `--via` use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Link => "link",
            Self::Propagated => "propagated",
        }
    }
}

/// One message, as the frontend describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    pub destination: [u8; 16],
    pub title: Vec<u8>,
    pub body: Vec<u8>,
    /// The LXMF fields, raw. The messenger itself sends none; a test's
    /// correspondent sends telemetry through it, as a Sideband peer would.
    pub fields: Vec<Field>,
    pub via: Via,
}

/// What the frontend asks the engine to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Learn the destination's identity and a path to it. A message cannot be
    /// encrypted to a peer whose public key we have never seen, so this is a
    /// precondition and not an optimisation.
    Resolve { destination: [u8; 16] },
    /// Queue one message.
    Send(Box<SendRequest>),
    /// Select the propagation node every later [`Via::Propagated`] send and
    /// every [`Command::Fetch`] talks to. `preferred` is the `--pn` flag or
    /// the configured default, and when it is set `mailbox` is not read.
    /// Without it the mailbox rule ([`crate::mailbox::select`]) decides from
    /// `mailbox` — the reference `LXMRouter` keeps no autoselection of its
    /// own (its clients pick; NomadNet and Sideband both do), so the rule is
    /// loquax's, recorded in `docs/src/concepts/loquax-mailbox.md`.
    SelectPn {
        preferred: Option<[u8; 16]>,
        mailbox: Option<MailboxInput>,
    },
    /// Drain the selected node's mailbox with the genuine list/fetch/confirm
    /// round (`message_get_request` semantics). `path_budget_ms` is how long
    /// the drain may keep asking for a path to the node: the path request
    /// repeats every `PATH_REQUEST_WAIT_MS` for all of it (leviculum#719).
    Fetch { path_budget_ms: u64 },
    /// Remove a queued message. The auto fallback cancels the stranded
    /// direct copy before uploading the propagated one, so a peer that
    /// reappears mid-fallback cannot be handed the same message twice.
    Cancel { message_id: [u8; 32] },
    /// Check a received message's signature once its sender's key is
    /// known: one the store kept unverified from an earlier run
    /// ([`OutboxEvent::Unverified`]). `packed` is the message as it came.
    Verify {
        message_id: [u8; 32],
        source: [u8; 16],
        packed: Vec<u8>,
    },
    /// Take in one message pulled from the home node, packed as it came
    /// (`docs/src/concepts/loquax-home-node.md`): it is reported as
    /// [`OutboxEvent::Received`], its signature checked against its
    /// sender's key as a received message's is.
    Ingest { packed: Vec<u8> },
    /// Announce our delivery destination again, on the next tick. The home
    /// node does it on a slow timer, as lxmd's `announce_interval` would.
    Announce,
    /// Queue one message signed and packed before, as it is: the home
    /// node's outbox (`crate::home::outgoing`), which hands the same bytes
    /// to the direct leg and the propagated one. `via` is [`Via::Link`] or
    /// [`Via::Propagated`]. A message the router will not take is reported
    /// as [`OutboxEvent::Unsent`].
    SendPacked { packed: Vec<u8>, via: Via },
    /// On a home node: the message handed out as [`OutboxEvent::Packed`] is
    /// archived, so its proof goes out now
    /// ([`leviculum_lxmf::router::LxmfRouter::release_proofs`]). A sender
    /// is told DELIVERED only for a message the archive holds (1239).
    Prove { message_id: [u8; 32] },
    /// Answered with [`OutboxEvent::Fenced`] once every command before it
    /// was taken: a runner that waits for it knows the events those
    /// commands caused were handed out before it.
    Fence { token: u64 },
    /// Answered with [`OutboxEvent::Census`]: the engine's own tables and
    /// its router's, for the mailbox's `MAILBOX_CENSUS` (order 1585).
    Census,
    /// Announce our own position on `lev.position` (1302,
    /// [`crate::mesh_position`]), registering the destination of our
    /// identity on first use: `loquax announce`, the device walk's seed of
    /// a node nearby. A client has no identity of its own and ignores it.
    /// It goes out as it is, plain ([`crate::mesh_position::manual`]).
    AnnouncePosition {
        position: leviculum_lxmf::lev_position::Position,
    },
    /// On a client: the home node accepted a message the shell handed it
    /// with `/send`; reported as [`OutboxEvent::Queued`], so the screen
    /// names its bubble as it names one its own router queued.
    HomeAccepted { message_id: [u8; 32] },
    /// On a client: a state change of a message sent through the home node,
    /// pulled from its archive; reported as [`OutboxEvent::State`].
    HomeState {
        message_id: [u8; 32],
        state: MessageState,
    },
    /// On a client: a message one of the home's devices sent through the
    /// home node, pulled from its archive as the home node signed it;
    /// reported as [`OutboxEvent::HomeSent`].
    HomeSent { packed: Vec<u8> },
    /// On a client: a mark one of the home's devices set, pulled from the
    /// archive; reported as [`OutboxEvent::HomeMarked`].
    HomeMark { mark: crate::home::wire::Mark },
    /// On a client: the home node gave up a leg of a message sent through
    /// it that went without an end, and tries again
    /// ([`crate::home::KIND_STALLED`]); reported as
    /// [`OutboxEvent::HomeStalled`].
    HomeStalled { message_id: [u8; 32] },
    /// What the propagation node said of messages the home sent (1409,
    /// [`crate::home::KIND_PN_STATUS`]); reported as
    /// [`OutboxEvent::HomePnStatus`].
    HomePnStatus {
        answers: Vec<([u8; 32], leviculum_lxmf::lev_status::MessageStatus)>,
    },
    /// On a client: a hub the home heard announcing (1599,
    /// [`crate::home::KIND_HUB`]); reported as [`OutboxEvent::HubAnnounced`],
    /// as a hub the client heard itself would be.
    HomeHub { hub: crate::ui::hubs::HeardHub },
    /// On a client: a name the home node heard a peer announce, pulled from
    /// the archive; reported as [`OutboxEvent::Announced`], as an announce
    /// the client heard itself would be.
    HomeName { destination: [u8; 16], name: String },
    /// On a client: the reader's name for a peer one of the home's devices
    /// set, `None` for none, pulled from the archive (1139), and when the
    /// home took it, unix seconds on the home's clock (1635); reported as
    /// [`OutboxEvent::HomePetname`].
    HomePetname {
        destination: [u8; 16],
        petname: Option<String>,
        at: i64,
    },
    /// On a client: the reader's word on a peer one of the home's devices
    /// set, pulled from the archive (1146); reported as
    /// [`OutboxEvent::HomePeerState`].
    HomePeerState {
        destination: [u8; 16],
        state: crate::home::wire::PeerState,
    },
}

/// What the mailbox rule is fed from, read by the frontend before it selects:
/// the daemon's path table is an RPC round trip, which the engine, running
/// under the core lock, may not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxInput {
    /// The daemon's path table, or why it could not be read. Without it no
    /// node on this device can be told from one elsewhere, so nothing new is
    /// adopted; a stored node is kept.
    pub routes: Result<Vec<Route>, String>,
    /// What an earlier run selected (`${LNMSG_HOME}/propagation_node`).
    pub previous: Option<Stored>,
    /// Nodes that stopped answering ([`crate::mailbox::Unanswered`]): an
    /// adopted one among them is replaced, and none is adopted.
    pub excluded: Vec<[u8; 16]>,
}

/// One engine's own tables and its router's, by count and estimated heap
/// bytes (order 1585). The messages behind the router's outbound entries
/// are not followed, as in lnpnd's `PN_CENSUS mailbox=`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineCensus {
    /// [`leviculum_lxmf::router::LxmfRouter::heap_bytes`]; 0 for a client,
    /// which has no router of its own.
    pub router_bytes: usize,
    /// The engine's own maps and queues.
    pub engine_bytes: usize,
    /// Messages this run queued and not yet left.
    pub tracked: usize,
    /// Recipients of packed messages this run sent.
    pub recipients: usize,
    /// Senders whose messages wait on their key.
    pub unheard: usize,
    /// Events not yet taken by the daemon.
    pub events_queued: usize,
    /// The engine's ticks since it started (order 1723).
    pub ticks: u64,
}

/// What the engine reports back.
///
/// Ordered by when they can first appear, which is also the order
/// [`crate::send::run_send`] waits for them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxEvent {
    /// The engine registered its delivery destination. `address` is ours.
    Ready { address: [u8; 16] },
    /// The engine could not become ready at all, and never will.
    Broken { detail: String },
    /// A [`Command::Resolve`] destination is now reachable.
    Resolved { destination: [u8; 16] },
    /// The router accepted a message and gave it an id.
    Queued { message_id: [u8; 32] },
    /// The router refused a message. No id exists.
    Refused { detail: String },
    /// One outbound state transition.
    State {
        message_id: [u8; 32],
        state: MessageState,
    },
    /// A message one of the home's devices sent through the home node
    /// ([`Command::HomeSent`]): the screen shows it as sent, a bubble of its
    /// own where this device does not have it yet.
    HomeSent {
        message_id: [u8; 32],
        destination: [u8; 16],
        /// When the sending device composed it, the message's timestamp in
        /// unix seconds: the bubble's time on every other device.
        sent_at: i64,
        body: Vec<u8>,
        fields: Vec<Field>,
    },
    /// A mark one of the home's devices set ([`Command::HomeMark`]).
    HomeMarked { mark: crate::home::wire::Mark },
    /// The home node gave up a silent leg of a message and tries again
    /// ([`Command::HomeStalled`]).
    HomeStalled { message_id: [u8; 32] },
    /// What the propagation node said of messages the home sent
    /// ([`Command::HomePnStatus`]).
    HomePnStatus {
        answers: Vec<([u8; 32], leviculum_lxmf::lev_status::MessageStatus)>,
    },
    /// The reader's name for a peer, set on one of the home's devices
    /// ([`Command::HomePetname`]).
    HomePetname {
        destination: [u8; 16],
        petname: Option<String>,
        at: i64,
    },
    /// The reader's word on a peer, set on one of the home's devices
    /// ([`Command::HomePeerState`]).
    HomePeerState {
        destination: [u8; 16],
        state: crate::home::wire::PeerState,
    },
    /// The message left the router's outbound queue, which the router does
    /// only in a terminal state (`leviculum-lxmf/src/router.rs:872-874`).
    /// `last` is the last state seen for it, when there was one, and
    /// `failure` why its last attempt failed, when one did: `Failed` alone
    /// does not say whether a link never came or died mid-transfer (1330).
    Left {
        message_id: [u8; 32],
        last: Option<MessageState>,
        failure: Option<AttemptFailure>,
    },
    /// A [`Command::SelectPn`] found its node. `stamp_cost` is what the node
    /// announced, i.e. what a client will have to mine per upload.
    PnSelected {
        destination: [u8; 16],
        stamp_cost: Option<u64>,
    },
    /// A [`Command::SelectPn`] could not name a node at all.
    PnUnavailable { detail: String },
    /// The mailbox rule kept or adopted a node; [`OutboxEvent::PnSelected`]
    /// for it follows. Only a selection without `preferred` reports this.
    Mailbox(Decision),
    /// One inbound message reached our delivery destination — directly or
    /// out of a mailbox drain; the router has already de-duplicated within
    /// this run, and the frontend de-duplicates across runs by `message_id`.
    Received {
        message_id: [u8; 32],
        source: [u8; 16],
        title: Vec<u8>,
        body: Vec<u8>,
        /// The message's fields as they came, each value raw msgpack: the
        /// core reads telemetry from them ([`crate::telemetry`]) and the
        /// store keeps them.
        fields: Vec<Field>,
        /// Whether the signature verified against a known source identity.
        verified: bool,
        /// The message's LXMF timestamp in unix seconds: when its writer
        /// wrote it by the writer's clock (a group copy's is the home's
        /// acceptance, its writer's own rides the `lev` pair). The bubble
        /// shows this, not when this device fetched it (1445); 0 when not
        /// known.
        sent_at: i64,
    },
    /// The message just [`OutboxEvent::Received`] came through the
    /// propagation network: the router handed it on as
    /// `DeliveryMethod::Propagated` (set where a collected message is
    /// unpacked, `leviculum-lxmf/src/router/propagation_runtime.rs`). The
    /// screen owes its sender a propagation receipt when the message asks
    /// for one (1395, [`crate::ui::arrival`]).
    Propagated {
        message_id: [u8; 32],
        source: [u8; 16],
    },
    /// The message just [`OutboxEvent::Received`] came from a sender whose
    /// key is not known, so its signature could not be checked. The engine
    /// asks for the sender's path and checks it when the key arrives
    /// ([`OutboxEvent::Verified`]); `packed` is the message as it came, for
    /// the store to keep, so a later run can check it too
    /// ([`Command::Verify`]).
    Unverified {
        message_id: [u8; 32],
        source: [u8; 16],
        packed: Vec<u8>,
    },
    /// A held message's signature was checked against its sender's key, now
    /// known: `valid` is the verdict, and a message whose signature does not
    /// hold is said to be invalid, never left looking unverified.
    Verified {
        message_id: [u8; 32],
        source: [u8; 16],
        valid: bool,
    },
    /// A peer announced its delivery destination. `name` is the display
    /// name as the reference reads it from the app data
    /// (`LXMF.display_name_from_app_data`; [`crate::names::from_app_data`]),
    /// not yet sanitised: the screen does that ([`crate::names`]).
    /// `path_response` says the announce answered a path request, as a rule
    /// from a cache: it names the peer as well as a live one does, and says
    /// nothing of whether the peer is there (1330). A repeat of the last
    /// announce heard from a recipient of a packed message is one too,
    /// however it came: a shared instance forwards an answer from the
    /// network as a plain announce (1332, `Engine::replayed`).
    Announced {
        destination: [u8; 16],
        name: Option<String>,
        path_response: bool,
    },
    /// A propagation node announced its `lxmf.propagation` destination: the
    /// screen's mailbox schedule syncs on its own node's
    /// (`crate::sync::SyncEvent::Announce`).
    /// `status` is the `/lev/status` version its announce says it answers
    /// (`{"lev": {"status": 1}}` under `PN_META_CUSTOM`, lnpnd); none for a
    /// stock node, which a sender never asks (1395).
    /// `name` is the name it announces under `PN_META_NAME`, unsanitised:
    /// what the screen says the relay by (1587).
    NodeAnnounced {
        destination: [u8; 16],
        status: Option<u64>,
        name: Option<String>,
    },
    /// A propagation node proved the upload of the queued message
    /// `message_id`, which it stores under `transient_id`: what a sender
    /// asks it by later (`/lev/status`, 1395, [`crate::pn_status`]).
    /// `node` is the propagation node the router had selected, the one
    /// that proved it.
    Uploaded {
        message_id: [u8; 32],
        transient_id: [u8; 32],
        node: Option<[u8; 16]>,
    },
    /// A node announced its position on `lev.position` (1302,
    /// [`crate::mesh_position`]): `identity` is the hash of the identity
    /// the announce proved, `location` what its app data said, timed by
    /// when the node heard it, `name` the name it carried. A position
    /// sealed to somebody else never becomes one
    /// ([`crate::mesh_position::read`]). Handed to the screen as
    /// [`crate::ui::UiEvent::MeshPosition`].
    MeshPosition {
        identity: [u8; 16],
        location: leviculum_lxmf::telemetry::Location,
        name: Option<String>,
    },
    /// A hub announced its `rrc.hub` destination (1599,
    /// [`crate::ui::hubs`]): heard by this node, timed and with the hops of
    /// the hearing, or pulled from the home ([`Command::HomeHub`]). The
    /// screen lists it among the hubs ([`crate::ui::rooms::RoomsEvent::Heard`]);
    /// a home keeps it for its devices.
    HubAnnounced { hub: crate::ui::hubs::HeardHub },
    /// A NomadNet node announced its pages (1579, `nomadnetwork.node`),
    /// heard by this node, timed and with the hops of the hearing. A home
    /// keeps it for its devices ([`crate::home::pages`]); a device's
    /// Browse tab lists the nodes its own fetch hears.
    PageAnnounced {
        node: lnomad::discovery::DiscoveredNode,
    },
    /// A [`Command::Fetch`]'s link to the node came up: from here a failure
    /// is the node's answer or the transfer, not a node that never answered.
    SyncLinked,
    /// A [`Command::Fetch`] finished its round. `received` and `duplicates`
    /// are the router's own counts for the drain.
    SyncDone { received: usize, duplicates: usize },
    /// A [`Command::Fetch`] failed, with the client state that ended it.
    /// `path` is what the path phase did, when it was a missing path that
    /// ended it.
    SyncFailed {
        detail: String,
        path: Option<PropagationPathAttempts>,
    },
    /// A propagated [`Command::SendPacked`] the router did not take since
    /// this node does not hold the key of its `destination`: the copy on
    /// the node is encrypted for the recipient. The recipient's path was
    /// asked, and the announce that answers it brings the key (1406: the
    /// router held such a leg without a word until the home's stall bound
    /// gave it up). No queue entry, so nothing else is said of it.
    KeyMissing {
        message_id: [u8; 32],
        destination: [u8; 16],
    },
    /// A [`Command::SendPacked`] the router did not take: no queue entry, so
    /// nothing else will be said of it.
    Unsent {
        message_id: [u8; 32],
        detail: String,
    },
    /// Every command before the [`Command::Fence`] with this token was
    /// taken.
    Fenced { token: u64 },
    /// The engine's tables at the moment it took a [`Command::Census`].
    Census(EngineCensus),
    /// The message just [`OutboxEvent::Received`], packed as it came, for a
    /// home node's archive. Only an engine asked for it hands it out
    /// ([`crate::engine::Engine::handing_out_packed`]), and that engine holds
    /// the message's proof until a [`Command::Prove`] says it is archived.
    Packed {
        message_id: [u8; 32],
        packed: Vec<u8>,
        arrived: Arrived,
    },
}

/// How a received message reached us, as the router read it: the `via` of a
/// home's `HOME_ACCEPT` (1305).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrived {
    /// Over a delivery link.
    Direct,
    /// As one single packet, encrypted to the delivery destination.
    Opportunistic,
    /// Collected from a propagation node.
    Propagated,
    /// From an `lxm://` paper message.
    Paper,
}

impl Arrived {
    /// The word the event log uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Opportunistic => "opportunistic",
            Self::Propagated => "propagated",
            Self::Paper => "paper",
        }
    }
}

impl From<leviculum_lxmf::DeliveryMethod> for Arrived {
    fn from(method: leviculum_lxmf::DeliveryMethod) -> Self {
        use leviculum_lxmf::DeliveryMethod;
        match method {
            DeliveryMethod::Direct => Self::Direct,
            DeliveryMethod::Opportunistic => Self::Opportunistic,
            DeliveryMethod::Propagated => Self::Propagated,
            DeliveryMethod::Paper => Self::Paper,
        }
    }
}

/// The engine went away — in this slice, the driver detached a panicking
/// processor, or the node was torn down under us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxGone;

impl core::fmt::Display for OutboxGone {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the message engine stopped answering")
    }
}

impl core::error::Error for OutboxGone {}

/// The engine, as a frontend sees it.
///
/// `submit` must not block: in the in-process implementation the far end is a
/// hook running under the core mutex, and a frontend that could stall there
/// would stall the whole node. `try_next_event` must not block either, for the
/// same reason from the other side — the caller decides how long to wait and
/// how to spend the time.
pub trait Outbox {
    /// Hand one command to the engine. Non-blocking.
    fn submit(&self, command: Command) -> Result<(), OutboxGone>;

    /// Take the next event if one is waiting. Never blocks.
    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone>;
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted [`Outbox`] for the frontend's own tests.

    use alloc::collections::VecDeque;
    use core::cell::RefCell;

    use super::*;

    /// Answers commands from a script, records what it was asked to do.
    pub struct FakeOutbox {
        /// Events handed out in order, one per `try_next_event` call.
        pub events: VecDeque<OutboxEvent>,
        /// Every command the frontend submitted, in order.
        pub commands: RefCell<Vec<Command>>,
        /// When set, every call fails as though the engine had died.
        pub gone: bool,
    }

    impl FakeOutbox {
        pub fn new(events: Vec<OutboxEvent>) -> Self {
            Self {
                events: events.into(),
                commands: RefCell::new(Vec::new()),
                gone: false,
            }
        }
    }

    impl Outbox for FakeOutbox {
        fn submit(&self, command: Command) -> Result<(), OutboxGone> {
            if self.gone {
                return Err(OutboxGone);
            }
            self.commands.borrow_mut().push(command);
            Ok(())
        }

        fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
            if self.gone {
                return Err(OutboxGone);
            }
            Ok(self.events.pop_front())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn via_spells_itself_the_way_the_flag_does() {
        assert_eq!(Via::Direct.as_str(), "direct");
        assert_eq!(Via::Link.as_str(), "link");
        assert_eq!(Via::Propagated.as_str(), "propagated");
    }
}
