//! The in-process message engine: an [`LxmfRouter`] driven from inside the
//! driver's tick, and the [`Outbox`] the frontend sees it through.
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
//! (`docs/src/concepts/lnmsg-architecture.md` §3). This slice only sends, but
//! the feed is the same one the reading half will need, and picking the wrong
//! one now would have to be undone later.
//!
//! # The rule this file is written against
//!
//! Both hooks run **with the core mutex held, and that mutex is not
//! reentrant**. So this type owns no `ReticulumNode` and no handle derived
//! from one, and every side effect is a push onto an unbounded queue. Nothing
//! here does I/O, and the one unbounded computation in LXMF — proof-of-work
//! for a stamp — leaves through a channel and is mined on a thread of its own.
//!
//! # What it deliberately does not do
//!
//! No persistence. `LxmfRouter::persist` writes the outbound queue to an
//! `LxmfStorage`, and a `FileLxmfStorage` for it exists
//! (`leviculum-std/src/file_lxmf_store.rs:27`) — but in this slice nothing
//! would ever drain a restored queue, since there is no daemon and no
//! `lnmsg sync`. Persisting would buy a file on disk that no code reads, at
//! the price of a write inside the core lock. The moment daemon mode or a
//! resume path exists, this is where it goes.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use leviculum_core::identity::Identity;
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::{Destination, DestinationHash, Storage as _};
use leviculum_lxmf::propagation::PropagationNodeAnnounce;
use leviculum_lxmf::propagation_client::{PropagationTransport, PROPAGATION_ASPECT};
use leviculum_lxmf::router::{
    LxmfRouter, MessageState, PropagationClientConfig, PropagationClientState,
    PropagationStampRequest, RouterConfig, RouterError, RouterEvent, RouterOutput,
};
use leviculum_lxmf::{
    announce, CooperativeStamper, DeliveryMethod, DeliveryStampRequest, LxmfNode, LxmfNodeConfig,
    Verification,
};
use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder, StdNodeCore};
use leviculum_std::ReticulumNode;

use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone, SendRequest, Via};

/// How soon the engine asks the driver to come back.
///
/// The command queue needs a periodic slot — an event tap can never
/// *initiate* anything, because it only fires when the core has something to
/// say — and so does a pending resolve. 200 ms is what `leviculum-lxmf-node`
/// uses for the same job, and it bounds command pickup latency at a fifth of
/// the driver's 1 s idle cadence.
const POLL_INTERVAL_MS: u64 = 200;

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

/// One proof-of-work job on its way to the mining thread.
enum StampJob {
    /// A recipient delivery stamp, at the cost the recipient announced.
    Delivery(DeliveryStampRequest),
    /// The independent propagation-node stamp over the transient ID, at the
    /// cost the selected node announced.
    Propagation(PropagationStampRequest),
}

impl StampJob {
    fn key(&self) -> (StampKind, [u8; 32]) {
        match self {
            Self::Delivery(request) => (StampKind::Delivery, request.message_id),
            Self::Propagation(request) => (StampKind::Propagation, request.message_id),
        }
    }
}

/// Which of a message's two possible stamps a mining job is for. A propagated
/// message to a cost-announcing recipient legitimately mines both, one after
/// the other, so the single-flight set is keyed by kind as well as id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum StampKind {
    Delivery,
    Propagation,
}

/// A stamp the mining thread has finished with.
enum StampAnswer {
    Delivery {
        request: DeliveryStampRequest,
        result: Result<[u8; 32], String>,
    },
    Propagation {
        request: PropagationStampRequest,
        result: Result<[u8; 32], String>,
    },
}

/// A destination the frontend asked to have resolved.
struct Resolve {
    destination: [u8; 16],
    /// A path is asked for once per resolve, as Python's clients do.
    path_requested: bool,
    /// Set once the answer has gone out, so a repeated ask is not answered
    /// twice.
    answered: bool,
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
}

