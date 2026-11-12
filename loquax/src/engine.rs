//! The in-process message engine: an [`LxmfRouter`] driven from inside the
//! driver's tick, and the [`Outbox`](crate::outbox::Outbox) the frontend
//! sees it through.
//!
//! # Why the engine is a `CoreProcessor`
//!
//! `leviculum-std`'s public event stream classifies before it delivers, and
//! seven of the event types LXMF needs — `PacketReceived` and
//! `LinkDataReceived` among them, i.e. how a message *arrives* — are
//! `EventClass::Data` and droppable under load
//! (`leviculum-std/src/driver/processor.rs`, "Where the events come from").
//! A client fed from there would silently lose messages with nothing
//! underneath to retransmit them, so the tap is the only correct feed
//! (`docs/src/concepts/loquax-architecture.md` §3). This slice only sends, but
//! the feed is the same one the reading half will need, and picking the wrong
//! one now would have to be undone later.
//!
//! # The rule this file is written against
//!
//! Both hooks run **with the core mutex held, and that mutex is not
//! reentrant**. So this type owns no `ReticulumNode` and no handle derived
//! from one, and every side effect is a push onto an unbounded queue. Nothing
//! here does I/O, and the one unbounded computation in LXMF — proof-of-work
//! for a stamp — leaves through a queue and is mined by the host, the
//! terminal on a thread of its own ([`mine`] is what every host runs).
//!
//! # The core and the shell
//!
//! This file is the core, `no_std` plus `alloc`, and generic over the node
//! core (`NodeCore<R, C, S>`). The `CoreProcessor` impl for the driver's
//! `StdNodeCore`, the channels to the frontend ([`InProcessOutbox`]),
//! [`attach`], the stamp worker thread, its tokio runtime and `OsRng` are
//! the shell, in `engine/shell.rs` behind the `std` feature.
//!
//! # What it deliberately does not do
//!
//! No persistence. `LxmfRouter::persist` writes the outbound queue to an
//! `LxmfStorage`, and a `FileLxmfStorage` for it exists
//! (`leviculum-std/src/file_lxmf_store.rs:27`) — but in this slice nothing
//! would ever drain a restored queue, since there is no daemon and no
//! `loquax sync`. Persisting would buy a file on disk that no code reads, at
//! the price of a write inside the core lock. The moment daemon mode or a
//! resume path exists, this is where it goes.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::Poll;

use leviculum_core::constants::RANDOM_HASHBYTES;
use leviculum_core::identity::Identity;
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::{Clock as CoreClock, DestinationHash, NodeCore, Storage};
use leviculum_lxmf::propagation_client::PropagationTransport;
use leviculum_lxmf::router::{
    AttemptFailure, LxmfRouter, MessageState, PropagationClientConfig, PropagationClientState,
    PropagationStampRequest, RouterConfig, RouterError, RouterEvent, RouterOutput,
    PATH_REQUEST_WAIT_MS,
};
use leviculum_lxmf::{
    announce, CooperativeStamper, DeliveryMethod, DeliveryStampRequest, LxmfNode, LxmfNodeConfig,
    StampCancel, StampError, Verification,
};
use rand_core::CryptoRngCore;

use crate::clock::Clock;
use crate::effects::Effects;
use crate::mailbox::{self, Candidate, Previous, Selection};
use crate::mailbox_state::{Decision, Route};
use crate::outbox::{Command, MailboxInput, OutboxEvent, SendRequest, Via};

#[cfg(feature = "std")]
mod shell;
#[cfg(feature = "std")]
pub(crate) use shell::tcp_section;
#[cfg(feature = "std")]
pub use shell::{
    attach, attach_as, attach_leaf, attach_mesh, locale, parse_server, start_command,
    start_command_here, AttachConfig, AttachError, Attached, HomeRequest, InProcessOutbox,
    LeafConfig, Lender, Lent, MeshConfig, Role, LNSD_UNITS,
};

/// How soon the engine asks the driver to come back.
///
/// The command queue needs a periodic slot — an event tap can never
/// *initiate* anything, because it only fires when the core has something to
/// say — and so does a pending resolve. 200 ms is what `leviculum-lxmf-node`
/// uses for the same job, and it bounds command pickup latency at a fifth of
/// the driver's 1 s idle cadence.
const POLL_INTERVAL_MS: u64 = 200;

/// Why a client's engine sends nothing itself: a client holds no LXMF
/// identity, and its messages go through the home node's `/send`
/// (`docs/src/concepts/loquax-home-node.md`), which the shell asks, not the
/// engine.
pub const CLIENT_SENDS: &str = "this device is a client of a home node, and sends only \
                                through the home node";

/// How many path-table entries a selection asks the daemon to replay.
///
/// A client hears only the announces made while it is attached, and a node
/// announces every few hours, so on a first run the router knows no
/// propagation node at all. Asking the daemon for a path it already holds is
/// answered from its cache with the announce (`Transport.py:2943`, local
/// clients are served without transport), and that announce is what tells
/// the router which entries are propagation nodes. The fewest-hops entries
/// first; a mailbox is a near thing, and a transport node's table can hold
/// thousands.
const DISCOVERY_LIMIT: usize = 32;

/// How long a selection waits for those replays. They cross one local
/// socket, so anything not back by then is not coming from the cache.
const DISCOVERY_WINDOW_MS: u64 = 2_000;

/// How many times one hook call will re-feed router output into the router.
///
/// A core call made *inside* a hook can return events synchronously, and the
/// driver never hands those back, so closing that loop is the consumer's job
/// (Codeberg #204). An LXMF event that provokes an LXMF event is legitimate,
/// so the fixpoint is not something the router can promise, and an unbounded
/// loop under the core lock is a node hang rather than a bug report.
const MAX_ABSORB_ROUNDS: usize = 8;

/// Above this announced cost the mining worker reports its progress on
/// stderr: a grind the operator cannot see ending is indistinguishable from a
/// hang, and default propagation nodes announce 13 now.
const STAMP_PROGRESS_COST: u8 = 10;

/// How often a narrated grind says it is still going.
const STAMP_PROGRESS_EVERY_MS: u64 = 5_000;

/// How long the engine waits before asking again for the path of a sender
/// whose messages still wait on its key. The first request goes out on the
/// tick that saw the message; a repeat covers a request lost on the way,
/// and is rare because it is a mesh-wide packet for a key the reader can do
/// without in the meantime.
const UNHEARD_PATH_REQUEST_INTERVAL_MS: u64 = 5 * 60_000;

/// One proof-of-work job on its way to the miner.
///
/// Each job carries the handle that calls it off. The terminal's miner is one
/// thread and single-consumer, so an unfinishable grind does not only park
/// its own message, it parks every stamp queued behind it (Codeberg #185) —
/// the handle is what gets the miner back.
pub enum StampJob {
    /// A recipient delivery stamp, at the cost the recipient announced.
    Delivery(DeliveryStampRequest, Arc<StampCancel>),
    /// The independent propagation-node stamp over the transient ID, at the
    /// cost the selected node announced.
    Propagation(PropagationStampRequest, Arc<StampCancel>),
}

impl StampJob {
    /// Which stamp of which message this is.
    pub fn key(&self) -> (StampKind, [u8; 32]) {
        match self {
            Self::Delivery(request, _) => (StampKind::Delivery, request.message_id),
            Self::Propagation(request, _) => (StampKind::Propagation, request.message_id),
        }
    }

    /// The handle [`Engine`] fires when the message is cancelled.
    pub fn cancel(&self) -> Arc<StampCancel> {
        match self {
            Self::Delivery(_, cancel) | Self::Propagation(_, cancel) => Arc::clone(cancel),
        }
    }
}

/// Which of a message's two possible stamps a mining job is for. A propagated
/// message to a cost-announcing recipient legitimately mines both, one after
/// the other, so the single-flight set is keyed by kind as well as id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StampKind {
    Delivery,
    Propagation,
}

/// A stamp the miner has finished with.
pub enum StampAnswer {
    Delivery {
        request: DeliveryStampRequest,
        result: Result<[u8; 32], StampError>,
    },
    Propagation {
        request: PropagationStampRequest,
        result: Result<[u8; 32], StampError>,
    },
}

/// A destination the frontend asked to have resolved.
struct Resolve {
    destination: [u8; 16],
    /// When the path is next asked for, on the tick clock; `None` until the
    /// first request. Repeated every `PATH_REQUEST_WAIT_MS` while no path has
    /// come, the reference's wait between a path request and the next attempt
    /// (`reference/LXMF/LXMF/LXMRouter.py:33`, `:1779`): one request is one
    /// packet, and a packet lost to a daemon whose interface is still
    /// reconnecting must not cost the whole send (leviculum#699).
    ///
    /// There is no deadline here on purpose: the repeat runs until the
    /// frontend's own budget (`loquax send --timeout`) ends the run, the rule
    /// a fetch's path phase follows too (leviculum#719).
    next_path_request_ms: Option<u64>,
    /// When the first path request went out, and how many have, for the
    /// `PATH found` log line.
    first_path_request_ms: Option<u64>,
    path_requests: u32,
    /// Set once the answer has gone out, so a repeated ask is not answered
    /// twice.
    answered: bool,
}

/// The received messages of one sender whose key is not known yet, held
/// until it is (leviculum 1079).
///
/// The reference checks a signature once, when the message is unpacked, and
/// leaves an unknown source `SOURCE_UNKNOWN` for good
/// (`reference/LXMF/LXMF/LXMessage.py:807-817`); nor does `LXMRouter` ask for
/// the source's path (`LXMRouter.lxmf_delivery`, `LXMRouter.py:1837-1924`). Asking for
/// it and checking again changes nothing on the wire a peer sees, one path
/// request aside, and is what turns a genuine message from someone this
/// device has not heard yet into a verified one.
struct Unheard {
    /// Each message's id and the bytes it came as, which its signature
    /// covers.
    messages: Vec<([u8; 32], Vec<u8>)>,
    /// When the sender's path is next asked for, on the tick clock; `None`
    /// until the first request.
    next_path_request_ms: Option<u64>,
    path_requests: u32,
}

impl Unheard {
    /// The held messages' heap: the list and every message's bytes.
    fn heap_bytes(&self) -> usize {
        self.messages.capacity() * core::mem::size_of::<([u8; 32], Vec<u8>)>()
            + self
                .messages
                .iter()
                .map(|(_, packed)| packed.capacity())
                .sum::<usize>()
    }
}

/// A selection waiting for the daemon's announce replays.
struct PendingSelection {
    routes: Vec<Route>,
    previous: Option<Previous>,
    /// The nodes that stopped answering ([`MailboxInput::excluded`]).
    excluded: Vec<[u8; 16]>,
    /// The entries whose replay is awaited, removed as the engine's own
    /// event hook sees each announce. Not "the core knows the identity":
    /// the core stores that before the hook runs, and a tick in between
    /// would select before the router had heard the node.
    asked: Vec<[u8; 16]>,
    deadline_ms: u64,
}

/// The router, once it exists.
struct Ready {
    router: LxmfRouter,
    address: [u8; 16],
}

enum State {
    /// The identity is loaded but nothing is registered: registering needs
    /// `&mut StdNodeCore`, and a processor is installed on the *builder*,
    /// before the node it will run inside exists.
    Unregistered(Box<Identity>),
    Ready(Box<Ready>),
    /// Registration failed and was reported.
    Failed,
    /// A client of a home node (`docs/src/concepts/loquax-home-node.md`):
    /// no LXMF destination at all, so in particular none for the home
    /// identity, whose private key this device never holds. Messages come
    /// in as [`Command::Ingest`], pulled from the home node by the shell.
    Client(Client),
}

/// What a client engine keeps: the home node's LXMF address, which the
/// screen shows as ours, and whether `Ready` has been said.
struct Client {
    address: [u8; 16],
    said_ready: bool,
}

/// The LXMF engine: the router glue, driven from the node core's two hooks.
///
/// Generic over the node core the way the router it holds is, so the driver's
/// `StdNodeCore` and a board's core run the same engine. It reaches its host
/// through four queues of its own, each a `VecDeque`: commands in, events
/// out, stamp jobs out, stamp answers in. Unbounded on purpose, as the
/// `mpsc` channels they replace were: the hooks run under the core lock and
/// must neither block nor drop, and a dropped event can be a received
/// message. The host drains them around every hook, so what they hold is one
/// hook's worth; a bounded ring would add a drop policy and buy nothing.
pub struct Engine {
    display_name: Vec<u8>,
    events: VecDeque<OutboxEvent>,
    commands: VecDeque<Command>,
    stamps: VecDeque<StampJob>,
    stamp_answers: VecDeque<StampAnswer>,
    state: State,
    resolves: Vec<Resolve>,
    /// Messages this run queued, and the last state seen for each. The router
    /// forgets a message the moment it reaches a terminal state, so without
    /// this the removal could not be attributed.
    tracked: BTreeMap<[u8; 32], Option<MessageState>>,
    /// Why the last failed attempt of a tracked message failed, for its
    /// [`OutboxEvent::Left`] (1330).
    failures: BTreeMap<[u8; 32], AttemptFailure>,
    /// Stamps being mined, so a re-offer does not queue a second mine for the
    /// same work behind the first. Keyed by kind as well as id: a propagated
    /// message can need a recipient stamp and a node stamp in one run.
    ///
    /// The value is the handle that calls that mine off (Codeberg #185). It
    /// lives here rather than in the router because the router knows a stamp
    /// is wanted, while only this side knows one is actually running.
    mining: BTreeMap<(StampKind, [u8; 32]), Arc<StampCancel>>,
    /// Whether our own delivery announce has gone out. See [`Engine::announce`].
    announced: bool,
    /// A [`Command::SelectPn`] without a preference, waiting for the
    /// announces that tell it which path-table entries are propagation nodes.
    pending_selection: Option<PendingSelection>,
    /// Received messages waiting on their sender's key, by sender. See
    /// [`Unheard`].
    unheard: BTreeMap<[u8; 16], Unheard>,
    /// Whether every received message is also handed out packed
    /// ([`OutboxEvent::Packed`]): a home node archives it as it came.
    hands_out_packed: bool,
    /// Packets to our delivery destination that no key we hold opened,
    /// since the start: each is an `LXMF_RX_UNDECRYPTABLE` line and carries
    /// this count. See [`Engine::undecryptable`].
    undecryptable: u64,
    /// Whether this engine answers for a home, and says its announces as
    /// `HOME_ANNOUNCE` (1305). A hosted group's engine hands out packed too,
    /// but announces the group, not the home.
    says_home: bool,
    /// The recipients of packed messages this run sent, each with the random
    /// hash of the last announce heard from it. See [`Engine::replayed`].
    recipients: BTreeMap<[u8; 16], Option<[u8; RANDOM_HASHBYTES]>>,
    /// The recipient whose announce the event being handled repeats, set
    /// for the length of one [`Engine::on_event`].
    replayed: Option<[u8; 16]>,
    /// Calls of [`Engine::on_tick`] this run (order 1723): an idle engine
    /// ticks every `POLL_INTERVAL_MS`, so a tick deadline stuck at zero shows
    /// here as a count far above that pace, whatever the host's speed.
    ticks: u64,
}

impl Engine {
    /// An engine that registers `identity`'s delivery destination on its
    /// first hook and announces it under `display_name`.
    pub fn new(identity: Identity, display_name: Vec<u8>) -> Self {
        Self::starting_in(State::Unregistered(Box::new(identity)), display_name)
    }

    fn starting_in(state: State, display_name: Vec<u8>) -> Self {
        Self {
            display_name,
            events: VecDeque::new(),
            commands: VecDeque::new(),
            stamps: VecDeque::new(),
            stamp_answers: VecDeque::new(),
            state,
            resolves: Vec::new(),
            tracked: BTreeMap::new(),
            failures: BTreeMap::new(),
            mining: BTreeMap::new(),
            announced: false,
            pending_selection: None,
            unheard: BTreeMap::new(),
            hands_out_packed: false,
            undecryptable: 0,
            says_home: false,
            recipients: BTreeMap::new(),
            replayed: None,
            ticks: 0,
        }
    }

    /// An engine for a client of the home node whose LXMF address is
    /// `address`. It registers nothing, by construction: the state it starts
    /// in has no identity to register, and no path leads from it to
    /// `register`. The guard test is
    /// `a_client_engine_registers_no_delivery_destination`.
    pub fn client(address: [u8; 16]) -> Self {
        Self::starting_in(
            State::Client(Client {
                address,
                said_ready: false,
            }),
            Vec::new(),
        )
    }

