//! The propagation-node engine: a [`leviculum_std::driver::CoreProcessor`] driving
//! [`PropagationNode`] from inside the driver's tick.
//!
//! # Why a `CoreProcessor` and not the public event receiver
//!
//! A client upload arrives as a raw link packet, surfaced as
//! `LinkDataReceived` — an `EventClass::Data` event the public receiver may
//! drop under load (`leviculum-std/src/driver/processor.rs`, "Where the
//! events come from"). A mailbox that silently loses uploads under load
//! would still have proven them if the proof were automatic; instead the
//! propagation destination runs `ProofStrategy::App`, the proof is sent
//! only after the store append returned, and both the data event and the
//! `LinkProofRequested` it must answer ride the tap. This is the same
//! reasoning that put lnmsg's engine on the tap
//! (`docs/src/concepts/lnmsg-architecture.md` §3).
//!
//! # Event order the proof correlation relies on
//!
//! For one inbound link packet the core emits `LinkProofRequested` first
//! and `LinkDataReceived` second, from the same
//! `handle_plain_data_packet` call
//! (`leviculum-core/src/node/link_management.rs:1562-1590`). The engine
//! therefore queues the proof hash per link and pops it when the matching
//! data event is processed; per-link FIFO order is the emission order.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use leviculum_core::node::NodeEvent;
use leviculum_core::resource::ResourceStrategy;
use leviculum_core::transport::TickOutput;
use leviculum_core::{
    Destination, DestinationHash, DestinationType, Direction, Identity, LinkId, ProofStrategy,
    RequestError, RequestPolicy,
};
use leviculum_lxmf::constants::STAMP_SIZE;
use leviculum_lxmf::control::{
    encode_control_ack, encode_control_error, ControlNodeStats, CONTROL_ASPECTS, STATS_GET_PATH,
    SYNC_REQUEST_PATH, UNPEER_REQUEST_PATH,
};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::peering::{PeerStore, PeeringConfig, OFFER_REQUEST_PATH};
use leviculum_lxmf::propagation::{MessageListResponse, PeerError};
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_lxmf::{
    Eviction, EvictionReason, FetchPlan, GetOutcome, Message, PropagationNode,
    PropagationNodeConfig, PropagationStore, TransientId, UploadOutcome, MESSAGE_GET_PATH,
};

use crate::mailbox::{MailboxConfig, MailboxRuntime};
use crate::peering::{InboundSync, InboundSyncStart, PeeringRuntime};
use crate::validation::{
    validate_stamp_value, Carrier, ValidationDone, ValidationJob, ValidationWorker,
};

/// The reference announces the propagation destination 20 s after the role
/// comes up (`NODE_ANNOUNCE_DELAY`, `reference/LXMF/LXMF/LXMRouter.py:41`,
/// applied in `announce_propagation_node` `:338-341`).
pub const ANNOUNCE_DELAY_SECS: u64 = 20;

/// Default re-announce cadence: `lxmd`'s default config announces the node
/// every 360 minutes (`announce_interval = 360`,
/// `reference/LXMF/LXMF/Utilities/lxmd.py:981`).
pub const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 360 * 60;

/// Store-maintenance cadence: the reference cleans its message store every
/// `JOB_STORE_INTERVAL × PROCESSING_INTERVAL` = 120 × 4 s
/// (`reference/LXMF/LXMF/LXMRouter.py:871-900`).
const STORE_MAINTENANCE_SECS: u64 = 480;

/// How soon the engine asks the driver back when nothing else is due; the
/// value lnmsg's engine settled on for the same job.
const POLL_INTERVAL_MS: u64 = 200;

/// How many messages one hook invocation may persist before it hands the
/// core back.
///
/// A [`CoreProcessor`](leviculum_std::driver::CoreProcessor) hook runs with
/// the core mutex held, under a 5 ms budget
/// (`PROCESSOR_TICK_BUDGET`, `leviculum-std/src/driver/processor.rs`), and
/// storing one message is one durable append: write, `fsync`, rename,
/// `fsync` the directory. Measured on the coder host's ext4 with the host
/// store, 288-byte bodies (`append_cost`,
/// `leviculum-std/src/file_propagation_store.rs`): **1.1-1.5 ms per
/// message, worst case 6.4 ms**, so the budget pays for one message and
/// not two. The whole inbound batch used to be stored in a single hook —
/// 105 messages, 239 ms of core lock, miauhaus 2026-09-21 — and that is
/// what this bounds.
///
/// One append can still overrun the budget on a slow device; nothing this
/// side of the lock can prevent that, because the latency is the disk's.
/// What this removes is the multiplier.
///
/// The arithmetic — this many appends at what an append costs, against the
/// budget — is asserted in `the_hook_budget_pays_for_the_slice_it_takes`,
/// against a price re-measured on the coder host (`PRICED_APPEND_COST`).
const PERSIST_PER_HOOK: usize = 1;

/// What the engine needs before the node exists (a processor is installed
/// on the builder, before the node it runs inside).
pub struct EngineConfig<S> {
    pub identity: Identity,
    pub node_config: PropagationNodeConfig,
    pub store: S,
    pub announce_interval_secs: u64,
    /// Delay before the first announce; [`ANNOUNCE_DELAY_SECS`] is the
    /// reference's behaviour and the production value, tests shorten it.
    pub announce_delay_secs: u64,
    /// Peering configuration (part 2); reference key names.
    pub peering: PeeringConfig,
    /// Where the peer table persists — a file on the host, the record log
    /// on the board (part 3).
    pub peer_store: Box<dyn PeerStore + Send>,
    /// Extra identities allowed on the control destination, from the
    /// config's `control_allowed` (`reference/LXMF/LXMF/Utilities/lxmd.py:219-222`);
    /// the node's own identity is always allowed
    /// (`reference/LXMF/LXMF/LXMRouter.py:672`).
    pub control_allowed: Vec<[u8; 16]>,
    /// `auth_required` with the `allowed` file's identity hashes: when
    /// `Some`, only these identities may drain mailboxes over `/get`
    /// (`identity_allowed`, `reference/LXMF/LXMF/LXMRouter.py:1472-1480`).
    pub auth_allowed: Option<Vec<[u8; 16]>>,
    /// The daemon's own mailbox (deliverable 2); `None` runs the
    /// propagation role alone (the helper's embedding, which has its own
    /// delivery router).
    pub mailbox: Option<MailboxConfig>,
    /// The message store's capacity in bytes, for the stats response
    /// (`messagestore.limit`); the store itself enforces it.
    pub store_limit_bytes: u64,
    /// `delivery_transfer_max_accepted_size` in kilobytes, reported in the
    /// stats' `delivery_limit`.
    pub delivery_limit_kb: u64,
}

enum State<S> {
    Unregistered(Box<EngineConfig<S>>),
    Ready(Box<Ready<S>>),
    Failed,
}

struct Ready<S> {
    node: PropagationNode<S>,
    destination_hash: DestinationHash,
    /// Whether stamp validation is armed, decided once from the config.
    min_cost: u8,
    /// The peering half: peer table, `/offer`, outbound sync.
    peering: PeeringRuntime,
    /// The control destination (`lxmf.propagation.control`) and its allow
    /// list (part 4, deliverable 1).
    control_hash: DestinationHash,
    control_allowed: Vec<[u8; 16]>,
    /// `/get` authentication: `Some` = only these identity hashes.
    auth_allowed: Option<Vec<[u8; 16]>>,
    /// The daemon's own mailbox (part 4, deliverable 2).
    mailbox: Option<MailboxRuntime>,
    /// Node-level counters for the stats response.
    started_unix: u64,
    client_received: u64,
    client_served: u64,
    store_limit_bytes: u64,
    delivery_limit_kb: u64,
}

/// One observable engine event, mirrored to the structured log; the channel
/// exists so `main` (and tests) can watch the role without scraping logs.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Ready {
        destination_hash: [u8; 16],
    },
    Broken {
        detail: String,
    },
    Announced,
    Accepted {
        transient_id: TransientId,
        duplicate: bool,
    },
    Rejected {
        detail: String,
    },
    Served {
        form: &'static str,
        count: usize,
    },
    Evicted {
        count: usize,
    },
    /// Peer-table change: `action` is add/drop/decline, `reason` names why.
    Peer {
        action: &'static str,
        destination_hash: [u8; 16],
        reason: &'static str,
    },
    /// One `/offer` round observed, either direction.
    Offer {
        dir: &'static str,
        peer: [u8; 16],
        offered: usize,
        wanted: usize,
    },
    /// One sync round ended, either direction.
    SyncDone {
        dir: &'static str,
        peer: [u8; 16],
        transferred: usize,
        bytes: u64,
        result: &'static str,
    },
    /// A peering key was mined (once per peer; it persists).
    KeyMined {
        peer: [u8; 16],
        value: u16,
    },
    /// The daemon's own mailbox registered (deliverable 2).
    MailboxReady {
        delivery_hash: [u8; 16],
    },
    /// The delivery destination's announce went out.
    MailboxAnnounced,
    /// A message arrived in the daemon's own mailbox. `main` writes it
    /// to the messages directory and runs the `on_inbound` hook — off
    /// the core lock, which is why this crosses the channel.
    Inbound {
        message: Box<Message>,
    },
    /// A remote-management request was answered (deliverable 1).
    ControlServed {
        path: &'static str,
        ok: bool,
    },
}

pub struct Engine<S> {
    state: State<S>,
    events: std::sync::mpsc::Sender<EngineEvent>,
    /// Proof hashes awaiting their data event, per link, in emission order.
    pending_proofs: HashMap<LinkId, VecDeque<[u8; 32]>>,
    next_announce_at: Option<u64>,
    announce_interval_secs: u64,
    announce_delay_secs: u64,
    next_maintenance_at: u64,
    /// Stamp validation off the core lock (deliverable 5): the hook
    /// queues, the worker grinds, the drain applies.
    validation: ValidationWorker,
    /// Validated payloads waiting to be stored, in arrival order.
    ///
    /// The store is the slow part and the hook holds the core lock while it
    /// runs, so a hook takes [`PERSIST_PER_HOOK`] messages off this queue
    /// and asks the driver to come straight back for the rest. FIFO, one
    /// queue for every link, so the per-link order of
    /// validate → append → prove the proof correlation relies on (module
    /// docs) is the order the payloads arrived in.
    pending: VecDeque<Pending>,
}

