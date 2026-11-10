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
//! only after the store append is durable, and both the data event and the
//! `LinkProofRequested` it must answer ride the tap. This is the same
//! reasoning that put loquax's engine on the tap
//! (`docs/src/concepts/loquax-architecture.md` §3).
//!
//! # Event order the proof correlation relies on
//!
//! For one inbound link packet the core emits `LinkProofRequested` first
//! and `LinkDataReceived` second, from the same
//! `handle_plain_data_packet` call
//! (`leviculum-core/src/node/link_management.rs:1562-1590`). The engine
//! therefore queues the proof hash per link and pops it when the matching
//! data event is processed; per-link FIFO order is the emission order.

use std::collections::{HashMap, HashSet, VecDeque};
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
use leviculum_lxmf::peering::{
    PeerStore, PeeringConfig, OFFER_REQUEST_PATH, ROTATION_INTERVAL_SECS, SYNC_INTERVAL_SECS,
};
use leviculum_lxmf::propagation::{MessageListResponse, PeerError, PropagationUpload};
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_lxmf::propagation_node::link_idle_past_bound;
use leviculum_lxmf::MAX_EXPIRED_PER_TICK;
use leviculum_lxmf::{
    Eviction, EvictionReason, FetchPlan, GetOutcome, Message, PropagationBodyStore,
    PropagationNode, PropagationNodeConfig, PropagationStore, TransientId, UploadOutcome,
    LEV_STATUS_PATH, MESSAGE_GET_PATH,
};

use leviculum_std::file_propagation_store::SettledWrites;

use crate::census::{CensusHandle, EventSender, PnCensus, CENSUS_PERIOD_MS};
use crate::hook_profile::{HookProfile, HookTimes, Section};
use crate::mailbox::{MailboxConfig, MailboxRuntime};
use crate::peering::{InboundSync, InboundSyncStart, PeeringRuntime, SyncSender};
use crate::validation::{
    validate_stamp_value, Carrier, ValidationDone, ValidationJob, ValidationWorker,
};

/// The reference announces the propagation destination 20 s after the role
/// comes up (`NODE_ANNOUNCE_DELAY`, `reference/LXMF/LXMF/LXMRouter.py:41`,
/// applied in `announce_propagation_node` `:338-341`); the board's role too.
pub const ANNOUNCE_DELAY_SECS: u64 = leviculum_announce_policy::PN_ANNOUNCE_INITIAL_DELAY_MS / 1000;