    /// Hand every received message out packed as well
    /// ([`OutboxEvent::Packed`]), for a home node's archive, and prove it
    /// only on the [`Command::Prove`] that says it is archived.
    pub fn handing_out_packed(mut self) -> Self {
        self.hands_out_packed = true;
        self
    }

    /// Say every announce of the identity as `HOME_ANNOUNCE` (1305): the
    /// engine of a home node.
    pub fn saying_home(mut self) -> Self {
        self.says_home = true;
        self
    }

    /// Queue one frontend command for the next [`Engine::on_tick`].
    pub fn submit(&mut self, command: Command) {
        self.commands.push_back(command);
    }

    /// The oldest event the frontend has not been handed yet.
    pub fn next_event(&mut self) -> Option<OutboxEvent> {
        self.events.pop_front()
    }

    /// The oldest stamp job no miner has been handed yet.
    pub fn next_stamp_job(&mut self) -> Option<StampJob> {
        self.stamps.pop_front()
    }

    /// Hand back a finished stamp, for the next [`Engine::on_tick`].
    pub fn stamp_answered(&mut self, answer: StampAnswer) {
        self.stamp_answers.push_back(answer);
    }

    /// The miner is gone and `job` never reached it, so its answer will never
    /// come: drop the marker, so a later re-offer is dispatched normally.
    pub fn stamp_undelivered(&mut self, job: &StampJob) {
        self.mining.remove(&job.key());
    }

    fn emit(&mut self, event: OutboxEvent) {
        self.events.push_back(event);
    }