/// One validated payload waiting for the store.
enum Pending {
    /// A client upload that arrived as a link packet: one append, then the
    /// proof that says it is stored.
    Packet {
        link_id: LinkId,
        data: Vec<u8>,
        /// The queued proof hash for the packet, if one was requested.
        proof: Option<[u8; 32]>,
        verdicts: Verdicts,
    },
    /// A completed inbound resource, not yet classified: peer sync or
    /// single-message client upload is [`PeeringRuntime::begin_sync_resource`]'s
    /// decision, and it is made when the payload reaches the head of the
    /// queue.
    Resource {
        link_id: LinkId,
        data: Vec<u8>,
        /// The link's validated sync peer as of the moment the resource
        /// concluded; see [`ValidationJob::sync_peer`].
        sync_peer: Option<[u8; 16]>,
        verdicts: Verdicts,
    },
    /// A validated peer's batch, part-way through its messages.
    Sync {
        job: InboundSync,
        verdicts: Verdicts,
    },
}

/// How a queued payload's stamps are judged when it reaches the store.
enum Verdicts {
    /// Ground by the validation worker before the payload was queued.
    Ground(HashMap<TransientId, Option<u16>>),
    /// Cheap enough to run here: [`ValidationWorker::worth_deferring`] said
    /// no, which means no cost gate and no value to measure, so this is a
    /// constant rather than a hash grind.
    Inline { min_cost: u8, compute_value: bool },
}

impl Verdicts {
    fn value(&self, transient_id: &TransientId, stamp: &[u8; STAMP_SIZE]) -> Option<u16> {
        match self {
            Verdicts::Ground(verdicts) => verdicts.get(transient_id).copied().flatten(),
            Verdicts::Inline {
                min_cost,
                compute_value,
            } => validate_stamp_value(transient_id, stamp, *min_cost, *compute_value),
        }
    }
}

impl Pending {
    /// A finished validation, ready for the store.
    fn from_validated(done: ValidationDone) -> Self {
        let ValidationDone {
            link_id,
            carrier,
            data,
            proof,
            sync_peer,
            verdicts,
        } = done;
        let verdicts = Verdicts::Ground(verdicts);
        match carrier {
            Carrier::Packet => Pending::Packet {
                link_id,
                data,
                proof,
                verdicts,
            },
            Carrier::Resource => Pending::Resource {
                link_id,
                data,
                sync_peer,
                verdicts,
            },
        }
    }
}

/// Unix seconds, the timestamp domain of the store and the announce.
fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// One rejected upload, by reason and carrier.
///
/// `reason` is a fixed word rather than a message: the whole point of the
/// event is that an analysis can count rejections per reason without
/// parsing prose, and a free-text field is one refactor away from breaking
/// that. The prose stays on the `EngineEvent::Rejected` the operator sees.
pub(crate) fn log_reject(reason: &'static str, via: &'static str) {
    tracing::debug!(event = "PN_REJECT", reason = reason, via = via);
}