/// The LXMF engine, as the driver sees it.
pub struct Engine {
    display_name: Vec<u8>,
    events: Sender<OutboxEvent>,
    commands: Receiver<Command>,
    stamps: Sender<StampJob>,
    stamp_answers: Receiver<StampAnswer>,
    state: State,
    resolves: Vec<Resolve>,
    /// Messages this run queued, and the last state seen for each. The router
    /// forgets a message the moment it reaches a terminal state, so without
    /// this the removal could not be attributed.
    tracked: BTreeMap<[u8; 32], Option<MessageState>>,
    /// Stamps being mined, so a re-offer does not queue a second mine for the
    /// same work behind the first. Keyed by kind as well as id: a propagated
    /// message can need a recipient stamp and a node stamp in one run.
    mining: HashSet<(StampKind, [u8; 32])>,
    /// Whether our own delivery announce has gone out. See [`Engine::announce`].
    announced: bool,
    /// The most recently announced propagation node this run has heard —
    /// what [`Command::SelectPn`] falls back to when neither `--pn` nor the
    /// config names one.
    last_pn: Option<[u8; 16]>,
}

impl Engine {
    fn emit(&self, event: OutboxEvent) {
        // A dropped receiver means the frontend has already finished; the
        // engine keeps running until the node stops, and has nothing to say
        // about it.
        let _ = self.events.send(event);
    }

