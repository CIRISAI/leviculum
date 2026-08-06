//! The helper itself: an [`LxmfRouter`] driven from inside the driver's tick.
//!
//! # Why the whole helper is a `CoreProcessor`
//!
//! `leviculum-std`'s public event stream
//! ([`take_event_receiver`](leviculum_std::driver::ReticulumNode::take_event_receiver))
//! classifies before it delivers, and seven of the event types LXMF needs —
//! `PacketReceived` and `LinkDataReceived` among them, i.e. how a message
//! *arrives* — are `EventClass::Data` and droppable under load
//! (`leviculum-std/src/driver/processor.rs`, "Where the events come from").
//! An LXMF stack fed from there would silently lose inbound messages with
//! nothing underneath to retransmit them. The tap is therefore not an
//! optimisation here; it is the only correct feed.
//!
//! # The rule this file is written against
//!
//! Both hooks run **with the core mutex held, and that mutex is not
//! reentrant**. So this type owns:
//!
//! * no [`ReticulumNode`](leviculum_std::driver::ReticulumNode) and no handle
//!   derived from one — roughly forty of its `pub fn`s open with a lock on the
//!   very mutex we are inside, and one of them in a hook body is a deadlock in
//!   ordinary safe code;
//! * only channels whose sends cannot block: `std::sync::mpsc::Sender` (an
//!   unbounded queue) and `tokio::sync::mpsc::UnboundedSender` (a sync,
//!   non-blocking `send`).
//!
//! Every side effect the helper has — a line on stdout, a line on stderr, a
//! proof-of-work stamp, process shutdown — is therefore a queue push, and the
//! work happens on the far side. Nothing here does I/O.
//!
//! # Where the time goes
//!
//! [`PROCESSOR_TICK_BUDGET`](leviculum_std::driver::PROCESSOR_TICK_BUDGET) is
//! 5 ms per hook call. The costly LXMF operations are message packing and
//! signature verification, which
//! `docs/src/concepts/core-lock-budget.md` measures at 0.8 ms and 3.2 ms
//! respectively for a 1 MiB message — the page lists both as costs that do
//! *not* justify phasing. The one composed call that would blow the budget,
//! `NodeCore::send_resource` (141 ms for 1 MiB), is never reached: the router
//! drives resource sends through the three-phase form itself.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Instant;

use leviculum_core::identity::Identity;
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::{DestinationHash, Storage as _};
use leviculum_lxmf::router::{LxmfRouter, RouterConfig, RouterEvent, RouterOutput};
use leviculum_lxmf::{
    announce, DeliveryMethod, DeliveryStampRequest, LxmfNode, LxmfNodeConfig, Verification,
};
use leviculum_std::driver::{CoreProcessor, StdNodeCore};

use crate::protocol::{self, b64_encode, hex_encode, Command};

/// How soon the helper asks the driver to come back.
///
/// Two things need a periodic slot: the command queue (an event tap can never
/// *initiate* anything, because it only fires when the core has something to
/// say) and a pending `wait_for_peer`. 200 ms is Python's own poll interval
/// for the latter (`periculum/assets/scripts/lxmf_node.py:150`, `time.sleep(0.2)`), and it bounds
/// command pickup latency at a fifth of what the driver's 1 s idle cadence
/// would.
///
/// Deliberately a fixed future instant, never a stale one: a deadline already
/// in the past pins the driver to its 1 ms floor, i.e. a thousand lock
/// acquisitions a second.
const POLL_INTERVAL_MS: u64 = 200;

/// How many times one hook call will re-feed router output into the router.
///
/// A core call made *inside* a hook can return events synchronously, and the
/// driver never hands those back — it dispatches a processor's `TickOutput`
/// with the processor detached, so the tap would not see them
/// (`driver::processor::run_event_tap`, "The recursion bound is one"). The
/// consumer therefore has to close that loop itself, and this bounds it: an
/// LXMF event that provokes an LXMF event is legitimate, so the fixpoint is
/// not something the router can promise, and an unbounded loop under the core
/// lock is a node hang rather than a bug report.
const MAX_ABSORB_ROUNDS: usize = 8;

/// One line the helper wants written, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// A structured `EVENT …` line for stdout. The driver parses these.
    Event(String),
    /// Human-readable diagnostics for stderr. `periculum/assets/scripts/lxmf_node.py:60-61` puts the
    /// same category there, and the driver tees it to a `.stderr.log`.
    Log(String),
}