    /// Register the delivery destination on first use and announce readiness.
    ///
    /// Called from both hooks: the driver's timer branch normally fires first,
    /// but nothing in the seam promises that.
    fn register_if_needed<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
    ) {
        let identity = match core::mem::replace(&mut self.state, State::Failed) {
            State::Unregistered(identity) => *identity,
            other => {
                self.state = other;
                return;
            }
        };
        let registered = register(core, identity).and_then(|mut ready| {
            if self.hands_out_packed {
                ready
                    .router
                    .hold_proofs_until_kept(core)
                    .map_err(|e| format!("hold proofs: {e:?}"))?;
            }
            Ok(ready)
        });
        self.state = match registered {
            Ok(ready) => {
                self.emit(OutboxEvent::Ready {
                    address: ready.address,
                });
                State::Ready(Box::new(ready))
            }
            Err(detail) => {
                self.emit(OutboxEvent::Broken { detail });
                State::Failed
            }
        };
    }

    /// Register if needed, then move the router out of `self` for the duration
    /// of the hook, which is what lets the rest of the hook borrow the router
    /// and `self` at once.
    fn take_ready<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
    ) -> Option<Box<Ready>> {
        self.register_if_needed(core);
        match core::mem::replace(&mut self.state, State::Failed) {
            State::Ready(ready) => Some(ready),
            other => {
                self.state = other;
                None
            }
        }
    }

    /// Route one router output: report its events, collect its wire actions,
    /// and re-feed the core events it produced. See [`MAX_ABSORB_ROUNDS`].
    fn absorb<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        first: RouterOutput,
        out: &mut TickOutput,
    ) {
        let mut queue = VecDeque::from([first]);
        let mut rounds = 0usize;
        while let Some(router_output) = queue.pop_front() {
            for event in router_output.events {
                self.report(ready, core, event);
            }
            let mut core_output = router_output.core;
            let events = core::mem::take(&mut core_output.events);
            out.merge(core_output);

            rounds += 1;
            let refeed = rounds <= MAX_ABSORB_ROUNDS;
            for event in events {
                if refeed {
                    match ready.router.handle_event(core, &event) {
                        Ok(next) => queue.push_back(next),
                        Err(e) => self.emit(OutboxEvent::Broken {
                            detail: format!("router handle_event: {e:?}"),
                        }),
                    }
                }
                out.events.push(event);
            }
        }
    }

    /// Turn one router event into something the frontend can act on.
    fn report<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &Ready,
        core: &NodeCore<R, C, S>,
        event: RouterEvent,
    ) {
        match event {
            RouterEvent::MessageQueued(id) => {
                self.tracked.insert(id, None);
                self.emit(OutboxEvent::Queued { message_id: id });
            }
            RouterEvent::MessageState { message_id, state } => {
                if let Some(slot) = self.tracked.get_mut(&message_id) {
                    *slot = Some(state);
                    self.emit(OutboxEvent::State { message_id, state });
                }
            }
            RouterEvent::AttemptFailed { message_id, reason } => {
                if self.tracked.contains_key(&message_id) {
                    self.failures.insert(message_id, reason);
                }
            }
            RouterEvent::StampPending(request) => {
                self.dispatch_stamp(StampJob::Delivery(request, Arc::default()));
            }
            RouterEvent::PropagationStampPending(request) => {
                self.dispatch_stamp(StampJob::Propagation(request, Arc::default()));
            }
            RouterEvent::PropagationUploaded {
                message_id,
                transient_id,
            } => {
                if self.tracked.contains_key(&message_id) {
                    let node = ready
                        .router
                        .outbound_propagation_node()
                        .map(|node| *node.as_bytes());
                    self.emit(OutboxEvent::Uploaded {
                        message_id,
                        transient_id,
                        node,
                    });
                }
            }
            RouterEvent::MessageReceived(message) => {
                let message = *message;
                let verified = message.verification == Verification::Valid;
                // The router drops a signature that was checked and failed
                // (`leviculum-lxmf/src/router.rs`, `accept_inbound`), so one
                // that did not verify here had no key to be checked against.
                let held =
                    (!verified).then(|| (message.message_id, message.source_hash, message.pack()));
                let packed = self
                    .hands_out_packed
                    .then(|| (message.message_id, message.pack(), message.method.into()));
                let propagated = (message.method == DeliveryMethod::Propagated)
                    .then_some((message.message_id, message.source_hash));
                self.emit(OutboxEvent::Received {
                    message_id: message.message_id,
                    source: message.source_hash,
                    verified,
                    sent_at: message.timestamp as i64,
                    title: message.title,
                    body: message.content,
                    fields: message.fields,
                });
                if let Some((message_id, source)) = propagated {
                    self.emit(OutboxEvent::Propagated { message_id, source });
                }
                if let Some((message_id, source, packed)) = held {
                    self.hold(message_id, source, packed.clone());
                    self.emit(OutboxEvent::Unverified {
                        message_id,
                        source,
                        packed,
                    });
                }
                if let Some((message_id, packed, arrived)) = packed {
                    self.emit(OutboxEvent::Packed {
                        message_id,
                        packed,
                        arrived,
                    });
                }
            }
            RouterEvent::PeerAnnounced {
                destination,
                announce,
                path_response,
            } => {
                let destination = destination.into_bytes();
                self.emit(OutboxEvent::Announced {
                    destination,
                    name: announce.display_name(),
                    path_response: path_response || self.replayed == Some(destination),
                });
            }
            RouterEvent::PropagationSyncComplete(result) => {
                self.emit(OutboxEvent::SyncDone {
                    received: result.received,
                    duplicates: result.duplicates,
                });
            }
            RouterEvent::PropagationSyncState(status) => {
                if status.state == PropagationClientState::LinkEstablished {
                    self.emit(OutboxEvent::SyncLinked);
                }
                if let Some(word) = sync_failure(status.state) {
                    let path = (status.state == PropagationClientState::NoPath)
                        .then(|| ready.router.propagation_client_path_attempts(core))
                        .flatten();
                    self.emit(OutboxEvent::SyncFailed {
                        detail: word.to_string(),
                        path,
                    });
                }
            }
            // Everything else is either inbound traffic this slice does not
            // read or bookkeeping the router does for itself. Deliberately not
            // forwarded: an event the frontend cannot act on is noise on a
            // seam that a daemon protocol will one day have to carry.
            _ => {}
        }
    }

    /// Queue one proof-of-work job for the miner.
    ///
    /// Off the lock: mining is unbounded work at a cost the peer chooses, and
    /// the generator is async. Both are things a hook body may least afford.
    ///
    /// Single-flight: the router re-offers a still-queued message every retry
    /// interval, and a second mine for one job would queue behind the first
    /// for the same answer.
    fn dispatch_stamp(&mut self, job: StampJob) {
        let key = job.key();
        if self.mining.contains_key(&key) {
            return;
        }
        self.mining.insert(key, job.cancel());
        self.stamps.push_back(job);
    }

    /// Notice messages the router has forgotten. A terminal state removes the
    /// entry from the outbound map (`leviculum-lxmf/src/router.rs:872-874`),
    /// and the frontend has to be told, because the state event and the
    /// removal can arrive in the same tick.
    ///
    /// The predicate is membership of `outbound()`, deliberately **not**
    /// `LxmfRouter::has_message`: that one asks the delivered-id dedup cache
    /// whether this id has already been *received*
    /// (`leviculum-lxmf/src/router.rs:780-782`), which is false for every
    /// message we send. Reading it as "is still queued" reported every message
    /// as departed in the tick that queued it, with no state ever seen — see
    /// the regression test below.
    fn report_departures(&mut self, ready: &Ready) {
        let gone: Vec<_> = self
            .tracked
            .keys()
            .filter(|id| !ready.router.outbound().contains_key(*id))
            .copied()
            .collect();
        for id in gone {
            let last = self.tracked.remove(&id).flatten();
            let failure = self.failures.remove(&id);
            self.emit(OutboxEvent::Left {
                message_id: id,
                last,
                failure,
            });
        }
    }

    /// Drain the frontend's command queue. Non-blocking by construction.
    fn pump_commands<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        out: &mut TickOutput,
    ) {
        while let Some(command) = self.commands.pop_front() {
            match command {
                Command::Resolve { destination } => self.resolves.push(Resolve {
                    destination,
                    next_path_request_ms: None,
                    first_path_request_ms: None,
                    path_requests: 0,
                    answered: false,
                }),
                Command::Send(request) => self.send(ready, core, *request, out),
                Command::SelectPn { preferred, mailbox } => {
                    self.select_pn(ready, core, preferred, mailbox, out);
                }
                Command::Fetch { path_budget_ms } => {
                    self.fetch(ready, core, path_budget_ms, out);
                }
                Command::Cancel { message_id } => self.cancel(ready, core, &message_id, out),
                Command::Verify {
                    message_id,
                    source,
                    packed,
                } => self.hold(message_id, source, packed),
                Command::Ingest { packed } => self.ingest(core, ready.address, &packed),
                Command::Announce => self.announced = false,
                Command::SendPacked { packed, via } => {
                    self.send_packed(ready, core, &packed, via, out);
                }
                Command::Prove { message_id } => {
                    match ready.router.release_proofs(core, &message_id) {
                        Ok(output) => self.absorb(ready, core, output, out),
                        Err(e) => self.emit(OutboxEvent::Broken {
                            detail: format!("router release_proofs: {e:?}"),
                        }),
                    }
                }
                Command::Fence { token } => self.emit(OutboxEvent::Fenced { token }),
                Command::Census { seq } => {
                    let census = self.census(seq, ready.router.heap_bytes());
                    self.emit(OutboxEvent::Census(census));
                }
                Command::AnnouncePosition { position } => {
                    self.announce_position(ready, core, &position, out)
                }
                // A client's commands: a messenger learns its states from its
                // own router.
                Command::HomeAccepted { .. }
                | Command::HomeState { .. }
                | Command::HomeSent { .. }
                | Command::HomeMark { .. }
                | Command::HomeStalled { .. }
                | Command::HomePnStatus { .. }
                | Command::HomeName { .. }
                | Command::HomeHub { .. }
                | Command::HomePetname { .. }
                | Command::HomePeerState { .. } => {}
            }
        }
    }

    /// Queue a message packed and signed before, as it is: a home node's
    /// outbox hands the same bytes to every leg, so the direct copy and the
    /// propagated one have one hash, which the recipient keeps once
    /// (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`).
    fn send_packed<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        packed: &[u8],
        via: Via,
        out: &mut TickOutput,
    ) {
        let mut message =
            match leviculum_lxmf::Message::unpack(packed, None, None, DeliveryMethod::Direct) {
                Ok(message) => message,
                Err(error) => {
                    self.emit(OutboxEvent::Refused {
                        detail: format!("not an LXMF message: {error:?}"),
                    });
                    return;
                }
            };
        let message_id = message.message_id;
        message.method = match via {
            Via::Propagated => DeliveryMethod::Propagated,
            Via::Direct | Via::Link => DeliveryMethod::Direct,
        };
        self.recipients
            .entry(message.destination_hash)
            .or_insert(None);
        // A recipient whose key this node does not hold (a group's engine
        // started again after a kill, 1307 G2, keeps no known identities):
        // ask its path, as a stock app does before it writes to an address
        // it holds no key for. The answer is an announce and brings the key;
        // the router only asks for a path it lacks, and a client of a shared
        // instance has the path without the key.
        let destination = DestinationHash::new(message.destination_hash);
        let keyless = core
            .storage()
            .get_identity(destination.as_bytes())
            .is_none();
        if keyless {
            tracing::info!(
                "ENGINE no key for {}: asking its path",
                crate::address::to_hex(&message.destination_hash)
            );
            let output = core.request_path_unthrottled(&destination);
            self.absorb(
                ready,
                core,
                RouterOutput {
                    core: output,
                    events: Vec::new(),
                },
                out,
            );
        }
        if message.method == DeliveryMethod::Propagated
            && ready.router.outbound_propagation_node().is_none()
        {
            self.emit(OutboxEvent::Unsent {
                message_id,
                detail: "no propagation node is selected".to_string(),
            });
            return;
        }
        // A propagated leg cannot be encrypted for the node without the key.
        // The router holds such a message, asks the path every
        // `PATH_REQUEST_WAIT_MS` and charges no attempt
        // (`leviculum-lxmf/src/router/propagation_runtime.rs`, the
        // `Err(RouterError::Node(_))` arm after `ensure_prepared_upload`), so
        // the leg never ended: on the field mailbox every such leg waited out
        // the home's stall bound, 779 s, every hour for seven days (1406). It
        // is not queued now, and said so; the next leg finds the key once
        // the recipient's announce answered the path request above.
        if message.method == DeliveryMethod::Propagated && keyless {
            self.emit(OutboxEvent::KeyMissing {
                message_id,
                destination: message.destination_hash,
            });
            return;
        }
        match ready.router.enqueue(core, message) {
            Ok(output) => self.absorb(ready, core, output, out),
            Err(e) => self.emit(OutboxEvent::Unsent {
                message_id,
                detail: format!("{e:?}"),
            }),
        }
    }

    /// Take in one message pulled from a home node, as it came: check that
    /// it is addressed to `address`, check its signature against the
    /// sender's key when that is known, and report it as a message that
    /// arrived. One whose sender is not known yet is held, as a received
    /// one is; one whose signature does not hold is dropped, as the router
    /// drops one (`leviculum-lxmf/src/router.rs`, `accept_inbound`).
    fn ingest<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
        address: [u8; 16],
        packed: &[u8],
    ) {
        let message =
            match leviculum_lxmf::Message::unpack(packed, None, None, DeliveryMethod::Direct) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!("HOME ingest: not an LXMF message: {error:?}");
                    return;
                }
            };
        if message.destination_hash != address {
            tracing::warn!(
                "HOME ingest: message {} is for {}, not for us",
                crate::address::to_hex(&message.message_id),
                crate::address::to_hex(&message.destination_hash)
            );
            return;
        }
        let verification = core
            .storage()
            .get_identity(&message.source_hash)
            .map(|identity| reverify(packed, identity));
        if verification == Some(Verification::Invalid) {
            tracing::warn!(
                "HOME ingest: message {} does not carry its sender's signature",
                crate::address::to_hex(&message.message_id)
            );
            return;
        }
        let verified = verification == Some(Verification::Valid);
        let (message_id, source) = (message.message_id, message.source_hash);
        self.emit(OutboxEvent::Received {
            message_id,
            source,
            verified,
            sent_at: message.timestamp as i64,
            title: message.title,
            body: message.content,
            fields: message.fields,
        });
        if !verified {
            self.hold(message_id, source, packed.to_vec());
            self.emit(OutboxEvent::Unverified {
                message_id,
                source,
                packed: packed.to_vec(),
            });
        }
    }

    /// The client engine's tick: say `Ready` once, take the commands a
    /// client has, and settle held messages as their senders' keys come.
    fn client_tick<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
        now_ms: u64,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        let State::Client(client) = &mut self.state else {
            return out;
        };
        let address = client.address;
        let say_ready = !client.said_ready;
        client.said_ready = true;
        if say_ready {
            self.emit(OutboxEvent::Ready { address });
        }
        while let Some(command) = self.commands.pop_front() {
            match command {
                Command::Ingest { packed } => self.ingest(core, address, &packed),
                Command::Verify {
                    message_id,
                    source,
                    packed,
                } => self.hold(message_id, source, packed),
                Command::Send(_) => self.emit(OutboxEvent::Refused {
                    detail: CLIENT_SENDS.to_string(),
                }),
                Command::SendPacked { packed, .. } => {
                    let message_id = leviculum_lxmf::Message::unpack(
                        &packed,
                        None,
                        None,
                        DeliveryMethod::Direct,
                    )
                    .map_or([0; 32], |message| message.message_id);
                    self.emit(OutboxEvent::Unsent {
                        message_id,
                        detail: CLIENT_SENDS.to_string(),
                    });
                }
                Command::Fence { token } => self.emit(OutboxEvent::Fenced { token }),
                Command::Census { seq } => {
                    let census = self.census(seq, 0);
                    self.emit(OutboxEvent::Census(census));
                }
                // A client receives nothing over the mesh, so it holds no proof.
                Command::Prove { .. } => {}
                Command::HomeAccepted { message_id } => {
                    self.emit(OutboxEvent::Queued { message_id });
                }
                Command::HomeState { message_id, state } => {
                    self.emit(OutboxEvent::State { message_id, state });
                }
                Command::HomeSent { packed } => {
                    match leviculum_lxmf::Message::unpack(
                        &packed,
                        None,
                        None,
                        DeliveryMethod::Direct,
                    ) {
                        Ok(message) => self.emit(OutboxEvent::HomeSent {
                            message_id: message.message_id,
                            destination: message.destination_hash,
                            sent_at: message.timestamp as i64,
                            body: message.content,
                            fields: message.fields,
                        }),
                        Err(error) => {
                            tracing::warn!("HOME sent entry does not unpack: {error}")
                        }
                    }
                }
                Command::HomeMark { mark } => self.emit(OutboxEvent::HomeMarked { mark }),
                Command::HomePnStatus { answers } => {
                    self.emit(OutboxEvent::HomePnStatus { answers });
                }
                Command::HomeStalled { message_id } => {
                    self.emit(OutboxEvent::HomeStalled { message_id });
                }
                Command::HomeHub { hub } => self.emit(OutboxEvent::HubAnnounced { hub }),
                Command::HomeName { destination, name } => self.emit(OutboxEvent::Announced {
                    destination,
                    name: Some(name),
                    path_response: false,
                }),
                Command::HomePetname {
                    destination,
                    petname,
                    at,
                } => self.emit(OutboxEvent::HomePetname {
                    destination,
                    petname,
                    at,
                }),
                Command::HomePeerState { destination, state } => {
                    self.emit(OutboxEvent::HomePeerState { destination, state })
                }
                Command::Fetch { .. } => self.emit(OutboxEvent::SyncFailed {
                    detail: "a client of a home node pulls from the home node, not a \
                             propagation node"
                        .to_string(),
                    path: None,
                }),
                Command::SelectPn { .. } => self.emit(OutboxEvent::PnUnavailable {
                    detail: "a client of a Loquax Mailbox has no propagation node of its own"
                        .to_string(),
                }),
                Command::Resolve { .. }
                | Command::Cancel { .. }
                | Command::Announce
                | Command::AnnouncePosition { .. } => {}
            }
        }
        for source in self.unheard_due(core, now_ms) {
            // No router to feed: what the request produces besides packets
            // is nothing a client reads.
            let mut output = core.request_path(&DestinationHash::new(source));
            output.events.clear();
            out.merge(output);
        }
        let poll = now_ms.saturating_add(POLL_INTERVAL_MS);
        out.next_deadline_ms = Some(match out.next_deadline_ms {
            Some(existing) => existing.min(poll),
            None => poll,
        });
        out
    }

    /// Pick the propagation node for this run's uploads and drains: the
    /// preferred one when there is one, else the mailbox rule's answer.
    fn select_pn<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        preferred: Option<[u8; 16]>,
        mailbox: Option<MailboxInput>,
        out: &mut TickOutput,
    ) {
        if let Some(destination) = preferred {
            self.commit_pn(ready, core, destination, out);
            return;
        }
        let Some(input) = mailbox else {
            self.emit(OutboxEvent::PnUnavailable {
                detail: "no propagation node is known: none was given with --pn and none \
                         is configured"
                    .to_string(),
            });
            return;
        };
        let routes = match (input.routes, input.previous) {
            (Ok(routes), _) => routes,
            // A stored node stays without the table: it was vetted against
            // the table when it was adopted, or the user chose it, and an
            // identity does not move onto this device by itself. Only a new
            // adoption needs the table.
            (Err(_), Some(stored)) => {
                self.emit(OutboxEvent::Mailbox(Decision::KeptWithoutTable {
                    node: stored.node,
                }));
                self.commit_pn(ready, core, stored.node, out);
                return;
            }
            (Err(why), None) => {
                self.emit(OutboxEvent::PnUnavailable {
                    detail: format!(
                        "the daemon's path table could not be read \
                         ({why}), and without it a propagation node on this device cannot \
                         be told from one elsewhere; name one with --pn"
                    ),
                });
                return;
            }
        };
        let previous = input.previous.map(|stored| stored.previous(&routes));
        let excluded = input.excluded;
        let selection = mailbox::select(&candidates(ready, &routes), previous, &excluded);
        if matches!(selection, Selection::Keep(_)) {
            self.finish_selection(ready, core, selection, out);
            return;
        }
        // Ask for the replays of what the router cannot classify yet.
        let mut unknown: Vec<&Route> = routes
            .iter()
            .filter(|route| {
                ready
                    .router
                    .known_propagation_node(&DestinationHash::new(route.node))
                    .is_none()
            })
            .collect();
        unknown.sort_by_key(|route| (route.hops, route.node));
        unknown.truncate(DISCOVERY_LIMIT);
        let asked: Vec<[u8; 16]> = unknown.iter().map(|route| route.node).collect();
        if asked.is_empty() {
            self.finish_selection(ready, core, selection, out);
            return;
        }
        for node in &asked {
            let output = core.request_path_unthrottled(&DestinationHash::new(*node));
            self.absorb(
                ready,
                core,
                RouterOutput {
                    core: output,
                    events: Vec::new(),
                },
                out,
            );
        }
        self.pending_selection = Some(PendingSelection {
            routes,
            previous,
            excluded,
            asked,
            deadline_ms: core.now_ms().saturating_add(DISCOVERY_WINDOW_MS),
        });
    }

    /// Finish a waiting selection once every replay is in or the window has
    /// closed.
    fn poll_pending_selection<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        let Some(pending) = &self.pending_selection else {
            return;
        };
        if !pending.asked.is_empty() && now_ms < pending.deadline_ms {
            return;
        }
        let Some(pending) = self.pending_selection.take() else {
            return;
        };
        let selection = mailbox::select(
            &candidates(ready, &pending.routes),
            pending.previous,
            &pending.excluded,
        );
        self.finish_selection(ready, core, selection, out);
    }

    /// Report the rule's answer and select what it named.
    fn finish_selection<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        selection: Selection,
        out: &mut TickOutput,
    ) {
        match selection {
            Selection::Keep(node) => {
                self.emit(OutboxEvent::Mailbox(Decision::Kept { node }));
                self.commit_pn(ready, core, node, out);
            }
            Selection::Adopt(candidate, reason) => {
                self.emit(OutboxEvent::Mailbox(Decision::Adopted {
                    node: candidate.node,
                    hops: candidate.hops,
                    seen_ms: candidate.last_seen_ms,
                    reason,
                }));
                self.commit_pn(ready, core, candidate.node, out);
            }
            Selection::None(reason) => self.emit(OutboxEvent::PnUnavailable {
                detail: reason.as_str().to_string(),
            }),
        }
    }

    /// Make `destination` the router's propagation node.
    fn commit_pn<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        destination: [u8; 16],
        out: &mut TickOutput,
    ) {
        let hash = DestinationHash::new(destination);
        // The announced cost is read before `absorb` takes `ready` mutably;
        // `None` means the node has not been heard yet this run, in which
        // case the router requests its path and learns the cost from the
        // replayed announce.
        let stamp_cost = ready
            .router
            .known_propagation_node(&hash)
            .map(|known| known.announce.stamp_cost);
        match ready
            .router
            .select_outbound_propagation_node(core, Some(hash))
        {
            Ok(output) => {
                self.absorb(ready, core, output, out);
                self.emit(OutboxEvent::PnSelected {
                    destination,
                    stamp_cost,
                });
            }
            Err(e) => self.emit(OutboxEvent::PnUnavailable {
                detail: format!("{e:?}"),
            }),
        }
    }

    /// Start one mailbox drain against the selected node, asking for its path
    /// for as long as `path_budget_ms` (leviculum#719).
    fn fetch<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        path_budget_ms: u64,
        out: &mut TickOutput,
    ) {
        let started = ready
            .router
            .set_propagation_path_timeout_ms(path_budget_ms)
            .and_then(|()| {
                ready
                    .router
                    .request_messages_from_propagation_node(core, None)
            });
        match started {
            Ok(output) => self.absorb(ready, core, output, out),
            Err(e) => self.emit(OutboxEvent::SyncFailed {
                detail: format!("{e:?}"),
                path: None,
            }),
        }
    }

    /// Drop a queued message. `NotFound` is not an error here: the message
    /// may have reached a terminal state between the frontend's decision and
    /// this tick, which is exactly the outcome a cancel wants.
    fn cancel<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        message_id: &[u8; 32],
        out: &mut TickOutput,
    ) {
        // Call off any proof-of-work for this message first, and do it
        // whatever the router says: at a cost a peer announced the search can
        // be unfinishable in practice, so a mine left running would hold the
        // single mining thread — and every stamp queued behind it — for the
        // rest of the run (Codeberg #185). The handles are fired, not
        // removed: the entry goes when the worker's answer comes back, which
        // is what keeps a re-offer from queueing a second mine meanwhile.
        for kind in [StampKind::Delivery, StampKind::Propagation] {
            if let Some(cancel) = self.mining.get(&(kind, *message_id)) {
                cancel.cancel();
            }
        }
        match ready.router.cancel(core, message_id) {
            Ok(output) => self.absorb(ready, core, output, out),
            Err(RouterError::NotFound) => {}
            Err(e) => self.emit(OutboxEvent::Refused {
                detail: format!("cancel: {e:?}"),
            }),
        }
    }

    /// Drain finished proof-of-work.
    fn pump_stamps<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        out: &mut TickOutput,
    ) {
        while let Some(answer) = self.stamp_answers.pop_front() {
            match answer {
                StampAnswer::Delivery { request, result } => {
                    self.mining
                        .remove(&(StampKind::Delivery, request.message_id));
                    match result {
                        Ok(stamp) => match ready.router.set_outbound_stamp_result(
                            core,
                            &request,
                            stamp.to_vec(),
                        ) {
                            Ok(output) => self.absorb(ready, core, output, out),
                            // The advertised cost moved while we mined; the
                            // router re-offers the message and a fresh mine
                            // runs at the current cost.
                            Err(RouterError::StaleStampRequest) => {}
                            Err(e) => self.emit(OutboxEvent::Refused {
                                detail: format!("stamp rejected: {e:?}"),
                            }),
                        },
                        // The frontend asked for this one to stop, and the
                        // cancel already reported the message's state.
                        Err(StampError::Cancelled) => {}
                        Err(e) => self.emit(OutboxEvent::Refused {
                            detail: format!("proof-of-work failed: {e}"),
                        }),
                    }
                }
                StampAnswer::Propagation { request, result } => {
                    self.mining
                        .remove(&(StampKind::Propagation, request.message_id));
                    match result {
                        Ok(stamp) => {
                            let now_ms = core.now_ms();
                            match ready
                                .router
                                .set_outbound_propagation_stamp_result(&request, stamp, now_ms)
                            {
                                Ok(output) => self.absorb(ready, core, output, out),
                                Err(RouterError::StaleStampRequest) => {}
                                Err(e) => self.emit(OutboxEvent::Refused {
                                    detail: format!("propagation stamp rejected: {e:?}"),
                                }),
                            }
                        }
                        Err(StampError::Cancelled) => {}
                        Err(e) => self.emit(OutboxEvent::Refused {
                            detail: format!("proof-of-work failed: {e}"),
                        }),
                    }
                }
            }
        }
    }

    fn send<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        request: SendRequest,
        out: &mut TickOutput,
    ) {
        let method = match request.via {
            Via::Direct => delivery_method(content_len(&request.body, &request.fields)),
            Via::Link => DeliveryMethod::Direct,
            Via::Propagated => {
                // Guarded here rather than only in the CLI ordering, because
                // the seam is what a daemon-mode frontend would speak and a
                // send with nowhere to go must not sit silently in the queue.
                if ready.router.outbound_propagation_node().is_none() {
                    self.emit(OutboxEvent::Refused {
                        detail: "no propagation node is selected; SelectPn must succeed first"
                            .to_string(),
                    });
                    return;
                }
                DeliveryMethod::Propagated
            }
        };
        let message = match ready.router.create_message(
            core,
            request.destination,
            request.title,
            request.body,
            request.fields,
            method,
        ) {
            Ok(message) => message,
            Err(e) => {
                self.emit(OutboxEvent::Refused {
                    detail: format!("{e:?}"),
                });
                return;
            }
        };
        match ready.router.enqueue(core, message) {
            // `MessageQueued` comes out of `absorb`, which is what carries the
            // id to the frontend: one path for the id, whoever queued it.
            Ok(output) => self.absorb(ready, core, output, out),
            Err(e) => self.emit(OutboxEvent::Refused {
                detail: format!("{e:?}"),
            }),
        }
    }

    /// Announce our delivery destination, once per run.
    ///
    /// Not optional, and not politeness. A receiver validates an LXMF
    /// signature against the source identity it has recalled; without an
    /// announce from us the reference marks our message
    /// `SOURCE_UNKNOWN`/unverified (`reference/LXMF/LXMF/LXMessage.py:815-816`)
    /// even though it still delivers the body — and nobody can reply to an
    /// address they have no key for.
    ///
    /// Once per run is a deliberate airtime choice. A cron job that sends
    /// every five minutes spends one announce per send, which is the cost of
    /// staying repliable; rate-limiting it against a stored last-announced
    /// timestamp belongs with the sync policy of a later slice, not here,
    /// where it would be a guess with no reader to justify it.
    /// Announce `position` on our `lev.position` destination (1302),
    /// registered beside the delivery destination, of the same identity, on
    /// first use. A failure is said in the log and nothing else: a position
    /// announce is a courtesy to the mesh, never a message's fate.
    fn announce_position<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        position: &leviculum_lxmf::lev_position::Position,
        out: &mut TickOutput,
    ) {
        let Some(identity) = core
            .destination(&DestinationHash::new(ready.address))
            .and_then(|d| d.identity())
            .and_then(|identity| identity.private_key_bytes().ok())
            .and_then(|bytes| Identity::from_private_key_bytes(&bytes).ok())
        else {
            tracing::warn!("loquax: no identity to announce a position with");
            return;
        };
        let hash = leviculum_core::Destination::compute_destination_hash(
            &crate::mesh_position::name_hash(),
            identity.hash(),
        );
        if core.destination(&hash).is_none() {
            match leviculum_core::Destination::new(
                Some(identity),
                leviculum_core::Direction::In,
                leviculum_core::DestinationType::Single,
                crate::mesh_position::APP_NAME,
                &[crate::mesh_position::ASPECT],
            ) {
                Ok(destination) => core.register_destination(destination),
                Err(e) => {
                    tracing::warn!("loquax: lev.position destination: {e:?}");
                    return;
                }
            }
        }
        let app_data = position.encode();
        match core.announce_destination(&hash, Some(&app_data)) {
            Ok(output) => {
                if self.says_home {
                    crate::events::home_announce(&ready.address, "all", "position");
                }
                self.absorb(
                    ready,
                    core,
                    RouterOutput {
                        core: output,
                        events: Vec::new(),
                    },
                    out,
                )
            }
            Err(e) => tracing::warn!("loquax: could not announce our position: {e:?}"),
        }
    }

    fn announce<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        out: &mut TickOutput,
    ) {
        if self.announced {
            return;
        }
        self.announced = true;
        let app_data = announce::delivery(Some(&self.display_name), None);
        match core.announce_destination(&DestinationHash::new(ready.address), Some(&app_data)) {
            Ok(output) => {
                // A broadcast on every interface (`NodeCore::announce_destination`).
                if self.says_home {
                    crate::events::home_announce(&ready.address, "all", "delivery");
                }
                self.absorb(
                    ready,
                    core,
                    RouterOutput {
                        core: output,
                        events: Vec::new(),
                    },
                    out,
                )
            }
            // Not fatal, and deliberately not an `OutboxEvent::Refused`: the
            // message still goes out, it just arrives unverifiable at a peer
            // that has never heard of us. Failing the send over it would turn
            // a cosmetic problem into a lost status line.
            Err(e) => tracing::warn!("loquax: could not announce our own address: {e:?}"),
        }
    }

    /// A packet to our delivery destination that no key we hold opened: the
    /// sender encrypted it to a ratchet we never had or no longer keep, and
    /// it is lost. Never silent: one warning per packet, with how many
    /// ratchets we do hold and the count since the start. The core sends
    /// no proof for it, as the reference does not, so the sender is not
    /// told it arrived. Which ratchet the sender used cannot be read from
    /// the packet ([`NodeEvent::PacketUndecryptable`]).
    fn undecryptable<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &NodeCore<R, C, S>,
        destination: DestinationHash,
        len: usize,
    ) {
        self.undecryptable += 1;
        let ratchets = core
            .destination(&destination)
            .map_or(0, leviculum_core::Destination::ratchet_count);
        tracing::warn!(
            event = "LXMF_RX_UNDECRYPTABLE",
            dst = %crate::address::to_hex(destination.as_bytes()),
            len = len,
            ratchets = ratchets,
            count = self.undecryptable,
        );
    }

    /// Hold one received message until its sender's key is known.
    fn hold(&mut self, message_id: [u8; 32], source: [u8; 16], packed: Vec<u8>) {
        let unheard = self.unheard.entry(source).or_insert_with(|| Unheard {
            messages: Vec::new(),
            next_path_request_ms: None,
            path_requests: 0,
        });
        if !unheard.messages.iter().any(|(id, _)| *id == message_id) {
            unheard.messages.push((message_id, packed));
        }
    }

    /// Check every held message whose sender's key has arrived, and ask for
    /// the path of each sender whose key has not, at most once every
    /// [`UNHEARD_PATH_REQUEST_INTERVAL_MS`]: the answer to a path request is
    /// the sender's announce, which carries the key.
    fn poll_unheard<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        for source in self.unheard_due(core, now_ms) {
            // Through `absorb`, for the reason `poll_resolves` gives.
            let output = core.request_path(&DestinationHash::new(source));
            self.absorb(
                ready,
                core,
                RouterOutput {
                    core: output,
                    events: Vec::new(),
                },
                out,
            );
        }
    }

    /// The router-free half of [`Engine::poll_unheard`], which a client of
    /// a home node runs too: settle every held message whose sender's key
    /// has arrived, and name the senders whose path is due to be asked for.
    fn unheard_due<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
        now_ms: u64,
    ) -> Vec<[u8; 16]> {
        let mut due = Vec::new();
        if self.unheard.is_empty() {
            return due;
        }
        let sources: Vec<[u8; 16]> = self.unheard.keys().copied().collect();
        for source in sources {
            if let Some(identity) = core.storage().get_identity(&source) {
                let verdicts: Vec<([u8; 32], bool)> = self
                    .unheard
                    .remove(&source)
                    .map(|unheard| unheard.messages)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(id, packed)| (id, reverify(&packed, identity) == Verification::Valid))
                    .collect();
                for (message_id, valid) in verdicts {
                    crate::events::verify_result(&source, &message_id, valid);
                    self.emit(OutboxEvent::Verified {
                        message_id,
                        source,
                        valid,
                    });
                }
                continue;
            }
            let Some(unheard) = self.unheard.get_mut(&source) else {
                continue;
            };
            if unheard.next_path_request_ms.is_some_and(|due| now_ms < due) {
                continue;
            }
            unheard.next_path_request_ms =
                Some(now_ms.saturating_add(UNHEARD_PATH_REQUEST_INTERVAL_MS));
            unheard.path_requests = unheard.path_requests.saturating_add(1);
            crate::events::verify_path_request(
                &source,
                unheard.messages.len(),
                unheard.path_requests,
            );
            due.push(source);
        }
        due
    }

    /// Answer every resolve that can be answered now.
    ///
    /// A destination counts as resolved when its identity is known *and* a
    /// path exists: the first is what a message is encrypted to, the second is
    /// where it goes. Unlike `leviculum-lxmf-node`, the path is requested
    /// without waiting for the identity to turn up first — a path request is
    /// how an address nobody has announced near us becomes known at all, and a
    /// messenger's whole job is sending to an address a user was given.
    fn poll_resolves<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        ready: &mut Ready,
        core: &mut NodeCore<R, C, S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        if self.resolves.is_empty() {
            return;
        }
        for index in 0..self.resolves.len() {
            let destination = self.resolves[index].destination;
            let hash = DestinationHash::new(destination);
            if self.resolves[index].answered {
                continue;
            }
            if core.storage().get_identity(&destination).is_some() && core.has_path(&hash) {
                let resolve = &mut self.resolves[index];
                resolve.answered = true;
                crate::events::path_found(
                    &destination,
                    core.hops_to(&hash),
                    resolve.path_requests,
                    resolve
                        .first_path_request_ms
                        .map_or(0, |first| now_ms.saturating_sub(first)),
                );
                self.emit(OutboxEvent::Resolved { destination });
                continue;
            }
            if self.resolves[index]
                .next_path_request_ms
                .is_none_or(|due| now_ms >= due)
            {
                let resolve = &mut self.resolves[index];
                resolve.next_path_request_ms = Some(now_ms.saturating_add(PATH_REQUEST_WAIT_MS));
                resolve.first_path_request_ms.get_or_insert(now_ms);
                resolve.path_requests = resolve.path_requests.saturating_add(1);
                // Through `absorb` rather than merged straight into `out`:
                // `request_path` ends in `process_events_and_actions`, so its
                // output can carry events, and every other core call made from
                // a hook here has them re-fed to the router for the reason
                // `MAX_ABSORB_ROUNDS` gives. Unthrottled, because the core's
                // per-destination interval (20 s) would drop the repeat this
                // resolve paces itself.
                let output = core.request_path_unthrottled(&hash);
                self.absorb(
                    ready,
                    core,
                    RouterOutput {
                        core: output,
                        events: Vec::new(),
                    },
                    out,
                );
            }
        }
        self.resolves.retain(|resolve| !resolve.answered);
    }
}