fn short_hex(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn full_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl<S: PropagationStore> Engine<S> {
    pub fn new(config: EngineConfig<S>) -> (Self, std::sync::mpsc::Receiver<EngineEvent>) {
        let (events, receiver) = std::sync::mpsc::channel();
        let announce_interval_secs = config.announce_interval_secs;
        let announce_delay_secs = config.announce_delay_secs;
        (
            Self {
                state: State::Unregistered(Box::new(config)),
                events,
                pending_proofs: HashMap::new(),
                next_announce_at: None,
                announce_interval_secs,
                announce_delay_secs,
                next_maintenance_at: 0,
                validation: ValidationWorker::spawn(),
                pending: VecDeque::new(),
            },
            receiver,
        )
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    /// The propagation destination hash, once registered.
    pub fn destination_hash(&self) -> Option<[u8; 16]> {
        match &self.state {
            State::Ready(ready) => Some(*ready.destination_hash.as_bytes()),
            _ => None,
        }
    }

    /// Number of stored messages, once registered. For operator surfaces and
    /// the periculum helper's store probe.
    pub fn store_count(&self) -> Option<usize> {
        match &self.state {
            State::Ready(ready) => ready.node.store().count().ok(),
            _ => None,
        }
    }

    /// Register the propagation destination and the `/get` handler, once.
    ///
    /// The destination accepts links and proves link data via the
    /// application (`ProofStrategy::App`): the upload proof is the node's
    /// statement "stored", so it must not leave before the append returns
    /// ("persist before you prove",
    /// `docs/src/concepts/propagation-node-on-a-board.md` §3). The two
    /// request handlers on the reference's destination are `/offer` and
    /// `/get` (`reference/LXMF/LXMF/LXMRouter.py:669-670`); `/offer` is
    /// part 2, and an unregistered path is silently dropped by the stack,
    /// which a keyless would-be peer reads as a failed request.
    fn register_if_needed(&mut self, core: &mut StdNodeCoreRef<'_>) {
        let config = match std::mem::replace(&mut self.state, State::Failed) {
            State::Unregistered(config) => *config,
            other => {
                self.state = other;
                return;
            }
        };
        let EngineConfig {
            identity,
            node_config,
            store,
            announce_interval_secs: _,
            announce_delay_secs: _,
            peering,
            peer_store,
            control_allowed,
            auth_allowed,
            mailbox,
            store_limit_bytes,
            delivery_limit_kb,
        } = config;
        let identity_copy = identity.clone();
        let control_identity = identity.clone();
        let mut destination = match Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            APP_NAME,
            &[PROPAGATION_ASPECT],
        ) {
            Ok(destination) => destination,
            Err(error) => {
                self.emit(EngineEvent::Broken {
                    detail: format!("propagation destination: {error:?}"),
                });
                return;
            }
        };
        destination.set_accepts_links(true);
        destination.set_proof_strategy(ProofStrategy::App);
        let destination_hash = *destination.hash();
        core.register_destination(destination);
        core.register_request_handler(destination_hash, MESSAGE_GET_PATH, RequestPolicy::AllowAll);
        // The second handler on the reference's destination
        // (`reference/LXMF/LXMF/LXMRouter.py:669-670`): identity handling
        // is the handler's own (`offer_request` answers ERROR_NO_IDENTITY).
        core.register_request_handler(
            destination_hash,
            OFFER_REQUEST_PATH,
            RequestPolicy::AllowAll,
        );

        // The control destination, on the same identity, with its three
        // request handlers behind the allow list — the reference's exact
        // registration (`reference/LXMF/LXMF/LXMRouter.py:672-676`). The
        // node's own identity hash is always first in the list (`:672`).
        let mut allowed = Vec::with_capacity(control_allowed.len() + 1);
        allowed.push(*control_identity.hash());
        for hash in control_allowed {
            if !allowed.contains(&hash) {
                allowed.push(hash);
            }
        }
        let control_hash = match Destination::new(
            Some(control_identity),
            Direction::In,
            DestinationType::Single,
            APP_NAME,
            &CONTROL_ASPECTS,
        ) {
            Ok(mut control) => {
                control.set_accepts_links(true);
                let control_hash = *control.hash();
                core.register_destination(control);
                control_hash
            }
            Err(error) => {
                self.emit(EngineEvent::Broken {
                    detail: format!("control destination: {error:?}"),
                });
                return;
            }
        };
        Self::register_control_handlers(core, &control_hash);

        // The daemon's own mailbox (deliverable 2), on the same identity.
        let mailbox = match mailbox {
            Some(config) => {
                match MailboxRuntime::register(core, &identity_copy, config, self.events.clone()) {
                    Ok(runtime) => Some(runtime),
                    Err(detail) => {
                        self.emit(EngineEvent::Broken {
                            detail: format!("mailbox: {detail}"),
                        });
                        return;
                    }
                }
            }
            None => None,
        };

        let mut node = PropagationNode::new(store, node_config);
        let min_cost = node.min_accepted_cost();
        let peering = PeeringRuntime::new(peering, peer_store, identity_copy, self.events.clone());
        // Restored peers may already require true stamp values (§5).
        node.set_compute_stamp_value(peering.requires_stamp_values());
        self.emit(EngineEvent::Ready {
            destination_hash: *destination_hash.as_bytes(),
        });
        self.state = State::Ready(Box::new(Ready {
            node,
            destination_hash,
            min_cost,
            peering,
            control_hash,
            control_allowed: allowed,
            auth_allowed,
            mailbox,
            started_unix: unix_secs(),
            client_received: 0,
            client_served: 0,
            store_limit_bytes,
            delivery_limit_kb,
        }));
    }

    /// Register the three control request handlers
    /// (`reference/LXMF/LXMF/LXMRouter.py:674-676`).
    ///
    /// The core admits every request and `answer_control` decides — a
    /// deliberate deviation from the reference's `ALLOW_LIST`
    /// registration, and the only one here. `ALLOW_LIST` drops a
    /// disallowed request without a word
    /// (`reference/Reticulum/RNS/Link.py:867-874`), so the asker sits out
    /// its whole timeout and reports "timed out" — which claims *nobody
    /// answered* and sends the reader to the mesh, the instance and the
    /// interfaces, when the truth was *you are not allowed to ask*. On a
    /// node that was up, healthy and peering, that cost a field operator
    /// an afternoon.
    ///
    /// Answering costs no compatibility. The refusal we send is the
    /// reference's own `ERROR_NO_ACCESS`: its handlers already return it
    /// (`LXMRouter.py:840`, dead code behind its own `ALLOW_LIST`) and its
    /// client already decodes it, into exit code 204 (`lxmd.py:561-578`).
    /// A Python peer therefore hears a value it was written to understand,
    /// and hears it instead of silence. The permission itself is
    /// unchanged: `answer_control` checks the same list against the same
    /// remote identity hash the core would have checked
    /// (`link_management.rs`, `RequestPolicy::AllowList`), which is why
    /// this function no longer needs the list — and why the handler's
    /// `ERROR_NO_IDENTITY` branch (`LXMRouter.py:839`) becomes reachable
    /// too.
    fn register_control_handlers(core: &mut StdNodeCoreRef<'_>, control_hash: &DestinationHash) {
        for path in [STATS_GET_PATH, SYNC_REQUEST_PATH, UNPEER_REQUEST_PATH] {
            core.register_request_handler(*control_hash, path, RequestPolicy::AllowAll);
        }
    }

    /// Number of table peers, once registered — the periculum helper's
    /// peer probe.
    pub fn peer_count(&self) -> Option<usize> {
        match &self.state {
            State::Ready(ready) => Some(ready.peering.peer_count()),
            _ => None,
        }
    }

    /// Reset one peer's sync cursor for a bounded full re-offer (§5's
    /// reboot case, driven by the conformance cells).
    pub fn reoffer(&mut self, destination_hash: &[u8; 16]) -> bool {
        match &mut self.state {
            State::Ready(ready) => ready.peering.reoffer(destination_hash),
            _ => false,
        }
    }

    /// The table's peers, once registered.
    pub fn peer_hashes(&self) -> Vec<[u8; 16]> {
        match &self.state {
            State::Ready(ready) => ready.peering.peers(),
            _ => Vec::new(),
        }
    }

    /// Live store entries above one peer's cursor — this node's
    /// equivalent of the reference's per-peer unhandled count. `None`
    /// when unregistered or the peer is unknown.
    pub fn unhandled_toward(&self, destination_hash: &[u8; 16]) -> Option<usize> {
        match &self.state {
            State::Ready(ready) => ready
                .peering
                .unhandled_toward(&ready.node, destination_hash),
            _ => None,
        }
    }

    fn take_ready(&mut self, core: &mut StdNodeCoreRef<'_>) -> Option<Box<Ready<S>>> {
        self.register_if_needed(core);
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Ready(ready) => Some(ready),
            other => {
                self.state = other;
                None
            }
        }
    }

    fn owns_link(core: &StdNodeCoreRef<'_>, ready: &Ready<S>, link_id: &LinkId) -> bool {
        core.link(link_id)
            .is_some_and(|link| link.destination_hash() == &ready.destination_hash)
    }

    /// The delivery destination hash of the identified client: the mailbox
    /// selector, derived exactly as the reference derives it from the
    /// remote identity (`message_get_request`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1487`).
    fn delivery_hash_of(identity: &Identity) -> [u8; 16] {
        let name_hash = Destination::compute_name_hash(APP_NAME, &["delivery"]);
        *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
    }

    /// One upload envelope, from either carrier. Returns whether it was
    /// accepted (so the packet path can prove it).
    ///
    /// `validate` is the caller's verdict source: the drain path passes a
    /// lookup into the worker's finished verdicts, the inline path (cost
    /// 0, no value computation — no work to speak of) the direct
    /// validator. See `crate::validation` for why the grinding never
    /// happens here.
    #[allow(clippy::too_many_arguments)]
    fn ingest(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        data: &[u8],
        via: &'static str,
        validate: &mut dyn FnMut(&TransientId, &[u8; STAMP_SIZE]) -> Option<u16>,
        out: &mut TickOutput,
    ) -> bool {
        let outcome = ready.node.handle_upload(data, unix_secs(), &mut *validate);
        match outcome {
            UploadOutcome::Accepted {
                transient_id,
                destination_hash,
                size,
                stamp_value,
                duplicate,
                evicted,
            } => {
                self.log_evictions(&evicted);
                tracing::debug!(
                    event = "PN_ACCEPT",
                    tid = short_hex(&transient_id),
                    dst = full_hex(&destination_hash),
                    bytes = size,
                    value = stamp_value,
                    dup = duplicate,
                    via = via,
                );
                self.emit(EngineEvent::Accepted {
                    transient_id,
                    duplicate,
                });
                if !duplicate {
                    // Directly from a client, the stats' clients bucket
                    // (`client_propagation_messages_received`,
                    // `reference/LXMF/LXMF/LXMRouter.py:2250`).
                    ready.client_received += 1;
                    self.deliver_own_mailbox(ready, core, &transient_id, &destination_hash, out);
                }
                true
            }
            UploadOutcome::InvalidStamp { reject } => {
                // The reference packs [ERROR_INVALID_STAMP] into a raw link
                // packet and tears the link down
                // (reference/LXMF/LXMF/LXMRouter.py:2257-2260).
                if let Ok((_, send)) = core.send_packet_on_link(link_id, &reject) {
                    out.merge(send);
                }
                out.merge(core.close_link(link_id));
                log_reject("invalid_stamp", via);
                self.emit(EngineEvent::Rejected {
                    detail: "invalid stamp".into(),
                });
                false
            }
            UploadOutcome::PeerSyncForm => {
                // Multi-message without a peering key: not supposed to
                // happen, the reference tears the link down
                // (reference/LXMF/LXMF/LXMRouter.py:2382-2385).
                out.merge(core.close_link(link_id));
                log_reject("peer_sync_form", via);
                self.emit(EngineEvent::Rejected {
                    detail: "peer sync form without peering support".into(),
                });
                false
            }
            UploadOutcome::Malformed(error) => {
                // The reference logs and ignores
                // (reference/LXMF/LXMF/LXMRouter.py:2262-2264).
                tracing::debug!("lnpnd: undecodable upload ignored: {error}");
                log_reject("malformed", via);
                false
            }
            UploadOutcome::StoreFailed(error) => {
                // No proof leaves: the client keeps its retry, which is the
                // honest outcome for a store that cannot hold the message.
                log_reject("store_failed", via);
                self.emit(EngineEvent::Rejected {
                    detail: format!("store failed: {error}"),
                });
                false
            }
        }
    }

    fn log_evictions(&self, evicted: &[Eviction]) {
        for eviction in evicted {
            tracing::debug!(
                event = "PN_EVICT",
                tid = short_hex(&eviction.transient_id),
                bytes = eviction.size,
                age_s = eviction.age_secs,
                reason = match eviction.reason {
                    EvictionReason::Expired => "expired",
                    EvictionReason::Displaced => "displaced",
                },
            );
        }
        if !evicted.is_empty() {
            self.emit(EngineEvent::Evicted {
                count: evicted.len(),
            });
        }
    }

    fn answer_get(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        data: &[u8],
        out: &mut TickOutput,
    ) {
        let Some(identity) = core.get_remote_identity(link_id).cloned() else {
            // Unidentified link: the reference answers ERROR_NO_IDENTITY
            // (reference/LXMF/LXMF/LXMRouter.py:1483).
            let response = MessageListResponse::Error(PeerError::NoIdentity)
                .encode()
                .unwrap_or_default();
            self.respond(core, link_id, request_id, &response, out);
            return;
        };
        // `auth_required`: only listed identities may drain
        // (`identity_allowed`, `reference/LXMF/LXMF/LXMRouter.py:1472-1480`,
        // applied at `:1484`).
        if let Some(allowed) = &ready.auth_allowed {
            if !allowed.contains(identity.hash()) {
                let response = MessageListResponse::Error(PeerError::NoAccess)
                    .encode()
                    .unwrap_or_default();
                self.respond(core, link_id, request_id, &response, out);
                return;
            }
        }
        let mailbox = Self::delivery_hash_of(&identity);
        match ready.node.handle_get(data, &mailbox, unix_secs()) {
            Ok(GetOutcome::List { response, count }) => {
                tracing::debug!(
                    event = "PN_GET",
                    dst = full_hex(&mailbox),
                    form = "list",
                    count = count,
                    bytes = 0u64,
                    purged = 0usize,
                );
                self.emit(EngineEvent::Served {
                    form: "list",
                    count,
                });
                self.respond(core, link_id, request_id, &response, out);
            }
            Ok(GetOutcome::Fetch { plan, purged }) => {
                tracing::debug!(
                    event = "PN_GET",
                    dst = full_hex(&mailbox),
                    form = "fetch",
                    count = plan.count(),
                    bytes = plan.served_bytes(),
                    purged = purged.len(),
                );
                self.emit(EngineEvent::Served {
                    form: "fetch",
                    count: plan.count(),
                });
                // The stats' clients bucket
                // (`client_propagation_messages_served`,
                // `reference/LXMF/LXMF/LXMRouter.py:1555`).
                ready.client_served += plan.count() as u64;
                self.respond_fetch(core, ready, link_id, request_id, &plan, out);
            }
            Err(error) => {
                // The reference answers a request it could not process with
                // None (reference/LXMF/LXMF/LXMRouter.py:1558-1560); on the
                // wire that is msgpack nil.
                tracing::debug!("lnpnd: /get failed: {error}");
                self.respond(core, link_id, request_id, &[0xC0], out);
            }
        }
    }

    /// Answer a fetch: one data packet when the response fits one, a
    /// STREAMED response resource when it does not (Codeberg #384 B2).
    ///
    /// The fork is on the length the plan already knows, not on a
    /// `PayloadTooLarge` the framing has to be built to discover: below
    /// the MDU the bytes are a few hundred and materialising them is the
    /// cheaper answer, above it the resource reads the records out of the
    /// store as the parts are cut and the response never exists as a
    /// buffer. `lnpnd` runs on hosts with heap to spare, but it runs the
    /// same code path the board does, which is how the board's path stays
    /// tested by every host run.
    fn respond_fetch(
        &self,
        core: &mut StdNodeCoreRef<'_>,
        ready: &Ready<S>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        plan: &FetchPlan,
        out: &mut TickOutput,
    ) {
        if core.response_fits_packet(link_id, plan.encoded_len()) {
            match ready.node.encode_fetch(plan) {
                Ok(response) => self.respond(core, link_id, request_id, &response, out),
                Err(error) => {
                    tracing::warn!("lnpnd: fetch response could not be built: {error}");
                    self.respond(core, link_id, request_id, &[0xC0], out);
                }
            }
            return;
        }
        let mut source = ready.node.fetch_source(plan);
        match core.send_response_resource_from_source(link_id, request_id, &mut source) {
            Ok((_, send)) => out.merge(send),
            Err(error) => tracing::warn!("lnpnd: streamed response resource failed: {error}"),
        }
    }

    fn respond(
        &self,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        response: &[u8],
        out: &mut TickOutput,
    ) {
        match core.send_response(link_id, request_id, response) {
            Ok(send) => out.merge(send),
            Err(RequestError::PayloadTooLarge) => {
                match core.send_response_resource(link_id, request_id, response) {
                    Ok((_, send)) => out.merge(send),
                    Err(error) => tracing::warn!("lnpnd: response resource failed: {error:?}"),
                }
            }
            Err(error) => tracing::warn!("lnpnd: response failed: {error:?}"),
        }
    }

    fn announce(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        out: &mut TickOutput,
    ) {
        let app_data = ready.node.announce_app_data(unix_secs());
        match core.announce_destination(&ready.destination_hash, Some(&app_data)) {
            Ok(send) => {
                out.merge(send);
                self.emit(EngineEvent::Announced);
            }
            Err(error) => tracing::warn!("lnpnd: announce failed: {error:?}"),
        }
        // The control destination announces alongside the node. The
        // reference announces it only when remotes are allowed
        // (`announce_propagation_node`, `reference/LXMF/LXMF/LXMRouter.py:342`)
        // because its local `--status` runs inside the router process and
        // needs no path; ours is always a separate shared-instance client,
        // so without this announce even the local operator's `lnpnd
        // --status` has nothing to resolve a path from. One extra announce
        // per cadence, wire-legal, and measured to be the difference
        // between the query answering and timing out (deviation rule).
        match core.announce_destination(&ready.control_hash, None) {
            Ok(send) => out.merge(send),
            Err(error) => tracing::warn!("lnpnd: control announce failed: {error:?}"),
        }
    }

    /// Allow one more identity on the control destination at runtime (the
    /// conformance helper's verb; the config file's `control_allowed` is
    /// applied at construction). Announces afterwards, so the remote can
    /// resolve the control destination. The handlers are re-registered
    /// because they used to carry the list; they no longer do
    /// (`register_control_handlers`), and the re-registration is kept only
    /// because it is idempotent and the verb is the natural place to
    /// notice if that ever changes back.
    pub fn allow_control(
        &mut self,
        core: &mut StdNodeCoreRef<'_>,
        identity_hash: [u8; 16],
        out: &mut TickOutput,
    ) -> bool {
        let Some(mut ready) = self.take_ready(core) else {
            return false;
        };
        if !ready.control_allowed.contains(&identity_hash) {
            ready.control_allowed.push(identity_hash);
        }
        Self::register_control_handlers(core, &ready.control_hash);
        self.announce(&mut ready, core, out);
        self.state = State::Ready(ready);
        true
    }

    /// A stored message addressed to our own mailbox is delivered locally
    /// instead of waiting for a client that will never come — the
    /// reference's own short-circuit (`lxmf_propagation`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2501-2509`, which delivers and
    /// never stores). Ours stores first (the accept path is shared with
    /// every other destination), then purges the entry it just proved:
    /// the proof said "stored", and delivered-to-the-operator is that
    /// promise kept, not broken.
    fn deliver_own_mailbox(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        transient_id: &TransientId,
        destination_hash: &[u8; 16],
        out: &mut TickOutput,
    ) {
        let Some(mailbox) = ready.mailbox.as_mut() else {
            return;
        };
        if destination_hash != &mailbox.delivery_hash {
            return;
        }
        let stamped = match ready.node.store().read_body(transient_id) {
            Ok(Some(body)) => body,
            _ => return,
        };
        if stamped.len() <= STAMP_SIZE {
            return;
        }
        let unstamped = &stamped[..stamped.len() - STAMP_SIZE];
        mailbox.deliver_propagated(core, unstamped, out);
        if let Err(error) = ready.node.store_mut().purge(transient_id) {
            tracing::warn!("lnpnd: own-mailbox purge failed: {error}");
        }
    }

    /// Answer one control request (`stats_get_request` /
    /// `peer_sync_request` / `peer_unpeer_request`,
    /// `reference/LXMF/LXMF/LXMRouter.py:838-865`). These identity checks
    /// are the reference's own, line for line — and here they are the
    /// enforcement rather than belt and braces, because the core admits
    /// the request so that a refusal can be spoken
    /// (`register_control_handlers`). The list they check is the one the
    /// core would have checked, compared against the same remote identity
    /// hash, so nothing is permitted that was permitted before.
    fn answer_control(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        path: &str,
        data: &[u8],
    ) -> Vec<u8> {
        let identity_hash = core.get_remote_identity(link_id).map(|id| *id.hash());
        let allowed = match identity_hash {
            None => {
                return encode_control_error(PeerError::NoIdentity);
            }
            Some(hash) => ready.control_allowed.contains(&hash),
        };
        if !allowed {
            return encode_control_error(PeerError::NoAccess);
        }
        match path {
            STATS_GET_PATH => {
                self.emit(EngineEvent::ControlServed {
                    path: STATS_GET_PATH,
                    ok: true,
                });
                self.compile_stats(ready, core).encode()
            }
            SYNC_REQUEST_PATH | UNPEER_REQUEST_PATH => {
                // The request data is one msgpack value: the reference
                // packs the raw destination hash, which umsgpack encodes
                // as a 16-byte bin (`peer_sync_request` checks the
                // unpacked bytes, `LXMRouter.py:847-848`).
                let peer_hash = {
                    let mut position = 0;
                    leviculum_lxmf::msgpack::read_bin(data, &mut position)
                        .ok()
                        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                };
                let Some(peer_hash) = peer_hash else {
                    return encode_control_error(PeerError::InvalidData);
                };
                let found = if path == SYNC_REQUEST_PATH {
                    ready.peering.trigger_sync(&peer_hash)
                } else {
                    ready.peering.unpeer(&peer_hash)
                };
                let served = if path == SYNC_REQUEST_PATH {
                    SYNC_REQUEST_PATH
                } else {
                    UNPEER_REQUEST_PATH
                };
                self.emit(EngineEvent::ControlServed {
                    path: served,
                    ok: found,
                });
                if found {
                    encode_control_ack()
                } else {
                    encode_control_error(PeerError::NotFound)
                }
            }
            _ => encode_control_error(PeerError::InvalidData),
        }
    }

    /// The stats response — `compile_stats`
    /// (`reference/LXMF/LXMF/LXMRouter.py:769-836`) over this engine's
    /// state. Byte and message counters count since process start; the
    /// reference persists some of them across restarts (`node_stats`,
    /// `:646-662`), a difference the numbers wear honestly rather than
    /// approximating.
    fn compile_stats(&self, ready: &Ready<S>, core: &mut StdNodeCoreRef<'_>) -> ControlNodeStats {
        let config = ready.node.config();
        let peering_config = ready.peering.config();
        let mut peers = ready.peering.control_peer_stats(core);
        for peer in &mut peers {
            peer.unhandled = ready
                .peering
                .unhandled_toward(&ready.node, &peer.peer_id)
                .unwrap_or(0) as u64;
        }
        let static_peers = ready.peering.static_peer_count() as u64;
        let total_peers = peers.len() as u64;
        let store_bytes = ready
            .store_limit_bytes
            .saturating_sub(ready.node.store().free_space());
        let (unpeered_incoming, unpeered_rx_bytes) = ready.peering.unpeered_incoming();
        ControlNodeStats {
            identity_hash: ready.peering.our_identity_hash(),
            destination_hash: *ready.destination_hash.as_bytes(),
            uptime_secs: unix_secs().saturating_sub(ready.started_unix) as f64,
            delivery_limit_kb: Some(ready.delivery_limit_kb),
            propagation_limit_kb: Some(config.transfer_limit_kb),
            sync_limit_kb: Some(config.sync_limit_kb),
            target_stamp_cost: config.stamp_cost as u64,
            stamp_cost_flexibility: config.stamp_cost_flexibility as u64,
            peering_cost: config.peering_cost as u64,
            max_peering_cost: peering_config.remote_peering_cost_max as u64,
            autopeer_maxdepth: Some(peering_config.autopeer_maxdepth as u64),
            from_static_only: peering_config.from_static_only,
            messagestore_count: ready.node.store().count().unwrap_or(0) as u64,
            messagestore_bytes: store_bytes,
            messagestore_limit_bytes: Some(ready.store_limit_bytes),
            client_propagation_messages_received: ready.client_received,
            client_propagation_messages_served: ready.client_served,
            unpeered_propagation_incoming: unpeered_incoming,
            unpeered_propagation_rx_bytes: unpeered_rx_bytes,
            static_peers,
            discovered_peers: total_peers.saturating_sub(static_peers),
            total_peers,
            max_peers: Some(peering_config.max_peers as u64),
            peers,
        }
    }

    /// Store one queued payload's worth of work: at most [`PERSIST_PER_HOOK`]
    /// messages, re-entering the path the event would have taken with the
    /// verdicts the payload was queued with.
    ///
    /// Returns how much of the budget it spent, in messages offered to the
    /// store. A classification that stores nothing — a torn-down batch, an
    /// envelope that turned out to be a client upload — costs nothing and
    /// lets the caller go on to the payload behind it.
    fn store_one(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        pending: Pending,
        budget: usize,
        out: &mut TickOutput,
    ) -> usize {
        match pending {
            Pending::Packet {
                link_id,
                data,
                proof,
                verdicts,
            } => {
                let mut validate = |transient_id: &TransientId, stamp: &[u8; STAMP_SIZE]| {
                    verdicts.value(transient_id, stamp)
                };
                let accepted =
                    self.ingest(ready, core, &link_id, &data, "packet", &mut validate, out);
                if accepted {
                    if let Some(packet_hash) = proof {
                        // Prove only now that the append returned: the proof
                        // is the "stored" statement (persist before you
                        // prove; packet.prove after storing,
                        // reference/LXMF/LXMF/LXMRouter.py:2255).
                        match core.send_data_proof(&link_id, &packet_hash) {
                            Ok(send) => out.merge(send),
                            Err(error) => tracing::warn!("lnpnd: proof failed: {error:?}"),
                        }
                    }
                }
                1
            }
            Pending::Resource {
                link_id,
                data,
                sync_peer,
                verdicts,
            } => match ready
                .peering
                .begin_sync_resource(core, &link_id, sync_peer, &data, out)
            {
                InboundSyncStart::Batch(job) => {
                    // Classification stores nothing; the batch goes back to
                    // the head of the queue and is stored a slice at a time.
                    self.pending.push_front(Pending::Sync { job, verdicts });
                    0
                }
                InboundSyncStart::ClientUpload => {
                    let mut validate = |transient_id: &TransientId, stamp: &[u8; STAMP_SIZE]| {
                        verdicts.value(transient_id, stamp)
                    };
                    let _ =
                        self.ingest(ready, core, &link_id, &data, "resource", &mut validate, out);
                    1
                }
                InboundSyncStart::Handled => 0,
            },
            Pending::Sync { mut job, verdicts } => {
                let before = job.remaining();
                let accepted = {
                    let mut validate = |transient_id: &TransientId, stamp: &[u8; STAMP_SIZE]| {
                        verdicts.value(transient_id, stamp)
                    };
                    ready.peering.advance_sync_resource(
                        core,
                        &mut ready.node,
                        &mut job,
                        budget,
                        &mut validate,
                        out,
                    )
                };
                let spent = before.saturating_sub(job.remaining());
                if !job.is_finished() {
                    self.pending.push_front(Pending::Sync { job, verdicts });
                }
                for (transient_id, destination_hash) in accepted {
                    self.deliver_own_mailbox(ready, core, &transient_id, &destination_hash, out);
                }
                spent
            }
        }
    }

    /// A completed inbound resource on one of the role's links: a
    /// validated sync peer's transfer is the peering half's at any
    /// message count (`PeeringRuntime::begin_sync_resource`), everything
    /// else is the client upload path — and the multi-message form from a
    /// sender without a validated key is torn down there
    /// (LXMRouter.py:2381-2389). The resource protocol has its own
    /// acknowledgement; no packet proof exists to send here.
    ///
    /// Nothing is stored in this call. The payload joins the store queue
    /// and is persisted from the drain, [`PERSIST_PER_HOOK`] messages per
    /// hook, because a peer's batch is as many durable appends as it has
    /// messages and they may not all run under one core lock.
    ///
    /// The validated sync peer is captured NOW, not at the drain: the
    /// reference peer tears its link down as soon as its transfer
    /// concludes, and the `LinkClosed` that follows clears the peering
    /// runtime's link state while the stamps may still be on the worker.
    fn on_inbound_resource_completed(
        &mut self,
        ready: &mut Ready<S>,
        link_id: &LinkId,
        data: &[u8],
    ) {
        let min_cost = ready.min_cost;
        let compute_value = ready.node.compute_stamp_value();
        let sync_peer = ready.peering.validated_peer(link_id);
        if ValidationWorker::worth_deferring(min_cost, compute_value) {
            self.validation.enqueue(ValidationJob {
                link_id: *link_id,
                carrier: Carrier::Resource,
                data: data.to_vec(),
                proof: None,
                sync_peer,
                min_cost,
                compute_value,
            });
        } else {
            self.pending.push_back(Pending::Resource {
                link_id: *link_id,
                data: data.to_vec(),
                sync_peer,
                verdicts: Verdicts::Inline {
                    min_cost,
                    compute_value,
                },
            });
        }
    }

    /// Take the worker's finished validations onto the store queue, in
    /// completion order (which is submission order — one worker), then
    /// store as much of the queue as this hook's budget allows.
    ///
    /// Queueing is cheap; storing is not, which is why only the second half
    /// is bounded. Whatever is left asks the driver to come straight back:
    /// the deadline is now, and the driver clamps that to its next
    /// millisecond (`leviculum-std/src/driver/mod.rs`, "Advance next_poll").
    fn drain_pending(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        out: &mut TickOutput,
    ) {
        while let Some(done) = self.validation.try_recv() {
            self.pending.push_back(Pending::from_validated(done));
        }
        let mut budget = PERSIST_PER_HOOK;
        while budget > 0 {
            let Some(pending) = self.pending.pop_front() else {
                break;
            };
            budget -= self
                .store_one(ready, core, pending, budget, out)
                .min(budget);
        }
        if !self.pending.is_empty() {
            let now_ms = core.now_ms();
            out.next_deadline_ms = Some(match out.next_deadline_ms {
                Some(existing) => existing.min(now_ms),
                None => now_ms,
            });
        }
    }
}