/// How long after the router's start announce the daemon's deferred one
/// follows when `[propagation] announce_at_start` is on: `lxmd` runs its
/// deferred start jobs `DEFFERED_JOBS_DELAY` = 10 s after start
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:33`, `:501-503`), and that
/// announce sleeps the same `NODE_ANNOUNCE_DELAY` as the router's own
/// (`reference/LXMF/LXMF/LXMRouter.py:338-341`), so the two leave 10 s
/// apart. One constant with the board's role, which sends the same pair.
pub const START_ANNOUNCE_GAP_SECS: u64 = leviculum_announce_policy::PN_START_ANNOUNCE_GAP_MS / 1000;

/// Default re-announce cadence: `lxmd`'s default config announces the node
/// every 360 minutes (`announce_interval = 360`,
/// `reference/LXMF/LXMF/Utilities/lxmd.py:981`).
pub const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 360 * 60;

/// Store-maintenance cadence: the reference cleans its message store every
/// `JOB_STORE_INTERVAL × PROCESSING_INTERVAL` = 120 × 4 s
/// (`reference/LXMF/LXMF/LXMRouter.py:871-900`).
const STORE_MAINTENANCE_SECS: u64 = 480;

/// How soon the engine asks the driver back when nothing else is due; the
/// value loquax's engine settled on for the same job.
const POLL_INTERVAL_MS: u64 = 200;

/// How soon the engine asks the driver back while the validation worker
/// holds a job: from the hook that queued it and from every tick until the
/// verdict drains.
///
/// The verdict gates the proof, and a stock Python client gives the proof
/// a few milliseconds over a LAN (`max(rtt * 6, 5 ms)`,
/// `reference/Reticulum/RNS/Packet.py:431`) before it tears the link down
/// and parks the message (`reference/LXMF/LXMF/LXMessage.py:616-621`).
/// The worker has no way to wake the driver, so the engine polls it. At
/// 20 ms, and with the queueing hook asking for no wake at all, release
/// lnpnd proved a loopback upload in 26 to 50 ms and lost four links of
/// five where lxmd proves in 16 ms (leviculum order 709). One stamp is a
/// 1.2 ms grind in a release build, so a 1 ms poll is about one extra
/// tick per upload, and the polling stops with the last verdict.
const VALIDATION_POLL_MS: u64 = 1;

/// The shortest spacing of two `/lev/status` requests on one link; a
/// faster one is answered [`PeerError::Throttled`]. A home asks once per
/// sync round (`docs/src/concepts/loquax-propagation-receipts.md`,
/// section B), so a link that asks faster is not one; the bound keeps a
/// stranger from turning one cheap link into a store walk per packet.
const LEV_STATUS_MIN_SPACING_MS: u64 = 1_000;

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
/// One append can still overrun the budget on a slow device when the store
/// writes inline. The daemon's store does not: it hands the write to its
/// writer thread (order 1081, [`WriteBehind`]), so an append in the hook is
/// a queue push and the proof waits for the writer instead. The bound stays
/// for a store that writes inline, which is what this removes the
/// multiplier of.
///
/// The arithmetic — this many appends at what an append costs, against the
/// budget — is asserted in `the_hook_budget_pays_for_the_slice_it_takes`,
/// against a price re-measured on the coder host (`PRICED_APPEND_COST`).
const PERSIST_PER_HOOK: usize = 1;

/// A store whose appends become durable after they return (order 1081).
///
/// The upload proof is the node's statement "stored", so it may not leave
/// before the message survives a power cut ("persist before you prove",
/// `docs/src/concepts/propagation-node-on-a-board.md` §3). A store that
/// writes inline is durable when `append` returns and keeps the defaults:
/// no tickets, nothing to settle. [`FilePropagationStore`] switched to its
/// writer hands out a ticket per append, and the engine holds the proof
/// until the writer has passed it (`Engine::release_proofs`).
///
/// [`FilePropagationStore`]: leviculum_std::FilePropagationStore
pub trait WriteBehind {
    /// The ticket of the last append handed to the writer.
    fn writes_issued(&self) -> u64 {
        0
    }

    /// How far the writer has got, and what it could not write.
    fn settle_writes(&mut self) -> SettledWrites {
        SettledWrites::default()
    }

    /// Whether an append is still with the writer.
    fn writes_outstanding(&self) -> bool {
        false
    }

    /// What the store holds in RAM, for `PN_CENSUS` ([`crate::census`]).
    /// A store that cannot say reports nothing.
    fn heap_census(&self) -> leviculum_std::StoreHeapCensus {
        leviculum_std::StoreHeapCensus::default()
    }
}

impl WriteBehind for leviculum_std::FilePropagationStore {
    fn writes_issued(&self) -> u64 {
        leviculum_std::FilePropagationStore::writes_issued(self)
    }

    fn settle_writes(&mut self) -> SettledWrites {
        leviculum_std::FilePropagationStore::settle_writes(self)
    }

    fn writes_outstanding(&self) -> bool {
        leviculum_std::FilePropagationStore::writes_outstanding(self)
    }

    fn heap_census(&self) -> leviculum_std::StoreHeapCensus {
        leviculum_std::FilePropagationStore::heap_census(self)
    }
}

impl WriteBehind for leviculum_lxmf::MemoryPropagationStore {}

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
    /// `[propagation] announce_at_start`: follow the first announce with
    /// the daemon's deferred start announce [`START_ANNOUNCE_GAP_SECS`]
    /// later, as `lxmd` does (`reference/LXMF/LXMF/Utilities/lxmd.py:501-503`).
    pub announce_at_start: bool,
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
    events: crate::census::EventSender,
    /// Proof hashes awaiting their data event, per link, in emission order.
    pending_proofs: HashMap<LinkId, VecDeque<[u8; 32]>>,
    /// Links peers and clients opened to the propagation destination,
    /// the reference's `active_propagation_links`
    /// (`reference/LXMF/LXMF/LXMRouter.py:2194`): each is torn down once
    /// it carries no payload for
    /// [`PROPAGATION_LINK_MAX_INACTIVITY_SECS`](leviculum_lxmf::constants::PROPAGATION_LINK_MAX_INACTIVITY_SECS).
    inbound_links: HashSet<LinkId>,
    next_announce_at: Option<u64>,
    announce_interval_secs: u64,
    announce_delay_secs: u64,
    /// The deferred start announce is still owed: the announce after the
    /// first comes [`START_ANNOUNCE_GAP_SECS`] later instead of an
    /// interval later.
    start_announce_owed: bool,
    /// The `occasion=` of the next scheduled announce's `ANN_TX` line:
    /// `start` until the start announces are out, `periodic` after.
    next_announce_occasion: &'static str,
    /// Peering cadences in milliseconds, the reference's unless a test sets
    /// them ([`Self::with_sync_interval`], [`Self::with_rotation_interval`]);
    /// handed to the peering runtime when the role registers.
    sync_interval_ms: u64,
    rotation_interval_ms: u64,
    next_maintenance_at: u64,
    /// The last expiry pass stopped at [`MAX_EXPIRED_PER_TICK`], so the
    /// next hook takes the next slice instead of waiting a whole
    /// maintenance period.
    expiry_backlog: bool,
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
    /// Accepted uploads whose proof waits for the store's writer, in ticket
    /// order ([`Self::release_proofs`]).
    unproven: VecDeque<Unproven>,
    /// The clock of the hook invocation in progress (order 1009), kept on
    /// the engine so the drain can charge sync slices apart from the rest.
    profile: HookProfile,
    /// What the last finished invocation spent, section by section.
    last_profile: HookTimes,
    /// Where the role's departed ids persist across restarts
    /// ([`Self::with_departures_path`]); `None` keeps them in RAM only.
    departures_path: Option<std::path::PathBuf>,
    /// A message left the store early since the departures were last
    /// saved; the next tick saves them.
    departures_dirty: bool,
    /// When each open link last asked `/lev/status`
    /// ([`LEV_STATUS_MIN_SPACING_MS`]); dropped with the link.
    status_asked_at: HashMap<LinkId, u64>,
    /// The SIGUSR1 request and the event printer's count
    /// ([`crate::census`]).
    census: CensusHandle,
    /// When the next periodic `PN_CENSUS` line is due; 0 until the first
    /// tick books it [`CENSUS_PERIOD_MS`] on.
    next_census_at: u64,
}

/// An accepted upload whose proof waits until its append is durable.
struct Unproven {
    /// [`WriteBehind::writes_issued`] just after the append.
    ticket: u64,
    link_id: LinkId,
    packet_hash: [u8; 32],
    transient_id: TransientId,
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
        /// The resource's sender as of the moment it concluded; see
        /// [`ValidationJob::sender`].
        sender: SyncSender,
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
    /// Judged at the store, under the lock: either
    /// [`ValidationWorker::worth_deferring`] said no, which means no cost
    /// gate and no value to measure, so this is a constant rather than a
    /// hash grind; or the payload is a single upload ground in the hook that
    /// received it (`Engine::grinds_inline`), one stamp.
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
    /// Heap bytes the queued payload pins, for `PN_CENSUS sync_in=`.
    fn heap_bytes(&self) -> usize {
        let verdicts = |verdicts: &Verdicts| match verdicts {
            Verdicts::Ground(map) => crate::census::hash_table_bytes(
                map.capacity(),
                std::mem::size_of::<(TransientId, Option<u16>)>(),
            ),
            Verdicts::Inline { .. } => 0,
        };
        match self {
            Pending::Packet {
                data, verdicts: v, ..
            }
            | Pending::Resource {
                data, verdicts: v, ..
            } => data.capacity() + verdicts(v),
            Pending::Sync { job, verdicts: v } => job.heap_bytes() + verdicts(v),
        }
    }

    /// A finished validation, ready for the store.
    fn from_validated(done: ValidationDone) -> Self {
        let ValidationDone {
            link_id,
            carrier,
            data,
            proof,
            sender,
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
                sender,
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
/// One refused message or transfer, at INFO: a refusal is the sender's
/// loss until it retries, and at debug it was invisible on a node that
/// logs at the default level (miauhaus, 2026-11-02 20:55, a resource
/// upload refused with nothing in the node's log).
pub(crate) fn log_reject(reason: &'static str, via: &'static str) {
    tracing::info!(event = "PN_REJECT", reason = reason, via = via);
}

/// [`log_reject`] for a refusal by size: what was offered and the cap it
/// failed, both in bytes, so the line alone says which limit bit.
pub(crate) fn log_reject_sized(reason: &'static str, via: &'static str, bytes: u64, limit: u64) {
    tracing::info!(
        event = "PN_REJECT",
        reason = reason,
        via = via,
        bytes = bytes,
        limit = limit,
    );
}

/// What a cull leaves behind, whichever ingest path caused it: one
/// `PN_EVICT` debug event per message, and one `EngineEvent::Evicted` the
/// operator sees at the default log level. Client uploads, maintenance and
/// peer sync all come through here, so a cull is never logged in two
/// shapes.
pub(crate) fn log_evictions(evicted: &[Eviction], events: &crate::census::EventSender) {
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
        let _ = events.send(EngineEvent::Evicted {
            count: evicted.len(),
        });
    }
}

fn short_hex(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn full_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl<S: PropagationStore + PropagationBodyStore + WriteBehind> Engine<S> {
    pub fn new(config: EngineConfig<S>) -> (Self, std::sync::mpsc::Receiver<EngineEvent>) {
        let (events, receiver) = std::sync::mpsc::channel();
        let events = EventSender::from(events);
        let census = CensusHandle::new(&events);
        let announce_interval_secs = config.announce_interval_secs;
        let announce_delay_secs = config.announce_delay_secs;
        let start_announce_owed = config.announce_at_start;
        (
            Self {
                state: State::Unregistered(Box::new(config)),
                events,
                pending_proofs: HashMap::new(),
                inbound_links: HashSet::new(),
                next_announce_at: None,
                announce_interval_secs,
                announce_delay_secs,
                start_announce_owed,
                next_announce_occasion: "start",
                sync_interval_ms: SYNC_INTERVAL_SECS * 1000,
                rotation_interval_ms: ROTATION_INTERVAL_SECS * 1000,
                next_maintenance_at: 0,
                expiry_backlog: false,
                validation: ValidationWorker::spawn(),
                pending: VecDeque::new(),
                unproven: VecDeque::new(),
                profile: HookProfile::start("-"),
                last_profile: HookTimes::default(),
                departures_path: None,
                departures_dirty: false,
                status_asked_at: HashMap::new(),
                census,
                next_census_at: 0,
            },
            receiver,
        )
    }

    /// The handle the daemon's main loop asks for a census through, and
    /// marks the events it took (`PN_CENSUS`, [`crate::census`]).
    pub fn census_handle(&self) -> CensusHandle {
        self.census.clone()
    }

    /// Space outbound sync rounds by `interval` instead of the reference's
    /// 24 s (`SYNC_INTERVAL_SECS`). For tests that run a stock `lxmd` peer
    /// in real time; the daemon never calls it.
    pub fn with_sync_interval(mut self, interval: std::time::Duration) -> Self {
        self.sync_interval_ms = interval.as_millis() as u64;
        self
    }

    /// Persist the duplicate list's departed ids at `path`, and re-seed
    /// them from it when the role registers (`crate::departures`): a
    /// message collected or displaced before a restart is then still a
    /// duplicate after it.
    pub fn with_departures_path(mut self, path: std::path::PathBuf) -> Self {
        self.departures_path = Some(path);
        self
    }

    /// Write the departed ids if they moved, or always when `force`.
    fn save_departures(&mut self, ready: &Ready<S>, force: bool) {
        let Some(path) = &self.departures_path else {
            return;
        };
        if !(force || self.departures_dirty) {
            return;
        }
        match crate::departures::save(path, ready.node.departures()) {
            Ok(()) => self.departures_dirty = false,
            Err(error) => tracing::warn!("lnpnd: departures {}: {error}", path.display()),
        }
    }

    /// The heap census of the role (order 1515, [`crate::census`]): one
    /// byte estimate per table the engine holds. Walks the store's index
    /// once for its paths, so it runs on the census cadence, not per hook.
    fn census(&self, ready: &Ready<S>, transport: crate::census::TransportCensus) -> PnCensus {
        use crate::census::hash_table_bytes;
        use leviculum_core::heap_census::vec_deque_bytes;
        use std::mem::size_of;
        let store = ready.node.store().heap_census();
        let peering = ready.peering.heap_census();
        let pending: usize = vec_deque_bytes(&self.pending)
            + self.pending.iter().map(Pending::heap_bytes).sum::<usize>();
        let links = hash_table_bytes(
            self.pending_proofs.capacity(),
            size_of::<LinkId>() + size_of::<VecDeque<[u8; 32]>>(),
        ) + self
            .pending_proofs
            .values()
            .map(|proofs| proofs.capacity() * 32)
            .sum::<usize>()
            + hash_table_bytes(self.inbound_links.capacity(), size_of::<LinkId>())
            + vec_deque_bytes(&self.unproven)
            + hash_table_bytes(
                self.status_asked_at.capacity(),
                size_of::<LinkId>() + size_of::<u64>(),
            )
            + peering.links;
        let config = (ready.control_allowed.capacity()
            + ready.auth_allowed.as_ref().map_or(0, Vec::capacity))
            * 16
            + peering.config
            + ready
                .mailbox
                .as_ref()
                .map_or(0, MailboxRuntime::config_bytes);
        let events_queued = self.events.queued();
        let dup = ready.node.dup_counts();
        PnCensus {
            peers: peering.peers,
            store_index: store.index_bytes,
            store_writer: store.writer_bytes,
            sync_out: peering.sync_out,
            sync_in: pending + self.validation.bytes_in_flight(),
            dup: ready.node.heap_bytes() + peering.origins,
            links,
            mailbox: ready.mailbox.as_ref().map_or(0, MailboxRuntime::heap_bytes),
            config,
            eventbuf: events_queued * size_of::<EngineEvent>(),
            rss_kb: crate::census::rss_kb(),
            stored: store.messages,
            peered: ready.peering.peer_count(),
            events_queued,
            processed_ids: dup.processed,
            departed_ids: dup.departed,
            origin_ids: peering.origin_ids,
            // An origin goes with its message (`Origins`), so the store's
            // count bounds them.
            dup_cap_ids: dup.processed_cap + dup.departed_cap + store.messages,
            origin_slots: peering.origin_slots,
            store_index_entries: store.index_entry_bytes,
            store_index_spare: store.index_spare_bytes,
            transport,
        }
    }

    /// Whether a peer's sync batch is in progress: arrived as a resource
    /// and not yet all stored, whether it waits for the stamp worker, for
    /// its turn in the queue, or for its next slice. Gates `/offer` as the
    /// reference's sequential validation does
    /// ([`PeeringRuntime::answer_offer_request`]).
    fn batch_in_progress(&self) -> bool {
        self.validation.resources_in_flight() > 0
            || self
                .pending
                .iter()
                .any(|queued| matches!(queued, Pending::Resource { .. } | Pending::Sync { .. }))
    }

    /// The census of a registered engine, `None` before it registered.
    pub fn census_now(&self) -> Option<PnCensus> {
        match &self.state {
            State::Ready(ready) => Some(self.census(ready, Default::default())),
            _ => None,
        }
    }

    /// One `PN_DEPART` line per id that left the store since the last
    /// hook, whichever path removed it (a `/get` confirmation, an own-
    /// mailbox delivery, an eviction during an upload or a peer's sync, an
    /// expiry), and the departures marked for the next save.
    fn log_departures(&mut self, ready: &mut Ready<S>) {
        let fresh = ready.node.take_fresh_departures();
        if fresh.is_empty() {
            return;
        }
        // The departures' count bound (order 1558) is the host's to keep:
        // the role leaves the walk out of the board's image.
        ready.node.bound_departures();
        self.departures_dirty = true;
        for (transient_id, reason) in fresh {
            // Its origin goes with it (LV-1003).
            ready.peering.origins_mut().forget(&transient_id);
            tracing::debug!(
                event = "PN_DEPART",
                id = full_hex(&transient_id),
                reason = reason.word(),
            );
        }
    }

    /// Answer one `/lev/status` request on `link_id`, spaced per link by
    /// [`LEV_STATUS_MIN_SPACING_MS`]. The link's identity is never asked
    /// for, so the node learns nothing about who holds the ids.
    fn answer_lev_status(
        &mut self,
        ready: &Ready<S>,
        link_id: &LinkId,
        data: &[u8],
        now_ms: u64,
    ) -> Vec<u8> {
        let spaced = self
            .status_asked_at
            .get(link_id)
            .is_none_or(|last| now_ms.saturating_sub(*last) >= LEV_STATUS_MIN_SPACING_MS);
        if !spaced {
            tracing::debug!(event = "PN_STATUS", ids = 0usize, result = "throttled");
            return leviculum_lxmf::lev_status::encode_refusal(PeerError::Throttled);
        }
        self.status_asked_at.insert(*link_id, now_ms);
        match leviculum_lxmf::lev_status::decode_request(data) {
            Ok(ids) => {
                tracing::debug!(event = "PN_STATUS", ids = ids.len(), result = "answered");
                ready.node.answer_lev_status(&ids)
            }
            Err(error) => {
                tracing::debug!(event = "PN_STATUS", ids = 0usize, result = "refused");
                leviculum_lxmf::lev_status::encode_refusal(error)
            }
        }
    }

    /// Run rotation passes every `interval` instead of the reference's
    /// 22 min 24 s (`ROTATION_INTERVAL_SECS`). For tests, as above.
    pub fn with_rotation_interval(mut self, interval: std::time::Duration) -> Self {
        self.rotation_interval_ms = interval.as_millis() as u64;
        self
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
    /// statement "stored", so it must not leave before the append is durable
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
            announce_at_start: _,
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
        // Ours, announced under `PN_META_CUSTOM` (`leviculum_lxmf::lev_status`):
        // open to any link, identified or not, so the node never needs to
        // know who asks.
        core.register_request_handler(destination_hash, LEV_STATUS_PATH, RequestPolicy::AllowAll);

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

        // The static-only policy has one knob, the peering config's: the
        // role announces it, the peering runtime enforces it.
        let node_config = PropagationNodeConfig {
            from_static_only: peering.from_static_only,
            ..node_config
        };
        let mut node = PropagationNode::new(store, node_config);
        if let Some(path) = &self.departures_path {
            match crate::departures::load(path) {
                Ok(entries) => {
                    let restored = node.restore_departures(entries, unix_secs());
                    tracing::info!("lnpnd: {restored} departed message id(s) restored");
                }
                Err(error) => {
                    node.track_departures();
                    tracing::warn!("lnpnd: departures ignored, {error}");
                }
            }
        }
        // The status answer comes from the departures; without a file they
        // live in RAM and a restart answers `unknown`.
        node.serve_lev_status();
        let min_cost = node.min_accepted_cost();
        let mut peering =
            PeeringRuntime::new(peering, peer_store, identity_copy, self.events.clone());
        peering.set_cadence(self.sync_interval_ms, self.rotation_interval_ms);
        // Restored peers may already require true stamp values (§5).
        node.set_compute_stamp_value(peering.requires_stamp_values());
        // What a path response carries until the first announce refreshes
        // it: the reference parks its app data on the destination at
        // registration (`set_default_app_data`,
        // `reference/LXMF/LXMF/LXMRouter.py:191`). Without it a client that
        // asks for the path in the 20 s before that announce reads no stamp
        // cost and fails its upload (`LXMRouter.py:2634-2640`).
        if let Some(destination) = core.destination_mut(&destination_hash) {
            destination.set_default_app_data(&node.announce_app_data(unix_secs()));
        }
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

    /// One upload envelope, from either carrier. Returns the accepted
    /// message's transient id (so the packet path can prove it).
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
    ) -> Option<TransientId> {
        // A message above the delivery limit is refused before it is
        // stored, so the sender's retries fail where it can see them. No
        // proof, and the link goes, as for any refused upload. One above
        // the transfer limit but within the delivery limit is stored local
        // only (`is_local_only`): its recipient collects it here, no peer is
        // offered it, and the client is proven as for any other.
        let body_len = ready.node.upload_body_len(data);
        let delivery_limit = ready.delivery_limit_kb.saturating_mul(1000);
        if let Some(bytes) = body_len.filter(|bytes| *bytes > delivery_limit) {
            out.merge(core.close_link(link_id));
            log_reject_sized("too_large", via, bytes, delivery_limit);
            self.emit(EngineEvent::Rejected {
                detail: format!(
                    "message of {bytes} B exceeds the delivery limit of {delivery_limit} B"
                ),
            });
            return None;
        }
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
                log_evictions(&evicted, &self.events);
                if !evicted.is_empty() {
                    self.departures_dirty = true;
                }
                tracing::debug!(
                    event = "PN_ACCEPT",
                    tid = short_hex(&transient_id),
                    dst = full_hex(&destination_hash),
                    bytes = size,
                    value = stamp_value,
                    dup = duplicate,
                    via = via,
                    local_only = ready.node.is_local_only(size),
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
                Some(transient_id)
            }
            UploadOutcome::InvalidStamp { reject } => {
                // A resource from an identified remote throttles it for
                // PN_STAMP_THROTTLE, as the reference throttles any such
                // transfer (reference/LXMF/LXMF/LXMRouter.py:2447-2454); the
                // raw-packet path rejects without throttling (:2257-2260).
                // Read the identity before the close below forgets it.
                if via == "resource" {
                    if let Some(identity) = core.get_remote_identity(link_id) {
                        ready.peering.throttle_invalid_upload(identity);
                    }
                }
                // The reference packs [ERROR_INVALID_STAMP] into a raw link
                // packet and tears the link down
                // (reference/LXMF/LXMF/LXMRouter.py:2257-2260). Kept on the
                // resource path too: a Python client decodes it and stops
                // retrying.
                if let Ok((_, send)) = core.send_packet_on_link(link_id, &reject) {
                    out.merge(send);
                }
                out.merge(core.close_link(link_id));
                log_reject("invalid_stamp", via);
                self.emit(EngineEvent::Rejected {
                    detail: "invalid stamp".into(),
                });
                None
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
                None
            }
            UploadOutcome::Malformed(error) => {
                // The reference logs and ignores
                // (reference/LXMF/LXMF/LXMRouter.py:2262-2264).
                tracing::debug!("lnpnd: undecodable upload ignored: {error}");
                log_reject("malformed", via);
                None
            }
            UploadOutcome::StoreFailed(error) => {
                // No proof leaves: the client keeps its retry, which is the
                // honest outcome for a store that cannot hold the message.
                log_reject("store_failed", via);
                self.emit(EngineEvent::Rejected {
                    detail: format!("store failed: {error}"),
                });
                None
            }
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
                if !purged.is_empty() {
                    self.departures_dirty = true;
                }
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

    /// Tear down every inbound propagation link that carried no payload
    /// for more than
    /// [`PROPAGATION_LINK_MAX_INACTIVITY_SECS`](leviculum_lxmf::constants::PROPAGATION_LINK_MAX_INACTIVITY_SECS),
    /// by the rule the board's idle sweep shares
    /// ([`link_idle_past_bound`]), as the reference's `clean_links` does
    /// (`reference/LXMF/LXMF/LXMRouter.py:968-974`). Keepalives do not
    /// count: the core's own watchdog lets them hold a silent link open
    /// indefinitely, which on a public node fills the link table with
    /// clients that went quiet.
    fn close_idle_links(
        &mut self,
        core: &mut StdNodeCoreRef<'_>,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        let idle: Vec<(LinkId, u64)> = self
            .inbound_links
            .iter()
            .filter_map(|link_id| {
                let idle_ms = core.link(link_id)?.no_data_for_ms(now_ms);
                link_idle_past_bound(idle_ms).then_some((*link_id, idle_ms? / 1000))
            })
            .collect();
        for (link_id, idle_secs) in idle {
            tracing::debug!(
                "lnpnd: tearing down inbound link {} after {idle_secs} s without payload",
                short_hex(link_id.as_bytes()),
            );
            // The LinkClosed this raises reaches `on_event`, which clears
            // the peering state and this set as well.
            self.inbound_links.remove(&link_id);
            out.merge(core.close_link(&link_id));
        }
    }

    /// Announce the node and its control destination. `occasion` names
    /// why on the node's own `ANN_TX` line: `start` for the first and the
    /// deferred start announce, `periodic` for the interval, `host` for
    /// one asked for. The core logs no `ANN_TX` for an announce it sends
    /// on every interface, so without this line the node's own log could
    /// not prove its cadence; miauhaus showed only the path responses,
    /// 18 lines in 113 h, and the 6-hour announces only on lnsd's side.
    fn announce(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        occasion: &'static str,
        out: &mut TickOutput,
    ) {
        let app_data = ready.node.announce_app_data(unix_secs());
        match core.announce_destination(&ready.destination_hash, Some(&app_data)) {
            Ok(send) => {
                out.merge(send);
                tracing::debug!(
                    event = "ANN_TX",
                    occasion = occasion,
                    dst = full_hex(ready.destination_hash.as_bytes()),
                    hops = 0u8,
                    iface = "all",
                );
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
        self.announce(&mut ready, core, "host", out);
        self.state = State::Ready(ready);
        true
    }

    /// Announce the propagation destination now, outside the cadence: the
    /// conformance helper's `pn_announce`, which a cell uses for a second
    /// announce a collision cannot take for good. The reference helper does
    /// the same, `announce_propagation_node`'s body without its delay thread
    /// (`reference/LXMF/LXMF/LXMRouter.py:338-346`). The scheduled cadence is
    /// left as it was. Returns the destination hash, or `None` before the
    /// node is ready.
    pub fn announce_now(
        &mut self,
        core: &mut StdNodeCoreRef<'_>,
        out: &mut TickOutput,
    ) -> Option<[u8; 16]> {
        let mut ready = self.take_ready(core)?;
        self.announce(&mut ready, core, "host", out);
        let hash = *ready.destination_hash.as_bytes();
        self.state = State::Ready(ready);
        Some(hash)
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
        match ready.node.store_mut().purge(transient_id) {
            Ok(_) => {
                ready.node.note_delivered_locally(transient_id, unix_secs());
                self.departures_dirty = true;
            }
            Err(error) => tracing::warn!("lnpnd: own-mailbox purge failed: {error}"),
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
            messagestore_evicted: Some(ready.node.evicted_total()),
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
                if let (Some(transient_id), Some(packet_hash)) = (accepted, proof) {
                    // The proof is the "stored" statement (persist before
                    // you prove; packet.prove after storing,
                    // reference/LXMF/LXMF/LXMRouter.py:2255), so it waits
                    // for the writer: `release_proofs` sends it, in this
                    // hook already when the store writes inline.
                    self.unproven.push_back(Unproven {
                        ticket: ready.node.store().writes_issued(),
                        link_id,
                        packet_hash,
                        transient_id,
                    });
                }
                1
            }
            Pending::Resource {
                link_id,
                data,
                sender,
                verdicts,
            } => match ready
                .peering
                .begin_sync_resource(core, &link_id, sender, &data, out)
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
    /// The sender — validated sync peer, the link's identity and hops — is
    /// captured NOW, not at the drain: the reference peer tears its link
    /// down as soon as its transfer concludes, and the `LinkClosed` that
    /// follows clears the peering runtime's link state, and the core's
    /// link with it, while the stamps may still be on the worker (643).
    fn on_inbound_resource_completed(
        &mut self,
        ready: &mut Ready<S>,
        core: &StdNodeCoreRef<'_>,
        link_id: &LinkId,
        data: &[u8],
    ) {
        let min_cost = ready.min_cost;
        let compute_value = ready.node.compute_stamp_value();
        let sender = ready.peering.sync_sender(core, link_id);
        if ValidationWorker::worth_deferring(min_cost, compute_value) {
            self.validation.enqueue(ValidationJob {
                link_id: *link_id,
                carrier: Carrier::Resource,
                data: data.to_vec(),
                proof: None,
                sender,
                min_cost,
                compute_value,
            });
        } else {
            self.pending.push_back(Pending::Resource {
                link_id: *link_id,
                data: data.to_vec(),
                sender,
                verdicts: Verdicts::Inline {
                    min_cost,
                    compute_value,
                },
            });
        }
    }

    /// Take the worker's finished validations onto the store queue, in
    /// completion order (which is submission order — one worker).
    fn collect_validated(&mut self) {
        while let Some(done) = self.validation.try_recv() {
            self.pending.push_back(Pending::from_validated(done));
        }
    }

    /// [`Self::collect_validated`], then store as much of the queue as this
    /// hook's budget allows.
    ///
    /// Queueing is cheap; storing is not, which is why only the second half
    /// is bounded. Whatever is left asks the driver to come straight back
    /// ([`Self::ask_back_if_queued`]).
    fn drain_pending(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        out: &mut TickOutput,
    ) {
        self.collect_validated();
        let mut budget = PERSIST_PER_HOOK;
        while budget > 0 {
            let Some(pending) = self.pending.pop_front() else {
                break;
            };
            self.profile.lap(Section::Drain);
            let section = match pending {
                Pending::Sync { .. } => Section::Sync,
                _ => Section::Drain,
            };
            let spent = self.store_one(ready, core, pending, budget, out);
            self.profile.lap(section);
            self.profile.add_appends(spent);
            budget -= spent.min(budget);
        }
        // A sync message the store could not hold: sweep at the next tick,
        // before the next sync is taken (the peering runtime declines
        // until then, `PeeringRuntime::store_full`).
        if ready.peering.store_full() {
            self.next_maintenance_at = 0;
        }
        self.ask_back_if_queued(core, out);
    }

    /// Send the proofs whose appends the store's writer has made durable,
    /// in ticket order, and let go of what it could not write.
    ///
    /// A failed write left the store; the role forgets the id, so the
    /// client's retry is stored rather than answered as a proven duplicate
    /// (`PropagationNode::forget_processed`, the board's
    /// `settle_message_op`). A proof is sent only for a message the store
    /// still holds once its ticket is passed: a duplicate whose original
    /// failed, or a message purged meanwhile, gets none, and the client
    /// keeps its retry.
    fn release_proofs(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        out: &mut TickOutput,
    ) {
        let settled = ready.node.store_mut().settle_writes();
        for transient_id in &settled.failed {
            ready.node.forget_processed(transient_id);
            ready.peering.origins_mut().forget(transient_id);
            log_reject("store_failed", "writer");
            self.emit(EngineEvent::Rejected {
                detail: format!("store write failed for {}", short_hex(transient_id)),
            });
        }
        while self
            .unproven
            .front()
            .is_some_and(|unproven| unproven.ticket <= settled.through)
        {
            let Some(unproven) = self.unproven.pop_front() else {
                break;
            };
            if !ready
                .node
                .store()
                .contains(&unproven.transient_id)
                .unwrap_or(false)
            {
                continue;
            }
            match core.send_data_proof(&unproven.link_id, &unproven.packet_hash) {
                Ok(send) => out.merge(send),
                Err(error) => tracing::warn!("lnpnd: proof failed: {error:?}"),
            }
        }
    }

    /// While a proof waits for the writer, come back at the validation
    /// poll: nothing else wakes the engine when the write lands, and a
    /// stock client gives the proof a few milliseconds over a LAN
    /// ([`VALIDATION_POLL_MS`]).
    fn ask_back_for_proofs(&self, core: &StdNodeCoreRef<'_>, out: &mut TickOutput) {
        if !self.unproven.is_empty() {
            let due = core.now_ms() + VALIDATION_POLL_MS;
            out.next_deadline_ms = Some(match out.next_deadline_ms {
                Some(existing) => existing.min(due),
                None => due,
            });
        }
    }

    /// Whatever is left on the store queue asks the driver to come straight
    /// back: the deadline is now, and the driver clamps that to its next
    /// millisecond (`leviculum-std/src/driver/mod.rs`, "Advance next_poll").
    fn ask_back_if_queued(&self, core: &StdNodeCoreRef<'_>, out: &mut TickOutput) {
        if !self.pending.is_empty() {
            let now_ms = core.now_ms();
            out.next_deadline_ms = Some(match out.next_deadline_ms {
                Some(existing) => existing.min(now_ms),
                None => now_ms,
            });
        }
    }

    /// Close the hook invocation's profile; see [`crate::hook_profile`].
    fn finish_profile(&mut self, rest: Section) {
        let profile = std::mem::replace(&mut self.profile, HookProfile::start("-"));
        self.last_profile = profile.finish(rest);
    }

    /// Whether a link packet's stamp is ground in the hook that received
    /// it rather than on the worker (leviculum order 716).
    ///
    /// The proof waits on the verdict, and a stock client gives it
    /// `max(rtt * 6, 5 ms)` over a LAN (`reference/Reticulum/RNS/Packet.py:431`).
    /// Through the worker the verdict needs two wake-ups the hook does not
    /// control: the worker thread's, after the queueing, and the driver's,
    /// for the poll that drains it. Beside a CPU burner on the coder host
    /// one of the two came a scheduler tick (4 ms at `CONFIG_HZ=250`) late
    /// in a fifth of the uploads, nearly always the worker's, and the proof
    /// left at 6 to 7 ms. Ground here, the thread that already holds the CPU
    /// does the work. One stamp is a 1.2 ms grind in a release build, which
    /// with the one append the hook pays for stays inside the hook budget
    /// (`the_hook_budget_pays_for_the_slice_it_takes`); the
    /// reference grinds the packet path on its packet callback too
    /// (`reference/LXMF/LXMF/LXMRouter.py:2243`).
    ///
    /// Only a single upload, and only with nothing older in flight: a job on
    /// the worker or a payload in the store queue is earlier traffic, and
    /// overtaking it would break the per-link validate, append, prove order
    /// (`crate::validation`). A packet that is not a single upload is
    /// classified by the drain and gains nothing from grinding early.
    fn grinds_inline(&self, data: &[u8]) -> bool {
        !self.validation.busy()
            && self.pending.is_empty()
            && PropagationUpload::decode(data).is_ok()
    }
}

/// The concrete core type the driver hands its processors.
type StdNodeCoreRef<'a> = leviculum_std::driver::StdNodeCore;

impl<S: PropagationStore + PropagationBodyStore + WriteBehind + Send + 'static>
    leviculum_std::driver::CoreProcessor for Engine<S>
{
    fn on_event(
        &mut self,
        core: &mut leviculum_std::driver::StdNodeCore,
        event: &NodeEvent,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        self.profile = HookProfile::start("on_event");
        self.profile.set_arm(event.variant_name());
        let Some(mut ready) = self.take_ready(core) else {
            self.finish_profile(Section::Ready);
            return out;
        };
        self.profile.lap(Section::Ready);
        // Finished validations first: they are older traffic than this
        // event, and queueing them here keeps the per-link order.
        self.collect_validated();
        let queued = self.pending.len();
        self.profile.lap(Section::Drain);
        // The mailbox router sees every event and filters for its own
        // links, exactly as loquax's engine feeds it.
        if let Some(mut mailbox) = ready.mailbox.take() {
            mailbox.on_event(core, event, &mut out);
            ready.mailbox = Some(mailbox);
        }
        self.profile.lap(Section::Mailbox);
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
                self.profile.lap(Section::PeerTable);
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
                self.inbound_links.insert(*link_id);
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
                if ValidationWorker::worth_deferring(min_cost, compute_value)
                    && !self.grinds_inline(data)
                {
                    // The grinding goes to the worker (deliverable 5); the
                    // append and the proof follow at the drain, in order.
                    self.validation.enqueue(ValidationJob {
                        link_id: *link_id,
                        carrier: Carrier::Packet,
                        data: data.clone(),
                        proof,
                        sender: SyncSender::default(),
                        min_cost,
                        compute_value,
                    });
                } else {
                    // Nothing to grind, or one stamp ground here (see
                    // `grinds_inline`), but the append and the proof are
                    // still the drain's: one dispatch can carry several
                    // uploads, and storing them all here is the same
                    // unbounded hold a batch is. The drain after this match
                    // stores it in this hook when the budget is unspent.
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
            // refuse before transfer when a static-only node does not know
            // the sender as a static peer, or when it exceeds the announced
            // per-sync limit, in that order
            // (reference/LXMF/LXMF/LXMRouter.py:2206-2224).
            //
            // A client upload (no validated `/offer` on the link) is held to
            // the delivery limit as well: above the transfer limit it is
            // stored local only (`is_local_only`), above the delivery limit
            // its recipient could not take it either. Each refusal is a PN_REJECT at
            // INFO with the size and the cap; the sender learns only that the
            // resource was refused (`RNS.Resource.REJECTED`, no reason on the
            // wire).
            NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } if Self::owns_link(core, &ready, link_id) => {
                let client = ready.peering.validated_peer(link_id).is_none();
                let refusal = if !ready.peering.admits_resource_on(core, link_id) {
                    Some(("not_static", 0))
                } else if !ready.node.accepts_resource_of(*data_size) {
                    Some(("over_sync_limit", ready.node.resource_limit_bytes()))
                } else if client && *data_size > ready.delivery_limit_kb.saturating_mul(1000) {
                    Some(("too_large", ready.delivery_limit_kb.saturating_mul(1000)))
                } else if !client && ready.peering.store_full() {
                    // An offer accepted before the store failed: refusing
                    // the resource keeps the peer's cursor where it was.
                    Some(("store_full", 0))
                } else {
                    None
                };
                let verdict = match refusal {
                    None => {
                        ready.peering.on_inbound_resource_accepted(core, link_id);
                        core.accept_resource(link_id)
                    }
                    Some((reason, limit)) => {
                        log_reject_sized(reason, "resource", *data_size, limit);
                        core.reject_resource(link_id)
                    }
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
                self.on_inbound_resource_completed(&mut ready, core, link_id, data);
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
                let batch_in_progress = self.batch_in_progress();
                let response = ready.peering.answer_offer_request(
                    core,
                    &ready.node,
                    link_id,
                    data,
                    batch_in_progress,
                );
                self.respond(core, link_id, request_id, &response, &mut out);
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &ready.destination_hash && path == LEV_STATUS_PATH => {
                let response = self.answer_lev_status(&ready, link_id, data, core.now_ms());
                self.respond(core, link_id, request_id, &response, &mut out);
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                self.status_asked_at.remove(link_id);
                self.pending_proofs.remove(link_id);
                self.unproven
                    .retain(|unproven| &unproven.link_id != link_id);
                self.inbound_links.remove(link_id);
                ready.peering.on_link_closed(link_id);
            }
            _ => {}
        }
        self.profile.lap(Section::Event);
        // Only an event that queued a payload stores one here: an upload
        // proven in the hook that received it waits for no wake at all.
        //
        // Every other event leaves the queue to `on_tick`. The driver hands
        // a dispatch's events to this hook one by one under ONE lock and
        // ONE budget (`run_event_tap`), so a drain per event stored as many
        // messages as the dispatch had events: an announce is two, its
        // `AnnounceReceived` and its `PathFound`, and during an inbound sync
        // every announce dispatch stored two messages against a budget that
        // pays for one (order 1009,
        // `an_announce_dispatch_during_a_sync_stores_at_most_one_slice`).
        if self.pending.len() > queued {
            self.drain_pending(&mut ready, core, &mut out);
            self.profile.lap(Section::Drain);
        } else {
            self.ask_back_if_queued(core, &mut out);
        }
        self.release_proofs(&mut ready, core, &mut out);
        self.ask_back_for_proofs(core, &mut out);
        self.log_departures(&mut ready);
        self.profile.lap(Section::Drain);
        self.state = State::Ready(ready);
        // A job queued for the worker, here or by an earlier hook, comes
        // back at the next poll: nothing else would wake the engine for
        // its verdict, and the proof waits on it.
        if self.validation.busy() {
            let due = core.now_ms() + VALIDATION_POLL_MS;
            out.next_deadline_ms = Some(match out.next_deadline_ms {
                Some(existing) => existing.min(due),
                None => due,
            });
        }
        self.finish_profile(Section::Event);
        out
    }

    fn on_tick(
        &mut self,
        core: &mut leviculum_std::driver::StdNodeCore,
        now_ms: u64,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        self.profile = HookProfile::start("on_tick");
        let Some(mut ready) = self.take_ready(core) else {
            self.finish_profile(Section::Ready);
            return out;
        };
        self.profile.lap(Section::Ready);

        let next_announce = *self
            .next_announce_at
            .get_or_insert(now_ms + self.announce_delay_secs * 1000);
        if now_ms >= next_announce {
            self.announce(&mut ready, core, self.next_announce_occasion, &mut out);
            // The interval runs from the deferred start announce, as
            // `lxmd`'s timer runs from its deferred start jobs
            // (`reference/LXMF/LXMF/Utilities/lxmd.py:506`).
            let wait_secs = if std::mem::take(&mut self.start_announce_owed) {
                START_ANNOUNCE_GAP_SECS
            } else {
                self.next_announce_occasion = "periodic";
                self.announce_interval_secs
            };
            self.next_announce_at = Some(now_ms + wait_secs * 1000);
        }
        self.profile.lap(Section::Announce);

        let sweep_due = now_ms >= self.next_maintenance_at;
        if sweep_due || self.expiry_backlog {
            // A slice a hook, so a backlog (a node back from a long stop)
            // does not hold the core lock for the whole sweep.
            let evicted = ready.node.tick_at_most(unix_secs(), MAX_EXPIRED_PER_TICK);
            self.expiry_backlog = evicted.len() >= MAX_EXPIRED_PER_TICK;
            log_evictions(&evicted, &self.events);
        }
        if sweep_due {
            // Every removal reports a departure, which drops its origin;
            // more origins than messages means one did not, and only then
            // is the map walked (LV-1003, `Origins::prune`).
            let stored = ready.node.store().count().unwrap_or(0);
            let expiry = ready.node.config().message_expiry_secs;
            ready
                .peering
                .origins_mut()
                .prune_if_above(stored, unix_secs(), expiry);
        }
        if sweep_due {
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
            ready.peering.store_swept();
            // Every sweep, so a displacement during a peer's sync, which
            // the engine does not see, is on disk within one period.
            self.save_departures(&ready, true);
            self.next_maintenance_at = now_ms + STORE_MAINTENANCE_SECS * 1000;
        }
        // The heap census (order 1515): booked a period after the first
        // tick, and at once when SIGUSR1 asked for it.
        if self.next_census_at == 0 {
            self.next_census_at = now_ms + CENSUS_PERIOD_MS;
        }
        let requested = self.census.take_request();
        if requested || now_ms >= self.next_census_at {
            let transport = crate::census::TransportCensus::of(core.storage());
            let census = self.census(&ready, transport);
            census.log(if requested { "signal" } else { "periodic" });
            self.census.publish(&census);
            if !requested {
                self.next_census_at = now_ms + CENSUS_PERIOD_MS;
            }
        }
        self.profile.lap(Section::Tick);

        self.drain_pending(&mut ready, core, &mut out);
        self.release_proofs(&mut ready, core, &mut out);
        self.save_departures(&ready, false);
        self.profile.lap(Section::Drain);
        self.close_idle_links(core, now_ms, &mut out);
        self.profile.lap(Section::Links);

        if let Some(mut mailbox) = ready.mailbox.take() {
            mailbox.on_tick(core, now_ms, &mut out);
            ready.mailbox = Some(mailbox);
        }
        self.profile.lap(Section::Mailbox);

        ready
            .peering
            .on_tick(core, &mut ready.node, now_ms, &mut out);
        self.log_departures(&mut ready);
        self.profile.lap(Section::Peering);

        self.state = State::Ready(ready);

        let validation_poll = if self.validation.busy() || !self.unproven.is_empty() {
            // Grinding in flight, or a proof waiting for the store's writer:
            // come back quickly so the proof follows with minimal added
            // latency.
            now_ms + VALIDATION_POLL_MS
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
        self.finish_profile(Section::Peering);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_core::packet::{Packet, PacketType};
    use leviculum_lxmf::peering::{SyncPhase, SYNC_INTERVAL_SECS};
    use leviculum_lxmf::{MemoryPeerStore, MemoryPropagationStore};
    use leviculum_std::driver::{CoreProcessor as _, StdClock, StdStorage};

    fn core(dir: &std::path::Path) -> leviculum_std::driver::StdNodeCore {
        NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir).expect("storage under a fresh temp dir"),
        )
    }

    /// The role's default config at stamp cost 0: these tests sync and
    /// upload messages carrying placeholder stamps, which the shipped
    /// default of 13 would reject as invalid.
    ///
    /// And without the host's value pricing at cost 0 (leviculum#442 D6):
    /// that is a workblock expansion per message, about 75 ms each in a
    /// debug build on the coder host, and none of these tests is about
    /// pricing. Left on, every ingest went through the worker and a
    /// 105-message sync made the slice test a race between the forge
    /// runner's CPU and a wall clock (Woodpecker 511). The tests that ARE
    /// about pricing turn it back on: `a_peerless_host_computes_stamp_values_at_cost_zero`
    /// by config, `a_deferred_sync_survives_the_peer_closing_the_link_before_the_drain`
    /// by the peer-demand flag.
    pub(super) fn cost_free() -> PropagationNodeConfig {
        PropagationNodeConfig {
            stamp_cost: 0,
            always_compute_stamp_value: false,
            ..PropagationNodeConfig::default()
        }
    }

    fn engine_with(
        control_allowed: Vec<[u8; 16]>,
        mailbox: Option<MailboxConfig>,
    ) -> (
        Engine<MemoryPropagationStore>,
        std::sync::mpsc::Receiver<EngineEvent>,
    ) {
        engine_configured(cost_free(), control_allowed, mailbox)
    }

    fn engine_configured(
        node_config: PropagationNodeConfig,
        control_allowed: Vec<[u8; 16]>,
        mailbox: Option<MailboxConfig>,
    ) -> (
        Engine<MemoryPropagationStore>,
        std::sync::mpsc::Receiver<EngineEvent>,
    ) {
        Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config,
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
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

    /// How many announces of `destination` a tick put on the wire.
    fn announces_of(out: &TickOutput, destination: [u8; 16]) -> usize {
        announces_in(out)
            .iter()
            .filter(|packet| packet.destination_hash == destination)
            .count()
    }

    /// When, in seconds after start, the node announced its propagation
    /// destination over the first `horizon_secs`, ticked once a second.
    fn node_announce_times(announce_at_start: bool, horizon_secs: u64) -> Vec<u64> {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: cost_free(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: ANNOUNCE_DELAY_SECS,
            announce_at_start,
            peering: leviculum_lxmf::PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = core.now_ms();
        let out = engine.on_tick(&mut core, start);
        let node = engine.destination_hash().expect("registered");
        let mut seen = vec![0; announces_of(&out, node)];
        for secs in 1..=horizon_secs {
            let out = engine.on_tick(&mut core, start + secs * 1000);
            seen.extend(std::iter::repeat_n(secs, announces_of(&out, node)));
        }
        seen
    }

    /// With `[propagation] announce_at_start` on, the node announces twice
    /// near start, as a stock `lxmd` does: the router's own announce
    /// `NODE_ANNOUNCE_DELAY` after the role comes up
    /// (`reference/LXMF/LXMF/LXMRouter.py:41`, `:685`), and the daemon's
    /// deferred start announce, which runs `DEFFERED_JOBS_DELAY` after
    /// start and goes through the same delayed announce
    /// (`reference/LXMF/LXMF/Utilities/lxmd.py:33`, `:501-503`;
    /// `LXMRouter.py:338-341`), so 10 s after the first. The interval then
    /// runs from the second (`lxmd.py:481-485`, `:506`). A node whose one
    /// start announce is lost on a busy carrier is otherwise unknown to
    /// its peers for the whole interval.
    #[test]
    fn the_node_announces_twice_at_start_then_on_the_interval() {
        let second = ANNOUNCE_DELAY_SECS + START_ANNOUNCE_GAP_SECS;
        assert_eq!(
            node_announce_times(true, second + 3600),
            vec![ANNOUNCE_DELAY_SECS, second, second + 3600],
            "router announce, deferred start announce, then the interval \
             from the second (seconds after start)"
        );
    }

    /// The engine re-seeds the role from the departures file when it
    /// registers and writes it back at the sweep, so what a previous run
    /// collected survives this one too (order 1294).
    #[test]
    fn the_engine_restores_its_departures_and_saves_them_at_the_sweep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("departures");
        let left = [0x6e; 32];
        let at = leviculum_lxmf::Departure {
            at_secs: unix_secs() - 60,
            reason: leviculum_lxmf::DepartureReason::Collected,
        };
        crate::departures::save(&path, [(&left, &at)].into_iter()).expect("saves");

        let mut core = core(dir.path());
        let (engine, _events) = engine_with(Vec::new(), None);
        let mut engine = engine.with_departures_path(path.clone());
        let now = core.now_ms();
        let _ = engine.on_tick(&mut core, now);
        match &engine.state {
            State::Ready(ready) => assert_eq!(
                ready.node.departures().collect::<Vec<_>>(),
                vec![(&left, &at)]
            ),
            _ => panic!("engine must be ready"),
        }
        std::fs::remove_file(&path).expect("the restored file was there");
        // The next sweep writes it again.
        engine.next_maintenance_at = 0;
        let _ = engine.on_tick(&mut core, now + 1);
        assert_eq!(
            crate::departures::load(&path).expect("saved at the sweep"),
            vec![(left, at)]
        );
    }

    /// A seeded upload envelope for `destination`, unique per `seed`.
    fn seeded_upload(destination: [u8; 16], seed: u8) -> Vec<u8> {
        use leviculum_lxmf::constants::LXMF_OVERHEAD;
        let mut lxmf = destination.to_vec();
        lxmf.resize(LXMF_OVERHEAD + 40, seed);
        PropagationUpload::single(1.0, lxmf, [0xEE; STAMP_SIZE]).encode()
    }

    fn seed(engine: &mut Engine<MemoryPropagationStore>, upload: &[u8]) -> TransientId {
        let State::Ready(ready) = &mut engine.state else {
            panic!("engine must be ready");
        };
        match ready
            .node
            .handle_upload(upload, unix_secs(), |_, _| Some(0))
        {
            UploadOutcome::Accepted { transient_id, .. } => transient_id,
            other => panic!("{other:?}"),
        }
    }

    /// Order 1394, the request path end to end: a client that never
    /// identifies asks `/lev/status` over a real link and gets each id's
    /// status; a second request on the same link inside the spacing is
    /// answered `Throttled`. The collection the node saw before leaves its
    /// `PN_DEPART` line with the full id.
    #[test]
    fn a_status_request_on_an_unidentified_link_answers_each_id() {
        use leviculum_lxmf::lev_status::{decode_answer, encode_request};
        use leviculum_lxmf::propagation::MessageGetRequest;
        use leviculum_lxmf::MessageStatus;

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: cost_free(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 3600,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };

        let recipient = [0x3c; 16];
        let held = seed(&mut engine, &seeded_upload(recipient, 1));
        let collected = seed(&mut engine, &seeded_upload(recipient, 2));
        {
            let State::Ready(ready) = &mut engine.state else {
                panic!("engine must be ready");
            };
            let acknowledge = MessageGetRequest::acknowledge(vec![collected]);
            ready
                .node
                .handle_get(
                    &acknowledge.encode().expect("encodes"),
                    &recipient,
                    unix_secs(),
                )
                .expect("the recipient confirms");
        }

        let unknown = [0x55; 32];
        let request = encode_request(&[held, collected, unknown]);
        enable_log_dispatch();
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let (first, second) = tracing::subscriber::with_default(subscriber, || {
            let (link_id, _, out) = client
                .connect(destination_hash, &signing_key)
                .expect("connect");
            let (events, _) = converse(&mut engine, &mut server, &mut client, out);
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, NodeEvent::LinkEstablished { .. })),
                "the link must come up: {events:?}"
            );
            assert!(
                server.get_remote_identity(&link_id).is_none(),
                "the link must stay unidentified, or the test proves nothing"
            );
            let mut ask = || {
                let (_, out) = client
                    .send_request(&link_id, LEV_STATUS_PATH, Some(&request), None)
                    .expect("request");
                let (events, _) = converse(&mut engine, &mut server, &mut client, out);
                events
                    .into_iter()
                    .find_map(|event| match event {
                        NodeEvent::ResponseReceived { response_data, .. } => Some(response_data),
                        _ => None,
                    })
                    .expect("the node must answer")
            };
            (ask(), ask())
        });
        assert_eq!(
            decode_answer(&first).expect("decodes"),
            Ok(vec![
                (held, MessageStatus::Held),
                (collected, MessageStatus::Collected),
                (unknown, MessageStatus::Unknown),
            ])
        );
        assert_eq!(
            decode_answer(&second).expect("decodes"),
            Err(PeerError::Throttled.code()),
            "a second request inside the spacing is throttled"
        );

        let log = capture.unquoted();
        let departs: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_DEPART"))
            .collect();
        assert_eq!(departs.len(), 1, "{log}");
        assert!(
            departs[0].contains(&format!("id={}", full_hex(&collected)))
                && departs[0].contains("reason=collected"),
            "{}",
            departs[0]
        );
        let statuses: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_STATUS"))
            .collect();
        assert_eq!(statuses.len(), 2, "{log}");
        assert!(statuses[0].contains("ids=3") && statuses[0].contains("result=answered"));
        assert!(statuses[1].contains("result=throttled"), "{}", statuses[1]);
    }

    /// The own-mailbox path records `delivered`, not `collected`: the
    /// operator's mailbox took it, no recipient's client confirmed.
    #[test]
    fn an_own_mailbox_delivery_departs_as_delivered() {
        use leviculum_lxmf::{DepartureReason, MessageStatus};

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, _events) = engine_with(Vec::new(), Some(mailbox_config()));
        let now = core.now_ms();
        let _ = engine.on_tick(&mut core, now);
        let mailbox_hash = match &engine.state {
            State::Ready(ready) => ready.mailbox.as_ref().expect("mailbox").delivery_hash,
            _ => panic!("engine must be ready"),
        };
        let mine = seed(&mut engine, &seeded_upload(mailbox_hash, 1));
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let _ = ready.node.take_fresh_departures();
        let mut out = TickOutput::empty();
        engine.deliver_own_mailbox(&mut ready, &mut core, &mine, &mailbox_hash, &mut out);
        assert_eq!(ready.node.status_of(&mine), MessageStatus::Delivered);
        assert_eq!(
            ready.node.take_fresh_departures(),
            vec![(mine, DepartureReason::Delivered)]
        );
        assert!(engine.departures_dirty);
    }

    /// Each of those announces leaves an `ANN_TX` line in the node's own
    /// log, so the log proves the cadence: the core logs none for an
    /// announce it sends on every interface, and on miauhaus the 6-hour
    /// announces were visible only on lnsd's side (1294).
    #[test]
    fn every_own_announce_logs_an_ann_tx_with_its_occasion() {
        enable_log_dispatch();
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let second = ANNOUNCE_DELAY_SECS + START_ANNOUNCE_GAP_SECS;
        let times = tracing::subscriber::with_default(subscriber, || {
            node_announce_times(true, second + 3600)
        });
        assert_eq!(times.len(), 3, "the cadence the test above pins");
        let log = capture.unquoted();
        let occasions: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=ANN_TX"))
            .map(|line| {
                assert!(
                    line.contains("hops=0") && line.contains("iface=all"),
                    "{line}"
                );
                line.split_whitespace()
                    .find_map(|field| field.strip_prefix("occasion="))
                    .unwrap_or("-")
            })
            .collect();
        assert_eq!(occasions, vec!["start", "start", "periodic"], "{log}");
    }

    /// With `announce_at_start = no` only the router's own start announce
    /// goes out, as in `lxmd`, and the interval runs from it.
    #[test]
    fn without_announce_at_start_the_node_announces_once_at_start() {
        assert_eq!(
            node_announce_times(false, ANNOUNCE_DELAY_SECS + 3600),
            vec![ANNOUNCE_DELAY_SECS, ANNOUNCE_DELAY_SECS + 3600],
            "seconds after start"
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

    /// leviculum#442 D6: a peerless host computes true stamp values at cost
    /// 0 from the start. The engine clears the peer-demand flag while no
    /// peer requires a value; that must not stop the host computing them,
    /// or what it accepts before its first demanding peer is stored at 0
    /// and never offered to a stock peer (`LXMPeer.py:340`).
    #[test]
    fn a_peerless_host_computes_stamp_values_at_cost_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        // The host's shipped default, which `cost_free` turns off.
        let host = PropagationNodeConfig {
            stamp_cost: 0,
            ..PropagationNodeConfig::default()
        };
        let (mut engine, _events) = engine_configured(host, Vec::new(), None);
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);
        match &mut engine.state {
            State::Ready(ready) => {
                assert_eq!(ready.min_cost, 0);
                assert!(!ready.peering.requires_stamp_values());
                ready
                    .node
                    .set_compute_stamp_value(ready.peering.requires_stamp_values());
                assert!(ready.node.compute_stamp_value());
            }
            _ => panic!("engine must be ready"),
        }
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
        engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope.encode());
        // The close lands before any drain could run — the deterministic
        // stand-in for the LinkClosed event the engine routes here.
        ready.peering.on_link_closed(&link_id);
        engine.state = State::Ready(ready);

        // Liveness, not speed: the two stamps are ground on the worker, at
        // whatever pace the machine running the test allows.
        let deadline = std::time::Instant::now() + WORKER_LIVENESS;
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
            engine.on_inbound_resource_completed(&mut ready, &core, &client_link, &envelope);
            absorb(&mut engine, &mut ready, &mut core, &mut out);
            engine.on_inbound_resource_completed(&mut ready, &core, &peer_link, &envelope);
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
            node_config: cost_free(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
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
        // A host computes stamp values at cost 0 too (leviculum#442 D6), so
        // every ingest goes through the worker first.
        await_validation(engine);
        for _ in 0..10_000 {
            let Some(pending) = engine.pending.pop_front() else {
                return;
            };
            engine.store_one(ready, core, pending, PERSIST_PER_HOOK, out);
        }
        panic!("the store queue did not drain");
    }

    /// How long the validation worker may go without answering before a
    /// test calls it hung. A liveness bound, not a speed bound: a wall-clock
    /// budget for the whole wait measured the runner's CPU, and a fixed 10 s
    /// went red on the forge for a worker that was grinding correctly
    /// (Woodpecker 511, 105 messages at about 75 ms each in debug).
    pub(super) const WORKER_LIVENESS: std::time::Duration = std::time::Duration::from_secs(60);

    /// Move every deferred validation onto the store queue, waiting for the
    /// worker as the driver's next hook would. Bounded by progress, so a
    /// worker that stops answering fails the test instead of hanging it,
    /// while a slow machine only waits longer: every answer restarts the
    /// [`WORKER_LIVENESS`] budget.
    pub(super) fn await_validation<S: PropagationStore>(engine: &mut Engine<S>) {
        let mut deadline = std::time::Instant::now() + WORKER_LIVENESS;
        while engine.validation.busy() {
            match engine.validation.try_recv() {
                Some(done) => {
                    engine.pending.push_back(Pending::from_validated(done));
                    deadline = std::time::Instant::now() + WORKER_LIVENESS;
                }
                None if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                None => panic!("the validation worker did not answer"),
            }
        }
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
        engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
        absorb(&mut engine, &mut ready, &mut core, &mut out);
        engine.state = State::Ready(ready);

        assert_eq!(
            peer_count(&engine),
            1,
            "a node that synced its store to us, one hop out and with a \
             recallable propagation announce, must be a peer"
        );
    }

    /// A message a peer sync displaces reaches the operator line exactly as
    /// one a client upload displaces: `EngineEvent::Evicted`, which `main.rs`
    /// prints as `lnpnd: evicted N message(s)` at the default log level.
    /// The sync arm used to write only the `PN_EVICT` debug events, so on a
    /// node whose store fills by peer sync every cull was invisible (528).
    #[test]
    fn a_sync_that_displaces_a_message_tells_the_operator() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        // Room for two synced messages, not three.
        let (mut engine, events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: cost_free(),
            store: MemoryPropagationStore::new(400),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 400,
            delivery_limit_kb: 1000,
        });
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);

        let link_id = LinkId::new([0x42; 16]);
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        ready.peering.validate_link_for_tests(link_id, [0x24; 16]);
        let envelope = leviculum_lxmf::PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![
                synced_message(0x01),
                synced_message(0x03),
                synced_message(0x05),
            ],
        }
        .encode();
        let mut out = TickOutput::empty();
        engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
        absorb(&mut engine, &mut ready, &mut core, &mut out);
        let stored = ready.node.store().count().unwrap_or(0);
        engine.state = State::Ready(ready);
        assert_eq!(stored, 2, "the third message must displace one");

        let evicted: Vec<usize> = events
            .try_iter()
            .filter_map(|event| match event {
                EngineEvent::Evicted { count } => Some(count),
                _ => None,
            })
            .collect();
        assert_eq!(evicted, vec![1], "one displaced message, one operator line");
    }

    /// Order 1295: a store holding more expired messages than one slice
    /// drains them a slice per hook, each slice on the operator line, and
    /// the hook asks to come back at the poll interval rather than a
    /// maintenance period later; once drained, nothing is evicted until the
    /// next sweep. Positive control: the backlog is real, a single
    /// unbounded tick takes all of it.
    #[test]
    fn an_expiry_backlog_drains_a_slice_a_hook() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let backlog = 2 * MAX_EXPIRED_PER_TICK + 44;
        let received_at = unix_secs() - 40 * 24 * 60 * 60;
        let mut store = MemoryPropagationStore::new(1_000_000);
        for n in 0..backlog {
            let mut transient_id = [0u8; 32];
            transient_id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            store
                .append(&transient_id, received_at, 0, &synced_message(n as u8))
                .expect("room");
        }
        let mut control = leviculum_lxmf::PropagationNode::new(store.clone(), cost_free());
        assert_eq!(control.tick(unix_secs()).len(), backlog, "positive control");

        let (mut engine, events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: cost_free(),
            store,
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 1_000_000,
            delivery_limit_kb: 1000,
        });
        let start_ms = core.now_ms();
        let mut slices = Vec::new();
        for hook in 0..5u64 {
            let now_ms = start_ms + hook * POLL_INTERVAL_MS;
            let out = engine.on_tick(&mut core, now_ms);
            if hook < 2 {
                assert!(
                    out.next_deadline_ms
                        .is_some_and(|due| due <= now_ms + POLL_INTERVAL_MS),
                    "hook {hook}: a backlog comes back at the poll interval"
                );
            }
            slices.extend(events.try_iter().filter_map(|event| match event {
                EngineEvent::Evicted { count } => Some(count),
                _ => None,
            }));
        }
        assert_eq!(
            slices,
            vec![MAX_EXPIRED_PER_TICK, MAX_EXPIRED_PER_TICK, 44],
            "a slice a hook, then nothing until the next sweep"
        );
        let State::Ready(ready) = &engine.state else {
            panic!("engine must be ready");
        };
        assert_eq!(ready.node.store().count(), Ok(0));
        assert_eq!(
            ready.node.evicted_total(),
            backlog as u64,
            "the status count follows the evictions"
        );
        assert_eq!(
            engine.next_maintenance_at,
            start_ms + STORE_MAINTENANCE_SECS * 1000,
            "the drain does not move the sweep's cadence"
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
            engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
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

    /// 538: every refused candidate leaves a `PN_PEER action=decline` line
    /// naming why. The field case: after a restart the announce is still
    /// recallable but the path is gone, so `hops_to` answers `None`, the sync
    /// is declined as too deep, and until now nothing in lnpnd's log said
    /// so. The link ids here are not in the core, so no link hop count
    /// stands in for the path either (556 covers that case below). The second sync comes from a node we already peer, which must stay
    /// silent: it is every sync from every peer.
    #[test]
    fn a_declined_sync_says_why_and_a_known_peer_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, events) = peering_engine(PeeringConfig {
            autopeer_maxdepth: 1,
            ..PeeringConfig::default()
        });
        let start_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, start_ms);

        // Recallable, but with no path: the state a restart leaves behind.
        // Once the known node below is a peer, stamp values are computed,
        // so both syncs go through the validation worker.
        let (pathless, packet) = propagation_announce(core.emission_secs(), 0);
        hear_announce_off_the_role(&mut core, &packet);
        assert!(core.remove_path(&pathless), "the path must have existed");
        assert!(
            core.recall_app_data(&DestinationHash::new(pathless))
                .is_some(),
            "the announce must still be recallable, or this is the client case"
        );
        assert_eq!(core.hops_to(&DestinationHash::new(pathless)), None);
        // A node peered by its live announce.
        let (known, packet) = propagation_announce(core.emission_secs(), 0);
        let delivered = core.handle_packet(leviculum_core::InterfaceId(0), &packet);
        for event in &delivered.events {
            let _ = engine.on_event(&mut core, event);
        }
        assert_eq!(peer_count(&engine), 1, "the known node must be a peer");
        let _ = events.try_iter().count();

        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        enable_log_dispatch();
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut out = TickOutput::empty();
            for (index, remote) in [pathless, known].into_iter().enumerate() {
                let link_id = LinkId::new([0x70 + index as u8; 16]);
                ready.peering.validate_link_for_tests(link_id, remote);
                let envelope = leviculum_lxmf::PeerSyncEnvelope {
                    timestamp: 1.0,
                    messages: vec![synced_message(0x20 + index as u8)],
                }
                .encode();
                engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
                await_validation(&mut engine);
                absorb(&mut engine, &mut ready, &mut core, &mut out);
            }
        });
        let stored = ready.node.store().count().unwrap_or(0);
        engine.state = State::Ready(ready);
        let log = capture.unquoted();
        assert_eq!(stored, 2, "both syncs must have landed: {log}");
        let declines: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("PN_PEER") && line.contains("action=decline"))
            .collect();
        assert_eq!(declines.len(), 1, "one decline line, got: {declines:?}");
        assert!(
            declines[0].contains(&format!("peer={}", hex::encode(pathless)))
                && declines[0].contains("reason=too_deep"),
            "the pathless node must be declined as too_deep: {}",
            declines[0]
        );
        let operator: Vec<(&'static str, [u8; 16], &'static str)> = events
            .try_iter()
            .filter_map(|event| match event {
                EngineEvent::Peer {
                    action,
                    destination_hash,
                    reason,
                } => Some((action, destination_hash, reason)),
                _ => None,
            })
            .collect();
        assert_eq!(operator, vec![("decline", pathless, "too_deep")]);
    }

    /// 556: the sibling of the test above, over a real link. After a
    /// restart the path is gone, but the node syncing into us is on a link,
    /// and the link knows how far away it is. A link inside
    /// `autopeer_maxdepth` peers the node as `inbound_sync`; a link beyond
    /// it is still declined `too_deep`, so the depth rule is read off the
    /// link rather than switched off. The link is closed before the
    /// validation comes back, as it is in the cell (643): its hops are
    /// read while it still exists. With `advertised`, the core forgets the
    /// link even before the resource concludes, and only what was noted at
    /// the advertisement is left to judge by.
    #[test]
    fn a_pathless_node_syncing_over_a_link_is_judged_by_the_links_hops() {
        for (link_hops, expected, advertised) in [
            (1u8, ("add", "inbound_sync"), false),
            (2, ("decline", "too_deep"), false),
            (1, ("add", "inbound_sync"), true),
            (2, ("decline", "too_deep"), true),
        ] {
            let server_dir = tempfile::tempdir().expect("tempdir");
            let client_dir = tempfile::tempdir().expect("tempdir");
            let mut server = core(server_dir.path());
            let mut client = core(client_dir.path());
            let identity = leviculum_std::generate_identity();
            let signing_key = identity.ed25519_verifying().to_bytes();
            let (mut engine, events) = Engine::new(EngineConfig {
                identity,
                node_config: cost_free(),
                store: MemoryPropagationStore::new(64_000),
                announce_interval_secs: 3600,
                announce_delay_secs: 0,
                announce_at_start: true,
                peering: PeeringConfig {
                    autopeer_maxdepth: 1,
                    ..PeeringConfig::default()
                },
                peer_store: Box::new(MemoryPeerStore::default()),
                control_allowed: Vec::new(),
                auth_allowed: None,
                mailbox: None,
                store_limit_bytes: 64_000,
                delivery_limit_kb: 1000,
            });
            let start = server.now_ms();
            let _ = engine.on_tick(&mut server, start);
            let destination_hash = match &engine.state {
                State::Ready(ready) => ready.destination_hash,
                _ => panic!("engine must be ready"),
            };

            // Recallable, but with no path: the state a restart leaves.
            let (pathless, packet) = propagation_announce(server.emission_secs(), 0);
            hear_announce_off_the_role(&mut server, &packet);
            assert!(server.remove_path(&pathless), "the path must have existed");
            assert_eq!(server.hops_to(&DestinationHash::new(pathless)), None);

            let (link_id, _, out) = client
                .connect(destination_hash, &signing_key)
                .expect("connect");
            let _ = converse(&mut engine, &mut server, &mut client, out);
            server
                .link_mut(&link_id)
                .filter(|link| link.is_active())
                .expect("the link must come up, or the test proves nothing")
                .set_hops(link_hops);
            let _ = events.try_iter().count();

            let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
                State::Ready(ready) => ready,
                _ => panic!("engine must be ready"),
            };
            ready.peering.validate_link_for_tests(link_id, pathless);
            let envelope = leviculum_lxmf::PeerSyncEnvelope {
                timestamp: 1.0,
                messages: vec![synced_message(0x30)],
            }
            .encode();
            let mut out = TickOutput::empty();
            if advertised {
                ready
                    .peering
                    .on_inbound_resource_accepted(&server, &link_id);
                let _ = server.close_link(&link_id);
            }
            engine.on_inbound_resource_completed(&mut ready, &server, &link_id, &envelope);
            // 643: the peer tears the link down the moment its transfer
            // concludes, before the worker hands the validation back (a
            // one-message sync took 4 ms on the cell, the close followed at
            // once). The core forgets the link, and so does the engine.
            let _ = server.close_link(&link_id);
            ready.peering.on_link_closed(&link_id);
            assert!(server.link(&link_id).is_none(), "the link must be gone");
            await_validation(&mut engine);
            absorb(&mut engine, &mut ready, &mut server, &mut out);
            let stored = ready.node.store().count().unwrap_or(0);
            engine.state = State::Ready(ready);
            assert_eq!(stored, 1, "the sync must have landed");

            let operator: Vec<(&'static str, [u8; 16], &'static str)> = events
                .try_iter()
                .filter_map(|event| match event {
                    EngineEvent::Peer {
                        action,
                        destination_hash,
                        reason,
                    } => Some((action, destination_hash, reason)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                operator,
                vec![(expected.0, pathless, expected.1)],
                "a link of {link_hops} hops at autopeer_maxdepth 1, advertised {advertised}"
            );
            assert_eq!(
                peer_count(&engine),
                usize::from(expected.0 == "add"),
                "a link of {link_hops} hops at autopeer_maxdepth 1, advertised {advertised}"
            );
        }
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

    /// The path requests in one tick's output: Data broadcasts whose
    /// payload starts with the requested destination hash.
    fn path_requests_for(out: &TickOutput, destination: &[u8; 16]) -> usize {
        out.actions
            .iter()
            .filter_map(|action| match action {
                leviculum_core::transport::Action::Broadcast { data, .. } => {
                    Packet::unpack(data).ok()
                }
                _ => None,
            })
            .filter(|packet| {
                packet.flags.packet_type == PacketType::Data
                    && packet.data.as_slice().starts_with(destination)
            })
            .count()
    }

    fn peer_phase(
        engine: &Engine<MemoryPropagationStore>,
        destination: &[u8; 16],
    ) -> Option<(SyncPhase, u64, u8)> {
        match &engine.state {
            State::Ready(ready) => ready
                .peering
                .peer_for_tests(destination)
                .map(|peer| (peer.state, peer.last_heard, peer.stamp_cost)),
            _ => panic!("engine must be ready"),
        }
    }

    /// A static peering whose peer has never been heard: one stored message
    /// for it, and the engine's first tick already taken. Returns the
    /// peer's hash and the announce it would put on the air.
    fn static_engine_with_a_message(
        core: &mut leviculum_std::driver::StdNodeCore,
    ) -> (
        Engine<MemoryPropagationStore>,
        [u8; 16],
        Vec<u8>,
        TickOutput,
    ) {
        let (remote, packet) = propagation_announce(core.emission_secs(), 0);
        let (mut engine, _events) = peering_engine(PeeringConfig {
            // Autopeering off: whatever peers the remote here is the static
            // list, nothing else.
            autopeer: false,
            static_peers: vec![remote],
            ..PeeringConfig::default()
        });
        let start_ms = core.now_ms();
        let first = engine.on_tick(core, start_ms);
        match &mut engine.state {
            State::Ready(ready) => ready
                .node
                .store_mut()
                .append(&[0x77; 32], 1, 0, &synced_message(0x31))
                .expect("a message for the peer"),
            _ => panic!("engine must be ready"),
        }
        (engine, remote, packet, first)
    }

    /// 541: a configured static peer that was never heard is in the table
    /// and asked for from the first tick. The reference creates an
    /// `LXMPeer` for every static peer missing from the restored table and
    /// requests its path when `last_heard == 0`, inside `enable_propagation`
    /// (`reference/LXMF/LXMF/LXMRouter.py:633-642`); before this, an entry
    /// only appeared when the peer announced on its own, up to six hours.
    #[test]
    fn a_static_peer_is_in_the_table_and_asked_for_from_the_first_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, remote, packet, first) = static_engine_with_a_message(&mut core);

        assert_eq!(
            peer_count(&engine),
            1,
            "the static peer must be in the table before it is ever heard"
        );
        assert_eq!(
            path_requests_for(&first, &remote),
            1,
            "the first tick must request the never-heard static peer's path"
        );

        // Its announce facts are unknown, so no round may start: the
        // reference postpones a peer whose stamp costs are not yet known
        // (`stamp_costs_known`, `reference/LXMF/LXMF/LXMPeer.py:271-282`)
        // rather than mine a key at a cost nobody announced. The stack
        // knows its identity (heard off the role), so nothing but that
        // rule keeps the round from mining.
        hear_announce_off_the_role(&mut core, &packet);
        let later = core.now_ms() + (SYNC_INTERVAL_SECS + 1) * 1000;
        let _ = engine.on_tick(&mut core, later);
        assert_eq!(
            peer_phase(&engine, &remote).map(|(phase, ..)| phase),
            Some(SyncPhase::Idle),
            "a static peer with unknown costs must not start a round"
        );
    }

    /// The first path response for a never-heard static peer fills in its
    /// facts (`not is_path_response or static_peer.last_heard == 0`,
    /// `reference/LXMF/LXMF/Handlers.py:68-70`), and the next scheduler pass
    /// starts a round toward it.
    #[test]
    fn a_static_peers_first_path_response_fills_it_and_a_round_begins() {
        use leviculum_core::packet::PacketContext;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, remote, announce, _first) = static_engine_with_a_message(&mut core);

        let mut packet = Packet::unpack(&announce).expect("unpack the announce");
        packet.context = PacketContext::PathResponse;
        let mut buf = vec![0u8; 600];
        let len = packet.pack(&mut buf).expect("repack the announce");
        let delivered = core.handle_packet(leviculum_core::InterfaceId(0), &buf[..len]);
        for event in &delivered.events {
            let _ = engine.on_event(&mut core, event);
        }
        let (_, last_heard, stamp_cost) =
            peer_phase(&engine, &remote).expect("the static peer must still be there");
        assert!(last_heard > 0, "the path response must mark it heard");
        assert_eq!(stamp_cost, 16, "the path response must fill in its costs");

        let later = core.now_ms() + (SYNC_INTERVAL_SECS + 1) * 1000;
        let _ = engine.on_tick(&mut core, later);
        assert_ne!(
            peer_phase(&engine, &remote).map(|(phase, ..)| phase),
            Some(SyncPhase::Idle),
            "the filled static peer must be synced on the next pass"
        );
    }

    /// The same, when the static peer syncs to us before any path response
    /// arrives: its recalled announce fills the entry
    /// (`propagation_resource_concluded`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2355-2375`, which the reference
    /// itself skips for any hash already in its table, a never-heard static
    /// entry included), and a round toward it begins.
    #[test]
    fn a_static_peers_recalled_announce_fills_it_and_a_round_begins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (mut engine, remote, announce, _first) = static_engine_with_a_message(&mut core);
        hear_announce_off_the_role(&mut core, &announce);

        let link_id = LinkId::new([0x43; 16]);
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        ready.peering.validate_link_for_tests(link_id, remote);
        let envelope = leviculum_lxmf::PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![synced_message(0x41)],
        }
        .encode();
        let mut out = TickOutput::empty();
        engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
        await_validation(&mut engine);
        absorb(&mut engine, &mut ready, &mut core, &mut out);
        engine.state = State::Ready(ready);

        let (_, last_heard, stamp_cost) =
            peer_phase(&engine, &remote).expect("the static peer must still be there");
        assert!(last_heard > 0, "the recalled announce must mark it heard");
        assert_eq!(
            stamp_cost, 16,
            "the recalled announce must fill in its costs"
        );

        let later = core.now_ms() + (SYNC_INTERVAL_SECS + 1) * 1000;
        let _ = engine.on_tick(&mut core, later);
        assert_ne!(
            peer_phase(&engine, &remote).map(|(phase, ..)| phase),
            Some(SyncPhase::Idle),
            "the filled static peer must be synced on the next pass"
        );
    }

    // ------------------------------------------------------------------
    // 542: a closed node says so in its announce and at its door
    // ------------------------------------------------------------------

    /// A node peering from static peers only announces field 2 = false,
    /// as the reference does (`node_state = propagation_node and not
    /// from_static_only`, `reference/LXMF/LXMF/LXMRouter.py:326`): strangers
    /// then neither autopeer nor select it, and existing autopeers unpeer it
    /// (`reference/LXMF/LXMF/Handlers.py:98-99`). An open node still says
    /// true.
    #[test]
    fn a_static_only_node_announces_itself_closed() {
        use leviculum_lxmf::PropagationNodeAnnounce;

        for from_static_only in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut core = core(dir.path());
            let (mut engine, _events) = peering_engine(PeeringConfig {
                from_static_only,
                ..PeeringConfig::default()
            });
            let start = core.now_ms();
            let out = engine.on_tick(&mut core, start + 1);
            let destination_hash = match &engine.state {
                State::Ready(ready) => ready.destination_hash,
                _ => panic!("engine must be ready"),
            };
            let app_data = wire_of(&out)
                .into_iter()
                .find(|raw| {
                    Packet::unpack(raw).is_ok_and(|packet| {
                        packet.flags.packet_type == PacketType::Announce
                            && packet.destination_hash == *destination_hash.as_bytes()
                    })
                })
                .and_then(|raw| leviculum_core::cached_announce_app_data(&raw))
                .expect("the propagation destination announces at start");
            let decoded = PropagationNodeAnnounce::decode(&app_data).expect("our app data decodes");
            assert_eq!(
                decoded.enabled, !from_static_only,
                "from_static_only = {from_static_only} must announce field 2 = {}",
                !from_static_only
            );
        }
    }

    /// Every wire packet one output puts on the air.
    fn wire_of(out: &TickOutput) -> Vec<Vec<u8>> {
        out.actions
            .iter()
            .map(|action| match action {
                leviculum_core::transport::Action::SendPacket { data, .. }
                | leviculum_core::transport::Action::Broadcast { data, .. } => data.clone(),
            })
            .collect()
    }

    /// Ship the client's first output to the engine's core and play the
    /// exchange out until both sides fall silent. Every event the engine's
    /// core raises goes through the engine, as the driver routes it.
    /// Returns the client's events and every packet the engine's side sent.
    fn converse<S: PropagationStore + PropagationBodyStore + WriteBehind + Send + 'static>(
        engine: &mut Engine<S>,
        server: &mut leviculum_std::driver::StdNodeCore,
        client: &mut leviculum_std::driver::StdNodeCore,
        first: TickOutput,
    ) -> (Vec<NodeEvent>, Vec<Packet>) {
        let mut client_events = first.events.clone();
        let mut server_sent = Vec::new();
        let mut toward_server = wire_of(&first);
        for _ in 0..64 {
            if toward_server.is_empty() {
                return (client_events, server_sent);
            }
            let mut toward_client = Vec::new();
            for data in toward_server.drain(..) {
                let out = server.handle_packet(leviculum_core::InterfaceId(0), &data);
                toward_client.extend(wire_of(&out));
                for event in &out.events {
                    toward_client.extend(wire_of(&engine.on_event(server, event)));
                }
            }
            server_sent.extend(
                toward_client
                    .iter()
                    .filter_map(|data| Packet::unpack(data).ok()),
            );
            for data in toward_client {
                let out = client.handle_packet(leviculum_core::InterfaceId(0), &data);
                client_events.extend(out.events.iter().cloned());
                toward_server.extend(wire_of(&out));
            }
        }
        panic!("the exchange did not fall silent");
    }

    /// Bring up a link from `client` to the engine's propagation
    /// destination and identify it as `sender`, asserting both steps
    /// landed on the engine's side.
    fn identified_link(
        engine: &mut Engine<MemoryPropagationStore>,
        server: &mut leviculum_std::driver::StdNodeCore,
        client: &mut leviculum_std::driver::StdNodeCore,
        destination_hash: DestinationHash,
        signing_key: &[u8; 32],
        sender: &Identity,
    ) -> LinkId {
        let (link_id, _, out) = client
            .connect(destination_hash, signing_key)
            .expect("connect");
        let (events, _) = converse(engine, server, client, out);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, NodeEvent::LinkEstablished { .. })),
            "the link must come up, or the test proves nothing: {events:?}"
        );
        let out = client.identify_link(&link_id, sender).expect("identify");
        let _ = converse(engine, server, client, out);
        assert_eq!(
            server.get_remote_identity(&link_id).map(|id| *id.hash()),
            Some(*sender.hash()),
            "the engine's side must know who is on the link"
        );
        link_id
    }

    /// An inbound link to the propagation destination that carries no
    /// payload for `P_LINK_MAX_INACTIVITY` is torn down, as the reference
    /// tears it down (`clean_links`, `reference/LXMF/LXMF/LXMRouter.py:968-974`).
    /// The core alone never would: the client's keepalives refresh its
    /// watchdog, so its own bound (about 211 s on a LAN, the stale time
    /// plus RTT margin) only catches a peer that went dark, and a client
    /// that holds a silent link open held a link-table slot forever
    /// (Codeberg #442).
    #[test]
    fn an_idle_inbound_link_is_torn_down_after_the_reference_bound() {
        use leviculum_lxmf::constants::PROPAGATION_LINK_MAX_INACTIVITY_SECS as BOUND;

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: cost_free(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            // The deferred start announce would otherwise go out with the
            // first tick past start, the one at the bound below, which
            // asserts a silent wire.
            announce_at_start: false,
            peering: PeeringConfig {
                autopeer: false,
                ..PeeringConfig::default()
            },
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };
        let (link_id, _, out) = client
            .connect(destination_hash, &signing_key)
            .expect("connect");
        let _ = converse(&mut engine, &mut server, &mut client, out);
        assert!(
            server.link(&link_id).is_some_and(|link| link.is_active()),
            "the link must come up, or the test proves nothing"
        );
        // The data clock starts at establishment, to the millisecond since
        // de601f92 (#664). Rounding it down to a whole second, as this read
        // before, put the tick past the bound short of a full second past it
        // whenever establishment fell inside a second, so the strict `>`
        // never fired. Read the link's own clock instead.
        let now_ms = server.now_ms();
        let established_ms = now_ms
            - server
                .link(&link_id)
                .and_then(|link| link.no_data_for_ms(now_ms))
                .expect("an established link has a data clock");

        let out = engine.on_tick(&mut server, established_ms + BOUND * 1000);
        assert!(
            server.link(&link_id).is_some(),
            "a link idle for exactly the bound stays: the reference tears down strictly above it"
        );
        assert!(wire_of(&out).is_empty(), "nothing goes out at the bound");

        let out = engine.on_tick(&mut server, established_ms + (BOUND + 1) * 1000);
        assert!(
            server.link(&link_id).is_none(),
            "a link idle past the bound must be torn down"
        );
        let closes = wire_of(&out)
            .iter()
            .filter_map(|data| Packet::unpack(data).ok())
            .filter(|packet| packet.context == leviculum_core::packet::PacketContext::LinkClose)
            .count();
        assert_eq!(closes, 1, "the client must hear the teardown");
    }

    /// Advertise one propagation resource from `sender` over an identified
    /// link to a `from_static_only` engine listing `static_peers`, and
    /// report whether the engine's side requested it (accept) or cancelled
    /// it (reject) before a byte of it moved.
    fn advertise_to_a_closed_node(sender: &Identity, static_peers: Vec<[u8; 16]>) -> (bool, bool) {
        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: cost_free(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig {
                autopeer: false,
                from_static_only: true,
                static_peers,
                ..PeeringConfig::default()
            },
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };

        let link_id = identified_link(
            &mut engine,
            &mut server,
            &mut client,
            destination_hash,
            &signing_key,
            sender,
        );

        let upload = vec![0x5a; 2_000];
        let (_, out) = client
            .send_resource(&link_id, &upload, None, false)
            .expect("advertise");
        let (_, sent) = converse(&mut engine, &mut server, &mut client, out);
        let requested = sent
            .iter()
            .any(|packet| packet.context == leviculum_core::packet::PacketContext::ResourceReq);
        let cancelled = sent
            .iter()
            .any(|packet| packet.context == leviculum_core::packet::PacketContext::ResourceRcl);
        (requested, cancelled)
    }

    /// A node peering from static peers only refuses a resource from a link
    /// whose identity is not a static peer before a byte moves, and still
    /// takes one from a static peer: `propagation_resource_advertised`
    /// checks the remote identity, then the static list, then the size
    /// (`reference/LXMF/LXMF/LXMRouter.py:2206-2224`).
    #[test]
    fn a_static_only_node_takes_resources_from_static_peers_alone() {
        let stranger = leviculum_std::generate_identity();
        let (requested, cancelled) = advertise_to_a_closed_node(&stranger, vec![[0x24; 16]]);
        assert!(
            cancelled && !requested,
            "a stranger's resource must be rejected at the advertisement \
             (requested={requested} cancelled={cancelled})"
        );

        let peer = leviculum_std::generate_identity();
        let name_hash = Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]);
        let peer_hash = *Destination::compute_destination_hash(&name_hash, peer.hash()).as_bytes();
        let (requested, cancelled) = advertise_to_a_closed_node(&peer, vec![peer_hash]);
        assert!(
            requested && !cancelled,
            "a static peer's resource must be accepted \
             (requested={requested} cancelled={cancelled})"
        );
    }

    /// An identified client whose upload resource carried an invalid stamp
    /// is throttled for `PN_STAMP_THROTTLE`, exactly as a peer's sync is:
    /// the reference throttles any identified remote of a resource transfer
    /// with invalid stamps (`propagation_resource_concluded`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2447-2454`), and its next `/offer`
    /// answers `ERROR_THROTTLED` (`offer_request`, `:2286-2291`). Without
    /// it the remote can resubmit at link rate, each attempt a full stamp
    /// validation on this node.
    /// LV-1000 (order 1515): a peer's sync batch that has arrived and is
    /// not yet all stored postpones every other non-static peer's `/offer`
    /// with `ERROR_THROTTLED`, as the reference's sequential validation
    /// does (`reference/LXMF/LXMF/LXMRouter.py:2273-2278`). Without the gate
    /// 14 peers queued batches faster than the store took them, and the
    /// queue grew from 6.8 to 37 MB in seven minutes of the growth soak.
    #[test]
    fn a_peer_batch_in_progress_postpones_the_next_offer() {
        use leviculum_lxmf::{OfferResponse, PeerSyncEnvelope};

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: cost_free(),
            store: MemoryPropagationStore::new(4_000_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 4_000_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };

        // A validated peer's batch of 50 messages, arrived and queued.
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let batch_link = LinkId::new([0x42; 16]);
        ready
            .peering
            .validate_link_for_tests(batch_link, [0x17; 16]);
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: (0..50u32)
                .map(|seed| {
                    let mut message = vec![0x11u8; leviculum_lxmf::constants::LXMF_OVERHEAD + 240];
                    message[16..20].copy_from_slice(&seed.to_be_bytes());
                    message.extend_from_slice(&[0x5b; STAMP_SIZE]);
                    message
                })
                .collect(),
        }
        .encode();
        engine.on_inbound_resource_completed(&mut ready, &server, &batch_link, &envelope);
        engine.state = State::Ready(ready);
        assert!(
            engine.batch_in_progress(),
            "the batch must be queued, or the test proves nothing"
        );

        // Another peer, identified on its own link, offers.
        let sender = leviculum_std::generate_identity();
        let link_id = identified_link(
            &mut engine,
            &mut server,
            &mut client,
            destination_hash,
            &signing_key,
            &sender,
        );
        let offer = |engine: &mut Engine<MemoryPropagationStore>,
                     server: &mut leviculum_std::driver::StdNodeCore,
                     client: &mut leviculum_std::driver::StdNodeCore| {
            let (_, out) = client
                .send_request(&link_id, OFFER_REQUEST_PATH, Some(&[0xc0]), None)
                .expect("request");
            let (events, _) = converse(engine, server, client, out);
            events
                .into_iter()
                .find_map(|event| match event {
                    NodeEvent::ResponseReceived { response_data, .. } => {
                        OfferResponse::decode(&response_data).ok()
                    }
                    _ => None,
                })
                .expect("the node must answer the offer")
        };
        assert_eq!(
            offer(&mut engine, &mut server, &mut client),
            OfferResponse::Error(PeerError::Throttled),
            "an offer while a batch is in progress is postponed"
        );

        // The batch stored, the same offer passes the gate (and fails later,
        // on the key it does not carry).
        let mut rounds = 0;
        while engine.batch_in_progress() {
            rounds += 1;
            assert!(rounds < 1000, "the batch did not drain");
            let now = server.now_ms();
            let _ = engine.on_tick(&mut server, now);
        }
        assert_ne!(
            offer(&mut engine, &mut server, &mut client),
            OfferResponse::Error(PeerError::Throttled),
            "with no batch in progress the gate must not postpone"
        );
    }

    /// LV-1003, finding 1: a message this node received, delivered and
    /// purged is not wanted back when a peer offers it. Before, `/offer`
    /// asked the store alone, wanted it, and the bodies crossed the wire to
    /// be dropped at ingest as duplicates (1524's cell
    /// `lnpnd_delivered_not_wanted_back`: `offered=5 wanted=5`, then five
    /// `dup=true via="sync"`). The message arrives by a peer's sync, so the
    /// engine's origin record is checked on the way. Positive control: a
    /// fresh id in the same offer is wanted.
    #[test]
    fn a_message_delivered_and_purged_here_is_not_wanted_back() {
        use leviculum_lxmf::{OfferResponse, PeerOffer, PeerSyncEnvelope};

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: cost_free(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            // At cost 0 any peering key passes, so the offer reaches the
            // membership answer.
            peering: PeeringConfig {
                peering_cost: 0,
                ..PeeringConfig::default()
            },
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };

        // A peer's sync stores one message here.
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let from_peer = [0x17; 16];
        let batch_link = LinkId::new([0x42; 16]);
        ready.peering.validate_link_for_tests(batch_link, from_peer);
        let message = synced_message(0x31);
        let transient_id =
            leviculum_core::crypto::full_hash(&message[..message.len() - STAMP_SIZE]);
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![message],
        }
        .encode();
        engine.on_inbound_resource_completed(&mut ready, &server, &batch_link, &envelope);
        let mut out = TickOutput::empty();
        absorb(&mut engine, &mut ready, &mut server, &mut out);
        assert!(
            ready.node.store().contains(&transient_id).expect("store"),
            "the message must be stored, or the test proves nothing"
        );
        assert!(
            ready
                .peering
                .origins_mut()
                .came_from(&transient_id, &from_peer),
            "the engine records which peer a synced message came from"
        );

        // Delivered and purged, as the engine's local delivery does.
        assert!(ready.node.store_mut().purge(&transient_id).expect("purge"));
        ready
            .node
            .note_delivered_locally(&transient_id, unix_secs());
        engine.log_departures(&mut ready);
        assert!(
            !ready
                .peering
                .origins_mut()
                .came_from(&transient_id, &from_peer),
            "the origin goes with the message"
        );
        engine.state = State::Ready(ready);

        // A peer offers it back, beside one this node never saw.
        let sender = leviculum_std::generate_identity();
        let link_id = identified_link(
            &mut engine,
            &mut server,
            &mut client,
            destination_hash,
            &signing_key,
            &sender,
        );
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let fresh = [0x99; 32];
        let offer = PeerOffer {
            peering_key: [0; 32],
            transient_ids: vec![transient_id, fresh],
        }
        .encode();
        let response =
            ready
                .peering
                .answer_offer_request(&mut server, &ready.node, &link_id, &offer, false);
        engine.state = State::Ready(ready);
        assert_eq!(
            OfferResponse::decode(&response).ok(),
            Some(OfferResponse::Wanted(vec![fresh])),
            "the delivered message is not wanted back, the fresh one is"
        );
    }

    #[test]
    fn an_invalid_stamp_upload_throttles_its_identified_sender() {
        use leviculum_lxmf::peering::PN_STAMP_THROTTLE_SECS;
        use leviculum_lxmf::{OfferResponse, PeerSyncEnvelope};

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, events) = Engine::new(EngineConfig {
            identity,
            // A real cost: the placeholder stamp below cannot meet it.
            node_config: PropagationNodeConfig::default(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };

        let sender = leviculum_std::generate_identity();
        let link_id = identified_link(
            &mut engine,
            &mut server,
            &mut client,
            destination_hash,
            &signing_key,
            &sender,
        );
        // Big enough that it rides a resource, not a link packet.
        let mut message = vec![0x5au8; 1_200];
        message.extend_from_slice(&[0x5b; STAMP_SIZE]);
        let upload = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![message],
        }
        .encode();
        let (_, out) = client
            .send_resource(&link_id, &upload, None, false)
            .expect("advertise");
        let _ = converse(&mut engine, &mut server, &mut client, out);
        await_validation(&mut engine);
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let mut out = TickOutput::empty();
        absorb(&mut engine, &mut ready, &mut server, &mut out);
        assert!(
            events.try_iter().any(|event| matches!(
                &event,
                EngineEvent::Rejected { detail } if detail == "invalid stamp"
            )),
            "the placeholder stamp must fail validation, or the test proves nothing"
        );
        let now = unix_secs();

        // The same identity comes back on a fresh link and offers.
        engine.state = State::Ready(ready);
        let second = identified_link(
            &mut engine,
            &mut server,
            &mut client,
            destination_hash,
            &signing_key,
            &sender,
        );
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        let response =
            ready
                .peering
                .answer_offer_request(&mut server, &ready.node, &second, &[], false);
        assert_eq!(
            OfferResponse::decode(&response).ok(),
            Some(OfferResponse::Error(PeerError::Throttled)),
            "an /offer right after an invalid-stamp upload must be throttled"
        );
        let remote_hash = *Destination::compute_destination_hash(
            &Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]),
            sender.hash(),
        )
        .as_bytes();
        assert!(
            ready
                .peering
                .is_throttled_for_tests(&remote_hash, now + PN_STAMP_THROTTLE_SECS - 1),
            "the throttle must last PN_STAMP_THROTTLE"
        );
        assert!(
            !ready
                .peering
                .is_throttled_for_tests(&remote_hash, now + PN_STAMP_THROTTLE_SECS + 1),
            "and no longer"
        );
    }

    /// A client's upload resource larger than the delivery limit is refused
    /// at the advertisement, with a `PN_REJECT` that names the size and the
    /// cap, and nothing is stored. One between the transfer and the delivery
    /// limit is stored local only and says so in its `PN_ACCEPT` (order
    /// 1295, reversing 1294's refusal: the reference stores it too, and its
    /// recipient collects it here); one under the transfer limit is stored
    /// as any other.
    #[test]
    fn a_client_upload_over_the_delivery_limit_is_refused_and_one_over_the_transfer_limit_is_local()
    {
        use leviculum_lxmf::PeerSyncEnvelope;

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: PropagationNodeConfig {
                transfer_limit_kb: 1,
                ..cost_free()
            },
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 2,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };
        let sender = leviculum_std::generate_identity();

        enable_log_dispatch();
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let mut upload = |engine: &mut Engine<MemoryPropagationStore>, len: usize| {
            let link_id = identified_link(
                engine,
                &mut server,
                &mut client,
                destination_hash,
                &signing_key,
                &sender,
            );
            // Both ride a resource, not a link packet.
            let mut message = vec![len as u8; len];
            message.extend_from_slice(&[0x5b; STAMP_SIZE]);
            let envelope = PeerSyncEnvelope {
                timestamp: 1.0,
                messages: vec![message],
            }
            .encode();
            let (_, out) = client
                .send_resource(&link_id, &envelope, None, false)
                .expect("advertise");
            let _ = converse(engine, &mut server, &mut client, out);
            await_validation(engine);
            let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
                State::Ready(ready) => ready,
                _ => panic!("engine must be ready"),
            };
            let mut out = TickOutput::empty();
            absorb(engine, &mut ready, &mut server, &mut out);
            let stored = ready.node.store().len();
            engine.state = State::Ready(ready);
            (stored, envelope.len())
        };
        let ((over, over_len), (local, _), (under, _)) =
            tracing::subscriber::with_default(subscriber, || {
                (
                    upload(&mut engine, 2_500),
                    upload(&mut engine, 1_200),
                    upload(&mut engine, 600),
                )
            });
        assert_eq!(
            over, 0,
            "the upload over the delivery limit must not be stored"
        );
        assert_eq!(local, 1, "the one over the transfer limit must be");
        assert_eq!(under, 2, "and the one under it");
        let log = capture.unquoted();
        let accepts: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_ACCEPT"))
            .collect();
        assert_eq!(accepts.len(), 2, "{log}");
        assert!(accepts[0].contains("local_only=true"), "{}", accepts[0]);
        assert!(accepts[0].contains("bytes=1232"), "{}", accepts[0]);
        assert!(accepts[1].contains("local_only=false"), "{}", accepts[1]);
        let rejects: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_REJECT"))
            .collect();
        assert_eq!(rejects.len(), 1, "{log}");
        // The resource's size, not the message's: refused before it moved.
        let bytes = format!("bytes={over_len} ");
        for field in [
            "reason=too_large",
            "via=resource",
            "limit=2000",
            " INFO ",
            &bytes,
        ] {
            assert!(rejects[0].contains(field), "{field}: {}", rejects[0]);
        }
    }

    /// Order 1663: a node started without a config takes a voice message.
    /// 60 KB is fifty seconds of Loquax voice (Opus at 1.2 KB/s,
    /// docs/src/concepts/loquax-voice.md); under lnpnd's pre-1294 fallback
    /// of 4 KB it was stored local only and never reached another node, so
    /// a recipient collecting elsewhere never got it. The limits come from [`crate::config::SizeLimits::resolve`] over
    /// an empty config, the path `main.rs` runs, so a regressed default
    /// turns this red. The 60 KB upload is stored as one any peer is
    /// offered (`local_only=false`), announced as field 3 = 256 (lxmd's
    /// `PROPAGATION_LIMIT`), and its recipient collects it whole. The
    /// control: one over lxmd's `DELIVERY_LIMIT` of 1000 KB is refused at
    /// the advertisement with `PN_REJECT reason=too_large`, and nothing of
    /// it is stored.
    #[test]
    fn a_node_without_a_config_takes_a_sixty_kb_voice_message_and_refuses_one_over_the_delivery_limit(
    ) {
        use leviculum_lxmf::propagation::{MessageGetRequest, MessageListResponse};
        use leviculum_lxmf::{PeerSyncEnvelope, PropagationNodeAnnounce, TransferLimit};
        use rand_core::RngCore as _;

        let limits = crate::config::SizeLimits::resolve(
            &crate::config::RawConfig::parse("").expect("an empty config parses"),
            None,
            None,
        )
        .expect("an empty config resolves");
        assert_eq!(
            limits,
            crate::config::SizeLimits {
                transfer_kb: 256,
                sync_kb: 10_240,
                delivery_kb: 1000,
            },
            "lxmd's PROPAGATION_LIMIT, SYNC_LIMIT and DELIVERY_LIMIT \
             (reference/LXMF/LXMF/LXMRouter.py:55-60)"
        );

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let store_bytes = 4_000_000;
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: PropagationNodeConfig {
                transfer_limit_kb: limits.transfer_kb,
                sync_limit_kb: limits.sync_kb,
                ..cost_free()
            },
            store: MemoryPropagationStore::new(store_bytes),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: store_bytes,
            delivery_limit_kb: limits.delivery_kb,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => {
                let announce =
                    PropagationNodeAnnounce::decode(&ready.node.announce_app_data(1_700_000_000))
                        .expect("our announce decodes");
                assert_eq!(
                    announce.transfer_limit_kb, 256,
                    "a stock peer offers us what fits field 3"
                );
                ready.destination_hash
            }
            _ => panic!("engine must be ready"),
        };
        let sender = leviculum_std::generate_identity();
        let recipient = [0x3c; 16];

        enable_log_dispatch();
        let capture = LogCapture::default();
        let uploads = tracing::subscriber::with_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .with_writer(capture.clone())
                .finish(),
            || {
                let mut upload = |len: usize| {
                    let link_id = identified_link(
                        &mut engine,
                        &mut server,
                        &mut client,
                        destination_hash,
                        &signing_key,
                        &sender,
                    );
                    // Random, so no compression shrinks the resource
                    // under the limit it is meant to test.
                    let mut message = vec![0u8; len];
                    rand_core::OsRng.fill_bytes(&mut message);
                    message[..16].copy_from_slice(&recipient);
                    message.extend_from_slice(&[0x5b; STAMP_SIZE]);
                    let envelope = PeerSyncEnvelope {
                        timestamp: 1.0,
                        messages: vec![message],
                    }
                    .encode();
                    let (_, out) = client
                        .send_resource(&link_id, &envelope, None, false)
                        .expect("advertise");
                    let _ = converse(&mut engine, &mut server, &mut client, out);
                    await_validation(&mut engine);
                    let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
                        State::Ready(ready) => ready,
                        _ => panic!("engine must be ready"),
                    };
                    let mut out = TickOutput::empty();
                    absorb(&mut engine, &mut ready, &mut server, &mut out);
                    let stored = ready.node.store().len();
                    engine.state = State::Ready(ready);
                    (stored, envelope.len())
                };
                (upload(60_000), upload(1_000_001))
            },
        );
        let ((voice, _), (after_over, over_len)) = uploads;
        assert_eq!(voice, 1, "the 60 KB voice message must be stored");
        assert_eq!(
            after_over, 1,
            "the one over the delivery limit must not be: the store still \
             holds the voice message alone"
        );

        let log = capture.unquoted();
        let accepts: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_ACCEPT"))
            .collect();
        assert_eq!(accepts.len(), 1, "{log}");
        assert!(
            accepts[0].contains("local_only=false"),
            "under the transfer limit, so offered to every peer: {}",
            accepts[0]
        );
        let rejects: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_REJECT"))
            .collect();
        assert_eq!(rejects.len(), 1, "{log}");
        let bytes = format!("bytes={over_len} ");
        for field in ["reason=too_large", "via=resource", "limit=1000000", &bytes] {
            assert!(rejects[0].contains(field), "{field}: {}", rejects[0]);
        }

        // The recipient collects it: listed, then fetched whole under the
        // reference client's own limit (`delivery_per_transfer_limit`,
        // 1000 KB, `reference/LXMF/LXMF/LXMRouter.py:1592`).
        let State::Ready(ready) = &mut engine.state else {
            panic!("engine must be ready");
        };
        let listed = match ready
            .node
            .handle_get(
                &MessageGetRequest::list().encode().expect("encodes"),
                &recipient,
                unix_secs(),
            )
            .expect("the recipient lists")
        {
            GetOutcome::List { response, .. } => {
                match MessageListResponse::decode(&response).expect("decodes") {
                    MessageListResponse::TransientIds(ids) => ids,
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("a list request must list: {other:?}"),
        };
        assert_eq!(listed.len(), 1, "the voice message waits for its recipient");
        let fetch = MessageGetRequest {
            wants: Some(listed.clone()),
            haves: None,
            transfer_limit_kb: Some(TransferLimit::Integer(1000)),
        };
        match ready
            .node
            .handle_get(&fetch.encode().expect("encodes"), &recipient, unix_secs())
            .expect("the recipient fetches")
        {
            GetOutcome::Fetch { plan, .. } => {
                assert_eq!(plan.served_ids(), listed, "served whole, in one fetch");
                let served: Vec<u32> = plan.messages().map(|(_, len)| len).collect();
                assert_eq!(served, vec![60_000], "the body ships unstamped");
            }
            other => panic!("a fetch must fetch: {other:?}"),
        }
    }

    /// A sync message the store cannot hold is an event, not silence: the
    /// round logs `PN_STORE_FULL` with what was offered and dropped, the
    /// next `/offer` is declined with `ERROR_THROTTLED` (the peer keeps
    /// its messages, `LXMPeer.py:421-425`), and once the maintenance
    /// sweep has run at the next tick offers pass the gate again
    /// (order 1294).
    #[test]
    fn a_store_failure_during_a_sync_is_logged_and_declines_until_the_sweep() {
        use leviculum_lxmf::{OfferResponse, PeerSyncEnvelope};

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config: cost_free(),
            // Smaller than the message below: no eviction can make room.
            store: MemoryPropagationStore::new(500),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 500,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };
        let sender = leviculum_std::generate_identity();
        let link_id = identified_link(
            &mut engine,
            &mut server,
            &mut client,
            destination_hash,
            &signing_key,
            &sender,
        );
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        ready.peering.validate_link_for_tests(link_id, [0x24; 16]);
        let mut message = vec![0x11u8; 900];
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
            engine.on_inbound_resource_completed(&mut ready, &server, &link_id, &envelope);
            engine.state = State::Ready(ready);
            await_validation(&mut engine);
            let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
                State::Ready(ready) => ready,
                _ => panic!("engine must be ready"),
            };
            // The hook's own drain, which books the sweep.
            let mut out = TickOutput::empty();
            engine.drain_pending(&mut ready, &mut server, &mut out);
            engine.state = State::Ready(ready);
        });
        let log = capture.unquoted();
        let full: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("event=PN_STORE_FULL"))
            .collect();
        assert_eq!(full.len(), 1, "{log}");
        assert!(
            full[0].contains("offered=1") && full[0].contains("dropped=1"),
            "{}",
            full[0]
        );

        let offer = |engine: &mut Engine<MemoryPropagationStore>,
                     server: &mut leviculum_std::driver::StdNodeCore| {
            let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
                State::Ready(ready) => ready,
                _ => panic!("engine must be ready"),
            };
            let response =
                ready
                    .peering
                    .answer_offer_request(server, &ready.node, &link_id, &[], false);
            engine.state = State::Ready(ready);
            OfferResponse::decode(&response).ok()
        };
        assert_eq!(
            offer(&mut engine, &mut server),
            Some(OfferResponse::Error(PeerError::Throttled)),
            "an /offer after the failure must be declined, not answered"
        );
        assert_eq!(
            engine.next_maintenance_at, 0,
            "the sweep is booked for the next tick"
        );
        let now = server.now_ms();
        let _ = engine.on_tick(&mut server, now);
        assert_ne!(
            offer(&mut engine, &mut server),
            Some(OfferResponse::Error(PeerError::Throttled)),
            "after the sweep an /offer passes the gate again"
        );
    }

    /// A path request answered before the node's first announce must carry
    /// the node's announce data: a stock client reads the stamp cost it has
    /// to mine out of whatever announce it last heard for the node
    /// (`get_outbound_propagation_cost`,
    /// `reference/LXMF/LXMF/LXMRouter.py:421-442`), and a response without
    /// app data leaves it nothing, so it fails the message ("cannot
    /// generate propagation stamp", `LXMRouter.py:2634-2640`). The
    /// reference parks the app data on the destination at registration
    /// (`set_default_app_data`, `LXMRouter.py:191`), which is what a path
    /// response reads (#158's pin, `mvr_default_announce_app_data.rs`).
    /// lnpnd announces 20 s after start, so every client that asked in that
    /// window lost its first upload (leviculum order 709).
    #[test]
    fn a_path_response_before_the_first_announce_carries_the_stamp_cost() {
        use leviculum_lxmf::propagation::PropagationNodeAnnounce;

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: PropagationNodeConfig::default(),
            store: MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            // No announce within the test: the response is all there is.
            announce_delay_secs: 3600,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let destination_hash = match &engine.state {
            State::Ready(ready) => ready.destination_hash,
            _ => panic!("engine must be ready"),
        };

        // The response leaves after the path-request grace, from the
        // node's timer, not from the packet's own handling.
        let request = client.request_path(&destination_hash);
        for data in wire_of(&request) {
            let _ = server.handle_packet(leviculum_core::InterfaceId(0), &data);
        }
        std::thread::sleep(std::time::Duration::from_millis(
            leviculum_core::constants::PATH_REQUEST_GRACE_MS + 50,
        ));
        let response = server.handle_timeout();
        let mut events = Vec::new();
        for data in wire_of(&response) {
            let out = client.handle_packet(leviculum_core::InterfaceId(0), &data);
            events.extend(out.events);
        }
        let app_data = events
            .iter()
            .find_map(|event| match event {
                NodeEvent::AnnounceReceived { announce, .. }
                    if announce.destination_hash() == &destination_hash =>
                {
                    Some(announce.app_data().to_vec())
                }
                _ => None,
            })
            .expect("the node must answer the path request");
        let announce = PropagationNodeAnnounce::decode(&app_data).unwrap_or_else(|error| {
            panic!(
                "the path response carries no propagation app data ({} bytes): {error:?}",
                app_data.len()
            )
        });
        assert_eq!(
            announce.stamp_cost,
            u64::from(PropagationNodeConfig::default().stamp_cost)
        );
    }

    /// A link up between a client core and the engine's core, and a
    /// single upload stamped at the node's minimum, ready to send.
    struct UploadFixture<S = MemoryPropagationStore> {
        engine: Engine<S>,
        server: leviculum_std::driver::StdNodeCore,
        client: leviculum_std::driver::StdNodeCore,
        link_id: LinkId,
        upload: Vec<u8>,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
    }

    fn upload_fixture() -> UploadFixture {
        let fixture = upload_fixture_on(
            MemoryPropagationStore::new(64_000),
            PropagationNodeConfig::default(),
        );
        let min_cost = match &fixture.engine.state {
            State::Ready(ready) => ready.min_cost,
            _ => panic!("engine must be ready"),
        };
        assert!(
            ValidationWorker::worth_deferring(min_cost, false),
            "the stamp must need grinding, or the test proves nothing"
        );
        fixture
    }

    /// [`upload_fixture`] over any store and node configuration.
    fn upload_fixture_on<
        S: PropagationStore + PropagationBodyStore + WriteBehind + Send + 'static,
    >(
        store: S,
        node_config: PropagationNodeConfig,
    ) -> UploadFixture<S> {
        use leviculum_lxmf::constants::{LXMF_OVERHEAD, WORKBLOCK_EXPAND_ROUNDS_PN};
        use leviculum_lxmf::{CooperativeStamper, PropagationUpload, StampCancel};

        let server_dir = tempfile::tempdir().expect("tempdir");
        let client_dir = tempfile::tempdir().expect("tempdir");
        let mut server = core(server_dir.path());
        let mut client = core(client_dir.path());
        let identity = leviculum_std::generate_identity();
        let signing_key = identity.ed25519_verifying().to_bytes();
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity,
            node_config,
            store,
            announce_interval_secs: 3600,
            announce_delay_secs: 3600,
            announce_at_start: true,
            peering: PeeringConfig::default(),
            peer_store: Box::new(MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let start = server.now_ms();
        let _ = engine.on_tick(&mut server, start);
        let (destination_hash, min_cost) = match &engine.state {
            State::Ready(ready) => (ready.destination_hash, ready.min_cost),
            _ => panic!("engine must be ready"),
        };

        let (link_id, _, out) = client
            .connect(destination_hash, &signing_key)
            .expect("connect");
        let (events, _) = converse(&mut engine, &mut server, &mut client, out);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, NodeEvent::LinkEstablished { .. })),
            "the link must come up: {events:?}"
        );

        let mut unstamped = vec![0x42u8; LXMF_OVERHEAD + 40];
        unstamped[16..].fill(0x17);
        let transient_id = leviculum_core::crypto::full_hash(&unstamped);
        let stamp = futures::executor::block_on(
            CooperativeStamper::cooperative(rand_core::OsRng).generate(
                &transient_id,
                min_cost,
                WORKBLOCK_EXPAND_ROUNDS_PN,
                &StampCancel::new(),
            ),
        )
        .expect("a stamp at the node's minimum");
        let upload = PropagationUpload::single(1_700_000_000.0, unstamped, stamp).encode();
        UploadFixture {
            engine,
            server,
            client,
            link_id,
            upload,
            _dirs: (server_dir, client_dir),
        }
    }

    fn is_proof(data: &[u8]) -> bool {
        Packet::unpack(data).is_ok_and(|packet| packet.flags.packet_type == PacketType::Proof)
    }

    /// Send the fixture's upload and run every event it raises through the
    /// engine, as the driver routes them. Returns whether a proof left and
    /// the earliest wake any hook asked for.
    fn deliver_upload<S: PropagationStore + PropagationBodyStore + WriteBehind + Send + 'static>(
        fixture: &mut UploadFixture<S>,
    ) -> (bool, Option<u64>) {
        let (_, out) = fixture
            .client
            .send_packet_on_link(&fixture.link_id, &fixture.upload)
            .expect("upload packet");
        let mut wake: Option<u64> = None;
        let mut proven = false;
        for data in wire_of(&out) {
            let core_out = fixture
                .server
                .handle_packet(leviculum_core::InterfaceId(0), &data);
            for event in &core_out.events {
                let hook = fixture.engine.on_event(&mut fixture.server, event);
                proven |= wire_of(&hook).iter().any(|data| is_proof(data));
                if let Some(deadline) = hook.next_deadline_ms {
                    wake = Some(wake.map_or(deadline, |w| w.min(deadline)));
                }
            }
        }
        (proven, wake)
    }

    /// leviculum order 716: a single upload with nothing older in flight is
    /// ground, stored and proven in the hook that received it.
    ///
    /// **The named failure mode:** the stamp went to the validation worker,
    /// so the proof needed two wake-ups the hook does not control, the
    /// worker thread's and the driver's poll. Beside a CPU burner on the
    /// coder host (`scripts/pn-upload-ab.py --pn lnpnd --burn 8`) one of
    /// them came a 4 ms scheduler tick late in a fifth of the uploads and
    /// the proof left at 6 to 7 ms, past a loopback client's 5 to 6 ms
    /// receipt timeout (`max(rtt * 6, 5 ms)`,
    /// `reference/Reticulum/RNS/Packet.py:431`); 714 saw a link torn down
    /// for it under a running corpus. A refactor that hands the lone upload
    /// back to the worker makes this red, however fast the worker is.
    #[test]
    fn a_lone_upload_is_proven_in_the_hook_that_received_it() {
        let mut fixture = upload_fixture();
        let (proven, _) = deliver_upload(&mut fixture);
        assert!(
            !fixture.engine.validation.busy(),
            "the lone upload went to the worker"
        );
        assert!(
            proven,
            "no proof left the hooks that received the upload; it waits for a wake"
        );
    }

    /// leviculum#709: an upload that does go through the validation worker
    /// (here: behind an older job, which it may not overtake) must still be
    /// proven within the client's packet-receipt timeout, which over a LAN
    /// or a loopback shared instance is a few milliseconds
    /// (`max(rtt * 6, 5 ms)`, `reference/Reticulum/RNS/Packet.py:431`,
    /// `RNS/Link.py:81-82`). A late proof is a lost upload: the client's
    /// timeout callback tears the link down and parks the message as
    /// OUTBOUND (`reference/LXMF/LXMF/LXMessage.py:616-621`).
    ///
    /// **The named failure mode:** the stamp goes to the validation worker
    /// and the hook returns asking for nothing, so the verdict waited for
    /// the 200 ms idle tick or for the next unrelated event; on_tick, once
    /// it ran, polled the worker at 20 ms. A stock client against release
    /// lnpnd over loopback: proofs at 26 to 50 ms, four of the first five
    /// links torn down, against lxmd's 16 ms (leviculum order 709).
    ///
    /// This plays the driver: every hook's `next_deadline_ms` is the next
    /// wake, as `tighten_next_poll` makes it. The worker is held on an
    /// older, empty job until the upload has queued behind it, so the
    /// upload takes the worker path whatever the machine's speed; what is
    /// asserted is that the engine never sleeps past its verdict by more
    /// than [`VALIDATION_POLL_MS`].
    #[test]
    fn a_queued_upload_asks_the_driver_back_until_its_proof_leaves() {
        let mut fixture = upload_fixture();
        let (held, gate) = ValidationWorker::spawn_held();
        fixture.engine.validation = held;
        assert!(fixture.engine.validation.enqueue(ValidationJob {
            link_id: LinkId::new([0xEE; 16]),
            carrier: Carrier::Packet,
            data: Vec::new(),
            proof: None,
            sender: SyncSender::default(),
            min_cost: 13,
            compute_value: false,
        }));

        let (proven, wake) = deliver_upload(&mut fixture);
        assert!(!proven, "the proof cannot leave before the verdict");
        let engine = &mut fixture.engine;
        let server = &mut fixture.server;
        let queued_at = server.now_ms();
        let wake_after_queue = wake.expect(
            "the hook that queued the upload must ask the driver back; \
             it asked for nothing, so the verdict waits for an unrelated wake",
        );
        assert!(
            wake_after_queue <= queued_at + VALIDATION_POLL_MS,
            "asked back at +{} ms, past the {VALIDATION_POLL_MS} ms poll",
            wake_after_queue.saturating_sub(queued_at)
        );
        drop(gate);

        let mut wake = wake_after_queue;
        let mut proven = false;
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !proven {
            assert!(std::time::Instant::now() < give_up, "no proof within 10 s");
            let now = server.now_ms();
            if wake > now {
                std::thread::sleep(std::time::Duration::from_millis(wake - now));
            }
            let now = server.now_ms();
            let tick = engine.on_tick(server, now);
            proven = wire_of(&tick).iter().any(|data| is_proof(data));
            if proven {
                break;
            }
            wake = tick
                .next_deadline_ms
                .expect("the engine always names a wake");
            assert!(
                wake <= now + VALIDATION_POLL_MS,
                "while the worker grinds the engine slept to +{} ms, past the \
                 {VALIDATION_POLL_MS} ms poll",
                wake.saturating_sub(now)
            );
        }
    }

    /// Order 1081: the durable write of an upload runs on the store's
    /// writer, and the proof follows the write.
    mod write_behind {
        use super::*;

        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::thread::ThreadId;
        use std::time::{Duration, Instant};

        use leviculum_std::driver::PROCESSOR_TICK_BUDGET;
        use leviculum_std::durable::Fsync;
        use leviculum_std::FilePropagationStore;

        /// The order's slow device: every `fsync` takes 20 ms, and the seam
        /// notes the thread each one ran on.
        #[derive(Default)]
        struct SlowFsync {
            threads: Mutex<Vec<ThreadId>>,
        }

        impl Fsync for SlowFsync {
            fn sync(&self, handle: &std::fs::File) -> std::io::Result<()> {
                std::thread::sleep(Duration::from_millis(20));
                if let Ok(mut threads) = self.threads.lock() {
                    threads.push(std::thread::current().id());
                }
                handle.sync_all()
            }
        }

        /// Holds every `fsync` until the test drops the sender, and counts
        /// the ones that finished.
        struct GatedFsync {
            gate: Mutex<std::sync::mpsc::Receiver<()>>,
            finished: AtomicUsize,
        }

        impl Fsync for GatedFsync {
            fn sync(&self, handle: &std::fs::File) -> std::io::Result<()> {
                if let Ok(gate) = self.gate.lock() {
                    let _ = gate.recv();
                }
                handle.sync_all()?;
                self.finished.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        /// The writer dies between writing the temp file and renaming it,
        /// as a process killed mid-write would.
        struct DyingFsync;

        impl Fsync for DyingFsync {
            fn sync(&self, _handle: &std::fs::File) -> std::io::Result<()> {
                panic!("the writer dies before the write is durable");
            }
        }

        /// The first write fails, every later one lands.
        #[derive(Default)]
        struct FailOnceFsync {
            calls: AtomicUsize,
        }

        impl Fsync for FailOnceFsync {
            fn sync(&self, handle: &std::fs::File) -> std::io::Result<()> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(std::io::Error::other("the device refused"));
                }
                handle.sync_all()
            }
        }

        fn store_on(dir: &std::path::Path, fsync: Arc<dyn Fsync>) -> FilePropagationStore {
            FilePropagationStore::open_with(dir, 1_000_000, fsync)
                .expect("open the store")
                .write_behind()
                .expect("start the writer")
        }

        /// One dispatch of the upload's events, as the driver hands them
        /// over (`run_event_tap`: one lock, every event). Returns whether a
        /// proof left and how long the dispatch held the core.
        fn upload(fixture: &mut UploadFixture<FilePropagationStore>) -> (bool, Duration) {
            let (_, out) = fixture
                .client
                .send_packet_on_link(&fixture.link_id, &fixture.upload)
                .expect("upload packet");
            let mut proven = false;
            let mut held = Duration::ZERO;
            for data in wire_of(&out) {
                let core_out = fixture
                    .server
                    .handle_packet(leviculum_core::InterfaceId(0), &data);
                let started = Instant::now();
                for event in &core_out.events {
                    let hook = fixture.engine.on_event(&mut fixture.server, event);
                    proven |= wire_of(&hook).iter().any(|data| is_proof(data));
                }
                held += started.elapsed();
            }
            (proven, held)
        }

        /// Play the driver for `ticks` ticks, or until a proof leaves, each
        /// at the wake the engine asked for. While the engine has a proof
        /// to send it must ask back within the validation poll.
        fn tick_for_proof(fixture: &mut UploadFixture<FilePropagationStore>, ticks: usize) -> bool {
            let mut wake = fixture.server.now_ms();
            for _ in 0..ticks {
                let now = fixture.server.now_ms();
                if wake > now {
                    std::thread::sleep(Duration::from_millis(wake - now));
                }
                let now = fixture.server.now_ms();
                let tick = fixture.engine.on_tick(&mut fixture.server, now);
                if wire_of(&tick).iter().any(|data| is_proof(data)) {
                    return true;
                }
                wake = tick
                    .next_deadline_ms
                    .expect("the engine always names a wake");
                if !fixture.engine.unproven.is_empty() {
                    assert!(
                        wake <= now + VALIDATION_POLL_MS,
                        "a proof waits for the writer and the engine slept to +{} ms",
                        wake.saturating_sub(now)
                    );
                }
            }
            false
        }

        /// Tick until the store's writer has nothing outstanding.
        fn tick_until_settled(fixture: &mut UploadFixture<FilePropagationStore>) -> bool {
            let give_up = Instant::now() + Duration::from_secs(10);
            let mut proven = false;
            loop {
                let outstanding = match &fixture.engine.state {
                    State::Ready(ready) => ready.node.store().writes_outstanding(),
                    _ => false,
                };
                if !outstanding {
                    return proven;
                }
                assert!(Instant::now() < give_up, "the writer never settled");
                proven |= tick_for_proof(fixture, 1);
            }
        }

        fn records_in(dir: &std::path::Path) -> Vec<String> {
            std::fs::read_dir(dir)
                .expect("list the store")
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| !name.starts_with('.'))
                .collect()
        }

        /// mvr, order 1081: a hook that stores an upload stays inside the
        /// core-lock budget on a slow disk, because no `fsync` runs in it.
        ///
        /// **The named failure mode:** the store's append ran its two
        /// `fsync`s inside the hook, under the core lock: one store append
        /// p50 1.0 to 1.8 ms on the coder host, 3 to 9 ms on the field
        /// node's Odroid (order 1009), and `CORE_PROCESSOR_OVER_BUDGET` 4520
        /// times in ten hours on miauhaus. With a device that takes 20 ms
        /// per `fsync` the upload's dispatch held the lock 40.5 ms on the
        /// inline store. What is asserted is the mechanism, not a wall
        /// clock: every `fsync` of the write ran on a thread other than the
        /// one that holds the core, so the hold cannot contain the device's
        /// latency however slow it is or however the test is scheduled.
        #[test]
        fn a_slow_disk_does_not_hold_the_hook_that_stores_an_upload() {
            let dir = tempfile::tempdir().expect("tempdir");
            let fsync = Arc::new(SlowFsync::default());
            let mut fixture = upload_fixture_on(
                store_on(dir.path(), Arc::clone(&fsync) as Arc<dyn Fsync>),
                cost_free(),
            );
            let (_, held) = upload(&mut fixture);
            let proven = tick_until_settled(&mut fixture) || tick_for_proof(&mut fixture, 5);
            assert!(proven, "the upload was never proven");
            let hook_thread = std::thread::current().id();
            let threads = fsync.threads.lock().map(|t| t.clone()).unwrap_or_default();
            assert_eq!(threads.len(), 2, "the write is two fsyncs: {threads:?}");
            assert!(
                threads.iter().all(|thread| *thread != hook_thread),
                "an fsync ran on the thread that holds the core lock; the dispatch held \
                 it {held:?} against a budget of {PROCESSOR_TICK_BUDGET:?}"
            );
        }

        /// Order 1081: the proof for an upload leaves only after its write
        /// is durable, and soon after: the engine asks the driver back at
        /// the validation poll while it waits.
        #[test]
        fn the_proof_waits_for_the_durable_write_and_follows_it() {
            let dir = tempfile::tempdir().expect("tempdir");
            let (release, gate) = std::sync::mpsc::channel();
            let fsync = Arc::new(GatedFsync {
                gate: Mutex::new(gate),
                finished: AtomicUsize::new(0),
            });
            let mut fixture = upload_fixture_on(
                store_on(dir.path(), Arc::clone(&fsync) as Arc<dyn Fsync>),
                cost_free(),
            );
            let (proven, _) = upload(&mut fixture);
            assert!(!proven, "the proof left before the write was durable");
            assert!(
                !tick_for_proof(&mut fixture, 20),
                "the proof left while the write was held"
            );
            assert_eq!(records_in(dir.path()).len(), 0, "the held write landed");

            drop(release);
            let proven = tick_until_settled(&mut fixture) || tick_for_proof(&mut fixture, 5);
            assert!(proven, "no proof after the write landed");
            assert_eq!(
                fsync.finished.load(Ordering::SeqCst),
                2,
                "the proof left before both fsyncs finished"
            );
            assert_eq!(
                records_in(dir.path()).len(),
                1,
                "the record was not in place"
            );
        }

        /// Order 1081: a writer that dies between accept and durable write
        /// leaves no proof and no record.
        #[test]
        fn a_writer_dead_before_the_write_leaves_no_proof_and_no_record() {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut fixture =
                upload_fixture_on(store_on(dir.path(), Arc::new(DyingFsync)), cost_free());
            let (proven, _) = upload(&mut fixture);
            assert!(!proven, "the proof left before the write");
            assert!(
                !tick_until_settled(&mut fixture) && !tick_for_proof(&mut fixture, 5),
                "a proof left for a message the writer never stored"
            );
            assert!(
                fixture.engine.unproven.is_empty(),
                "the proof is still held"
            );
            let UploadFixture { engine, .. } = fixture;
            drop(engine);
            // Positive control: the writer did die mid-write, leaving the
            // temp file a power cut leaves.
            let torn = std::fs::read_dir(dir.path())
                .expect("list the store")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
                .count();
            assert_eq!(torn, 1, "the writer did not die between write and rename");
            let reopened = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
            assert!(
                reopened.is_empty(),
                "the reopened store holds a record the writer never finished"
            );
        }

        /// Order 1081: a write the device refused takes the message out of
        /// the store and out of the role's memory, so the client's retry is
        /// stored and proven rather than answered as a proven duplicate.
        #[test]
        fn a_refused_write_is_not_proven_and_its_retry_is() {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut fixture = upload_fixture_on(
                store_on(dir.path(), Arc::new(FailOnceFsync::default())),
                cost_free(),
            );
            let (proven, _) = upload(&mut fixture);
            assert!(
                !proven && !tick_until_settled(&mut fixture) && !tick_for_proof(&mut fixture, 5),
                "a proof left for a write the device refused"
            );
            let (proven, _) = upload(&mut fixture);
            let proven =
                proven || tick_until_settled(&mut fixture) || tick_for_proof(&mut fixture, 5);
            assert!(proven, "the retry was not proven");
            assert_eq!(records_in(dir.path()).len(), 1, "the retry was not stored");
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
        MemoryPeerStore, MemoryPropagationStore, PeerSyncEnvelope, PropagationBodyStore,
        PropagationStore, TransientId,
    };
    use leviculum_std::driver::{CoreProcessor as _, StdClock, StdStorage, PROCESSOR_TICK_BUDGET};

    use super::tests::cost_free;

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

    /// One cost-13 propagation stamp validated in a release build: the
    /// worker's own grind, 1.08 to 1.45 ms over 60 uploads idle and beside
    /// `stress-ng --cpu 8` on the coder host (leviculum order 716,
    /// 2026-10-14), rounded up. `Engine::grinds_inline` pays it under the
    /// lock once per lone upload.
    const PRICED_STAMP_GRIND: Duration = Duration::from_micros(1_500);

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

    impl super::WriteBehind for ModelledStore {}

    impl PropagationBodyStore for ModelledStore {
        fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
            self.inner.read_body(transient_id)
        }

        fn read_body_at(
            &self,
            transient_id: &TransientId,
            offset: usize,
            buf: &mut [u8],
        ) -> Result<Option<usize>, StorageError> {
            self.inner.read_body_at(transient_id, offset, buf)
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
            node_config: cost_free(),
            store: ModelledStore {
                inner: MemoryPropagationStore::new(10_000_000),
                appends: Arc::clone(&appends),
            },
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
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
        engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
        engine.state = State::Ready(ready);
        // The verdicts first: the hooks below measure the storing alone.
        // At `cost_free` they are inline constants and this returns at
        // once; the slicing under test is the same either way, because
        // both kinds of verdict queue onto the one store queue.
        super::tests::await_validation(&mut engine);

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

    /// mvr (order 1009): the slice is per driver dispatch, not per event.
    ///
    /// **The named failure mode:** the driver hands one dispatch's events to
    /// `on_event` one by one under one core lock and one budget
    /// (`run_event_tap`, `leviculum-std/src/driver/processor.rs`), and every
    /// `on_event` call stored a message off the queue, whatever its event.
    /// An announce is a dispatch of two events, `AnnounceReceived` and
    /// `PathFound`, so while a peer's sync was being stored every announce
    /// that arrived stored two messages under a budget that pays for one.
    /// Measured on the coder host with the production stores
    /// (`hook_load`, 40 000 known destinations, a 100-message sync): such
    /// a dispatch held the lock p95 3.0 to 7.3 ms against 1.0 to 2.0 ms for
    /// one append.
    ///
    /// Counted, not timed: the dispatch's appends against
    /// [`PERSIST_PER_HOOK`], so the test asks nothing of the host's disk or
    /// scheduler.
    #[test]
    fn an_announce_dispatch_during_a_sync_stores_at_most_one_slice() {
        use leviculum_core::{Destination, DestinationType, Direction, InterfaceId};

        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir.path()).expect("storage under a fresh temp dir"),
        );
        // The daemon's uplink runs no ingress limiter (`spawn_local_client`).
        core.set_interface_ingress_control(0, false);
        let appends = Arc::new(AtomicUsize::new(0));
        let (mut engine, _events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: cost_free(),
            store: ModelledStore {
                inner: MemoryPropagationStore::new(10_000_000),
                appends: Arc::clone(&appends),
            },
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: true,
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
        let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
            State::Ready(ready) => ready,
            _ => panic!("engine must be ready"),
        };
        ready.peering.validate_link_for_tests(link_id, [0x24u8; 16]);
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: (0..BATCH as u16).map(stamped).collect(),
        }
        .encode();
        engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
        engine.state = State::Ready(ready);
        super::tests::await_validation(&mut engine);

        let mut worst_dispatch = 0usize;
        let mut widest_dispatch = 0usize;
        for index in 0..(BATCH * 2 + 16) {
            if appends.load(Ordering::Relaxed) == BATCH {
                break;
            }
            // A fresh destination's announce, as it reaches the daemon.
            let aspect = format!("d{index}");
            let mut destination = Destination::new(
                Some(leviculum_std::generate_identity()),
                Direction::In,
                DestinationType::Single,
                "dispatch",
                &[&aspect],
            )
            .expect("a destination");
            let packet = destination
                .announce(
                    None,
                    &mut rand_core::OsRng,
                    core.now_ms(),
                    core.emission_secs(),
                )
                .expect("announce");
            let mut wire = vec![0u8; 600];
            let len = packet.pack(&mut wire).expect("pack the announce");
            let dispatch = core.handle_packet(InterfaceId(0), &wire[..len]).events;
            widest_dispatch = widest_dispatch.max(dispatch.len());

            let before = appends.load(Ordering::Relaxed);
            for event in &dispatch {
                let _ = engine.on_event(&mut core, event);
            }
            worst_dispatch = worst_dispatch.max(appends.load(Ordering::Relaxed) - before);

            // The driver's poll, which the queue asked for.
            let now_ms = core.now_ms();
            let _ = engine.on_tick(&mut core, now_ms);
        }

        assert!(
            widest_dispatch >= 2,
            "an announce dispatch carried {widest_dispatch} event(s): with \
             fewer than two, a drain per event cannot have stored more than \
             a slice and this test describes nothing"
        );
        assert_eq!(
            appends.load(Ordering::Relaxed),
            BATCH,
            "every message of the batch must be stored"
        );
        assert!(
            worst_dispatch <= PERSIST_PER_HOOK,
            "one dispatch of {widest_dispatch} events stored {worst_dispatch} \
             messages under one lock; the budget pays for {PERSIST_PER_HOOK}"
        );
    }

    /// The claim the slice above implements, stated as arithmetic: a hook
    /// stores at most [`PERSIST_PER_HOOK`] messages, one durable append
    /// each, plus at most one stamp ground inline, and that is what the hook
    /// budget pays for.
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

        // A lone upload's stamp, ground in the hook that stores it
        // (`Engine::grinds_inline`), is paid on top of the one append.
        let inline = sliced + PRICED_STAMP_GRIND;
        assert!(
            inline <= PROCESSOR_TICK_BUDGET,
            "an inline grind and its append cost {inline:?}, past the \
             {PROCESSOR_TICK_BUDGET:?} budget: the lone upload has to go back \
             to the worker, or the slice has to shrink"
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

#[cfg(test)]
#[path = "engine_hook_load.rs"]
mod hook_load;