    /// Register the delivery destination on first use and announce readiness.
    ///
    /// Called from both hooks: the driver's timer branch normally fires first,
    /// but nothing in the seam promises that.
    fn register_if_needed(&mut self, core: &mut StdNodeCore) {
        let identity = match std::mem::replace(&mut self.state, State::Failed) {
            State::Unregistered(identity) => *identity,
            other => {
                self.state = other;
                return;
            }
        };
        self.state = match register(core, identity) {
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
    fn take_ready(&mut self, core: &mut StdNodeCore) -> Option<Box<Ready>> {
        self.register_if_needed(core);
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Ready(ready) => Some(ready),
            other => {
                self.state = other;
                None
            }
        }
    }

    /// Route one router output: report its events, collect its wire actions,
    /// and re-feed the core events it produced. See [`MAX_ABSORB_ROUNDS`].
    fn absorb(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        first: RouterOutput,
        out: &mut TickOutput,
    ) {
        let mut queue = VecDeque::from([first]);
        let mut rounds = 0usize;
        while let Some(router_output) = queue.pop_front() {
            for event in router_output.events {
                self.report(event);
            }
            let mut core_output = router_output.core;
            let events = std::mem::take(&mut core_output.events);
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
    fn report(&mut self, event: RouterEvent) {
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
            RouterEvent::StampPending(request) => self.dispatch_stamp(StampJob::Delivery(request)),
            RouterEvent::PropagationStampPending(request) => {
                self.dispatch_stamp(StampJob::Propagation(request));
            }
            RouterEvent::MessageReceived(message) => {
                let message = *message;
                self.emit(OutboxEvent::Received {
                    message_id: message.message_id,
                    source: message.source_hash,
                    verified: message.verification == Verification::Valid,
                    title: message.title,
                    body: message.content,
                });
            }
            RouterEvent::PropagationSyncComplete(result) => {
                self.emit(OutboxEvent::SyncDone {
                    received: result.received,
                    duplicates: result.duplicates,
                });
            }
            RouterEvent::PropagationSyncState(status) => {
                if let Some(word) = sync_failure(status.state) {
                    self.emit(OutboxEvent::SyncFailed {
                        detail: word.to_string(),
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

    /// Hand one proof-of-work job to the mining thread.
    ///
    /// Off the lock: mining is unbounded work at a cost the peer chooses, and
    /// the generator is async. Both are things a hook body may least afford.
    ///
    /// Single-flight: the router re-offers a still-queued message every retry
    /// interval, and a second mine for one job would queue behind the first
    /// for the same answer.
    fn dispatch_stamp(&mut self, job: StampJob) {
        let key = job.key();
        if self.mining.insert(key) && self.stamps.send(job).is_err() {
            // The worker is gone, so the answer will never come; drop the
            // marker so a later re-offer is dispatched normally.
            self.mining.remove(&key);
        }
    }

    /// Notice messages the router has forgotten. A terminal state removes the
    /// entry from the outbound map (`leviculum-lxmf/src/router.rs:771-773`),
    /// and the frontend has to be told, because the state event and the
    /// removal can arrive in the same tick.
    ///
    /// The predicate is membership of `outbound()`, deliberately **not**
    /// `LxmfRouter::has_message`: that one asks the delivered-id dedup cache
    /// whether this id has already been *received*
    /// (`leviculum-lxmf/src/router.rs:679-681`), which is false for every
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
            self.emit(OutboxEvent::Left {
                message_id: id,
                last,
            });
        }
    }

    /// Drain the frontend's command queue. Non-blocking by construction.
    fn pump_commands(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        loop {
            match self.commands.try_recv() {
                Ok(Command::Resolve { destination }) => self.resolves.push(Resolve {
                    destination,
                    path_requested: false,
                    answered: false,
                }),
                Ok(Command::Send(request)) => self.send(ready, core, *request, out),
                Ok(Command::SelectPn { preferred }) => self.select_pn(ready, core, preferred, out),
                Ok(Command::Fetch) => self.fetch(ready, core, out),
                Ok(Command::Cancel { message_id }) => self.cancel(ready, core, &message_id, out),
                Err(TryRecvError::Empty) => return,
                // The frontend is gone; the node is being torn down.
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Pick the propagation node for this run's uploads and drains.
    fn select_pn(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        preferred: Option<[u8; 16]>,
        out: &mut TickOutput,
    ) {
        let Some(destination) = preferred.or(self.last_pn) else {
            self.emit(OutboxEvent::PnUnavailable {
                detail: "no propagation node is known: none was given with --pn, none is \
                         configured, and none has announced since this run attached"
                    .to_string(),
            });
            return;
        };
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

    /// Start one mailbox drain against the selected node.
    fn fetch(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        match ready
            .router
            .request_messages_from_propagation_node(core, None)
        {
            Ok(output) => self.absorb(ready, core, output, out),
            Err(e) => self.emit(OutboxEvent::SyncFailed {
                detail: format!("{e:?}"),
            }),
        }
    }

    /// Drop a queued message. `NotFound` is not an error here: the message
    /// may have reached a terminal state between the frontend's decision and
    /// this tick, which is exactly the outcome a cancel wants.
    fn cancel(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        message_id: &[u8; 32],
        out: &mut TickOutput,
    ) {
        match ready.router.cancel(core, message_id) {
            Ok(output) => self.absorb(ready, core, output, out),
            Err(RouterError::NotFound) => {}
            Err(e) => self.emit(OutboxEvent::Refused {
                detail: format!("cancel: {e:?}"),
            }),
        }
    }

    /// Drain finished proof-of-work.
    fn pump_stamps(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        loop {
            match self.stamp_answers.try_recv() {
                Ok(StampAnswer::Delivery { request, result }) => {
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
                        Err(detail) => self.emit(OutboxEvent::Refused {
                            detail: format!("proof-of-work failed: {detail}"),
                        }),
                    }
                }
                Ok(StampAnswer::Propagation { request, result }) => {
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
                        Err(detail) => self.emit(OutboxEvent::Refused {
                            detail: format!("proof-of-work failed: {detail}"),
                        }),
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn send(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        request: SendRequest,
        out: &mut TickOutput,
    ) {
        let method = match request.via {
            Via::Direct => delivery_method(request.body.len()),
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
            Vec::new(),
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
    fn announce(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        if self.announced {
            return;
        }
        self.announced = true;
        let app_data = announce::delivery(Some(&self.display_name), None);
        match core.announce_destination(&DestinationHash::new(ready.address), Some(&app_data)) {
            Ok(output) => self.absorb(
                ready,
                core,
                RouterOutput {
                    core: output,
                    events: Vec::new(),
                },
                out,
            ),
            // Not fatal, and deliberately not an `OutboxEvent::Refused`: the
            // message still goes out, it just arrives unverifiable at a peer
            // that has never heard of us. Failing the send over it would turn
            // a cosmetic problem into a lost status line.
            Err(e) => tracing::warn!("lnmsg: could not announce our own address: {e:?}"),
        }
    }

    /// Answer every resolve that can be answered now.
    ///
    /// A destination counts as resolved when its identity is known *and* a
    /// path exists: the first is what a message is encrypted to, the second is
    /// where it goes. Unlike `leviculum-lxmf-node`, the path is requested
    /// without waiting for the identity to turn up first — a path request is
    /// how an address nobody has announced near us becomes known at all, and a
    /// messenger's whole job is sending to an address a user was given.
    fn poll_resolves(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
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
                self.resolves[index].answered = true;
                self.emit(OutboxEvent::Resolved { destination });
                continue;
            }
            if !self.resolves[index].path_requested {
                self.resolves[index].path_requested = true;
                // Through `absorb` rather than merged straight into `out`:
                // `request_path` ends in `process_events_and_actions`, so its
                // output can carry events, and every other core call made from
                // a hook here has them re-fed to the router for the reason
                // `MAX_ABSORB_ROUNDS` gives.
                let output = core.request_path(&hash);
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

/// Which LXMF delivery method a body of this size should use.
///
/// Opportunistic delivery is one encrypted packet with no link setup, so for
/// the status line this program was built for it is the whole exchange.
/// Anything that cannot fit goes over a link instead. Airtime is the scarcest
/// resource on these networks (`docs/src/concepts/lnmsg.md` §3), and a link
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

/// The delivery address `identity` registers as — what `lnmsg address`
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
fn register(core: &mut StdNodeCore, identity: Identity) -> Result<Ready, String> {
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
    let destination =
        LxmfNode::delivery_destination(copy).map_err(|e| format!("delivery destination: {e:?}"))?;
    let address = *destination.hash().as_bytes();
    let node = LxmfNode::register(core, destination, LxmfNodeConfig::default())
        .map_err(|e| format!("register delivery destination: {e:?}"))?;
    let mut router = LxmfRouter::new(node, identity_hash, RouterConfig::default());
    // The propagation client is always on, exactly as `leviculum-lxmf-node`
    // has it (`leviculum-lxmf-node/src/processor.rs`, "The propagation
    // *client* is always on"): `--via propagated`, the auto fallback and
    // `lnmsg fetch` all need it, and it registers a second local destination
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

impl CoreProcessor for Engine {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        let mut out = TickOutput::empty();
        // Recency, not ranking: `Command::SelectPn` without a preference
        // takes the most recently announced node, so the order of arrival is
        // tracked here — the router's transport keeps the set but not the
        // order. A node announcing itself disabled is not a candidate.
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            if announce.name_hash()
                == &Destination::compute_name_hash(
                    leviculum_lxmf::node::APP_NAME,
                    &[PROPAGATION_ASPECT],
                )
                && PropagationNodeAnnounce::decode(announce.app_data())
                    .is_ok_and(|decoded| decoded.enabled)
            {
                self.last_pn = Some(*announce.destination_hash().as_bytes());
            }
        }
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        match ready.router.handle_event(core, event) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.emit(OutboxEvent::Broken {
                detail: format!("router handle_event: {e:?}"),
            }),
        }
        // An announce arriving is exactly what a pending resolve is waiting
        // for. Answering here rather than on the next timer tick puts the
        // latency at the announce instead of `POLL_INTERVAL_MS` after it.
        self.poll_resolves(&mut ready, core, &mut out);
        self.report_departures(&ready);
        self.state = State::Ready(ready);
        out
    }

    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };

        self.announce(&mut ready, core, &mut out);
        self.pump_commands(&mut ready, core, &mut out);
        self.pump_stamps(&mut ready, core, &mut out);
        self.poll_resolves(&mut ready, core, &mut out);
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

/// The frontend's end of the in-process engine.
pub struct InProcessOutbox {
    commands: Sender<Command>,
    events: Receiver<OutboxEvent>,
}

impl Outbox for InProcessOutbox {
    fn submit(&self, command: Command) -> Result<(), OutboxGone> {
        self.commands.send(command).map_err(|_| OutboxGone)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(OutboxGone),
        }
    }
}

/// What attaching to a shared instance needs to know.
pub struct AttachConfig {
    /// The shared instance to join, i.e. which daemon.
    pub instance: String,
    /// Where this client keeps its node state (learned identities, paths).
    ///
    /// Deliberately not the daemon's storage directory: `lncp` and `lnstatus`
    /// share that safely because a client with `enable_transport(false)`
    /// writes no paths, announces or packet hashes — this one registers a
    /// destination and learns identities, so it gets its own.
    pub storage_dir: PathBuf,
    /// The persistent identity whose delivery destination is our address.
    pub identity: Identity,
    /// The name recipients see as the sender: it goes out in the delivery
    /// announce `Engine::announce` sends, and a reference client shows it
    /// next to every message from us. Resolved by
    /// [`crate::display_name::from_process`], which never yields an empty one.
    pub display_name: Vec<u8>,
}

/// Why attaching failed.
#[derive(Debug)]
pub enum AttachError {
    Storage(PathBuf, std::io::Error),
    /// No daemon answered. The message names the instance, because the usual
    /// cause is the right daemon running under a different name.
    ///
    /// Both the build and the start of a shared-instance client fail this way,
    /// and they are one error to a user: the IPC socket is the only thing
    /// either step reaches for. Which of the two noticed is in `detail`.
    NoDaemon {
        instance: String,
        detail: String,
    },
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(path, error) => write!(f, "{}: {error}", path.display()),
            Self::NoDaemon { instance, detail } => write!(
                f,
                "could not join the Reticulum shared instance named '{instance}'.\n  \
                 lnmsg talks to a daemon that is already running; it does not start a \
                 Reticulum stack of its own.\n  \
                 Start one with `lnsd` (or Python's `rnsd`), or name another instance \
                 with --instance / --config.\n  \
                 The stack said: {detail}"
            ),
        }
    }
}

impl std::error::Error for AttachError {}

/// A running client node and the outbox that talks to its engine.
pub struct Attached {
    node: ReticulumNode,
    /// The frontend's end of the seam.
    pub outbox: InProcessOutbox,
}

impl Attached {
    /// Stop the node. Best effort: the send outcome is already decided by the
    /// time anything calls this.
    pub async fn stop(mut self) {
        let _ = self.node.stop().await;
    }
}

/// Attach to a running shared instance with an LXMF engine installed.
pub async fn attach(config: AttachConfig) -> Result<Attached, AttachError> {
    std::fs::create_dir_all(&config.storage_dir)
        .map_err(|e| AttachError::Storage(config.storage_dir.clone(), e))?;

    let (commands_tx, commands_rx) = std::sync::mpsc::channel::<Command>();
    let (events_tx, events_rx) = std::sync::mpsc::channel::<OutboxEvent>();
    let (stamps_tx, stamps_rx) = std::sync::mpsc::channel::<StampJob>();
    let (answers_tx, answers_rx) = std::sync::mpsc::channel::<StampAnswer>();

    // Proof-of-work, off the core lock and off the runtime's workers: mining
    // is pure CPU that can run for minutes at a cost the *peer* chooses. Its
    // own thread with its own current-thread runtime, exactly as
    // `leviculum-lxmf-node` does it (`leviculum-lxmf-node/src/main.rs:224-255`),
    // because the generator is async. Dormant unless a peer advertises a stamp
    // cost, which most do not.
    std::thread::spawn(move || run_stamp_worker(&stamps_rx, &answers_tx));

    let engine = Engine {
        display_name: config.display_name,
        events: events_tx,
        commands: commands_rx,
        stamps: stamps_tx,
        stamp_answers: answers_rx,
        state: State::Unregistered(Box::new(config.identity)),
        resolves: Vec::new(),
        tracked: BTreeMap::new(),
        mining: HashSet::new(),
        announced: false,
        last_pn: None,
    };

    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&config.instance)
        .storage_path(config.storage_dir)
        .core_processor(engine)
        .build()
        .await
        .map_err(|e| AttachError::NoDaemon {
            instance: config.instance.clone(),
            detail: e.to_string(),
        })?;
    node.start().await.map_err(|e| AttachError::NoDaemon {
        instance: config.instance.clone(),
        detail: e.to_string(),
    })?;

    Ok(Attached {
        node,
        outbox: InProcessOutbox {
            commands: commands_tx,
            events: events_rx,
        },
    })
}

fn run_stamp_worker(jobs: &Receiver<StampJob>, answers: &Sender<StampAnswer>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    while let Ok(job) = jobs.recv() {
        let answer = match job {
            StampJob::Delivery(request) => {
                let cost = request.target_cost;
                let result = mine_with_progress(&runtime, cost, "delivery stamp", async {
                    let mut executor = CooperativeStamper::cooperative(rand_core::OsRng);
                    request.generate_with(&mut executor).await
                });
                StampAnswer::Delivery { request, result }
            }
            StampJob::Propagation(request) => {
                let cost = request.target_cost;
                let result = mine_with_progress(&runtime, cost, "propagation stamp", async {
                    let mut executor = CooperativeStamper::cooperative(rand_core::OsRng);
                    request.generate_with(&mut executor).await
                });
                StampAnswer::Propagation { request, result }
            }
        };
        if answers.send(answer).is_err() {
            return;
        }
    }
}

/// Run one mining future to completion, narrating it on stderr when the cost
/// is above [`STAMP_PROGRESS_COST`].
///
/// The narration is elapsed time, not a percentage: proof-of-work has no
/// knowable fraction-done, and inventing one would be the progress-bar lie.
/// stderr rather than stdout, so a scripted send's "success prints nothing"
/// contract on stdout is untouched.
fn mine_with_progress<F>(
    runtime: &tokio::runtime::Runtime,
    cost: u8,
    what: &str,
    future: F,
) -> Result<[u8; 32], String>
where
    F: std::future::Future<Output = Result<[u8; 32], leviculum_lxmf::StampError>>,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let started = std::time::Instant::now();
    let ticker = if cost > STAMP_PROGRESS_COST {
        eprintln!("lnmsg: mining {what} at cost {cost}...");
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let what = what.to_string();
        let handle = std::thread::spawn(move || {
            let mut waited = 0u64;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                waited += 500;
                if waited.is_multiple_of(5_000) && !flag.load(Ordering::Relaxed) {
                    eprintln!("lnmsg: still mining {what} ({}s)...", waited / 1_000);
                }
            }
        });
        Some((done, handle))
    } else {
        None
    };
    let result = runtime.block_on(future).map_err(|e| format!("{e:?}"));
    if let Some((done, handle)) = ticker {
        done.store(true, Ordering::Relaxed);
        let _ = handle.join();
        eprintln!(
            "lnmsg: {what} mined in {:.1}s",
            started.elapsed().as_secs_f32()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_std::driver::{StdClock, StdStorage};

    fn engine() -> (Engine, Receiver<OutboxEvent>, Sender<Command>) {
        engine_named(b"lnmsg-test")
    }

    fn engine_named(display_name: &[u8]) -> (Engine, Receiver<OutboxEvent>, Sender<Command>) {
        let (commands_tx, commands_rx) = std::sync::mpsc::channel::<Command>();
        let (events_tx, events_rx) = std::sync::mpsc::channel::<OutboxEvent>();
        let (stamps_tx, _stamps_rx) = std::sync::mpsc::channel::<StampJob>();
        let (_answers_tx, answers_rx) = std::sync::mpsc::channel::<StampAnswer>();
        let engine = Engine {
            display_name: display_name.to_vec(),
            events: events_tx,
            commands: commands_rx,
            stamps: stamps_tx,
            stamp_answers: answers_rx,
            state: State::Unregistered(Box::new(leviculum_std::generate_identity())),
            resolves: Vec::new(),
            tracked: BTreeMap::new(),
            mining: HashSet::new(),
            announced: false,
            last_pn: None,
        };
        (engine, events_rx, commands_tx)
    }

    fn core(dir: &std::path::Path) -> StdNodeCore {
        NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir).expect("storage under a fresh temp dir"),
        )
    }

    /// The first tick registers the delivery destination against a real core
    /// and hands the frontend our address. A stub would not get this far.
    #[test]
    fn the_first_tick_registers_and_reports_our_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, events, _commands) = engine();

        let now_ms = core.now_ms();
        let out = engine.on_tick(&mut core, now_ms);

        match events.try_recv() {
            Ok(OutboxEvent::Ready { address }) => {
                assert_ne!(address, [0u8; 16], "the address must be a real hash")
            }
            other => panic!("the first tick must report readiness, got {other:?}"),
        }
        assert!(
            out.next_deadline_ms.is_some(),
            "on_tick always asks the driver back for the command queue"
        );
    }

    /// The configured display name has to reach the wire, not just the struct.
    ///
    /// This is the assertion that would have caught the bug it was written for:
    /// `main.rs` passed the literal `lnmsg` into [`AttachConfig`], so a test of
    /// the config field alone would have been green while every recipient saw a
    /// message from a tool rather than from a person. What matters is the
    /// `app_data` of the announce that leaves here, so the announce is taken
    /// out of the tick's actions and unpacked.
    #[test]
    fn the_display_name_reaches_the_announce_that_goes_out() {
        use leviculum_core::packet::{Packet, PacketType};

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events, _commands) = engine_named(b"an-operator");

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
            !payload.ends_with(&announce::delivery(Some(b"lnmsg"), None)),
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
        let (mut engine, events, commands) = engine();

        commands
            .send(Command::Send(Box::new(SendRequest {
                destination: [0x11; 16],
                title: Vec::new(),
                body: b"body".to_vec(),
                via: Via::Propagated,
            })))
            .expect("queue the command");
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let refusals: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
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
        let (mut engine, events, commands) = engine();

        commands
            .send(Command::SelectPn { preferred: None })
            .expect("queue the command");
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let seen: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
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
        let (mut engine, events, commands) = engine();

        let node = [0x7e; 16];
        commands
            .send(Command::SelectPn {
                preferred: Some(node),
            })
            .expect("queue the command");
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let selected =
            std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
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

    /// A resolve for an address nobody has announced must stay unanswered.
    /// Answering it would send a message into a hole.
    #[test]
    fn an_unknown_destination_is_not_reported_as_resolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, events, commands) = engine();

        commands
            .send(Command::Resolve {
                destination: [0x22; 16],
            })
            .expect("queue the command");
        for _ in 0..3 {
            let now_ms = core.now_ms();
            let _ = engine.on_tick(&mut core, now_ms);
        }

        let resolved = std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, OutboxEvent::Resolved { .. }));
        assert!(
            !resolved,
            "an address with no identity and no path is not resolved"
        );
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
        let (mut engine, events, commands) = engine();

        // Register first: `create_message` needs our own delivery destination.
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        commands
            .send(Command::Send(Box::new(SendRequest {
                destination: [0x33; 16],
                title: b"t".to_vec(),
                body: b"a short status line".to_vec(),
                via: Via::Direct,
            })))
            .expect("queue the command");
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let seen: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
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

    /// A short status line must not pay for a link handshake; a body that
    /// cannot fit in one packet must not try.
    #[test]
    fn short_bodies_go_by_packet_and_long_ones_by_link() {
        assert_eq!(delivery_method(8), DeliveryMethod::Opportunistic);
        assert_eq!(delivery_method(64_000), DeliveryMethod::Direct);
    }
}