/// The concrete core type the driver hands its processors.
type StdNodeCoreRef<'a> = leviculum_std::driver::StdNodeCore;

impl<S: PropagationStore + Send + 'static> leviculum_std::driver::CoreProcessor for Engine<S> {
    fn on_event(
        &mut self,
        core: &mut leviculum_std::driver::StdNodeCore,
        event: &NodeEvent,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        // Finished validations first: they are older traffic than this
        // event, and applying them here keeps the per-link order.
        self.drain_pending(&mut ready, core, &mut out);
        // The mailbox router sees every event and filters for its own
        // links, exactly as lnmsg's engine feeds it.
        if let Some(mut mailbox) = ready.mailbox.take() {
            mailbox.on_event(core, event, &mut out);
            ready.mailbox = Some(mailbox);
        }
        match event {
            // A propagation announce: the peer table's input
            // (`LXMFPropagationAnnounceHandler`,
            // reference/LXMF/LXMF/Handlers.py:56-99).
            NodeEvent::AnnounceReceived { announce, .. }
                if announce.name_hash()
                    == &Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT])
                    && announce.destination_hash() != &ready.destination_hash =>
            {
                ready.peering.on_announce(
                    core,
                    *announce.destination_hash().as_bytes(),
                    announce.app_data(),
                    announce.is_path_response(),
                );
                ready
                    .node
                    .set_compute_stamp_value(ready.peering.requires_stamp_values());
            }
            // A client (or peer) opened a link to us: resources are
            // gated by the application, as the reference gates them
            // (`propagation_link_established` sets ACCEPT_APP,
            // reference/LXMF/LXMF/LXMRouter.py:2188-2193).
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator: false,
                destination_hash,
                ..
            } if destination_hash == &ready.destination_hash => {
                if let Err(error) = core.set_resource_strategy(link_id, ResourceStrategy::AcceptApp)
                {
                    tracing::warn!("lnpnd: resource strategy: {error:?}");
                }
            }
            // Our own sync link to a peer came up.
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator: true,
                ..
            } => {
                ready
                    .peering
                    .on_link_established(core, &ready.node, link_id, &mut out);
            }
            // The peer's answer to our /offer.
            NodeEvent::ResponseReceived {
                link_id,
                request_id,
                response_data,
                ..
            } => {
                ready.peering.on_response(
                    core,
                    &ready.node,
                    link_id,
                    request_id,
                    response_data,
                    &mut out,
                );
            }
            NodeEvent::RequestTimedOut {
                link_id,
                request_id,
            } => {
                ready
                    .peering
                    .on_request_timeout(core, link_id, request_id, &mut out);
            }
            // Emitted before the matching LinkDataReceived (module docs).
            NodeEvent::LinkProofRequested {
                link_id,
                packet_hash,
            } if Self::owns_link(core, &ready, link_id) => {
                self.pending_proofs
                    .entry(*link_id)
                    .or_default()
                    .push_back(*packet_hash);
            }
            NodeEvent::LinkDataReceived { link_id, data }
                if Self::owns_link(core, &ready, link_id) =>
            {
                let proof = self
                    .pending_proofs
                    .get_mut(link_id)
                    .and_then(VecDeque::pop_front);
                let min_cost = ready.min_cost;
                let compute_value = ready.node.compute_stamp_value();
                if ValidationWorker::worth_deferring(min_cost, compute_value) {
                    // The grinding goes to the worker (deliverable 5); the
                    // append and the proof follow at the drain, in order.
                    self.validation.enqueue(ValidationJob {
                        link_id: *link_id,
                        carrier: Carrier::Packet,
                        data: data.clone(),
                        proof,
                        sync_peer: None,
                        min_cost,
                        compute_value,
                    });
                } else {
                    // Nothing to grind, but the append and the proof are
                    // still the drain's: one dispatch can carry several
                    // uploads, and storing them all here is the same
                    // unbounded hold a batch is.
                    self.pending.push_back(Pending::Packet {
                        link_id: *link_id,
                        data: data.clone(),
                        proof,
                        verdicts: Verdicts::Inline {
                            min_cost,
                            compute_value,
                        },
                    });
                }
            }
            // An upload too big for one link packet arrives as a resource;
            // refuse before transfer when it exceeds the announced per-sync
            // limit (reference/LXMF/LXMF/LXMRouter.py:2220-2224).
            NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } if Self::owns_link(core, &ready, link_id) => {
                let accept = ready.node.accepts_resource_of(*data_size);
                let verdict = if accept {
                    ready.peering.on_inbound_resource_accepted(link_id);
                    core.accept_resource(link_id)
                } else {
                    core.reject_resource(link_id)
                };
                match verdict {
                    Ok(send) => out.merge(send),
                    Err(error) => tracing::warn!("lnpnd: resource verdict: {error:?}"),
                }
            }
            NodeEvent::ResourceCompleted {
                link_id,
                data,
                is_sender: false,
                ..
            } if Self::owns_link(core, &ready, link_id) => {
                self.on_inbound_resource_completed(&mut ready, link_id, data);
            }
            // Our outbound sync resource concluded (or failed).
            NodeEvent::ResourceCompleted {
                link_id,
                is_sender: true,
                segment_index,
                total_segments,
                ..
            } if segment_index == total_segments => {
                ready
                    .peering
                    .on_resource_sent(core, link_id, true, &mut out);
            }
            NodeEvent::ResourceFailed {
                link_id,
                is_sender: true,
                ..
            } => {
                ready
                    .peering
                    .on_resource_sent(core, link_id, false, &mut out);
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &ready.destination_hash && path == MESSAGE_GET_PATH => {
                self.answer_get(&mut ready, core, link_id, request_id, data, &mut out);
            }
            // Remote management on the control destination (part 4,
            // deliverable 1; `reference/LXMF/LXMF/LXMRouter.py:838-865`).
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &ready.control_hash => {
                let response = self.answer_control(&mut ready, core, link_id, path, data);
                self.respond(core, link_id, request_id, &response, &mut out);
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &ready.destination_hash && path == OFFER_REQUEST_PATH => {
                let response = ready
                    .peering
                    .answer_offer_request(core, &ready.node, link_id, data);
                self.respond(core, link_id, request_id, &response, &mut out);
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                self.pending_proofs.remove(link_id);
                ready.peering.on_link_closed(link_id);
            }
            _ => {}
        }
        self.state = State::Ready(ready);
        out
    }

    fn on_tick(
        &mut self,
        core: &mut leviculum_std::driver::StdNodeCore,
        now_ms: u64,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };

        let next_announce = *self
            .next_announce_at
            .get_or_insert(now_ms + self.announce_delay_secs * 1000);
        if now_ms >= next_announce {
            self.announce(&mut ready, core, &mut out);
            self.next_announce_at = Some(now_ms + self.announce_interval_secs * 1000);
        }

        if now_ms >= self.next_maintenance_at {
            let evicted = ready.node.tick(unix_secs());
            self.log_evictions(&evicted);
            // The store's own line: how full it is against its limit, and
            // -- because it fires on a fixed cadence whether or not any
            // traffic arrived -- the log's liveness heartbeat. A quiet node
            // and a dead one are otherwise the same absence of lines.
            let store = ready.node.store();
            tracing::debug!(
                event = "PN_STORE",
                used = store.capacity().saturating_sub(store.free_space()),
                limit = store.capacity(),
                count = store.count().unwrap_or(0),
            );
            self.next_maintenance_at = now_ms + STORE_MAINTENANCE_SECS * 1000;
        }

        self.drain_pending(&mut ready, core, &mut out);

        if let Some(mut mailbox) = ready.mailbox.take() {
            mailbox.on_tick(core, now_ms, &mut out);
            ready.mailbox = Some(mailbox);
        }

        ready
            .peering
            .on_tick(core, &mut ready.node, now_ms, &mut out);

        self.state = State::Ready(ready);

        let validation_poll = if self.validation.busy() {
            // Grinding in flight: come back quickly so the append and the
            // proof follow the worker with minimal added latency.
            now_ms + 20
        } else {
            now_ms + POLL_INTERVAL_MS
        };
        let due = [
            self.next_announce_at.unwrap_or(now_ms + POLL_INTERVAL_MS),
            self.next_maintenance_at,
            validation_poll,
        ]
        .into_iter()
        .filter(|deadline| *deadline > now_ms)
        .min()
        .unwrap_or(now_ms + POLL_INTERVAL_MS);
        out.next_deadline_ms = Some(match out.next_deadline_ms {
            Some(existing) => existing.min(due),
            None => due,
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_core::packet::{Packet, PacketType};
    use leviculum_lxmf::{MemoryPeerStore, MemoryPropagationStore};
    use leviculum_std::driver::{CoreProcessor as _, StdClock, StdStorage};

    fn core(dir: &std::path::Path) -> leviculum_std::driver::StdNodeCore {
        NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir).expect("storage under a fresh temp dir"),
        )
    }

    fn engine_with(
        control_allowed: Vec<[u8; 16]>,
        mailbox: Option<MailboxConfig>,
    ) -> (
        Engine<MemoryPropagationStore>,
        std::sync::mpsc::Receiver<EngineEvent>,
    ) {
        Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: PropagationNodeConfig::default(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            peering: leviculum_lxmf::PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed,
            auth_allowed: None,
            mailbox,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        })
    }

    fn mailbox_config() -> MailboxConfig {
        MailboxConfig {
            display_name: b"an-operator".to_vec(),
            stamp_cost: 7,
            announce_at_start: true,
            announce_interval_secs: None,
            delivery_limit_kb: 1000,
            ignored: Vec::new(),
        }
    }

    /// Make every `tracing` callsite in this binary dispatch-checked, once.
    ///
    /// `with_default` below redirects only the CALLING thread, but `tracing`
    /// caches one `Interest` per callsite for the whole PROCESS. A thread
    /// running under the default `NoSubscriber` that reaches a callsite
    /// first caches `Interest::never`, after which the event is dropped at
    /// the macro before any dispatcher is consulted — including the
    /// capturing thread's. Installing a global subscriber that enables
    /// DEBUG (to a sink; nothing is printed) makes every callsite cache
    /// "ask the current dispatcher", and `set_global_default` rebuilds the
    /// cache for callsites already registered.
    ///
    /// Without this, `a_peer_sync_and_a_client_upload_log_different_provenance`
    /// loses its `PN_ACCEPT` lines whenever another test ingests a message
    /// at the same moment — it failed 2 runs in 10 the moment this module
    /// gained three more tests that ingest.
    fn enable_log_dispatch() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(std::io::sink)
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    /// A `tracing` writer that keeps what was written, so a test can
    /// assert on the log line itself rather than on a proxy for it.
    #[derive(Clone, Default)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogCapture {
        /// The captured log, quotes stripped: `tracing`'s formatter quotes
        /// string fields (`via="sync"`), the firmware's `log_fmt` does not
        /// (`via=sync`), and the assertions want one spelling.
        fn unquoted(&self) -> String {
            let bytes = self.0.lock().expect("capture lock").clone();
            String::from_utf8_lossy(&bytes).replace('"', "")
        }
    }

    impl std::io::Write for LogCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
        type Writer = LogCapture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn announces_in(out: &TickOutput) -> Vec<Packet> {
        out.actions
            .iter()
            .filter_map(|action| match action {
                leviculum_core::transport::Action::Broadcast { data, .. } => {
                    Packet::unpack(data).ok()
                }
                _ => None,
            })
            .filter(|packet| packet.flags.packet_type == PacketType::Announce)
            .collect()
    }

    /// The first tick registers the propagation destination, the control
    /// destination and the mailbox, and reports all of them.
    #[test]
    fn the_first_tick_registers_role_control_and_mailbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, events) = engine_with(Vec::new(), Some(mailbox_config()));

        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let seen: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(
            seen.iter()
                .any(|event| matches!(event, EngineEvent::Ready { .. })),
            "the propagation role must report ready: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|event| matches!(event, EngineEvent::MailboxReady { .. })),
            "the mailbox must report ready: {seen:?}"
        );
    }

    /// The delivery announce carries the configured display name and stamp
    /// cost — the wire fact `lxmd`'s `register_delivery_identity` produces
    /// (`reference/LXMF/LXMF/Utilities/lxmd.py:422-423`). Sent after the
    /// reference's 10 s deferred start; the test crosses that boundary by
    /// ticking past it.
    #[test]
    fn the_mailbox_announce_carries_name_and_stamp_cost() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = engine_with(Vec::new(), Some(mailbox_config()));

        let start = core.now_ms();
        let _ = engine.on_tick(&mut core, start);
        // The mailbox books its deferred start announce on the first pass
        // and sends when the clock passes it.
        let out = engine.on_tick(
            &mut core,
            start + (crate::mailbox::MAILBOX_ANNOUNCE_DELAY_SECS + 1) * 1000,
        );
        let announces = announces_in(&out);
        let expected = leviculum_lxmf::announce::delivery(Some(b"an-operator"), Some(7));
        assert!(
            announces
                .iter()
                .any(|packet| packet.data.as_slice().ends_with(&expected)),
            "one announce must carry the delivery app data (name + stamp cost); \
             saw {} announce(s)",
            announces.len()
        );
    }

    /// The control destination announces alongside the node — always,
    /// because our query CLI is a separate shared-instance client with
    /// no other way to resolve a path to it (see `Engine::announce`) —
    /// and `allow_control` re-announces both immediately.
    #[test]
    fn the_control_destination_announces_with_the_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = engine_with(Vec::new(), None);

        let start = core.now_ms();
        let out = engine.on_tick(&mut core, start + 1);
        assert_eq!(
            announces_in(&out).len(),
            2,
            "the node AND its control destination announce at start"
        );

        let mut out = TickOutput::empty();
        assert!(engine.allow_control(&mut core, [0x5a; 16], &mut out));
        assert_eq!(
            announces_in(&out).len(),
            2,
            "allowing a remote re-announces both immediately"
        );
    }

    /// A control request on an unknown (never-identified) link answers the
    /// reference's ERROR_NO_IDENTITY (`stats_get_request`,
    /// `LXMRouter.py:838-839`) — the handler wiring end to end, minus the
    /// link the loopback test provides.
    #[test]
    fn an_unidentified_control_request_is_answered_no_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = engine_with(vec![[0x5a; 16]], None);
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let control_hash = match &engine.state {
            State::Ready(ready) => ready.control_hash,
            _ => panic!("engine must be ready"),
        };

        let event = NodeEvent::RequestReceived {
            link_id: LinkId::new([0x77; 16]),
            destination_hash: control_hash,
            request_id: [0x11; 16],
            path: STATS_GET_PATH.to_string(),
            path_hash: leviculum_core::crypto::truncated_hash(STATS_GET_PATH.as_bytes()),
            data: Vec::new(),
            requested_at: 0.0,
        };
        let _ = engine.on_event(&mut core, &event);
        // The response rides send_response on a link that does not exist,
        // which the core refuses — but the handler's decision is visible
        // in the encoded payload it tried to send. Assert the decision
        // directly instead:
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must still be ready"),
        };
        let response = engine.answer_control(
            &mut ready,
            &mut core,
            &LinkId::new([0x77; 16]),
            STATS_GET_PATH,
            &[],
        );
        assert_eq!(
            response,
            leviculum_lxmf::encode_control_error(leviculum_lxmf::PeerError::NoIdentity)
        );
        engine.state = State::Ready(ready);
    }

    /// leviculum#384 part 4 regression (conformance red
    /// `lxmf_pn_store_full_inbound`): the reference peer tears its sync
    /// link down the moment its transfer concludes, while the stamps are
    /// still on the validation worker. The peering association must be
    /// captured when the resource concludes — as the reference reads
    /// `validated_peer_links` inside the resource callback itself
    /// (`reference/LXMF/LXMF/LXMRouter.py:2381`) — because by the time the
    /// verdicts drain, `LinkClosed` has already cleared the link state,
    /// and a late lookup dropped the entire inbound batch.
    #[test]
    fn a_deferred_sync_survives_the_peer_closing_the_link_before_the_drain() {
        use leviculum_lxmf::constants::LXMF_OVERHEAD;
        use leviculum_lxmf::PeerSyncEnvelope;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = engine_with(Vec::new(), None);
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);

        let link_id = LinkId::new([0x42; 16]);
        let peer = [0x24u8; 16];
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        // Cost 0 with value computation on: the grinding is worth
        // deferring, and any stamp measures to some value — no mining.
        ready.node.set_compute_stamp_value(true);
        ready.peering.validate_link_for_tests(link_id, peer);

        let stamped = |seed: u8| {
            let mut message = vec![seed; LXMF_OVERHEAD + 40];
            message.extend_from_slice(&[seed.wrapping_add(1); STAMP_SIZE]);
            message
        };
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![stamped(0x01), stamped(0x02)],
        };
        engine.on_inbound_resource_completed(&mut ready, &link_id, &envelope.encode());
        // The close lands before any drain could run — the deterministic
        // stand-in for the LinkClosed event the engine routes here.
        ready.peering.on_link_closed(&link_id);
        engine.state = State::Ready(ready);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let now_ms = core.now_ms();
            let _ = engine.on_tick(&mut core, now_ms);
            let stored = match &engine.state {
                State::Ready(ready) => ready.node.store().count().unwrap_or(0),
                _ => panic!("engine must stay ready"),
            };
            if stored == 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the closed link orphaned the synced batch: store holds {stored} of 2"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    /// The same message, ingested once from a client and once from a
    /// validated sync peer, must be distinguishable in the log: a
    /// forwarded message says `via=sync`, a fresh one keeps the carrier it
    /// rode in on.
    ///
    /// Before this, the ingest was routed by message count, so the common
    /// sync — one message per round — took the client path and logged
    /// `via=resource`, with client accounting and no `PN_SYNC dir=in`.
    /// Both board cells (`hardware/ble_pn_board_upload.toml`,
    /// `hardware/lora_pn_board_sync.toml`) wait 600 s on the receiving
    /// board for `via=sync durable=1` and timed out on correct behaviour.
    #[test]
    fn a_peer_sync_and_a_client_upload_log_different_provenance() {
        use leviculum_lxmf::constants::LXMF_OVERHEAD;
        use leviculum_lxmf::PeerSyncEnvelope;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = engine_with(Vec::new(), None);
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);

        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let client_link = LinkId::new([0x11; 16]);
        let peer_link = LinkId::new([0x22; 16]);
        ready.peering.validate_link_for_tests(peer_link, [0x24; 16]);

        // One message, one envelope — the shape a client uploads and the
        // shape a peer syncs when it holds exactly one wanted message.
        let mut message = vec![0x5au8; LXMF_OVERHEAD + 40];
        message.extend_from_slice(&[0x5b; STAMP_SIZE]);
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![message],
        }
        .encode();

        enable_log_dispatch();
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut out = TickOutput::empty();
            engine.on_inbound_resource_completed(&mut ready, &client_link, &envelope);
            absorb(&mut engine, &mut ready, &mut core, &mut out);
            engine.on_inbound_resource_completed(&mut ready, &peer_link, &envelope);
            absorb(&mut engine, &mut ready, &mut core, &mut out);
        });
        engine.state = State::Ready(ready);

        let log = capture.unquoted();
        let accepts: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("PN_ACCEPT"))
            .collect();
        assert_eq!(accepts.len(), 2, "both ingests must log an accept:\n{log}");
        assert!(
            accepts[0].contains("via=resource"),
            "a client's upload keeps its carrier: {}",
            accepts[0]
        );
        assert!(
            accepts[1].contains("via=sync"),
            "a validated peer's message is a forwarded one, whatever the \
             envelope's message count: {}",
            accepts[1]
        );
        assert!(
            log.lines()
                .any(|line| line.contains("PN_SYNC") && line.contains("dir=in")),
            "a one-message transfer from a peer is still a sync round:\n{log}"
        );
    }

    // ------------------------------------------------------------------
    // Codeberg #417: who peers back, and who does not
    // ------------------------------------------------------------------

    /// A remote propagation node as this node would learn of it: its
    /// `lxmf.propagation` destination hash and the announce packet it puts
    /// on the air, already carrying `wire_hops` hops. The receipt increment
    /// makes the recorded path `wire_hops + 1`, which is what
    /// `autopeer_maxdepth` is compared against.
    fn propagation_announce(now_secs: u64, wire_hops: u8) -> ([u8; 16], Vec<u8>) {
        let mut destination = Destination::new(
            Some(leviculum_std::generate_identity()),
            Direction::In,
            DestinationType::Single,
            APP_NAME,
            &[PROPAGATION_ASPECT],
        )
        .expect("a propagation destination for the remote");
        let hash = *destination.hash().as_bytes();
        let app_data = leviculum_lxmf::PropagationNodeAnnounce {
            legacy_support: false,
            timebase: now_secs,
            enabled: true,
            transfer_limit_kb: 256,
            sync_limit_kb: 1024,
            stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 0,
            metadata: Vec::new(),
        }
        .encode()
        .expect("propagation announce app data");
        let mut packet = destination
            .announce(Some(&app_data), &mut rand_core::OsRng, 0, now_secs)
            .expect("announce packet");
        packet.hops = wire_hops;
        let mut buf = vec![0u8; 600];
        let len = packet.pack(&mut buf).expect("pack the announce");
        buf.truncate(len);
        (hash, buf)
    }

    /// One stamped LXMF message, the shape a sync envelope carries.
    fn synced_message(seed: u8) -> Vec<u8> {
        use leviculum_lxmf::constants::LXMF_OVERHEAD;
        let mut message = vec![seed; LXMF_OVERHEAD + 40];
        message.extend_from_slice(&[seed.wrapping_add(1); STAMP_SIZE]);
        message
    }

    fn peering_engine(
        peering: PeeringConfig,
    ) -> (
        Engine<MemoryPropagationStore>,
        std::sync::mpsc::Receiver<EngineEvent>,
    ) {
        Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: PropagationNodeConfig::default(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            peering,
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        })
    }

    /// Store everything the engine has queued, the way the driver would:
    /// hook after hook until the queue is empty. Bounded, because a queue
    /// that will not drain is a bug to fail on rather than hang on.
    fn absorb(
        engine: &mut Engine<MemoryPropagationStore>,
        ready: &mut Ready<MemoryPropagationStore>,
        core: &mut leviculum_std::driver::StdNodeCore,
        out: &mut TickOutput,
    ) {
        for _ in 0..10_000 {
            let Some(pending) = engine.pending.pop_front() else {
                return;
            };
            engine.store_one(ready, core, pending, PERSIST_PER_HOOK, out);
        }
        panic!("the store queue did not drain");
    }

    fn peer_count(engine: &Engine<MemoryPropagationStore>) -> usize {
        match &engine.state {
            State::Ready(ready) => ready.peering.peer_count(),
            _ => panic!("engine must be ready"),
        }
    }

    /// Feed one raw packet to the core WITHOUT showing the resulting events
    /// to the engine: the announce is heard by the stack, and the
    /// propagation role never sees it. That is exactly a neighbour that
    /// announced before this node took the role — and with a six-hour
    /// re-announce cadence, that is the state it stays in all day.
    fn hear_announce_off_the_role(core: &mut leviculum_std::driver::StdNodeCore, packet: &[u8]) {
        let _ = core.handle_packet(leviculum_core::InterfaceId(0), packet);
    }

    /// leviculum#417: a node syncs its whole store to us and we still do not
    /// know it. The sender's propagation announce is in our
    /// known-destinations table but was never heard during the role, so the
    /// announce path cannot peer it; the reference peers it off the
    /// RECALLED announce when the sync resource lands
    /// (`propagation_resource_concluded`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2350-2375`). Without that path the
    /// exchange stays one-directional until the next announce, up to
    /// `DEFAULT_ANNOUNCE_INTERVAL_SECS` away.
    #[test]
    fn a_sync_from_a_node_that_announced_before_the_role_peers_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = peering_engine(PeeringConfig {
            autopeer_maxdepth: 1,
            ..PeeringConfig::default()
        });
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);

        let (remote, packet) = propagation_announce(core.emission_secs(), 0);
        hear_announce_off_the_role(&mut core, &packet);
        assert!(
            core.recall_app_data(&DestinationHash::new(remote))
                .is_some(),
            "the announce must be recallable, or the test proves nothing"
        );
        assert_eq!(
            peer_count(&engine),
            0,
            "an announce heard off the role peers nobody, which is the bug's premise"
        );

        let link_id = LinkId::new([0x42; 16]);
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        ready.peering.validate_link_for_tests(link_id, remote);
        let envelope = leviculum_lxmf::PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![synced_message(0x01)],
        }
        .encode();
        let mut out = TickOutput::empty();
        engine.on_inbound_resource_completed(&mut ready, &link_id, &envelope);
        absorb(&mut engine, &mut ready, &mut core, &mut out);
        engine.state = State::Ready(ready);

        assert_eq!(
            peer_count(&engine),
            1,
            "a node that synced its store to us, one hop out and with a \
             recallable propagation announce, must be a peer"
        );
    }

    /// The other two ways the same decision can go wrong, both of which the
    /// conformance cell carries as standing negatives: a sender with no
    /// propagation announce behind it at all (a client upload), and a real
    /// propagation node beyond `autopeer_maxdepth`. Each one's sync lands —
    /// the store assertion says so — and neither may become a peer.
    #[test]
    fn a_sync_from_a_client_or_from_beyond_the_depth_peers_nobody() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = peering_engine(PeeringConfig {
            autopeer_maxdepth: 1,
            ..PeeringConfig::default()
        });
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);

        // A client: nothing was ever announced on its propagation
        // destination, so the recall answers nothing.
        let client = [0x5a_u8; 16];
        assert!(core
            .recall_app_data(&DestinationHash::new(client))
            .is_none());
        // Two hops out against a depth of one, and a genuine node.
        let (far, packet) = propagation_announce(core.emission_secs(), 1);
        hear_announce_off_the_role(&mut core, &packet);
        assert_eq!(
            core.hops_to(&DestinationHash::new(far)),
            Some(2),
            "the far node must really be two hops out"
        );

        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let mut out = TickOutput::empty();
        for (index, remote) in [client, far].into_iter().enumerate() {
            let link_id = LinkId::new([0x60 + index as u8; 16]);
            ready.peering.validate_link_for_tests(link_id, remote);
            let envelope = leviculum_lxmf::PeerSyncEnvelope {
                timestamp: 1.0,
                messages: vec![synced_message(0x10 + index as u8)],
            }
            .encode();
            engine.on_inbound_resource_completed(&mut ready, &link_id, &envelope);
            absorb(&mut engine, &mut ready, &mut core, &mut out);
        }
        let stored = ready.node.store().count().unwrap_or(0);
        engine.state = State::Ready(ready);

        assert_eq!(
            stored, 2,
            "both syncs must have landed, or the negatives are vacuous"
        );
        assert_eq!(
            peer_count(&engine),
            0,
            "neither a client nor a node outside autopeer_maxdepth may be peered by a sync"
        );
    }

    /// The opposite error on the same rule: a path response is an answer to
    /// somebody's path request, not a destination announcing itself, and the
    /// reference refuses to peer on it (`self.lxmrouter.autopeer and not
    /// is_path_response`, `reference/LXMF/LXMF/Handlers.py:80-84`). The same
    /// announce delivered normally must peer.
    #[test]
    fn a_path_response_does_not_peer_while_the_same_announce_does() {
        use leviculum_core::packet::PacketContext;

        for (context, expected) in [
            (PacketContext::PathResponse, 0usize),
            (PacketContext::None, 1usize),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut core = core(dir.path());
            let (mut engine, _events) = peering_engine(PeeringConfig {
                autopeer_maxdepth: 1,
                ..PeeringConfig::default()
            });
            let start_ms = core.now_ms();
            let _ = engine.on_tick(&mut core, start_ms);

            let (_remote, announce) = propagation_announce(core.emission_secs(), 0);
            let mut packet = Packet::unpack(&announce).expect("unpack the announce");
            packet.context = context;
            let mut buf = vec![0u8; 600];
            let len = packet.pack(&mut buf).expect("repack the announce");
            let delivered = core.handle_packet(leviculum_core::InterfaceId(0), &buf[..len]);
            assert!(
                delivered
                    .events
                    .iter()
                    .any(|event| matches!(event, NodeEvent::AnnounceReceived { .. })),
                "the stack must report the announce either way, {context:?}"
            );
            for event in &delivered.events {
                let _ = engine.on_event(&mut core, event);
            }

            assert_eq!(
                peer_count(&engine),
                expected,
                "a propagation announce delivered as {context:?} must {} peer",
                if expected == 0 { "not" } else { "" }
            );
        }
    }
}