/// The candidates the mailbox rule chooses among: every enabled propagation
/// node the router has heard, placed by the daemon's path table. A node the
/// table does not hold has no route, and is also one whose place nobody can
/// vouch for, so it is never adopted.
fn candidates(ready: &Ready, routes: &[Route]) -> Vec<Candidate> {
    ready
        .router
        .known_propagation_nodes()
        .filter(|known| known.announce.enabled)
        .map(|known| {
            let node = *known.destination.as_bytes();
            let route = routes.iter().find(|route| route.node == node);
            Candidate {
                node,
                hops: route.map_or(u8::MAX, |route| route.hops),
                last_seen_ms: route.map_or(0, |route| route.seen_ms),
                reachable: route.is_some(),
                is_own_device: route.is_some_and(|route| route.own_device),
            }
        })
        .collect()
}

/// The bytes a message's content costs on the wire: its body and its
/// fields with a generous allowance for each field's framing (key, array
/// and bin headers). A photo (1220) has an empty body and is all fields;
/// counting the body alone sent it opportunistically into the router's
/// oversized fallback.
fn content_len(body: &[u8], fields: &[leviculum_lxmf::Field]) -> usize {
    fields.iter().fold(body.len(), |sum, (_, value)| {
        sum.saturating_add(value.len() + 9)
    })
}

/// Which LXMF delivery method a body of this size should use.
///
/// Opportunistic delivery is one encrypted packet with no link setup, so for
/// the status line this program was built for it is the whole exchange.
/// Anything that cannot fit goes over a link instead. Airtime is the scarcest
/// resource on these networks (`docs/src/concepts/loquax.md` §3), and a link
/// handshake for twelve bytes spends it for nothing.
///
/// The threshold is the encrypted single-packet content limit less the LXMF
/// header the message adds on top of the body; below it the packet form is
/// certain to fit, and the router's own oversized fallback covers the margin
/// either way.
fn delivery_method(body_len: usize) -> DeliveryMethod {
    let budget = leviculum_lxmf::constants::ENCRYPTED_PACKET_MAX_CONTENT
        .saturating_sub(leviculum_lxmf::constants::LXMF_OVERHEAD);
    if body_len <= budget {
        DeliveryMethod::Opportunistic
    } else {
        DeliveryMethod::Direct
    }
}

/// The delivery address `identity` registers as — what `loquax address`
/// prints, and the same derivation the engine's registration performs, so
/// the two cannot disagree.
pub fn delivery_address(identity: &Identity) -> Result<[u8; 16], String> {
    let bytes = identity
        .private_key_bytes()
        .map_err(|e| format!("the identity has no private key: {e:?}"))?;
    let copy = Identity::from_private_key_bytes(&bytes)
        .map_err(|e| format!("could not copy the identity: {e:?}"))?;
    let destination =
        LxmfNode::delivery_destination(copy).map_err(|e| format!("delivery destination: {e:?}"))?;
    Ok(*destination.hash().as_bytes())
}

/// A held message's signature, checked against its sender's key.
///
/// The bytes are the whole message as it came (destination first, as
/// [`leviculum_lxmf::Message::pack`] gives a received one back), so they are
/// read as a direct delivery. Bytes that no longer parse are not a message
/// this key signed, and say so: never a quiet `unverified`.
fn reverify(packed: &[u8], identity: &Identity) -> Verification {
    leviculum_lxmf::Message::unpack(packed, None, Some(identity), DeliveryMethod::Direct)
        .map_or(Verification::Invalid, |message| message.verification)
}

/// The mailbox-drain failure states, in the words the frontend reports.
fn sync_failure(state: PropagationClientState) -> Option<&'static str> {
    match state {
        PropagationClientState::NoPath => Some("no path to the propagation node"),
        PropagationClientState::LinkFailed => Some("the propagation node did not answer"),
        PropagationClientState::TransferFailed => Some("the transfer failed"),
        PropagationClientState::NoIdentity => Some("the node does not know our identity"),
        PropagationClientState::NoAccess => Some("the node refused us access"),
        PropagationClientState::Failed => Some("the sync failed"),
        _ => None,
    }
}

/// Mint the delivery destination and the router that drives it.
fn register<R: CryptoRngCore, C: CoreClock, S: Storage>(
    core: &mut NodeCore<R, C, S>,
    identity: Identity,
) -> Result<Ready, String> {
    let identity_hash = *identity.hash();
    // `delivery_destination` consumes the identity, so the private key is
    // copied out first: the destination is what holds it afterwards, and the
    // router is addressed by the *identity* hash while the wire is addressed
    // by the destination hash.
    let bytes = identity
        .private_key_bytes()
        .map_err(|e| format!("the identity has no private key: {e:?}"))?;
    let copy = Identity::from_private_key_bytes(&bytes)
        .map_err(|e| format!("could not copy the identity: {e:?}"))?;
    let mut destination =
        LxmfNode::delivery_destination(copy).map_err(|e| format!("delivery destination: {e:?}"))?;
    // Ratchets, as every Python LXMF router has them on its delivery
    // destination (`reference/LXMF/LXMF/LXMRouter.py:357`). A peer that ever
    // heard this identity announced with one, from any program, encrypts
    // every single packet to it for 30 days (`RNS/Identity.py:69`); only an
    // announce that carries a ratchet replaces it (`:618-619`). Without
    // ours, such a packet decrypted with nothing (leviculum 1185). The
    // registration loads the ratchets the storage kept, an import's among
    // them, and every announce rotates on the reference cadence.
    core.enable_ratchets(&mut destination)
        .map_err(|e| format!("delivery destination ratchets: {e:?}"))?;
    let address = *destination.hash().as_bytes();
    let node = LxmfNode::register(core, destination, LxmfNodeConfig::default())
        .map_err(|e| format!("register delivery destination: {e:?}"))?;
    let mut router = LxmfRouter::new(node, identity_hash, RouterConfig::default());
    // The propagation client is always on, exactly as `leviculum-lxmf-node`
    // has it (`leviculum-lxmf-node/src/processor.rs`, "The propagation
    // *client* is always on"): `--via propagated`, the auto fallback and
    // `loquax fetch` all need it, and it registers a second local destination
    // that accepts no links, so a plain direct send cannot observe it.
    let client_copy = Identity::from_private_key_bytes(&bytes)
        .map_err(|e| format!("could not copy the propagation identity: {e:?}"))?;
    let transport_destination = PropagationTransport::destination(client_copy)
        .map_err(|e| format!("propagation destination: {e:?}"))?;
    let transport = PropagationTransport::register(core, transport_destination)
        .map_err(|e| format!("register propagation client: {e:?}"))?;
    router
        .enable_propagation_client(transport, PropagationClientConfig::default())
        .map_err(|e| format!("enable propagation client: {e:?}"))?;
    Ok(Ready { router, address })
}

/// Whether an announce's name hash is `lxmf.propagation`'s, which tells a
/// propagation node's announce from a peer's.
fn is_propagation_node(name_hash: &[u8]) -> bool {
    name_hash
        == leviculum_core::Destination::compute_name_hash(
            leviculum_lxmf::node::APP_NAME,
            &[leviculum_lxmf::propagation_client::PROPAGATION_ASPECT],
        )
}

impl Engine {
    /// The identity whose private key this engine holds, the home
    /// identity a board seals its position to; `None` on a client of a
    /// home node, which never holds it.
    fn own_identity<'a, R: CryptoRngCore, C: CoreClock, S: Storage>(
        &'a self,
        core: &'a NodeCore<R, C, S>,
    ) -> Option<&'a Identity> {
        match &self.state {
            State::Unregistered(identity) => Some(identity),
            State::Ready(ready) => core
                .destination(&DestinationHash::new(ready.address))
                .and_then(|destination| destination.identity()),
            State::Failed | State::Client(_) => None,
        }
    }
}