/// Non-blocking, cloneable line emitter.
///
/// Every clone shares one epoch so the `t=` values across two threads are
/// comparable. Python's are `time.monotonic()`, i.e. system-relative rather
/// than process-relative; nothing reads them across processes (the driver
/// timestamps arrivals itself, `periculum/src/lxmf.rs`, `EventLine::recv_at`),
/// so process-relative is both sufficient and easier to read in a log.
#[derive(Debug, Clone)]
pub struct Emitter {
    lines: Sender<Out>,
    epoch: Instant,
}

impl Emitter {
    pub fn new(lines: Sender<Out>, epoch: Instant) -> Self {
        Self { lines, epoch }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Emit one `EVENT` line. Dropped if the writer thread is gone, which
    /// only happens after shutdown has already been decided.
    pub fn event(&self, name: &str, fields: &[(&str, String)]) {
        let line = protocol::format_event(name, fields, self.now_ms());
        let _ = self.lines.send(Out::Event(line));
    }

    /// Emit `EVENT lxmf_error detail=…`, the disposition Python gives any
    /// exception out of `handle_command` (`periculum/assets/scripts/lxmf_node.py:114-117`). It fails
    /// the step and names the reason.
    pub fn error(&self, message: &str) {
        self.event("lxmf_error", &[("detail", protocol::detail(message))]);
    }

    /// Emit a stderr diagnostic.
    pub fn log(&self, message: impl Into<String>) {
        let _ = self.lines.send(Out::Log(message.into()));
    }
}

/// Detached proof-of-work, to be done off the core lock.
///
/// `DeliveryStampRequest::generate_with` is `async` and mines until it hits
/// the target cost — the two properties a hook body may least afford. The
/// request goes out here and the answer comes back as
/// [`Input::StampReady`].
#[derive(Debug, Clone, Copy)]
pub enum StampJob {
    Delivery(DeliveryStampRequest),
}

/// Everything that reaches the processor from outside the driver.
#[derive(Debug, Clone)]
pub enum Input {
    /// One raw line from stdin. Parsed in the hook so a malformed command is
    /// reported through the same `lxmf_error` path as a failing one.
    Line(String),
    /// stdin closed. Python falls out of its `for raw in sys.stdin` loop and
    /// shuts down without emitting anything (`periculum/assets/scripts/lxmf_node.py:108-121`).
    Eof,
    /// A stamp the executor finished mining.
    StampReady {
        request: DeliveryStampRequest,
        stamp: [u8; 32],
    },
    /// The executor gave up on a stamp.
    StampFailed {
        request: DeliveryStampRequest,
        detail: String,
    },
}

/// Why the helper is stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// The driver sent `quit`.
    Quit,
    /// stdin closed under us.
    Eof,
}

/// Static configuration, fixed before the node exists.
#[derive(Debug, Clone)]
pub struct HelperConfig {
    /// `argv[1]`, carried in the delivery announce. Default `lxmf-test`
    /// (`periculum/assets/scripts/lxmf_node.py:65`).
    pub display_name: Vec<u8>,
}

/// A `wait_for_peer` the helper has not answered yet.
struct PendingWait {
    peer: [u8; 16],
    deadline_ms: u64,
    /// Python requests the path at most once per wait (`periculum/assets/scripts/lxmf_node.py:147-149`).
    path_requested: bool,
}

/// The router, once it exists.
struct Ready {
    router: LxmfRouter,
    delivery_hash: [u8; 16],
}

enum State {
    /// The identity is minted but nothing is registered: registering needs
    /// `&mut StdNodeCore`, and a processor is installed on the *builder*,
    /// before the node it will run inside exists.
    Unregistered(Box<Identity>),
    Ready(Box<Ready>),
    /// Registration failed and was reported. `lxmf_ready` never comes, so the
    /// driver's `lxmf_start` step fails on the timeout with the reason already
    /// in the log.
    Failed,
}

/// The LXMF helper, as the driver sees it.
pub struct LxmfHelperProcessor {
    config: HelperConfig,
    emitter: Emitter,
    inputs: Receiver<Input>,
    stamps: tokio::sync::mpsc::UnboundedSender<StampJob>,
    shutdown: tokio::sync::mpsc::UnboundedSender<Shutdown>,
    state: State,
    waits: Vec<PendingWait>,
}