/// mvr: a peer's inbound sync must not be stored inside one hook.
///
/// **The named failure mode:** `lnpnd` stored every message of an inbound
/// peer sync in the same [`CoreProcessor`](leviculum_std::driver::CoreProcessor)
/// hook that classified it. Each message is one durable store append —
/// write, `fsync`, rename, `fsync` the directory — so the hold scaled with
/// the batch, and the hook holds the core mutex: nothing on the node moved
/// while it ran. miauhaus, 2026-09-21, in the public mesh:
///
/// ```text
/// 10:25:12 lnpnd: sync in peer 535d9c5db65bfcd4 transferred 105 (30240 B): ok
/// 10:25:12 CORE_PROCESSOR_OVER_BUDGET hook="on_tick" elapsed_us=239016 budget_us=5000 events=0
/// ```
///
/// A quarter of a second of silence, doing `events=0` worth of event work.
///
/// The store is modelled rather than used: a real [`FilePropagationStore`]
/// would make the test a measurement of this host's disk, and a memory
/// store would make it a measurement of nothing. Charging each append a
/// fixed latency is what a durable write is, from the hook's side.
///
/// [`FilePropagationStore`]: leviculum_std::FilePropagationStore
#[cfg(test)]
mod inbound_sync_slices {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_core::LinkId;
    use leviculum_lxmf::constants::{LXMF_OVERHEAD, STAMP_SIZE};
    use leviculum_lxmf::propagation_store::StoredMessage;
    use leviculum_lxmf::storage::StorageError;
    use leviculum_lxmf::{
        MemoryPeerStore, MemoryPropagationStore, PeerSyncEnvelope, PropagationNodeConfig,
        PropagationStore, TransientId,
    };
    use leviculum_std::driver::{CoreProcessor as _, StdClock, StdStorage, PROCESSOR_TICK_BUDGET};