/// The two hooks, in the shape of the driver's `CoreProcessor` and with the
/// same contract: both run with the core lock held, and neither blocks.
impl Engine {
    /// Consume one event the core produced.
    pub fn on_event<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
        event: &NodeEvent,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        // A position the mesh announces needs no router and no
        // destination of ours (1302): a client of a home node shows it as
        // the home node does.
        // Plain ones always; a sealed one only when it is sealed to our
        // identity (1313): our own boards, never a stranger's.
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            if crate::mesh_position::is_position(announce.name_hash()) {
                let position =
                    crate::mesh_position::read(announce.app_data(), self.own_identity(core));
                match position {
                    Some(position) => {
                        let heard_at = i64::try_from(core.emission_secs()).unwrap_or(i64::MAX);
                        self.emit(OutboxEvent::MeshPosition {
                            identity: announce.computed_identity_hash(),
                            location: position.location(heard_at),
                            name: position.name,
                        });
                    }
                    None => tracing::debug!(
                        "loquax: a lev.position announce we cannot read (sealed to another)"
                    ),
                }
            }
        }
        // A hub announcing (1599): every device lists the hubs its own node
        // hears, and a home keeps them for its devices.
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            if crate::ui::hubs::is_hub(announce.name_hash()) {
                let at = i64::try_from(core.emission_secs()).unwrap_or(i64::MAX);
                self.emit(OutboxEvent::HubAnnounced {
                    hub: crate::ui::hubs::HeardHub {
                        hash: *announce.destination_hash().as_bytes(),
                        name: crate::ui::hubs::announced_name(announce.app_data()),
                        hops: core.hops_to(announce.destination_hash()),
                        first: at,
                        last: at,
                        answered: None,
                    },
                });
            }
        }
        // A NomadNet node announcing its pages (1579): a home keeps it for
        // its devices, so a device switched on lists every page the home
        // heard (`crate::home::pages`).
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            if lnomad::discovery::is_nomad_node_announce(announce) {
                let at = core.emission_secs();
                self.emit(OutboxEvent::PageAnnounced {
                    node: lnomad::discovery::DiscoveredNode {
                        dest_hash: *announce.destination_hash().as_bytes(),
                        name: lnomad::discovery::decode_node_name(announce.app_data()),
                        first_seen: at,
                        last_seen: at,
                        hops: core.hops_to(announce.destination_hash()).map(u32::from),
                    },
                });
            }
        }
        if matches!(self.state, State::Client(_)) {
            return out;
        }
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        self.replayed = match event {
            NodeEvent::AnnounceReceived { announce, .. } => self.replayed(
                announce.destination_hash().into_bytes(),
                announce.random_hash(),
            ),
            _ => None,
        };
        if let (Some(pending), NodeEvent::AnnounceReceived { announce, .. }) =
            (&mut self.pending_selection, event)
        {
            let heard = *announce.destination_hash().as_bytes();
            pending.asked.retain(|node| *node != heard);
        }
        if let NodeEvent::PacketUndecryptable {
            destination, len, ..
        } = event
        {
            if *destination.as_bytes() == ready.address {
                self.undecryptable(core, *destination, *len);
            }
        }
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            if is_propagation_node(announce.name_hash()) {
                let decoded = leviculum_lxmf::propagation::PropagationNodeAnnounce::decode(
                    announce.app_data(),
                )
                .ok();
                self.emit(OutboxEvent::NodeAnnounced {
                    destination: *announce.destination_hash().as_bytes(),
                    status: decoded
                        .as_ref()
                        .and_then(leviculum_lxmf::lev_status::announced_version),
                    name: decoded.as_ref().and_then(|d| d.name()),
                });
            }
        }
        match ready.router.handle_event(core, event) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.emit(OutboxEvent::Broken {
                detail: format!("router handle_event: {e:?}"),
            }),
        }
        // An announce arriving is exactly what a pending resolve is waiting
        // for. Answering here rather than on the next timer tick puts the
        // latency at the announce instead of `POLL_INTERVAL_MS` after it.
        // The driver's tick clock is the core's, so the event reads it there.
        let now_ms = core.now_ms();
        self.poll_resolves(&mut ready, core, now_ms, &mut out);
        self.poll_pending_selection(&mut ready, core, now_ms, &mut out);
        self.poll_unheard(&mut ready, core, now_ms, &mut out);
        self.report_departures(&ready);
        self.replayed = None;
        self.state = State::Ready(ready);
        out
    }

    /// Whether an announce from `destination` with `random` repeats the
    /// last one heard from it, for a recipient this run sent to: `Some` of
    /// it if so, and the announce becomes the last one heard either way.
    ///
    /// A repeat is a cache's copy, which says nothing of whether the
    /// recipient is there (1332). The router asks a recipient's path again
    /// whenever a direct link to it closes with a message still waiting
    /// (`leviculum-lxmf/src/router.rs`, `needs_direct_link`), and a shared
    /// instance forwards the network's answer to its client as a plain
    /// announce, as the reference does (`Transport.py:1910-1930`, cited at
    /// the pending forward in `leviculum-core/src/transport.rs`), so the
    /// `PATH_RESPONSE` context 1330 reads is gone by then. On miauhaus every
    /// copy handed on was tried direct again some 46 s after each attempt
    /// nobody answered. Only a recipient that announces again sends a new
    /// random hash.
    fn replayed(
        &mut self,
        destination: [u8; 16],
        random: &[u8; RANDOM_HASHBYTES],
    ) -> Option<[u8; 16]> {
        let last = self.recipients.get_mut(&destination)?;
        let repeat = last.as_ref() == Some(random);
        *last = Some(*random);
        repeat.then_some(destination)
    }

    /// The engine's own tables beside `router_bytes`, its router's
    /// (order 1585), as the answer to the census asked under `seq`.
    fn census(&self, seq: u64, router_bytes: usize) -> crate::outbox::EngineCensus {
        use leviculum_core::heap_census::{btree_map_bytes, vec_deque_bytes};
        let engine_bytes = vec_deque_bytes(&self.events)
            + vec_deque_bytes(&self.commands)
            + vec_deque_bytes(&self.stamps)
            + vec_deque_bytes(&self.stamp_answers)
            + self.resolves.capacity() * core::mem::size_of::<Resolve>()
            + btree_map_bytes(&self.tracked)
            + btree_map_bytes(&self.failures)
            + btree_map_bytes(&self.mining)
            + btree_map_bytes(&self.unheard)
            + self
                .unheard
                .values()
                .map(Unheard::heap_bytes)
                .sum::<usize>()
            + btree_map_bytes(&self.recipients);
        crate::outbox::EngineCensus {
            seq,
            router_bytes,
            engine_bytes,
            tracked: self.tracked.len(),
            recipients: self.recipients.len(),
            unheard: self.unheard.len(),
            events_queued: self.events.len(),
            ticks: self.ticks,
        }
    }

    /// The periodic slot: commands, finished stamps, resolves and the
    /// router's own timers. Always asks to be called again within
    /// `POLL_INTERVAL_MS`, because nothing else pokes the command queue.
    pub fn on_tick<R: CryptoRngCore, C: CoreClock, S: Storage>(
        &mut self,
        core: &mut NodeCore<R, C, S>,
        now_ms: u64,
    ) -> TickOutput {
        self.ticks = self.ticks.saturating_add(1);
        if matches!(self.state, State::Client(_)) {
            return self.client_tick(core, now_ms);
        }
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };

        self.announce(&mut ready, core, &mut out);
        self.pump_commands(&mut ready, core, &mut out);
        self.pump_stamps(&mut ready, core, &mut out);
        self.poll_resolves(&mut ready, core, now_ms, &mut out);
        self.poll_pending_selection(&mut ready, core, now_ms, &mut out);
        self.poll_unheard(&mut ready, core, now_ms, &mut out);
        match ready.router.tick(core) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.emit(OutboxEvent::Broken {
                detail: format!("router tick: {e:?}"),
            }),
        }
        self.report_departures(&ready);
        self.state = State::Ready(ready);

        // Always a fresh future instant: the engine always has something to
        // wake for, at minimum the command queue, which nothing else pokes.
        let poll = now_ms.saturating_add(POLL_INTERVAL_MS);
        out.next_deadline_ms = Some(match out.next_deadline_ms {
            Some(existing) => existing.min(poll),
            None => poll,
        });
        out
    }
}

/// Mine one stamp job and say what came of it, the policy every host's miner
/// shares: which generator, which RNG it draws from, and what the person
/// waiting is told.
///
/// The executor is the cooperative one, so the future yields while it grinds
/// and the progress timer below gets its turns on a single thread. `rng` is
/// the host's (the terminal hands `OsRng`), `clock` measures the grind and
/// paces the progress lines, and the lines are notices on `effects`.
pub async fn mine<G, K, E>(job: StampJob, rng: G, clock: &K, effects: &mut E) -> StampAnswer
where
    G: CryptoRngCore + Send,
    K: Clock,
    E: Effects,
{
    match job {
        StampJob::Delivery(request, cancel) => {
            let cost = request.target_cost;
            let result = mine_with_progress(clock, effects, cost, "delivery stamp", async {
                let mut executor = CooperativeStamper::cooperative(rng);
                request.generate_with(&mut executor, &cancel).await
            })
            .await;
            StampAnswer::Delivery { request, result }
        }
        StampJob::Propagation(request, cancel) => {
            let cost = request.target_cost;
            let result = mine_with_progress(clock, effects, cost, "propagation stamp", async {
                let mut executor = CooperativeStamper::cooperative(rng);
                request.generate_with(&mut executor, &cancel).await
            })
            .await;
            StampAnswer::Propagation { request, result }
        }
    }
}