impl LxmfHelperProcessor {
    /// Build the processor. Runs before the node does, and touches nothing
    /// that could reach it.
    pub fn new(
        config: HelperConfig,
        emitter: Emitter,
        inputs: Receiver<Input>,
        stamps: tokio::sync::mpsc::UnboundedSender<StampJob>,
        shutdown: tokio::sync::mpsc::UnboundedSender<Shutdown>,
    ) -> Self {
        Self {
            config,
            emitter,
            inputs,
            stamps,
            shutdown,
            // A fresh identity per start, like Python's `RNS.Identity()`
            // (`periculum/assets/scripts/lxmf_node.py:75`). The helper is a test peer; persisting one
            // would make consecutive runs of a scenario share a destination.
            state: State::Unregistered(Box::new(Identity::generate(&mut rand_core::OsRng))),
            waits: Vec::new(),
        }
    }

    /// Register the delivery destination on first use and emit `lxmf_ready`.
    ///
    /// Called from both hooks: the driver's timer branch normally fires first,
    /// but nothing in the seam promises that, and an event handled before the
    /// router exists would be an inbound message lost at startup.
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
                self.emitter
                    .event("lxmf_ready", &[("hash", hex_encode(&ready.delivery_hash))]);
                State::Ready(Box::new(ready))
            }
            Err(detail) => {
                self.emitter.error(&detail);
                State::Failed
            }
        };
    }

    /// Register if needed, then move the router out of `self` for the duration
    /// of the hook.
    ///
    /// The move is what lets the rest of the hook borrow the router and the
    /// emitter at once. It goes back before the hook returns; a panic in
    /// between leaves the slot `Failed`, which costs nothing — the driver
    /// detaches a panicking processor permanently anyway.
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
    /// and re-feed the core events it produced.
    ///
    /// See [`MAX_ABSORB_ROUNDS`] for why the re-feed is the consumer's job.
    fn absorb(
        &self,
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
            // Actions and the deadline go to the driver; it dispatches them on
            // its own send path, which is the whole point of the seam.
            out.merge(core_output);

            rounds += 1;
            let refeed = rounds <= MAX_ABSORB_ROUNDS;
            if !refeed && !events.is_empty() {
                self.emitter.log(format!(
                    "[lxmf-node] absorb bound of {MAX_ABSORB_ROUNDS} rounds reached; \
                     {} event(s) forwarded to the application without re-entering the router",
                    events.len()
                ));
            }
            for event in events {
                if refeed {
                    match ready.router.handle_event(core, &event) {
                        Ok(next) => queue.push_back(next),
                        Err(e) => self.emitter.error(&format!("router handle_event: {e:?}")),
                    }
                }
                out.events.push(event);
            }
        }
    }

    /// Turn one router event into the helper's externally visible behaviour.
    fn report(&self, event: RouterEvent) {
        match event {
            RouterEvent::MessageReceived(message) => {
                // Python base64s `content_as_string().encode("utf-8")`
                // (`periculum/assets/scripts/lxmf_node.py:85-90`) and emits an empty body when the
                // content is not UTF-8. The raw bytes are used here instead:
                // they are what arrived, and the driver compares against
                // `B64.encode(body.as_bytes())` of a scenario's `body`
                // string, so the two agree on everything a scenario can send.
                self.emitter.event(
                    "lxmf_msg_received",
                    &[
                        ("src", hex_encode(&message.source_hash)),
                        ("body_b64", b64_encode(&message.content)),
                        (
                            "sig_valid",
                            (message.verification == Verification::Valid).to_string(),
                        ),
                        (
                            "transport_encryption",
                            transport_encryption(message.method).to_string(),
                        ),
                    ],
                );
            }
            RouterEvent::StampPending(request) => {
                // Off the lock: mining is unbounded work and the generator is
                // async. The result comes back as `Input::StampReady`.
                let _ = self.stamps.send(StampJob::Delivery(request));
            }
            // The rest are diagnostics. Python's helper reports nothing for
            // any of them, and adding events the driver does not know would
            // be a second protocol rather than the same one — a delivery that
            // never completes is meant to surface as the peer's
            // `lxmf_assert_received` timing out, not as a new event name.
            other => self
                .emitter
                .log(format!("[lxmf-node] router event: {other:?}")),
        }
    }

    /// Drain the input queue. Non-blocking by construction.
    fn pump_inputs(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        loop {
            match self.inputs.try_recv() {
                Ok(Input::Line(line)) => match protocol::parse_command(&line) {
                    Ok(None) => {}
                    Ok(Some(command)) => self.run(ready, core, command, out),
                    Err(e) => self.emitter.error(&e.0),
                },
                Ok(Input::Eof) => {
                    let _ = self.shutdown.send(Shutdown::Eof);
                }
                Ok(Input::StampReady { request, stamp }) => {
                    match ready
                        .router
                        .set_outbound_stamp_result(core, &request, stamp.to_vec())
                    {
                        Ok(output) => self.absorb(ready, core, output, out),
                        Err(e) => self.emitter.error(&format!("stamp result rejected: {e:?}")),
                    }
                }
                Ok(Input::StampFailed { request, detail }) => self.emitter.error(&format!(
                    "stamp generation failed for {}: {detail}",
                    hex_encode(&request.message_id)
                )),
                Err(TryRecvError::Empty) => return,
                // The sender is gone, which can only follow a shutdown that
                // has already been signalled.
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn run(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        command: Command,
        out: &mut TickOutput,
    ) {
        match command {
            Command::Announce => self.announce(ready, core, out),
            Command::WaitForPeer { peer, timeout_secs } => {
                let deadline_ms = core.now_ms().saturating_add((timeout_secs * 1000.0) as u64);
                self.waits.push(PendingWait {
                    peer,
                    deadline_ms,
                    path_requested: false,
                });
            }
            Command::Send {
                peer,
                body,
                body_b64,
            } => self.send(ready, core, peer, body, body_b64, out),
            Command::Quit => {
                self.emitter.event("lxmf_shutdown", &[]);
                let _ = self.shutdown.send(Shutdown::Quit);
            }
        }
    }

    fn announce(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        // Three-element LXMF delivery app data: display name, stamp cost,
        // compression support. `None` is the cost Python's helper advertises —
        // it registers with `stamp_cost=0`, which the reference maps to "no
        // cost" (`announce::DeliveryAnnounce::new`).
        let app_data = announce::delivery(Some(&self.config.display_name), None);
        let hash = DestinationHash::new(ready.delivery_hash);
        match core.announce_destination(&hash, Some(&app_data)) {
            Ok(core_output) => {
                self.absorb(
                    ready,
                    core,
                    RouterOutput {
                        core: core_output,
                        events: Vec::new(),
                    },
                    out,
                );
                self.emitter.event(
                    "lxmf_announce_sent",
                    &[("hash", hex_encode(&ready.delivery_hash))],
                );
            }
            Err(e) => self.emitter.error(&format!("announce failed: {e:?}")),
        }
    }

    fn send(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        peer: [u8; 16],
        body: Vec<u8>,
        body_b64: String,
        out: &mut TickOutput,
    ) {
        // Same precondition and same wording as Python (`periculum/assets/scripts/lxmf_node.py:162-164`):
        // without the peer's identity there is nothing to encrypt to, and the
        // step should say which call was skipped rather than time out later.
        if core.storage().get_identity(&peer).is_none() {
            self.emitter.error(&format!(
                "identity for {} not known; call wait_for_peer first",
                hex_encode(&peer)
            ));
            return;
        }
        // `create_message` stamps the timestamp from the node's own emission
        // clock (Codeberg #182) — the helper has no clock of its own to get
        // wrong. Title `test` and `Direct` mirror Python's LXMessage
        // (`periculum/assets/scripts/lxmf_node.py:172-179`, `desired_method=DIRECT`).
        let message = match ready.router.create_message(
            core,
            peer,
            b"test".to_vec(),
            body,
            Vec::new(),
            DeliveryMethod::Direct,
        ) {
            Ok(message) => message,
            Err(e) => {
                self.emitter
                    .error(&format!("could not create message: {e:?}"));
                return;
            }
        };
        match ready.router.enqueue(core, message) {
            Ok(output) => {
                self.absorb(ready, core, output, out);
                self.emitter.event(
                    "lxmf_msg_sent",
                    &[("dst", hex_encode(&peer)), ("body_b64", body_b64)],
                );
            }
            Err(e) => self
                .emitter
                .error(&format!("could not enqueue message: {e:?}")),
        }
    }

    /// Answer every `wait_for_peer` that can be answered now.
    ///
    /// The predicate is Python's, term for term (`periculum/assets/scripts/lxmf_node.py:141-150`): the
    /// peer counts as known when its identity has been recalled *and* a path
    /// exists, and a path is requested at most once, only once the identity is
    /// known. The deadline is tested first because Python's `while
    /// monotonic() < deadline` tests it before each probe, so a wait whose
    /// window has closed reports `timeout` without one last look.
    fn poll_waits(&mut self, ready: &mut Ready, core: &mut StdNodeCore, out: &mut TickOutput) {
        if self.waits.is_empty() {
            return;
        }
        let now_ms = core.now_ms();
        let mut pending = Vec::with_capacity(self.waits.len());
        for mut wait in std::mem::take(&mut self.waits) {
            let peer = DestinationHash::new(wait.peer);
            if now_ms >= wait.deadline_ms {
                self.emitter.event(
                    "lxmf_wait_for_peer_timeout",
                    &[("peer", hex_encode(&wait.peer))],
                );
                continue;
            }
            let identity_known = core.storage().get_identity(&wait.peer).is_some();
            if identity_known && core.has_path(&peer) {
                self.emitter
                    .event("lxmf_wait_for_peer_ok", &[("peer", hex_encode(&wait.peer))]);
                continue;
            }
            if identity_known && !wait.path_requested {
                // Through `absorb` rather than merged straight into `out`:
                // `request_path` ends in `process_events_and_actions`, so its
                // output can carry events, and every other core call made from
                // a hook here has them re-fed to the router for the reason
                // `MAX_ABSORB_ROUNDS` gives. Exempting this one call would be
                // an exemption nobody could see from the call site.
                let output = core.request_path(&peer);
                self.absorb(
                    ready,
                    core,
                    RouterOutput {
                        core: output,
                        events: Vec::new(),
                    },
                    out,
                );
                wait.path_requested = true;
            }
            pending.push(wait);
        }
        self.waits = pending;
    }
}

/// What Python reports in `transport_encryption` for a *received* message.
///
/// `reference/LXMF/LXMF/LXMRouter.py:1888-1900` sets it from the destination type the message
/// arrived on, not from the LXMF delivery method: `SINGLE` and `LINK` both
/// give `ENCRYPTION_DESCRIPTION_EC` = `"Curve25519"`
/// (`reference/LXMF/LXMF/LXMessage.py:98-100`). Opportunistic delivery is a packet to a SINGLE
/// destination and direct delivery runs over a LINK, so both map to the same
/// string. A paper message is carried out of band and is the only unencrypted
/// case; it cannot arrive on this path, and is spelled out so the match is
/// total rather than defaulted.
fn transport_encryption(method: DeliveryMethod) -> &'static str {
    match method {
        DeliveryMethod::Opportunistic | DeliveryMethod::Direct | DeliveryMethod::Propagated => {
            "Curve25519"
        }
        DeliveryMethod::Paper => "Unencrypted",
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
        .map_err(|e| format!("delivery identity has no private key: {e:?}"))?;
    let copy = Identity::from_private_key_bytes(&bytes)
        .map_err(|e| format!("could not copy the delivery identity: {e:?}"))?;
    let destination =
        LxmfNode::delivery_destination(copy).map_err(|e| format!("delivery destination: {e:?}"))?;
    let delivery_hash = *destination.hash().as_bytes();
    let node = LxmfNode::register(core, destination, LxmfNodeConfig::default())
        .map_err(|e| format!("register delivery destination: {e:?}"))?;
    Ok(Ready {
        router: LxmfRouter::new(node, identity_hash, RouterConfig::default()),
        delivery_hash,
    })
}

impl CoreProcessor for LxmfHelperProcessor {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        match ready.router.handle_event(core, event) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.emitter.error(&format!("router handle_event: {e:?}")),
        }
        // An announce arriving is exactly what a pending `wait_for_peer` is
        // waiting for. Answering it here rather than on the next timer tick
        // puts the helper's latency at the peer's announce instead of
        // `POLL_INTERVAL_MS` after it.
        self.poll_waits(&mut ready, core, &mut out);
        self.state = State::Ready(ready);
        out
    }

    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };

        self.pump_inputs(&mut ready, core, &mut out);
        self.poll_waits(&mut ready, core, &mut out);
        match ready.router.tick(core) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.emitter.error(&format!("router tick: {e:?}")),
        }

        self.state = State::Ready(ready);
        // Always a fresh future instant. The helper always has something to
        // wake for — at minimum the command queue, which nothing else pokes.
        let poll = now_ms.saturating_add(POLL_INTERVAL_MS);
        out.next_deadline_ms = Some(match out.next_deadline_ms {
            Some(existing) => existing.min(poll),
            None => poll,
        });
        out
    }
}