    use super::{Engine, EngineConfig, EngineEvent, State, PERSIST_PER_HOOK};

    /// The batch from the field report.
    const BATCH: usize = 105;

    /// What one durable append costs, priced for the budget arithmetic in
    /// `the_hook_budget_pays_for_the_slice_it_takes`.
    ///
    /// Measured on schneckenschreck (2026-09-24, ext4 on a virtio disk,
    /// `/var/tmp`) with `append_cost`
    /// (`leviculum-std/src/file_propagation_store.rs`): 105 bodies of 288
    /// bytes, debug profile, six runs, medians of 1160-1376 us per append,
    /// worst single append 4485 us, 135-160 ms for all 105. The memory-store
    /// control on the same bodies stays at 124-169 us for all 105, so the
    /// price is the device. The same bench against a tmpfs, where `fsync`
    /// never reaches a device, reads far cheaper and prices nothing, which
    /// is why the bench takes its directory as an input. This constant is
    /// the worst of the six medians, rounded up.
    ///
    /// The store is charged this in ARITHMETIC, not in `thread::sleep`.
    /// A sleep returns when the host's scheduler gets round to it: during a
    /// `just fast` run on a saturated coder host (2026-09-22) one hook that
    /// did exactly the one append the budget pays for measured 7.9 ms. What
    /// this test asserts on has to be the implementation's number — how many
    /// appends a hook takes times what an append costs — and never the
    /// host's wake-up latency.
    const PRICED_APPEND_COST: Duration = Duration::from_micros(1_400);