/// Run one mining future to completion, narrating it when the cost is above
/// [`STAMP_PROGRESS_COST`].
///
/// The narration is elapsed time, not a percentage: proof-of-work has no
/// knowable fraction-done, and inventing one would be the progress-bar lie.
/// The terminal writes the notices to stderr, so a scripted send's "success
/// prints nothing" contract on stdout is untouched.
async fn mine_with_progress<K, E, F>(
    clock: &K,
    effects: &mut E,
    cost: u8,
    what: &str,
    future: F,
) -> Result<[u8; 32], StampError>
where
    K: Clock,
    E: Effects,
    F: Future<Output = Result<[u8; 32], StampError>>,
{
    if cost <= STAMP_PROGRESS_COST {
        return future.await;
    }
    let started_ms = clock.now_ms();
    effects.notice(&format!("mining {what} at cost {cost}..."));
    let mut mining = pin!(future);
    let mut waited_ms = 0u64;
    let result = loop {
        // Whichever is ready first: the stamp, or the next progress line.
        let mut tick = pin!(clock.sleep_ms(STAMP_PROGRESS_EVERY_MS));
        let step = core::future::poll_fn(|cx| {
            if let Poll::Ready(result) = mining.as_mut().poll(cx) {
                return Poll::Ready(Some(result));
            }
            tick.as_mut().poll(cx).map(|()| None)
        })
        .await;
        match step {
            Some(result) => break result,
            None => {
                waited_ms += STAMP_PROGRESS_EVERY_MS;
                effects.notice(&format!("still mining {what} ({}s)...", waited_ms / 1_000));
            }
        }
    };
    // A cancelled grind is a withdrawal, not an achievement: saying it was
    // "mined" would be the one line an operator must not read here.
    let outcome = match result {
        Ok(_) => "mined",
        Err(_) => "abandoned",
    };
    let elapsed_ms = clock.now_ms().saturating_sub(started_ms);
    effects.notice(&format!(
        "{what} {outcome} after {:.1}s",
        elapsed_ms as f32 / 1_000.0
    ));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_std::driver::{StdClock, StdNodeCore, StdStorage};

    fn engine() -> Engine {
        engine_named(b"loquax-test")
    }

    fn engine_named(display_name: &[u8]) -> Engine {
        Engine::new(leviculum_std::generate_identity(), display_name.to_vec())
    }

    /// Every event the engine has queued for the frontend, oldest first.
    fn drain(engine: &mut Engine) -> Vec<OutboxEvent> {
        core::iter::from_fn(|| engine.next_event()).collect()
    }

    fn core(dir: &std::path::Path) -> StdNodeCore {
        NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir).expect("storage under a fresh temp dir"),
        )
    }

    /// The ratchet public key in the delivery announce among `out`'s
    /// packets, `None` when it carries none; panics without an announce.
    fn announced_ratchet(out: &TickOutput, address: [u8; 16]) -> Option<[u8; 32]> {
        out.actions
            .iter()
            .find_map(|action| match action {
                leviculum_core::Action::SendPacket { data, .. }
                | leviculum_core::Action::Broadcast { data, .. } => {
                    leviculum_core::packet::Packet::unpack(data)
                        .ok()
                        .filter(|p| {
                            p.flags.packet_type == leviculum_core::packet::PacketType::Announce
                                && p.destination_hash == address
                        })
                }
            })
            .map(|p| {
                // An announce with a ratchet says so in its context flag and
                // carries it after the name hash and the random blob
                // (`RNS/Destination.py`, announce): 64 + 10 + 10 bytes in.
                p.flags.context_flag.then(|| {
                    let data = p.data.as_slice();
                    let mut ratchet = [0u8; 32];
                    ratchet.copy_from_slice(&data[84..116]);
                    ratchet
                })
            })
            .expect("a delivery announce went out")
    }

    /// 1185: the delivery destination announces with a ratchet on the
    /// reference cadence, rotates on announce once the interval passed, and
    /// a restarted engine on the same storage holds what was announced.
    /// Before 1185 the announce carried none, and a packet a peer encrypted
    /// to the ratchet an earlier program of the same identity announced
    /// decrypted with nothing.
    #[test]
    fn the_delivery_destination_announces_a_ratchet_and_keeps_it_across_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = leviculum_std::generate_identity();
        let address = delivery_address(&identity).expect("address");
        let copy = || {
            Identity::from_private_key_bytes(&identity.private_key_bytes().expect("key"))
                .expect("copy")
        };
        let hash = DestinationHash::new(address);

        let mut core1 = core(dir.path());
        let mut engine = Engine::new(copy(), b"r".to_vec());
        let now_ms = core1.now_ms();
        let out = engine.on_tick(&mut core1, now_ms);
        let first = announced_ratchet(&out, address).expect("the announce carries a ratchet");
        let destination = core1.destination(&hash).expect("registered");
        assert_eq!(destination.current_ratchet_public(), Some(first));
        // The reference cadence: `RNS/Destination.py:85,90`.
        assert_eq!(destination.ratchet_interval(), 30 * 60 * 1000);
        assert_eq!(destination.retained_ratchet_count(), 512);

        // Rotation is the announce's, once the interval passed; shortened
        // here to the core's minimum so the test need not wait half an hour.
        core1
            .destination_mut(&hash)
            .expect("registered")
            .set_ratchet_interval(1000);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        engine.submit(Command::Announce);
        // The command is read after this tick's announce slot: the
        // announce goes out on the tick after it.
        let now_ms = core1.now_ms();
        let _ = engine.on_tick(&mut core1, now_ms);
        let out = engine.on_tick(&mut core1, now_ms);
        let second = announced_ratchet(&out, address).expect("a ratchet again");
        assert_ne!(second, first, "the announce after the interval rotates");
        assert_eq!(
            core1
                .destination(&hash)
                .expect("registered")
                .ratchet_count(),
            2
        );
        drop(core1);

        // A restart on the same storage: the same ratchets, the newest
        // announced first.
        let mut core2 = core(dir.path());
        let mut engine = Engine::new(copy(), b"r".to_vec());
        let now_ms = core2.now_ms();
        let out = engine.on_tick(&mut core2, now_ms);
        assert_eq!(announced_ratchet(&out, address), Some(second));
        assert_eq!(
            core2
                .destination(&hash)
                .expect("registered")
                .ratchet_count(),
            2
        );
    }

    /// A signed announce of `identity`'s `app.aspects` destination carrying
    /// `app_data`, as the bytes an interface hands the core.
    fn announce_bytes(
        identity: &Identity,
        app: &str,
        aspects: &[&str],
        app_data: &[u8],
    ) -> Vec<u8> {
        let copy = Identity::from_private_key_bytes(&identity.private_key_bytes().expect("key"))
            .expect("copy");
        let mut destination = leviculum_core::Destination::new(
            Some(copy),
            leviculum_core::Direction::In,
            leviculum_core::DestinationType::Single,
            app,
            aspects,
        )
        .expect("destination");
        let packet = destination
            .announce(Some(app_data), &mut rand_core::OsRng, 1_000, 1)
            .expect("announce");
        let mut bytes = [0u8; 512];
        let len = packet.pack(&mut bytes).expect("pack");
        bytes[..len].to_vec()
    }

    /// 1332: an announce from a recipient this run sent to that repeats the
    /// last one heard from it is a cache's copy, reported as heard from a
    /// cache however it came; the recipient's next own announce is live. A
    /// peer nothing was sent to is reported as the wire says.
    #[test]
    fn a_repeat_of_a_recipients_announce_is_reported_as_heard_from_a_cache() {
        use leviculum_core::transport::InterfaceId;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let _ = drain(&mut engine);

        let member = leviculum_std::generate_identity();
        let address = delivery_address(&member).expect("address");
        let sender = leviculum_std::generate_identity();
        let packed = leviculum_lxmf::Message::create(
            address,
            delivery_address(&sender).expect("address"),
            &sender,
            1_793_500_000.0,
            Vec::new(),
            b"Brot ist da".to_vec(),
            Vec::new(),
            DeliveryMethod::Direct,
        )
        .expect("message")
        .pack();
        engine.submit(Command::SendPacked {
            packed,
            via: Via::Link,
        });
        let _ = engine.on_tick(&mut core, now_ms);
        let _ = drain(&mut engine);

        let hear = |engine: &mut Engine, core: &mut StdNodeCore, bytes: &[u8]| {
            let out = core.handle_packet(InterfaceId(0), bytes);
            for event in &out.events {
                let _ = engine.on_event(core, event);
            }
            drain(engine)
                .into_iter()
                .filter_map(|event| match event {
                    OutboxEvent::Announced {
                        destination,
                        path_response,
                        ..
                    } if destination == address => Some(path_response),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let once = announce_bytes(&member, "lxmf", &["delivery"], b"");
        assert_eq!(
            hear(&mut engine, &mut core, &once),
            [false],
            "first heard: live"
        );
        // A direct link that fails its handshake expires the path, and the
        // router asks it again: a transport node's copy comes back, one hop
        // further, the same announce.
        assert!(core.remove_path(&address));
        let mut replay = once.clone();
        replay[1] = replay[1].saturating_add(1);
        assert_eq!(
            hear(&mut engine, &mut core, &replay),
            [true],
            "the same announce again"
        );
        let again = announce_bytes(&member, "lxmf", &["delivery"], b"");
        assert_eq!(
            hear(&mut engine, &mut core, &again),
            [false],
            "the member announced"
        );
    }

    /// 1579: a NomadNet node's `nomadnetwork.node` announce reaches the seam
    /// as the row lnomad's registry makes of the same announce (one type,
    /// one decoder), named, with its hops; the control: another aspect of
    /// the same name is no page.
    #[test]
    fn a_nomadnet_announce_reaches_the_seam_as_lnomads_row() {
        use leviculum_core::transport::InterfaceId;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let _ = drain(&mut engine);
        let mut hear = |engine: &mut Engine, bytes: &[u8]| {
            let out = core.handle_packet(InterfaceId(0), bytes);
            for event in &out.events {
                let _ = engine.on_event(&mut core, event);
            }
            drain(engine)
        };
        let pages = |events: &[OutboxEvent]| -> Vec<lnomad::discovery::DiscoveredNode> {
            events
                .iter()
                .filter_map(|e| match e {
                    OutboxEvent::PageAnnounced { node } => Some(node.clone()),
                    _ => None,
                })
                .collect()
        };
        let node = leviculum_std::generate_identity();
        let events = hear(
            &mut engine,
            &announce_bytes(&node, "nomadnetwork", &["node"], b"Marcel's node"),
        );
        let heard = pages(&events);
        assert_eq!(heard.len(), 1, "{events:?}");
        let mut registry = lnomad::NomadNodeRegistry::new();
        assert!(registry.observe_parts(
            &lnomad::nomad_node_name_hash(),
            heard[0].dest_hash,
            b"Marcel's node",
            heard[0].hops,
            heard[0].last_seen,
        ));
        assert_eq!(registry.nodes(), [&heard[0]]);
        assert_eq!(heard[0].name.as_deref(), Some("Marcel's node"));
        assert_eq!(heard[0].hops, Some(1), "heard directly: one hop");
        let other = leviculum_std::generate_identity();
        let events = hear(
            &mut engine,
            &announce_bytes(&other, "nomadnetwork", &["other"], b"x"),
        );
        assert!(pages(&events).is_empty(), "another aspect: {events:?}");
    }

    /// 1599: a hub's `rrc.hub` announce reaches the seam as a heard hub,
    /// named by its app data without the protocol's word, with its hops;
    /// the control: the same node's announce of another aspect is no hub.
    #[test]
    fn a_hub_announce_reaches_the_seam_as_a_heard_hub() {
        use leviculum_core::transport::InterfaceId;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let _ = drain(&mut engine);
        let mut hear = |engine: &mut Engine, bytes: &[u8]| {
            let out = core.handle_packet(InterfaceId(0), bytes);
            for event in &out.events {
                let _ = engine.on_event(&mut core, event);
            }
            drain(engine)
        };
        let hubs = |events: &[OutboxEvent]| -> Vec<crate::ui::hubs::HeardHub> {
            events
                .iter()
                .filter_map(|e| match e {
                    OutboxEvent::HubAnnounced { hub } => Some(hub.clone()),
                    _ => None,
                })
                .collect()
        };
        let salon = leviculum_std::generate_identity();
        let app_data = leviculum_rrc::Value::Map(vec![(
            leviculum_rrc::Value::Text("hub".into()),
            leviculum_rrc::Value::Text("Le_Salon_FR RRC".into()),
        )])
        .encode();
        let events = hear(
            &mut engine,
            &announce_bytes(&salon, "rrc", &["hub"], &app_data),
        );
        let heard = hubs(&events);
        assert_eq!(heard.len(), 1, "{events:?}");
        assert_eq!(heard[0].name.as_deref(), Some("Le Salon FR"));
        assert_eq!(heard[0].hops, Some(1), "heard directly: one hop");
        assert_eq!(heard[0].first, heard[0].last);
        let other = leviculum_std::generate_identity();
        let events = hear(
            &mut engine,
            &announce_bytes(&other, "rrc", &["room"], &app_data),
        );
        assert!(
            hubs(&events).is_empty(),
            "another aspect is no hub: {events:?}"
        );
    }

    /// 1302: a `lev.position` announce the core heard reaches the screen's
    /// seam as the announcing identity's position, the identity whose
    /// delivery address the core keys it by; the same node's delivery
    /// announce is no position, nor is a position announce in a layout
    /// the codec does not read. A client of a home node hears it too.
    #[test]
    fn a_position_announce_reaches_the_seam_as_the_identitys_position() {
        use leviculum_core::transport::InterfaceId;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let _ = drain(&mut engine);

        let anna = leviculum_std::generate_identity();
        let location = leviculum_lxmf::telemetry::Location {
            latitude_e6: 43_773_100,
            longitude_e6: 11_256_000,
            altitude_e2: 5_000,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 0,
            last_update: 1_793_500_000,
        };
        let mut hear = |engine: &mut Engine, bytes: &[u8]| {
            let out = core.handle_packet(InterfaceId(0), bytes);
            for event in &out.events {
                let _ = engine.on_event(&mut core, event);
            }
            drain(engine)
        };
        let fixture = crate::mesh_position::manual(&location, Some("Anna".into())).encode();
        let events = hear(
            &mut engine,
            &announce_bytes(&anna, "lev", &["position"], &fixture),
        );
        assert_eq!(
            mesh_fix(&events, anna.hash()),
            Some((
                location.latitude_e6,
                location.longitude_e6,
                Some("Anna".into())
            )),
            "{events:?}"
        );
        assert_eq!(
            crate::mesh_position::delivery_of(anna.hash()),
            delivery_address(&anna).expect("address"),
            "the position is keyed by the peer's delivery address"
        );

        // The delivery announce of the same node is a name, not a place.
        let events = hear(
            &mut engine,
            &announce_bytes(&anna, "lxmf", &["delivery"], &fixture),
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboxEvent::MeshPosition { .. })),
            "{events:?}"
        );
        // 1302's stand-in layout is no position.
        let bob = leviculum_std::generate_identity();
        let mut stand_in = [0u8; 18];
        stand_in[0] = 1;
        let events = hear(
            &mut engine,
            &announce_bytes(&bob, "lev", &["position"], &stand_in),
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboxEvent::MeshPosition { .. })),
            "{events:?}"
        );

        // `loquax announce`'s command: an engine announces its own position
        // on `lev.position` beside its delivery destination, and the engine
        // of another core hears it as that identity's.
        let sender_dir = tempfile::tempdir().expect("tempdir");
        let mut sender_core = self::core(sender_dir.path());
        let dave = leviculum_std::generate_identity();
        let dave_hash = *dave.hash();
        let mut sender = Engine::new(dave, b"dave".to_vec());
        let now_ms = sender_core.now_ms();
        let _ = sender.on_tick(&mut sender_core, now_ms);
        sender.submit(Command::AnnouncePosition {
            position: crate::mesh_position::manual(&location, Some("Dave".into())),
        });
        let mut out = sender.on_tick(&mut sender_core, now_ms);
        out.merge(sender.on_tick(&mut sender_core, now_ms));
        let sent: Vec<Vec<u8>> = out
            .actions
            .iter()
            .map(|action| match action {
                leviculum_core::Action::SendPacket { data, .. }
                | leviculum_core::Action::Broadcast { data, .. } => data.clone(),
            })
            .collect();
        assert!(!sent.is_empty(), "the position announce went out");
        // A fresh listener: the one above heard four unknown announces on
        // one interface, and the core's ingress burst limit holds a fifth
        // (`leviculum-core/src/transport.rs`, `handle_announce_inner`).
        let listener_dir = tempfile::tempdir().expect("tempdir");
        let mut listener_core = self::core(listener_dir.path());
        let mut listener = engine_named(b"listener");
        let now_ms = listener_core.now_ms();
        let _ = listener.on_tick(&mut listener_core, now_ms);
        let _ = drain(&mut listener);
        for bytes in &sent {
            let out = listener_core.handle_packet(InterfaceId(0), bytes);
            for event in &out.events {
                let _ = listener.on_event(&mut listener_core, event);
            }
        }
        let heard = drain(&mut listener);
        assert_eq!(
            mesh_fix(&heard, &dave_hash),
            Some((
                location.latitude_e6,
                location.longitude_e6,
                Some("Dave".into())
            )),
            "{heard:?}"
        );

        // A client of a home node shows it as the home node would.
        let dir = tempfile::tempdir().expect("tempdir");
        let (_, _, mut client, mut client_core) = client_setup(dir.path());
        let carol = leviculum_std::generate_identity();
        let out = client_core.handle_packet(
            InterfaceId(0),
            &announce_bytes(&carol, "lev", &["position"], &fixture),
        );
        for event in &out.events {
            let _ = client.on_event(&mut client_core, event);
        }
        let heard = drain(&mut client);
        assert_eq!(
            mesh_fix(&heard, carol.hash()),
            Some((
                location.latitude_e6,
                location.longitude_e6,
                Some("Anna".into())
            )),
            "{heard:?}"
        );
    }

    /// The coordinates and name of the first [`OutboxEvent::MeshPosition`]
    /// for `identity` among `events`, if one is there.
    fn mesh_fix(events: &[OutboxEvent], identity: &[u8; 16]) -> Option<(i32, i32, Option<String>)> {
        events.iter().find_map(|event| match event {
            OutboxEvent::MeshPosition {
                identity: heard,
                location,
                name,
            } if heard == identity => {
                Some((location.latitude_e6, location.longitude_e6, name.clone()))
            }
            _ => None,
        })
    }

    /// 1313: a board's position sealed to another identity is no pin on our
    /// map, though the announce is a valid `lev.position` one; the positive
    /// control seals the same position to our home identity, and that one
    /// is a pin. A client of the home holds no key, so it shows neither.
    #[test]
    fn a_position_sealed_to_another_identity_is_no_pin_and_ours_is() {
        use crate::ui::tests::Field;
        use crate::ui::{update, UiEvent, UiModel};
        use leviculum_core::transport::InterfaceId;
        use leviculum_lxmf::lev_position::AppData;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let home = leviculum_std::generate_identity();
        let home_copy =
            Identity::from_private_key_bytes(&home.private_key_bytes().expect("a private key"))
                .expect("identity");
        let mut engine = Engine::new(home, b"home".to_vec());
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let _ = drain(&mut engine);
        let mut model = UiModel::<Field>::new(&[]);
        let hear = |engine: &mut Engine,
                    core: &mut StdNodeCore,
                    model: &mut UiModel<Field>,
                    bytes: &[u8]| {
            let out = core.handle_packet(InterfaceId(0), bytes);
            for event in &out.events {
                let _ = engine.on_event(core, event);
            }
            let events = drain(engine);
            for event in &events {
                if matches!(event, OutboxEvent::MeshPosition { .. }) {
                    let _ = update(model, UiEvent::Engine(event.clone()));
                }
            }
            events
        };
        let position = crate::mesh_position::manual(
            &leviculum_lxmf::telemetry::Location {
                latitude_e6: 43_773_100,
                longitude_e6: 11_256_000,
                altitude_e2: 0,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 0,
                last_update: 1_793_500_000,
            },
            None,
        );

        // A stranger's board, sealed to its own owner.
        let board = leviculum_std::generate_identity();
        let owner = leviculum_std::generate_identity();
        let theirs = position.seal(&owner, &mut rand_core::OsRng).expect("seal");
        assert!(matches!(AppData::parse(&theirs), Some(AppData::Sealed(_))));
        let events = hear(
            &mut engine,
            &mut core,
            &mut model,
            &announce_bytes(&board, "lev", &["position"], &theirs),
        );
        assert_eq!(mesh_fix(&events, board.hash()), None, "{events:?}");
        let board_peer = crate::mesh_position::delivery_of(board.hash());
        assert!(
            model.map_pins().iter().all(|pin| pin.peer != board_peer),
            "a stranger's sealed position is no pin"
        );

        // Positive control: our own board, sealed to our home identity.
        let ours_board = leviculum_std::generate_identity();
        let ours = position
            .seal(&home_copy, &mut rand_core::OsRng)
            .expect("seal");
        let ours_bytes = announce_bytes(&ours_board, "lev", &["position"], &ours);
        let events = hear(&mut engine, &mut core, &mut model, &ours_bytes);
        assert_eq!(
            mesh_fix(&events, ours_board.hash()),
            Some((43_773_100, 11_256_000, None)),
            "{events:?}"
        );
        let ours_peer = crate::mesh_position::delivery_of(ours_board.hash());
        assert!(
            model.map_pins().iter().any(|pin| pin.peer == ours_peer),
            "our own board is a pin: {:?}",
            model.map_pins()
        );

        // A client of the home holds no key: our board's sealed announce
        // is no position there.
        let client_dir = tempfile::tempdir().expect("tempdir");
        let (_, _, mut client, mut client_core) = client_setup(client_dir.path());
        let out = client_core.handle_packet(InterfaceId(0), &ours_bytes);
        for event in &out.events {
            let _ = client.on_event(&mut client_core, event);
        }
        let heard = drain(&mut client);
        assert_eq!(mesh_fix(&heard, ours_board.hash()), None, "{heard:?}");
    }

    /// 1185: a packet to our delivery destination that did not decrypt is
    /// counted and said (`LXMF_RX_UNDECRYPTABLE`); one to another of the
    /// core's destinations is not ours to report.
    #[test]
    fn an_undecryptable_packet_to_our_address_is_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let identity = leviculum_std::generate_identity();
        let address = delivery_address(&identity).expect("address");
        let mut engine = Engine::new(identity, b"u".to_vec());
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let undecryptable = |destination: [u8; 16]| NodeEvent::PacketUndecryptable {
            destination: DestinationHash::new(destination),
            len: 211,
            interface_index: 0,
        };
        let _ = engine.on_event(&mut core, &undecryptable([0x77; 16]));
        assert_eq!(engine.undecryptable, 0);
        let _ = engine.on_event(&mut core, &undecryptable(address));
        let _ = engine.on_event(&mut core, &undecryptable(address));
        assert_eq!(engine.undecryptable, 2);
    }

    /// A home node's identity, the engine's client of it, and a core.
    fn client_setup(dir: &std::path::Path) -> (Identity, [u8; 16], Engine, StdNodeCore) {
        let home = leviculum_std::generate_identity();
        let address = delivery_address(&home).expect("address");
        (home, address, Engine::client(address), core(dir))
    }

    /// Every command a client is sent, and an event, through the engine.
    fn exercise_client(engine: &mut Engine, core: &mut StdNodeCore) -> Vec<TickOutput> {
        let mut outs = Vec::new();
        let now_ms = core.now_ms();
        outs.push(engine.on_tick(core, now_ms));
        for command in [
            Command::Announce,
            Command::Resolve {
                destination: [9; 16],
            },
            Command::SelectPn {
                preferred: Some([8; 16]),
                mailbox: None,
            },
            Command::Fetch {
                path_budget_ms: 1_000,
            },
            Command::Send(Box::new(SendRequest {
                destination: [9; 16],
                title: Vec::new(),
                body: b"x".to_vec(),
                fields: Vec::new(),
                via: Via::Link,
            })),
        ] {
            engine.submit(command);
            let now_ms = core.now_ms();
            outs.push(engine.on_tick(core, now_ms));
        }
        outs.push(engine.on_event(core, &NodeEvent::InterfaceDown(0)));
        outs
    }

    /// The guard of the home node's one rule: a client never registers a
    /// delivery destination for the home identity, nor any other, and never
    /// announces. The positive control runs the same probe on a messenger
    /// engine holding the home identity, which does register it, so a probe
    /// that could not see a registration would fail there.
    #[test]
    fn a_client_engine_registers_no_delivery_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (home, address, mut engine, mut client_core) = client_setup(dir.path());
        let outs = exercise_client(&mut engine, &mut client_core);
        assert!(
            client_core
                .destination(&DestinationHash::new(address))
                .is_none(),
            "a client registered the home identity's delivery destination"
        );
        assert!(
            outs.iter().all(|out| out.actions.is_empty()),
            "a client put something on the wire: {:?}",
            outs.iter().map(|out| out.actions.len()).collect::<Vec<_>>()
        );
        let events = drain(&mut engine);
        assert_eq!(events.first(), Some(&OutboxEvent::Ready { address }));
        assert!(events.contains(&OutboxEvent::Refused {
            detail: CLIENT_SENDS.to_string()
        }));

        // Positive control: the messenger engine with the same identity.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut messenger_core = core(dir.path());
        let mut messenger = Engine::new(home, b"home".to_vec());
        let now_ms = messenger_core.now_ms();
        let out = messenger.on_tick(&mut messenger_core, now_ms);
        assert!(messenger_core
            .destination(&DestinationHash::new(address))
            .is_some());
        assert!(!out.actions.is_empty(), "the messenger announces");
    }

    fn signed(to: [u8; 16], sender: &Identity, body: &[u8]) -> leviculum_lxmf::Message {
        leviculum_lxmf::Message::create(
            to,
            delivery_address(sender).expect("sender address"),
            sender,
            1_800_000_000.0,
            Vec::new(),
            body.to_vec(),
            Vec::new(),
            DeliveryMethod::Direct,
        )
        .expect("message")
    }

    #[test]
    fn an_ingested_message_from_a_known_sender_arrives_verified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_home, address, mut engine, mut core) = client_setup(dir.path());
        let sender = leviculum_std::generate_identity();
        let source = delivery_address(&sender).expect("source");
        core.remember_identity(DestinationHash::new(source), sender.clone());
        let message = signed(address, &sender, b"hello");
        engine.submit(Command::Ingest {
            packed: message.pack(),
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        assert!(events.contains(&OutboxEvent::Received {
            sent_at: 1_800_000_000,
            message_id: message.message_id,
            source,
            title: Vec::new(),
            body: b"hello".to_vec(),
            fields: Vec::new(),
            verified: true,
        }));
        assert!(!events
            .iter()
            .any(|event| matches!(event, OutboxEvent::Unverified { .. })));
    }

    #[test]
    fn an_ingested_message_from_an_unknown_sender_is_held_unverified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_home, address, mut engine, mut core) = client_setup(dir.path());
        let sender = leviculum_std::generate_identity();
        let message = signed(address, &sender, b"who");
        engine.submit(Command::Ingest {
            packed: message.pack(),
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        assert!(events.iter().any(|event| matches!(
            event,
            OutboxEvent::Received { verified: false, message_id, .. }
                if *message_id == message.message_id
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            OutboxEvent::Unverified { message_id, .. } if *message_id == message.message_id
        )));
    }

    #[test]
    fn an_ingested_message_for_someone_else_or_forged_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_home, address, mut engine, mut core) = client_setup(dir.path());
        let sender = leviculum_std::generate_identity();
        let source = delivery_address(&sender).expect("source");
        core.remember_identity(DestinationHash::new(source), sender.clone());
        let elsewhere = signed([0x77; 16], &sender, b"not ours");
        let mut forged = signed(address, &sender, b"genuine").pack();
        let last = forged.len() - 1;
        forged[last] ^= 0x01;
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine);
        engine.submit(Command::Ingest {
            packed: elsewhere.pack(),
        });
        engine.submit(Command::Ingest { packed: forged });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, OutboxEvent::Received { .. })),
            "{events:?}"
        );
    }

    /// The first tick registers the delivery destination against a real core
    /// and hands the frontend our address. A stub would not get this far.
    #[test]
    fn the_first_tick_registers_and_reports_our_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        let now_ms = core.now_ms();
        let out = engine.on_tick(&mut core, now_ms);

        match engine.next_event() {
            Some(OutboxEvent::Ready { address }) => {
                assert_ne!(address, [0u8; 16], "the address must be a real hash")
            }
            other => panic!("the first tick must report readiness, got {other:?}"),
        }
        assert!(
            out.next_deadline_ms.is_some(),
            "on_tick always asks the driver back for the command queue"
        );
    }

    /// Cancelling a message calls off the proof-of-work already running for
    /// it (Codeberg #185).
    ///
    /// The mining thread is single-consumer, so a grind at a cost the peer
    /// announced does not only hold its own message: every stamp queued
    /// behind it waits too. At a legal-but-large cost that search never ends
    /// on its own (`leviculum-lxmf/tests/stamp_cancellation.rs`), so the
    /// handle fired here is the only thing that gets the thread back.
    ///
    /// The job is read back off the queue the miner is fed from, so this
    /// asserts the handle the *miner* would mine under, not a copy kept here.
    #[test]
    fn cancelling_a_message_calls_off_the_stamp_being_mined_for_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        let now_ms = core.now_ms();
        // The tick's actions go nowhere: this test never puts a packet on an
        // interface, it only needs the engine past registration.
        let _ = engine.on_tick(&mut core, now_ms);

        let message_id = [0x5au8; 32];
        engine.dispatch_stamp(StampJob::Delivery(
            DeliveryStampRequest {
                message_id,
                // Inside the window the reference announces, and unfinishable.
                target_cost: 254,
            },
            Arc::default(),
        ));
        let job = engine
            .next_stamp_job()
            .expect("the miner is handed the job");
        assert!(
            !job.cancel().is_cancelled(),
            "a freshly dispatched job is not cancelled"
        );

        engine.submit(Command::Cancel { message_id });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        assert!(
            job.cancel().is_cancelled(),
            "the cancel must reach the grind the worker is running"
        );
        assert!(
            engine
                .mining
                .contains_key(&(StampKind::Delivery, message_id)),
            "the marker stays until the worker answers, so a re-offer does \
             not queue a second mine at the same cost"
        );
    }

    /// The configured display name has to reach the wire, not just the struct.
    ///
    /// This is the assertion that would have caught the bug it was written for:
    /// `main.rs` passed the literal `loquax` into [`AttachConfig`], so a test of
    /// the config field alone would have been green while every recipient saw a
    /// message from a tool rather than from a person. What matters is the
    /// `app_data` of the announce that leaves here, so the announce is taken
    /// out of the tick's actions and unpacked.
    #[test]
    fn the_display_name_reaches_the_announce_that_goes_out() {
        use leviculum_core::packet::{Packet, PacketType};

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine_named(b"an-operator");

        let now_ms = core.now_ms();
        let out = engine.on_tick(&mut core, now_ms);

        let announces: Vec<Packet> = out
            .actions
            .iter()
            .filter_map(|action| match action {
                leviculum_core::transport::Action::Broadcast { data, .. } => {
                    Packet::unpack(data).ok()
                }
                _ => None,
            })
            .filter(|packet| packet.flags.packet_type == PacketType::Announce)
            .collect();
        assert_eq!(
            announces.len(),
            1,
            "the first tick announces our delivery destination exactly once"
        );

        // `app_data` is the tail of an announce payload, so a payload ending in
        // the encoding of this name is that name having been announced.
        let payload = announces[0].data.as_slice();
        let expected = announce::delivery(Some(b"an-operator"), None);
        assert!(
            payload.ends_with(&expected),
            "the announce must carry the configured display name: {payload:02x?}"
        );
        assert!(
            !payload.ends_with(&announce::delivery(
                Some(crate::display_name::FALLBACK.as_bytes()),
                None
            )),
            "the tool's own name must not be what recipients see"
        );
    }

    /// A propagated send with no node selected is refused at the seam, not
    /// only in the CLI ordering: a daemon-mode frontend speaks this seam and
    /// must get the same answer instead of a message that sits forever.
    #[test]
    fn a_propagated_send_without_a_selected_node_is_refused_at_the_seam() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        engine.submit(Command::Send(Box::new(SendRequest {
            destination: [0x11; 16],
            title: Vec::new(),
            body: b"body".to_vec(),
            fields: Vec::new(),
            via: Via::Propagated,
        })));
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let refusals: Vec<_> = drain(&mut engine)
            .into_iter()
            .filter_map(|event| match event {
                OutboxEvent::Refused { detail } => Some(detail),
                _ => None,
            })
            .collect();
        assert_eq!(refusals.len(), 1, "exactly one refusal: {refusals:?}");
        assert!(
            refusals[0].contains("no propagation node is selected"),
            "the refusal must name the missing selection: {}",
            refusals[0]
        );
    }

    /// `SelectPn` with nothing to select from must say so, and must not
    /// invent a node.
    #[test]
    fn selecting_a_node_with_none_known_reports_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        engine.submit(Command::SelectPn {
            preferred: None,
            mailbox: None,
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let seen: Vec<_> = drain(&mut engine);
        assert!(
            seen.iter()
                .any(|event| matches!(event, OutboxEvent::PnUnavailable { .. })),
            "no known node and no preference must be reported: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, OutboxEvent::PnSelected { .. })),
            "nothing may be selected out of thin air: {seen:?}"
        );
    }

    /// A `--pn`/configured node is selectable before any announce from it has
    /// been heard — the router requests its path and learns the announced
    /// cost from the replay, exactly as Python accepts a configured hash
    /// before a route is known.
    #[test]
    fn selecting_a_preferred_node_works_before_its_announce_arrives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        let node = [0x7e; 16];
        engine.submit(Command::SelectPn {
            preferred: Some(node),
            mailbox: None,
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let selected = drain(&mut engine)
            .into_iter()
            .find_map(|event| match event {
                OutboxEvent::PnSelected {
                    destination,
                    stamp_cost,
                } => Some((destination, stamp_cost)),
                _ => None,
            });
        assert_eq!(
            selected,
            Some((node, None)),
            "the preferred node is selected as given, with no cost known yet"
        );
    }

    fn select_without_preference(input: MailboxInput) -> Vec<OutboxEvent> {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        engine.submit(Command::SelectPn {
            preferred: None,
            mailbox: Some(input),
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine)
    }

    /// A stored selection the path table still holds is kept without
    /// asking the daemon for anything, and selected.
    #[test]
    fn a_stored_reachable_selection_is_kept_at_once() {
        let node = [0x3c; 16];
        let seen = select_without_preference(MailboxInput {
            routes: Ok(vec![Route {
                node,
                hops: 2,
                own_device: false,
                seen_ms: 1,
            }]),
            previous: Some(crate::mailbox_state::Stored {
                node,
                chosen_by_user: false,
            }),
            excluded: Vec::new(),
        });
        assert!(
            seen.contains(&OutboxEvent::Mailbox(Decision::Kept { node })),
            "{seen:?}"
        );
        assert!(
            seen.iter().any(|event| matches!(
                event,
                OutboxEvent::PnSelected { destination, .. } if *destination == node
            )),
            "{seen:?}"
        );
    }

    /// Without the path table nothing new is adopted: whether a node is on
    /// this device is exactly what cannot be told.
    #[test]
    fn an_unreadable_path_table_selects_nothing() {
        let seen = select_without_preference(MailboxInput {
            routes: Err("no authkey".to_string()),
            previous: None,
            excluded: Vec::new(),
        });
        assert!(
            seen.iter().any(|event| matches!(
                event,
                OutboxEvent::PnUnavailable { detail } if detail.contains("path table could not be read")
            )),
            "{seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, OutboxEvent::PnSelected { .. })),
            "{seen:?}"
        );
    }

    /// Without the path table a stored node is kept and selected, adopted or
    /// chosen alike: it was vetted when it was written, and an identity does
    /// not move onto this device by itself.
    #[test]
    fn an_unreadable_path_table_keeps_a_stored_node() {
        for chosen_by_user in [false, true] {
            let node = [0x3c; 16];
            let seen = select_without_preference(MailboxInput {
                routes: Err("no authkey".to_string()),
                previous: Some(crate::mailbox_state::Stored {
                    node,
                    chosen_by_user,
                }),
                excluded: Vec::new(),
            });
            assert!(
                seen.contains(&OutboxEvent::Mailbox(Decision::KeptWithoutTable { node })),
                "{seen:?}"
            );
            assert!(
                seen.iter().any(|event| matches!(
                    event,
                    OutboxEvent::PnSelected { destination, .. } if *destination == node
                )),
                "{seen:?}"
            );
            assert!(
                !seen
                    .iter()
                    .any(|event| matches!(event, OutboxEvent::PnUnavailable { .. })),
                "{seen:?}"
            );
        }
    }

    /// A first run waits for the replays it asked for, and with none coming
    /// back it says nothing was heard once the window closes.
    #[test]
    fn a_first_run_waits_for_the_replays_then_reports_none_heard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        engine.submit(Command::SelectPn {
            preferred: None,
            mailbox: Some(MailboxInput {
                routes: Ok(vec![Route {
                    node: [0x44; 16],
                    hops: 1,
                    own_device: false,
                    seen_ms: 1,
                }]),
                previous: None,
                excluded: Vec::new(),
            }),
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let early: Vec<_> = drain(&mut engine);
        assert!(
            !early
                .iter()
                .any(|event| matches!(event, OutboxEvent::PnUnavailable { .. })),
            "the selection must wait for the replays: {early:?}"
        );
        // Read after the first tick: the deadline was taken from the core's
        // clock inside it, so `now_ms` from before it can fall short of the
        // window by however long that tick took.
        let later_ms = core.now_ms() + DISCOVERY_WINDOW_MS;
        let _ = engine.on_tick(&mut core, later_ms);
        let late: Vec<_> = drain(&mut engine);
        assert!(
            late.iter().any(|event| matches!(
                event,
                OutboxEvent::PnUnavailable { detail }
                    if detail == mailbox::NoneReason::NoCandidates.as_str()
            )),
            "{late:?}"
        );
    }

    /// A resolve for an address nobody has announced must stay unanswered.
    /// Answering it would send a message into a hole.
    #[test]
    fn an_unknown_destination_is_not_reported_as_resolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        engine.submit(Command::Resolve {
            destination: [0x22; 16],
        });
        for _ in 0..3 {
            let now_ms = core.now_ms();
            let _ = engine.on_tick(&mut core, now_ms);
        }

        let resolved = drain(&mut engine)
            .into_iter()
            .any(|event| matches!(event, OutboxEvent::Resolved { .. }));
        assert!(
            !resolved,
            "an address with no identity and no path is not resolved"
        );
    }

    /// A resolve repeats its path request while the path is missing
    /// (leviculum#699). One request is one packet, and a packet sent while the
    /// daemon's interface is still reconnecting is gone; a resolve that never
    /// asks again waits out its whole budget for an answer nobody was asked
    /// for. The ticks carry their own clock, so the wait is stepped, not slept.
    #[test]
    fn an_unanswered_resolve_asks_for_the_path_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let destination = [0x22; 16];
        // A path request carries the destination it asks for in its payload.
        let requests = |out: &TickOutput| {
            out.actions
                .iter()
                .filter(|action| match action {
                    leviculum_core::Action::SendPacket { data, .. }
                    | leviculum_core::Action::Broadcast { data, .. } => {
                        data.windows(16).any(|window| window == destination)
                    }
                })
                .count()
        };

        engine.submit(Command::Resolve { destination });
        let start = core.now_ms();
        let first = engine.on_tick(&mut core, start);
        assert_eq!(requests(&first), 1, "the resolve asks once at once");

        let early = engine.on_tick(&mut core, start + 1_000);
        assert_eq!(requests(&early), 0, "not again before the wait is over");

        let repeat = engine.on_tick(&mut core, start + PATH_REQUEST_WAIT_MS);
        assert_eq!(requests(&repeat), 1, "the unanswered resolve asks again");
    }

    /// A queued message must not be reported as having left the queue in the
    /// same breath.
    ///
    /// Regression test. `report_departures` first asked
    /// `LxmfRouter::has_message`, whose name reads like "is this message in the
    /// router" and whose body is a lookup in the *delivered-id* cache — always
    /// false for an outbound message. Every send therefore reported a departure
    /// with no state attached, which the frontend can only read as a failure.
    /// Sending to an address with no route reaches exactly the same code path,
    /// so no network is needed to hold the invariant.
    #[test]
    fn a_freshly_queued_message_is_not_reported_as_departed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();

        // Register first: `create_message` needs our own delivery destination.
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        engine.submit(Command::Send(Box::new(SendRequest {
            destination: [0x33; 16],
            title: b"t".to_vec(),
            body: b"a short status line".to_vec(),
            fields: Vec::new(),
            via: Via::Direct,
        })));
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let seen: Vec<_> = drain(&mut engine);
        assert!(
            seen.iter()
                .any(|event| matches!(event, OutboxEvent::Queued { .. })),
            "the message must be queued: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, OutboxEvent::Left { .. })),
            "a message still in the outbound queue has not left it: {seen:?}"
        );
    }

    /// 1330: the home's "direct failed" line said `reason=failed`, the
    /// router's state, which does not tell a link that died mid-transfer
    /// from one that never came. A failed attempt's reason is kept until the
    /// message leaves, and goes with it; control: a message whose attempts
    /// never failed leaves with none.
    #[test]
    fn a_message_leaves_with_why_its_last_attempt_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let queue = |engine: &mut Engine, core: &mut _, destination| {
            engine.submit(Command::Send(Box::new(SendRequest {
                destination,
                title: Vec::new(),
                body: vec![b'x'; 2_000],
                fields: Vec::new(),
                via: Via::Direct,
            })));
            let now_ms = leviculum_core::node::NodeCore::now_ms(core);
            let _ = engine.on_tick(core, now_ms);
            drain(engine)
                .into_iter()
                .find_map(|event| match event {
                    OutboxEvent::Queued { message_id } => Some(message_id),
                    _ => None,
                })
                .expect("queued")
        };
        let died = queue(&mut engine, &mut core, [0x33; 16]);
        let quiet = queue(&mut engine, &mut core, [0x34; 16]);
        let reason = AttemptFailure::Delivery(leviculum_lxmf::DeliveryFailure::LinkClosed(
            leviculum_core::LinkCloseReason::Stale,
        ));
        let ready = engine.take_ready(&mut core).expect("ready");
        engine.report(
            &ready,
            &core,
            RouterEvent::AttemptFailed {
                message_id: died,
                reason,
            },
        );
        engine.state = State::Ready(ready);

        engine.submit(Command::Cancel { message_id: died });
        engine.submit(Command::Cancel { message_id: quiet });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let left: Vec<_> = drain(&mut engine)
            .into_iter()
            .filter_map(|event| match event {
                OutboxEvent::Left {
                    message_id,
                    failure,
                    ..
                } => Some((message_id, failure)),
                _ => None,
            })
            .collect();
        assert!(left.contains(&(died, Some(reason))), "{left:?}");
        assert!(left.contains(&(quiet, None)), "{left:?}");
    }

    /// A short status line must not pay for a link handshake; a body that
    /// cannot fit in one packet must not try.
    #[test]
    fn short_bodies_go_by_packet_and_long_ones_by_link() {
        assert_eq!(delivery_method(8), DeliveryMethod::Opportunistic);
        assert_eq!(delivery_method(64_000), DeliveryMethod::Direct);
        // A photo: no body, its bytes in a field (1220).
        let photo = crate::ui::photos::photo_fields("jpg", vec![0; 100_000]);
        assert_eq!(
            delivery_method(content_len(b"", &photo)),
            DeliveryMethod::Direct
        );
        assert_eq!(content_len(b"hi", &[]), 2);
    }

    /// Mine `job` with the test clock and recorded effects, on a runtime
    /// with no timer: the stepping clock never needs one.
    fn mine_recorded(job: StampJob) -> (StampAnswer, Vec<String>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        let clock = crate::clock::fake::Stepping::default();
        let mut effects = crate::effects::fake::Recorded::default();
        let answer = runtime.block_on(mine(job, rand_core::OsRng, &clock, &mut effects));
        (answer, effects.notices)
    }

    /// A grind above the narration threshold is narrated through
    /// `Effects::notice`, the same lines the stamp worker wrote to stderr
    /// before the engine lost std, and a cancelled one says it was abandoned,
    /// never mined. Cancelled up front, so the search returns at its first
    /// check and the test costs nothing.
    #[test]
    fn a_costly_grind_is_narrated_through_the_effects() {
        let cancel = Arc::new(StampCancel::new());
        cancel.cancel();
        let (answer, notices) = mine_recorded(StampJob::Delivery(
            DeliveryStampRequest {
                message_id: [0x5a; 32],
                target_cost: 254,
            },
            cancel,
        ));
        assert!(matches!(
            answer,
            StampAnswer::Delivery {
                result: Err(StampError::Cancelled),
                ..
            }
        ));
        assert_eq!(
            notices,
            [
                "mining delivery stamp at cost 254...",
                "delivery stamp abandoned after 0.0s",
            ]
        );
    }

    /// The progress lines come every five seconds of the given clock while
    /// the grind runs, and the grind still ends when it is called off. The
    /// effects here fire the cancel on the second line, the way a frontend's
    /// cancel would; the search observes it at its first yield after the
    /// workblock, so the lines go on until then. The stepping clock moves
    /// only when the progress timer is polled, so every line is exact and
    /// the closing time is five seconds per line.
    #[test]
    fn a_running_grind_says_it_is_still_going() {
        struct CancelOnSecondLine {
            recorded: crate::effects::fake::Recorded,
            cancel: Arc<StampCancel>,
        }
        impl Effects for CancelOnSecondLine {
            fn notice(&mut self, line: &str) {
                if line.contains("(10s)") {
                    self.cancel.cancel();
                }
                self.recorded.notice(line);
            }
            fn print(&mut self, rendered: &[u8]) -> Result<(), String> {
                self.recorded.print(rendered)
            }
            fn save_mailbox(
                &mut self,
                stored: &crate::mailbox_state::Stored,
            ) -> Result<(), String> {
                self.recorded.save_mailbox(stored)
            }
            fn write_seen(&mut self, write: &crate::seen::SeenWrite) -> Result<(), String> {
                self.recorded.write_seen(write)
            }
            fn unix_now_ms(&self) -> u64 {
                self.recorded.unix_now_ms()
            }
        }

        let cancel = Arc::new(StampCancel::new());
        let mut effects = CancelOnSecondLine {
            recorded: crate::effects::fake::Recorded::default(),
            cancel: Arc::clone(&cancel),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        let clock = crate::clock::fake::Stepping::default();
        let job = StampJob::Delivery(
            DeliveryStampRequest {
                message_id: [0x5a; 32],
                target_cost: 254,
            },
            cancel,
        );
        let answer = runtime.block_on(mine(job, rand_core::OsRng, &clock, &mut effects));
        assert!(matches!(
            answer,
            StampAnswer::Delivery {
                result: Err(StampError::Cancelled),
                ..
            }
        ));
        let notices = effects.recorded.notices;
        let (last, still) = notices[1..].split_last().expect("more than one line");
        assert_eq!(notices[0], "mining delivery stamp at cost 254...");
        assert!(still.len() >= 2, "{notices:?}");
        for (index, line) in still.iter().enumerate() {
            assert_eq!(
                *line,
                format!("still mining delivery stamp ({}s)...", (index + 1) * 5)
            );
        }
        assert_eq!(
            *last,
            format!("delivery stamp abandoned after {}.0s", still.len() * 5)
        );
    }

    /// At or below the threshold a grind is silent.
    #[test]
    fn a_cheap_grind_says_nothing() {
        let cancel = Arc::new(StampCancel::new());
        cancel.cancel();
        let (_, notices) = mine_recorded(StampJob::Propagation(
            PropagationStampRequest {
                message_id: [0x5a; 32],
                transient_id: [0x11; 32],
                target_cost: STAMP_PROGRESS_COST,
            },
            cancel,
        ));
        assert!(notices.is_empty(), "{notices:?}");
    }

    /// A held message turns verified once its sender's key is known, and one
    /// whose signature another key made turns invalid, never left
    /// unverified (leviculum 1079). Until the key arrives the engine asks for
    /// the sender's path and says nothing about the message.
    #[test]
    fn a_held_message_is_checked_when_the_key_arrives_and_a_forgery_is_invalid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let ours = match drain(&mut engine).first() {
            Some(OutboxEvent::Ready { address }) => *address,
            other => panic!("expected Ready first, got {other:?}"),
        };
        let alice = leviculum_std::generate_identity();
        let mallory = leviculum_std::generate_identity();
        let alice_address = delivery_address(&alice).expect("alice's address");
        let signed_by = |key: &Identity, body: &[u8]| {
            leviculum_lxmf::Message::create(
                ours,
                alice_address,
                key,
                1_700_000_000.0,
                Vec::new(),
                body.to_vec(),
                Vec::new(),
                DeliveryMethod::Direct,
            )
            .expect("a signed message")
        };
        let genuine = signed_by(&alice, b"from alice");
        let forged = signed_by(&mallory, b"from alice, honest");
        for message in [&genuine, &forged] {
            engine.submit(Command::Verify {
                message_id: message.message_id,
                source: alice_address,
                packed: message.pack(),
            });
        }

        let _ = engine.on_tick(&mut core, now_ms + POLL_INTERVAL_MS);
        assert!(
            !drain(&mut engine)
                .iter()
                .any(|event| matches!(event, OutboxEvent::Verified { .. })),
            "nothing is checked before the key is known"
        );

        let public =
            Identity::from_public_key_bytes(&alice.public_key_bytes()).expect("alice's public key");
        core.storage_mut().set_identity(alice_address, public);
        let _ = engine.on_tick(&mut core, now_ms + 2 * POLL_INTERVAL_MS);
        let verdicts: Vec<([u8; 32], bool)> = drain(&mut engine)
            .into_iter()
            .filter_map(|event| match event {
                OutboxEvent::Verified {
                    message_id,
                    source,
                    valid,
                } => {
                    assert_eq!(source, alice_address);
                    Some((message_id, valid))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            verdicts,
            vec![(genuine.message_id, true), (forged.message_id, false)]
        );

        // Checked once: a later tick says nothing more.
        let _ = engine.on_tick(&mut core, now_ms + 3 * POLL_INTERVAL_MS);
        assert!(!drain(&mut engine)
            .iter()
            .any(|event| matches!(event, OutboxEvent::Verified { .. })));
    }

    /// Bytes that no longer parse as a message are not one the key signed.
    #[test]
    fn bytes_that_do_not_parse_are_invalid_not_unverified() {
        let alice = leviculum_std::generate_identity();
        assert_eq!(reverify(&[0u8; 40], &alice), Verification::Invalid);
    }

    /// One message signed by a fresh identity, packed, and its id.
    fn packed_message(destination: [u8; 16]) -> (Vec<u8>, [u8; 32]) {
        let sender = leviculum_std::generate_identity();
        let source = delivery_address(&sender).expect("address");
        let message = leviculum_lxmf::Message::create(
            destination,
            source,
            &sender,
            1_800_000_000.25,
            Vec::new(),
            b"durch das Zuhause".to_vec(),
            Vec::new(),
            DeliveryMethod::Direct,
        )
        .expect("message");
        (message.pack(), message.message_id)
    }

    /// A home node's outbox hands the router the bytes it signed once: the
    /// queued message has their id, on the direct leg as on any retry. A
    /// propagated leg with no node selected is said as unsent, with the id,
    /// so the outbox can move on; the fence comes back after both.
    #[test]
    fn a_packed_message_is_queued_under_its_own_id_or_said_unsent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine);

        let (packed, id) = packed_message([0x42; 16]);
        engine.submit(Command::SendPacked {
            packed: packed.clone(),
            via: Via::Link,
        });
        engine.submit(Command::SendPacked {
            packed: packed.clone(),
            via: Via::Propagated,
        });
        engine.submit(Command::Fence { token: 7 });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        let queued = events
            .iter()
            .position(|e| *e == OutboxEvent::Queued { message_id: id })
            .unwrap_or_else(|| panic!("not queued under its id: {events:?}"));
        let unsent = events
            .iter()
            .position(|e| matches!(e, OutboxEvent::Unsent { message_id, .. } if *message_id == id))
            .unwrap_or_else(|| panic!("the propagated leg was not said unsent: {events:?}"));
        let fenced = events
            .iter()
            .position(|e| *e == OutboxEvent::Fenced { token: 7 })
            .unwrap_or_else(|| panic!("no fence: {events:?}"));
        assert!(queued < unsent && unsent < fenced, "{events:?}");
        // The same bytes again while the first is queued: the router holds one
        // entry per id, and says so as unsent rather than queueing twice.
        engine.submit(Command::SendPacked {
            packed,
            via: Via::Link,
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, OutboxEvent::Unsent { message_id, .. } if *message_id == id)),
            "{events:?}"
        );
    }

    /// 1406, the field mailbox on 2026-11-04: a propagated leg to a
    /// recipient whose key this node does not hold cannot be encrypted for
    /// the node, and the router held it without a word until the home's
    /// stall bound gave it up after 779 s, every hour for seven days. It is
    /// said at once as a missing key, and the recipient's path is asked, as
    /// a direct leg asks it; nothing waits in the router.
    /// Control: the same message with the key known is queued.
    #[test]
    fn a_propagated_leg_without_the_recipients_key_fails_at_once_and_asks_its_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        engine.submit(Command::SelectPn {
            preferred: Some([8; 16]),
            mailbox: None,
        });
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine);
        let asks_path = |out: &TickOutput, destination: [u8; 16]| {
            out.actions.iter().any(|action| match action {
                leviculum_core::Action::SendPacket { data, .. }
                | leviculum_core::Action::Broadcast { data, .. } => {
                    data.windows(16).any(|window| window == destination)
                }
            })
        };

        let (packed, id) = packed_message([0x42; 16]);
        engine.submit(Command::SendPacked {
            packed,
            via: Via::Propagated,
        });
        let out = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        assert!(
            !events.contains(&OutboxEvent::Queued { message_id: id }),
            "a leg without the key was queued: {events:?}"
        );
        assert!(
            events.contains(&OutboxEvent::KeyMissing {
                message_id: id,
                destination: [0x42; 16],
            }),
            "{events:?}"
        );
        assert!(asks_path(&out, [0x42; 16]), "no path request: {out:?}");

        // Control: with the recipient's key held, the leg is queued.
        let recipient = leviculum_std::generate_identity();
        let known = delivery_address(&recipient).expect("address");
        core.remember_identity(
            DestinationHash::new(known),
            Identity::from_public_key_bytes(&recipient.public_key_bytes()).expect("public key"),
        );
        let (packed, id) = packed_message(known);
        engine.submit(Command::SendPacked {
            packed,
            via: Via::Propagated,
        });
        let out = engine.on_tick(&mut core, now_ms);
        let events = drain(&mut engine);
        assert!(
            events.contains(&OutboxEvent::Queued { message_id: id }),
            "the leg with the key was not queued: {events:?}"
        );
        assert!(!asks_path(&out, known), "a path asked with the key held");
    }

    /// A client's engine names a bubble after the id the home node gave it,
    /// and gives it the states pulled from the archive; it sends nothing
    /// itself; its fence comes back behind the commands before it.
    #[test]
    fn a_client_engine_relays_what_its_home_node_said() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_home, _address, mut engine, mut core) = client_setup(dir.path());
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine);
        let (packed, id) = packed_message([0x42; 16]);
        for command in [
            Command::HomeAccepted { message_id: id },
            Command::HomeState {
                message_id: id,
                state: MessageState::Delivered,
            },
            Command::SendPacked {
                packed,
                via: Via::Link,
            },
            Command::Fence { token: 3 },
        ] {
            engine.submit(command);
        }
        let now_ms = core.now_ms();
        let out = engine.on_tick(&mut core, now_ms);
        assert!(out.actions.is_empty(), "a client put something on the wire");
        let events = drain(&mut engine);
        assert_eq!(events[0], OutboxEvent::Queued { message_id: id });
        assert_eq!(
            events[1],
            OutboxEvent::State {
                message_id: id,
                state: MessageState::Delivered
            }
        );
        assert!(
            matches!(&events[2], OutboxEvent::Unsent { message_id, detail }
                if *message_id == id && detail == CLIENT_SENDS),
            "{events:?}"
        );
        assert_eq!(events[3], OutboxEvent::Fenced { token: 3 });
    }

    /// 1732: a home node's engine and a client's echo the number of the
    /// census they answer, so the mailbox can tell the census an answer
    /// belongs to.
    #[test]
    fn an_engine_answers_a_census_under_its_number() {
        let census_seq = |events: Vec<OutboxEvent>| {
            events
                .into_iter()
                .find_map(|e| match e {
                    OutboxEvent::Census(answer) => Some(answer.seq),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no census answer"))
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let mut engine = engine();
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine);
        engine.submit(Command::Census { seq: 41 });
        let _ = engine.on_tick(&mut core, now_ms);
        assert_eq!(census_seq(drain(&mut engine)), 41);

        let dir = tempfile::tempdir().expect("tempdir");
        let (_home, _address, mut engine, mut core) = client_setup(dir.path());
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        drain(&mut engine);
        engine.submit(Command::Census { seq: 42 });
        let _ = engine.on_tick(&mut core, now_ms);
        assert_eq!(census_seq(drain(&mut engine)), 42);
    }
}