    /// A store that counts its appends; the caller charges each one what a
    /// durable write costs.
    struct ModelledStore {
        inner: MemoryPropagationStore,
        appends: Arc<AtomicUsize>,
    }

    impl PropagationStore for ModelledStore {
        fn append(
            &mut self,
            transient_id: &TransientId,
            received_at: u64,
            stamp_value: u8,
            body: &[u8],
        ) -> Result<(), StorageError> {
            self.appends.fetch_add(1, Ordering::Relaxed);
            self.inner
                .append(transient_id, received_at, stamp_value, body)
        }

        fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
            self.inner.for_each(visit)
        }

        fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
            self.inner.read_body(transient_id)
        }

        fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
            self.inner.purge(transient_id)
        }

        fn free_space(&self) -> u64 {
            self.inner.free_space()
        }

        fn capacity(&self) -> u64 {
            self.inner.capacity()
        }

        fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
            self.inner.contains(transient_id)
        }
    }

    fn stamped(seed: u16) -> Vec<u8> {
        let mut message = vec![0x11u8; LXMF_OVERHEAD + 40];
        message[16..18].copy_from_slice(&seed.to_be_bytes());
        message.extend_from_slice(&[0x5b; STAMP_SIZE]);
        message
    }

    #[test]
    fn a_peers_inbound_sync_is_stored_a_slice_at_a_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir.path()).expect("storage under a fresh temp dir"),
        );
        let appends = Arc::new(AtomicUsize::new(0));
        let (mut engine, events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: PropagationNodeConfig::default(),
            store: ModelledStore {
                inner: MemoryPropagationStore::new(10_000_000),
                appends: Arc::clone(&appends),
            },
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            peering: leviculum_lxmf::PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 10_000_000,
            delivery_limit_kb: 1000,
        });
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);

        let link_id = LinkId::new([0x42; 16]);
        let peer = [0x24u8; 16];
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        ready.peering.validate_link_for_tests(link_id, peer);
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: (0..BATCH as u16).map(stamped).collect(),
        }
        .encode();
        // The event arm's call, with the batch already transferred: what
        // follows is only the storing of it.
        engine.on_inbound_resource_completed(&mut ready, &link_id, &envelope);
        engine.state = State::Ready(ready);

        let mut worst_hold = Duration::ZERO;
        let mut worst_slice = 0usize;
        let mut stored = 0usize;
        // Twice the batch plus slack: every hook stores at most a slice,
        // and a hook that stores nothing is one the classification took.
        for _ in 0..(BATCH * 2 + 16) {
            let before = appends.load(Ordering::Relaxed);
            let now_ms = core.now_ms();
            let started = Instant::now();
            let _ = engine.on_tick(&mut core, now_ms);
            let hook_work = started.elapsed();
            let slice = appends.load(Ordering::Relaxed) - before;
            // The hold the daemon would feel: the hook's own work plus the
            // store latency every append in this slice is charged for.
            worst_hold = worst_hold.max(hook_work + PRICED_APPEND_COST * slice as u32);
            worst_slice = worst_slice.max(slice);
            stored = match &engine.state {
                State::Ready(ready) => ready.node.store().count().unwrap_or(0),
                _ => panic!("engine must stay ready"),
            };
            if stored == BATCH {
                break;
            }
        }

        assert_eq!(stored, BATCH, "every message of the batch must be stored");
        assert!(
            worst_slice < BATCH,
            "one hook stored {worst_slice} of the {BATCH}-message batch: the \
             batch is the hold"
        );
        assert!(
            worst_slice <= PERSIST_PER_HOOK,
            "one hook stored {worst_slice} messages; the budget pays for \
             {PERSIST_PER_HOOK}"
        );
        // Information, not an assertion. Half of this hold is the hook's own
        // work on whatever machine runs the test, and a wall-clock bound is
        // an assertion about that machine: the same commit passed this gate
        // on a Codeberg agent at 06:01 and failed it at 13:20 under daytime
        // load with 5.9 ms (Woodpecker 460, 2026-09-24). What bounds the hold
        // by construction is `worst_slice`, asserted above, times the price
        // of an append, asserted in
        // `the_hook_budget_pays_for_the_slice_it_takes`.
        eprintln!(
            "HOOK_HOLD worst_hold_us={} worst_slice={worst_slice} budget_us={}",
            worst_hold.as_micros(),
            PROCESSOR_TICK_BUDGET.as_micros(),
        );

        // The round is still reported as a round: the slices are an
        // implementation of the hook's budget, not a change to what the
        // peer's sync was.
        let sync_done: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                EngineEvent::SyncDone {
                    dir, transferred, ..
                } => Some((dir, transferred)),
                _ => None,
            })
            .collect();
        assert_eq!(
            sync_done,
            vec![("in", BATCH)],
            "one inbound round, reporting the whole batch"
        );
    }

    /// The claim the slice above implements, stated as arithmetic: a hook
    /// stores at most [`PERSIST_PER_HOOK`] messages, one durable append
    /// each, and that is what the hook budget pays for.
    ///
    /// This is the deterministic half of the test above. It reads two
    /// constants and a price measured once on a named disk, and asks no
    /// question of the machine it runs on, so it cannot go red because a
    /// shared runner was busy.
    #[test]
    fn the_hook_budget_pays_for_the_slice_it_takes() {
        let sliced = PRICED_APPEND_COST * PERSIST_PER_HOOK as u32;
        assert!(
            sliced <= PROCESSOR_TICK_BUDGET,
            "a hook's {PERSIST_PER_HOOK} append(s) cost {sliced:?}, past the \
             {PROCESSOR_TICK_BUDGET:?} budget: either the slice grew or the \
             disk got slower, and one of the two has to move"
        );

        // Non-vacuous: what the slicing took away. Storing the batch in one
        // hook is the 239 ms hold from miauhaus that this bounds.
        let whole_batch = PRICED_APPEND_COST * BATCH as u32;
        assert!(
            whole_batch > PROCESSOR_TICK_BUDGET,
            "the {BATCH}-message batch prices at {whole_batch:?}, inside the \
             {PROCESSOR_TICK_BUDGET:?} budget: this test no longer describes \
             the failure it was written for"
        );
    }
}
