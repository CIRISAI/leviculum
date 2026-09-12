//! The propagation-node role on the board (Codeberg #384, part 3).
//!
//! The board twin of `lnpnd`'s engine: the same protocol crates —
//! [`leviculum_lxmf::PropagationNode`] for accept/`/get`,
//! [`leviculum_lxmf::peering`] for the peer table and sync decisions —
//! over the record-log adapters of `leviculum-pn-store` instead of files,
//! driven from the binaries' main loop instead of a driver tick.
//!
//! # How it rides the main loop
//!
//! Three entry points, all called from the loop that owns the node:
//!
//! * [`Engine::on_events`] — synchronous. Reads one dispatch's
//!   [`NodeEvent`]s, updates peer/link state, and **queues** everything
//!   that needs flash or stamp work as a [`Work`] item. Nothing here
//!   validates a stamp or touches the async flash.
//! * [`Engine::settle`] — asynchronous, and the only place the engine
//!   awaits. Runs the due periodic jobs (announce, maintenance, sync
//!   scheduling, stats), processes **at most one** queued work item —
//!   which is what bounds how long the main loop is away from its
//!   channels — and flushes the store adapters' queued writes through
//!   [`crate::record_store::pn_execute`], one channel round trip per op.
//!   The wire action a write gates (the upload proof, the `/get`
//!   response) is sent only after its flush reported durable: "persist
//!   before you prove" holds at this boundary.
//! * [`Engine::next_deadline_ms`] — what the loop folds into its sleep,
//!   so queued work resumes promptly without the loop polling.
//!
//! # Stamp validation on this core
//!
//! At the default announced cost of 13, every accepted upload and every
//! synced message walks the 1000-round PN workblock — about 41 000
//! SHA-256 compressions, 2.62 MB hashed (concept page §2) — through the
//! streaming validator with a cooperative yield every 64 rounds. The
//! engine validates **one message per settle pass** and reports every
//! validation as `PN_STAMP ms=<n>`, so the first rig run measures what
//! this page cannot: the wall-clock cost on the nRF52840. An inbound
//! sync batch is therefore drained incrementally — between messages the
//! main loop returns to its channels — and while one is draining, new
//! `/offer`s are answered `ERROR_THROTTLED` exactly as the reference
//! throttles during sequential validation
//! (`reference/LXMF/LXMF/LXMRouter.py:2273`).
//!
//! # Heap budget (#388)
//!
//! The boot line `HEAP_BUDGET links=<max> ble_links=<m> per_link=<b>
//! ble_session=<b> role=<b> reserve=<b> total=<b>` (emitted by
//! [`crate::heap_census::log_budget_and_assert`], asserted against
//! [`crate::HEAP_SIZE`]) sums the worst case every owner of the 96 KiB
//! heap may reach at once:
//!
//! ```text
//! total = node_box                            (size_of the boxed NodeCore,
//!                                              30 984 B: EmbeddedStorage's
//!                                              inline tables + core state)
//!       + links · per_link                    links = max_endpoint_links
//!         per_link = size_of::<Link>()        one boxed Reticulum link (2 600 B)
//!                  + link-table node share      (the map value is a Box)
//!       + ble_links · ble_session             ble_links = ble::MAX_LINKS (4)
//!                                             SESSION_BUDGET_BYTES: queue
//!                                             full + pump + defrag, only a
//!                                             link on a GATT connection has
//!                                             one — a LoRa link costs
//!                                             per_link alone
//!       + role                                ROLE_BUDGET_BYTES:
//!           BOARD_SYNC_LIMIT_KB·1000            one inbound sync batch
//!         + 2 · BOARD_TRANSFER_LIMIT_KB·1000    two phones' queued uploads
//!         + 5120                                peer table incl. 64 B keys
//!                                               per peer (#388 pass 3),
//!                                               offer plan, maps, duplicate
//!                                               cache, flush queues
//!       + reserve                             fragmentation reserve
//!                                             + the shared BLE channels
//!                                             + one incoming resource at
//!                                               MAX_INCOMING_RESOURCE_BYTES
//! ```
//!
//! `links` is DERIVED, not chosen
//! ([`crate::heap_census::max_endpoint_links`]): the fixed terms are
//! subtracted from the heap and the remainder divided by `per_link`.
//! With today's numbers — node box 30 984, role 21 120, reserve 18 848,
//! sessions 4 · 3 948 = 15 792, per_link 2 664 — the remainder is
//! 11 560 B and the division yields **4**: the heap affords exactly the
//! BLE-session count, no extra LoRa-backed links until a fixed term
//! shrinks. The same number is handed to `NodeCoreBuilder::max_links`,
//! so the budget's `links=` is an enforced cap, not a claim: the fifth
//! concurrent link — the allocation that used to die at 340 bytes in
//! the field — is refused (`LINK_REFUSED`, the initiator retries after
//! its establishment timeout) instead of taking every existing link
//! down with the heap.
//!
//! Every term is a named constant next to the state it bounds; the
//! census's `pn_batch=`/`pn_work=`/`ble_s<n>=` lines are the running
//! actuals these terms must dominate. Growing any capacity means
//! re-running this sum — the boot assertion makes forgetting that a
//! panic at the desk instead of an allocation failure in the field.
//!
//! # The clock (instruction item 6)
//!
//! The role needs a calendar three times over: the announce timebase
//! (field 1, what peers order updates by), age-based expiry/eviction,
//! and the peer table's own liveness stamps. A board without a GNSS fix
//! takes its seed from any of: the host's `--set-time` (#238, already
//! wired), a connected phone — the timestamp of the first upload
//! envelope it sends us — or a peer's announce timebase; both of the
//! latter seed through the transport's sanity window as
//! [`TimeSource::Overheard`], logged as `TIME_SEED source=overheard`.
//! What degrades when none of them ever arrives: expiry ages are
//! uptime-relative (harmless within one boot), peer `last_heard` starts
//! near zero (the 14-day cull simply never fires), and the announce is
//! withheld — deliberately, because an announce stamped from an unseeded
//! clock poisons exactly the path tables a propagation node exists to be
//! found through, and the first contact on any carrier delivers a seed
//! that ends the condition.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use leviculum_core::envelope::{PnConfigWire, PN_COST_KEEP};
use leviculum_core::identity::Identity;
use leviculum_core::node::{NodeCore, NodeEvent};
use leviculum_core::pn_config_store::StoredPnConfig;
use leviculum_core::resource::ResourceStrategy;
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::{TickOutput, TimeSource};
use leviculum_core::{
    Destination, DestinationHash, DestinationType, Direction, LinkId, ProofStrategy, RequestError,
};
use leviculum_lxmf::constants::{
    STAMP_SIZE, WORKBLOCK_EXPAND_ROUNDS_PEERING, WORKBLOCK_EXPAND_ROUNDS_PN,
};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::peering::{
    answer_offer, build_offer, peering_key_material, response_action, DeclineReason, DropReason,
    InboundGate, OfferPlan, OfferResponse, PeerChange, PeerOffer, PeerRecord, PeerStore,
    PeerSyncEnvelope, PeerTable, PeeringConfig, ResponseAction, SyncPhase, OFFER_REQUEST_PATH,
    SYNC_BACKOFF_STEP_SECS, SYNC_INTERVAL_SECS,
};
use leviculum_lxmf::propagation::{
    MessageListResponse, PeerError, PropagationNodeAnnounce, TransientId,
};
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_lxmf::propagation_store::StoredMessage;
use leviculum_lxmf::{
    CooperativeStamper, Eviction, EvictionReason, GetOutcome, PropagationNode,
    PropagationNodeConfig, PropagationStore, UploadOutcome, MESSAGE_GET_PATH,
};
use leviculum_pn_store::{FlushOp, PnPeerStore, PnStore, Region};
use leviculum_record_log::SECTOR_SIZE;
use rand_core::CryptoRngCore;

// ---------------------------------------------------------------------------
// Persisted configuration (instruction item 5)
// ---------------------------------------------------------------------------

/// The costs this boot runs (read once at boot) and the costs a reboot
/// would come up with (updated by [`apply_config`]). Two pairs, same
/// running-versus-configured split the media profile keeps, because a
/// `--stamp-cost` mid-run must not silently change what the role already
/// announced.
static RUNNING_STAMP: AtomicU8 = AtomicU8::new(0);
static RUNNING_PEERING: AtomicU8 = AtomicU8::new(0);
static CONFIGURED_STAMP: AtomicU8 = AtomicU8::new(0);
static CONFIGURED_PEERING: AtomicU8 = AtomicU8::new(0);

/// Store fill for the display, updated by the engine's stats pass:
/// live message records in the region.
static STORE_FILL: AtomicU32 = AtomicU32::new(0);

/// Live message records, as last counted — the display's `S` field.
pub fn store_fill() -> u32 {
    STORE_FILL.load(Ordering::Relaxed)
}

/// Whether a binary constructed the engine this boot.
static ROLE_ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The display's store-fill field: `Some(live records)` while the role
/// runs, `None` on a boot without it, so the line keeps its historical
/// shape there.
pub fn fill_indicator() -> Option<u16> {
    if ROLE_ACTIVE.load(Ordering::Relaxed) {
        Some(store_fill().min(u16::MAX as u32) as u16)
    } else {
        None
    }
}

/// Read the persisted costs (or the Lead's defaults), seed the
/// running/configured state, and log the boot `PN_CONFIG` line. Call once
/// at boot, before [`Engine::new`].
pub fn load_config_at_boot(page: u32) -> StoredPnConfig {
    let (config, src) = match crate::telemetry::load_pn_config(page) {
        Some(config) => (config, "flash"),
        None => (StoredPnConfig::DEFAULT, "default"),
    };
    RUNNING_STAMP.store(config.stamp_cost, Ordering::Relaxed);
    RUNNING_PEERING.store(config.peering_cost, Ordering::Relaxed);
    CONFIGURED_STAMP.store(config.stamp_cost, Ordering::Relaxed);
    CONFIGURED_PEERING.store(config.peering_cost, Ordering::Relaxed);
    crate::log::log_fmt_critical(
        "PN_CONFIG ",
        format_args!(
            "stamp_cost={} peering_cost={} src={}",
            config.stamp_cost, config.peering_cost, src
        ),
    );
    config
}

/// Merge one control frame over the configured costs and persist.
/// Runs on the serial task ([`crate::usb`]); takes effect at the next
/// boot, which the caller's answer states.
pub fn apply_config(wire: PnConfigWire) -> crate::telemetry::PendingSave {
    let merged = StoredPnConfig {
        stamp_cost: if wire.stamp_cost == PN_COST_KEEP {
            CONFIGURED_STAMP.load(Ordering::Relaxed)
        } else {
            wire.stamp_cost
        },
        peering_cost: if wire.peering_cost == PN_COST_KEEP {
            CONFIGURED_PEERING.load(Ordering::Relaxed)
        } else {
            wire.peering_cost
        },
    };
    CONFIGURED_STAMP.store(merged.stamp_cost, Ordering::Relaxed);
    CONFIGURED_PEERING.store(merged.peering_cost, Ordering::Relaxed);
    crate::log::log_fmt_critical(
        "PN_CONFIG ",
        format_args!(
            "stamp_cost={} peering_cost={} src=host effective=next-boot",
            merged.stamp_cost, merged.peering_cost
        ),
    );
    crate::telemetry::request_save_pn_config(merged)
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// First announce after boot, the reference's own delay
/// (`NODE_ANNOUNCE_DELAY`, `reference/LXMF/LXMF/LXMRouter.py:41`).
const ANNOUNCE_DELAY_SECS: u64 = 20;

/// Announce cadence for opportunistic (hilltop) contact — instruction
/// item 7, derived from the concept page's drain numbers rather than the
/// reference's 360-minute host default:
///
/// * One announce at the field settings is ≈550 ms of airtime (§2's
///   measured 544 ms for a 184 B frame); at 300 s that is ≈0.2 % duty
///   against the 10 % cap — noise.
/// * Draining messages costs ≈9 s of wall clock each at the duty cap
///   (904 ms airtime × the 10 % cap). A contact must therefore survive
///   discovery *and* leave transfer time: with a 300 s cadence the
///   worst-case discovery is 5 minutes, and a 10-minute hilltop stop
///   still moves a ≥30-message delta; the announce-heard sync trigger
///   below makes everything after discovery transfer.
/// * 300 s is also the discovery cadence precedent this project already
///   runs on BLE, so the two carriers advertise the role at one rhythm.
const PN_ANNOUNCE_INTERVAL_SECS: u64 = 300;

/// Store maintenance cadence (`JOB_STORE_INTERVAL × PROCESSING_INTERVAL`,
/// `reference/LXMF/LXMF/LXMRouter.py:871-900`).
const MAINTENANCE_SECS: u64 = 480;

/// `PN_STATS` cadence (instruction item 3).
const STATS_SECS: u64 = 300;

/// Outbound sync round watchdog, as on the host (`lnpnd/src/peering.rs`).
const OUTBOUND_DEADLINE_MS: u64 = 180_000;

/// `/offer` request timeout, as on the host.
const OFFER_REQUEST_TIMEOUT_MS: u64 = 60_000;

/// The board peer-table cap, §5's derivation re-checked against the
/// worst observed free heap (39 308 B, Pocket, end of the 12.69 h field
/// run): an 8 KiB peering slice gives `104·N + 6144 ≤ 8192`, N ≤ 19, and
/// 16 keeps margin for the sync round's own buffers. Re-measure on both
/// bins when the role has run on hardware; the number to compare is the
/// `[HEAP]` watermark with a full table.
const BOARD_MAX_PEERS: usize = 16;

/// Highest remote peering cost this board will mine a key for. The
/// reference accepts up to 26 (`MAX_PEERING_COST`,
/// `reference/LXMF/LXMF/LXMRouter.py:51`); at §2's orientation figures
/// cost 26 is 45-134 *minutes* of this board's single core, one-off per
/// peer but still the whole radio's CPU for the duration. 18 — the
/// reference's default announcement — is 10-32 s. Declining a costlier
/// peer is local policy the reference itself exercises at its own bound;
/// no wire byte differs.
const BOARD_REMOTE_PEERING_COST_MAX: u8 = 18;

/// Per-transfer (upload) limit announced in field 3, kilobytes of 1000
/// (`propagation_transfer_limit`, `reference/LXMF/LXMF/LXMPeer.py:370`).
/// The crate default, stated here because the heap budget below sums it.
const BOARD_TRANSFER_LIMIT_KB: u64 = 4;

/// Per-sync limit announced in field 4, kilobytes of 1000 (#388).
///
/// Was the crate default of 32 — an appetite this board cannot even
/// receive: the binaries cap incoming resources at
/// [`crate::MAX_INCOMING_RESOURCE_BYTES`] (8 KiB), so a peer taking the
/// 32 KB advertisement at its word watched its sync resource refused at
/// the resource layer anyway. 8 keeps the announced limit truthful AND
/// bounds the sync batch (`pn_batch=`) at a size the boot `HEAP_BUDGET`
/// can carry; a peer with more queued simply syncs in more rounds.
const BOARD_SYNC_LIMIT_KB: u64 = 8;

// The announced sync limit must stay receivable, or it is a lie peers
// pay for with a dead resource transfer. The limit bounds the WIRE
// resource, framing included (#388 pass 3 item 3): the peer's packing
// loop counts 24 B up front and 16 B per message and stops strictly
// below the limit (`per_message_overhead`/`cumulative_size`,
// `reference/LXMF/LXMF/LXMPeer.py:359-360`, strict skip at `:376`),
// while the shipped `msgpack([ts, [bin, ...]])` (`:466`) costs at most
// 13 B base + 3 B per message — each accounted term dominates its wire
// term (asserted at their definitions in `leviculum_lxmf::peering`,
// pinned by that crate's largest-legal-batch test), so the resource is
// strictly smaller than the accounted sum, which is strictly below
// this limit.
const _: () = assert!(BOARD_SYNC_LIMIT_KB as usize * 1000 <= crate::MAX_INCOMING_RESOURCE_BYTES);

/// The role's slice of the boot heap budget (`HEAP_BUDGET role=`, #388):
/// the worst case of the census's `pn_*` terms. Arithmetic:
///
/// * `pn_batch` — one inbound sync's parsed messages, bounded by the
///   announced [`BOARD_SYNC_LIMIT_KB`] (the batch drains one message per
///   settle pass, so at most one batch exists);
/// * `pn_work` — queued upload payloads, one per phone at the announced
///   [`BOARD_TRANSFER_LIMIT_KB`], two phones (the #388 field scenario);
/// * 5 KiB — everything structural: the peer table (§5's `104·16` plus
///   the 64 B of public keys each peer now keeps (#388 pass 3, ~1 KiB
///   at [`BOARD_MAX_PEERS`] — grown from the previous 4 KiB term
///   because the keys did not fit its slack), the offer plan
///   (`pn_out`), the per-link maps, the role's duplicate cache and the
///   store adapters' queued writes (`pn_flush`).
pub const ROLE_BUDGET_BYTES: usize =
    BOARD_SYNC_LIMIT_KB as usize * 1000 + 2 * BOARD_TRANSFER_LIMIT_KB as usize * 1000 + 5 * 1024;

/// Boot-time spine reservations (#388 step 3), allocated once in
/// [`Engine::new`] and reused: `VecDeque`/`Vec` never shrink, so the
/// event path re-enters these blocks instead of growing fresh ones into
/// an already-fragmented heap. Payload bytes ride IN the items and are
/// bounded by the announced limits above. Eight work slots: two phones'
/// uploads plus `/get` and `/offer` rounds in flight never reached five
/// on the field captures; growth past the reserve still works, it just
/// pays one reallocation.
const WORK_SLOTS: usize = 8;

/// Reserve for [`Engine::too_large`], same allocate-once reasoning.
const TOO_LARGE_SLOTS: usize = 8;

/// Active-delivery retry budget per stored message (instruction item 4).
const ACTIVE_ATTEMPTS: u8 = 3;

/// Gap between consecutive active-delivery sends: one in flight, and the
/// next only after the loop has been back to its channels.
const ACTIVE_GAP_MS: u64 = 1_000;

/// How soon the loop should call back while work is queued.
const WORK_POLL_MS: u64 = 25;

// ---------------------------------------------------------------------------
// Region plumbing
// ---------------------------------------------------------------------------

/// The store region as the synchronous scan sees it: a memory-mapped
/// byte view.
///
/// SAFETY of the view: the region is internal flash, memory-mapped and
/// always readable; the only writer is the record-store task through the
/// SoftDevice. A scan concurrent with an in-flight append sees the
/// record's commit word still erased and stops before it; a scan
/// concurrent with a page erase sees a header whose CRC fails and skips
/// the page (`leviculum_pn_store::scan` docs). CPU reads of flash stall
/// while the NVMC is busy and then return the settled bytes — the nRF52
/// has no torn word reads.
#[derive(Clone, Copy)]
pub struct FlashRegion {
    base: u32,
    len: u32,
}

impl FlashRegion {
    fn store_region() -> Self {
        let (base, len) = crate::record_store::region();
        Self { base, len }
    }
}

impl Region for FlashRegion {
    fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        // SAFETY: see the type docs.
        f(unsafe { core::slice::from_raw_parts(self.base as *const u8, self.len as usize) })
    }
}

/// `0x`-free lowercase hex of a byte slice, allocation-free.
struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Queued work
// ---------------------------------------------------------------------------

/// One deferred job: everything that needs the async flash or the stamp
/// validator, queued by [`Engine::on_events`] and drained one per
/// [`Engine::settle`].
enum Work {
    /// A client upload envelope, from a raw link packet (`proof` set) or
    /// a single-message resource (`proof` empty — the resource protocol
    /// acknowledges on its own).
    Upload {
        link_id: LinkId,
        data: Vec<u8>,
        proof: Option<[u8; 32]>,
        via: &'static str,
    },
    /// A `/get` request awaiting its (possibly purging) answer.
    Get {
        link_id: LinkId,
        request_id: [u8; 16],
        data: Vec<u8>,
    },
    /// An `/offer` request awaiting peering-key validation.
    Offer {
        link_id: LinkId,
        request_id: [u8; 16],
        data: Vec<u8>,
    },
}

/// An inbound multi-message sync resource being drained one message per
/// settle pass (module docs, §stamp validation).
struct SyncBatch {
    link_id: LinkId,
    remote: [u8; 16],
    messages: VecDeque<Vec<u8>>,
    accepted: usize,
    bytes: u64,
    invalid: usize,
}

/// One outbound sync round in flight, as on the host.
struct OutboundSync {
    peer: [u8; 16],
    link_id: LinkId,
    plan: OfferPlan,
    request_id: Option<[u8; 16]>,
    sending: Option<(usize, u64)>,
    deadline_ms: u64,
}

/// One packet-sized stored message being actively delivered (item 4).
struct ActiveDelivery {
    transient_id: TransientId,
    destination: [u8; 16],
    packet_hash: [u8; 16],
    attempts_left: u8,
}

/// A mined peering key coming back from [`miner_task`].
struct MineResult {
    peer: [u8; 16],
    key: [u8; 32],
    value: u16,
}

struct MineJob {
    peer: [u8; 16],
    material: [u8; 32],
    cost: u8,
}

static MINE_REQ: embassy_sync::channel::Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    MineJob,
    1,
> = embassy_sync::channel::Channel::new();
static MINE_RES: embassy_sync::channel::Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    MineResult,
    1,
> = embassy_sync::channel::Channel::new();

/// The peering-key miner: the board's stand-in for the host's worker
/// thread (`generate_peering_key` runs off the sync thread in the
/// reference too, `reference/LXMF/LXMF/LXMPeer.py:285-286`). Cooperative
/// — the stamper yields every 64 trials — so the node keeps meshing
/// while a key grinds; at the capped cost 18 that is 10-32 s of
/// background arithmetic, once per peer, persisted.
#[embassy_executor::task]
pub async fn miner_task() {
    loop {
        let job = MINE_REQ.receive().await;
        let started = embassy_time::Instant::now();
        let mut stamper = CooperativeStamper::cooperative(crate::rng::RawHwRng::new());
        let Ok(key) = stamper
            .generate(&job.material, job.cost, WORKBLOCK_EXPAND_ROUNDS_PEERING)
            .await
        else {
            // Only cost 255 is refused, and the table never admits it.
            continue;
        };
        let value = stamper
            .measure_stamp(&job.material, &key, WORKBLOCK_EXPAND_ROUNDS_PEERING)
            .await;
        crate::log::log_fmt(
            "PN_KEY ",
            format_args!(
                "mined peer={} cost={} value={} ms={}",
                Hex(&job.peer[..8]),
                job.cost,
                value,
                started.elapsed().as_millis()
            ),
        );
        MINE_RES
            .send(MineResult {
                peer: job.peer,
                key,
                value,
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// The engine's slice of the heap census (#388): estimated bytes per
/// part of the role's runtime state, from [`Engine::heap_census`]. The
/// message store itself is flash; what shows up here is only RAM the
/// role pins while working.
#[derive(Debug, Default, Clone, Copy)]
pub struct EngineHeapCensus {
    /// Live peers in the table.
    pub peer_count: usize,
    /// Peer table node structure.
    pub peers: usize,
    /// Queued work items (uploads, /get, /offer) with their payloads.
    pub work: usize,
    /// The inbound sync batch draining one message per settle pass.
    pub batch: usize,
    /// The outbound sync round's offer plan.
    pub outbound: usize,
    /// Per-link bookkeeping: pending proofs, validated links, inbound
    /// transfers, the too-large list.
    pub link_maps: usize,
    /// The role's own state ([`PropagationNode::heap_bytes`]): the
    /// processed-id duplicate cache and the announced name.
    pub role: usize,
    /// Unflushed writes queued at both store adapters.
    pub flush: usize,
}

impl EngineHeapCensus {
    /// Sum of every accounted field.
    pub fn total(&self) -> usize {
        self.peers
            + self.work
            + self.batch
            + self.outbound
            + self.link_maps
            + self.role
            + self.flush
    }
}

pub struct Engine {
    role: PropagationNode<PnStore<FlashRegion>>,
    peers: PeerTable,
    peer_store: PnPeerStore<FlashRegion>,
    gate: InboundGate,
    dest_hash: DestinationHash,
    identity: Identity,
    identity_hash: [u8; 16],
    pending_proofs: BTreeMap<LinkId, VecDeque<[u8; 32]>>,
    work: VecDeque<Work>,
    sync_batch: Option<SyncBatch>,
    outbound: Option<OutboundSync>,
    /// Inbound links whose `/offer` key validated
    /// (`validated_peer_links`, `reference/LXMF/LXMF/LXMRouter.py:2316`).
    validated_links: BTreeMap<LinkId, [u8; 16]>,
    inbound_transfers: Vec<LinkId>,
    mining_peer: Option<[u8; 16]>,
    last_synced: Option<[u8; 16]>,
    next_sync_at_ms: u64,
    next_maintenance_at_ms: u64,
    next_announce_at_ms: Option<u64>,
    next_stats_at_ms: u64,
    announce_withheld_logged: bool,
    active: Option<ActiveDelivery>,
    next_active_at_ms: u64,
    /// Records active delivery skipped as too large for one packet —
    /// left for `/get`, never retried actively.
    too_large: Vec<TransientId>,
}

impl Engine {
    /// Register the `lxmf.propagation` destination and its two request
    /// handlers on the node, and build the engine over the store region.
    ///
    /// `None` only if the node's identity cannot be re-derived, the same
    /// failure mode the delivery destination has.
    pub fn new<R, C, S>(node: &mut NodeCore<R, C, S>, config: StoredPnConfig) -> Option<Self>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let identity_bytes = node.identity().private_key_bytes().ok()?;
        let identity = Identity::from_private_key_bytes(&identity_bytes).ok()?;
        let identity_hash = *identity.hash();
        let mut destination = Destination::new(
            Some(identity.clone()),
            Direction::In,
            DestinationType::Single,
            APP_NAME,
            &[PROPAGATION_ASPECT],
        )
        .ok()?;
        destination.set_accepts_links(true);
        // The upload proof is the node's "stored" statement, so it must
        // not leave before the flush returns ("persist before you
        // prove"): the application, not the stack, proves.
        destination.set_proof_strategy(ProofStrategy::App);
        let dest_hash = *destination.hash();
        node.register_destination(destination);
        node.register_request_handler(
            dest_hash,
            MESSAGE_GET_PATH,
            leviculum_core::RequestPolicy::AllowAll,
        );
        node.register_request_handler(
            dest_hash,
            OFFER_REQUEST_PATH,
            leviculum_core::RequestPolicy::AllowAll,
        );

        let region = FlashRegion::store_region();
        let pages = region.len / SECTOR_SIZE;
        let store = PnStore::new(region, pages, crate::record_store::free_bytes_hint());
        let name = crate::name::mesh_name(&identity_hash);
        let role_config = PropagationNodeConfig {
            stamp_cost: config.stamp_cost,
            peering_cost: config.peering_cost,
            name: Some(name.as_bytes().to_vec()),
            transfer_limit_kb: BOARD_TRANSFER_LIMIT_KB,
            // The announced sync appetite must fit both the resource cap
            // and the heap budget (#388) — see BOARD_SYNC_LIMIT_KB.
            sync_limit_kb: BOARD_SYNC_LIMIT_KB,
            ..PropagationNodeConfig::default()
        };
        let mut role = PropagationNode::new(store, role_config);

        let peer_store = PnPeerStore::new(region);
        let mut peers = PeerTable::new(PeeringConfig {
            max_peers: BOARD_MAX_PEERS,
            peering_cost: config.peering_cost,
            remote_peering_cost_max: BOARD_REMOTE_PEERING_COST_MAX,
            ..PeeringConfig::default()
        });
        if let Ok(records) = peer_store.load_all() {
            peers.restore(records);
        }
        role.set_compute_stamp_value(peers.max_peer_min_cost() > 0);

        ROLE_ACTIVE.store(true, Ordering::Relaxed);
        crate::log::log_fmt_critical(
            "PN ",
            format_args!(
                "role dst={} peers={} store_pages={}",
                Hex(dest_hash.as_bytes()),
                peers.len(),
                pages
            ),
        );

        Some(Self {
            role,
            peers,
            peer_store,
            gate: InboundGate::default(),
            dest_hash,
            identity,
            identity_hash,
            pending_proofs: BTreeMap::new(),
            // #388 step 3: spines at their working size from boot,
            // reused for the engine's lifetime (see WORK_SLOTS).
            work: VecDeque::with_capacity(WORK_SLOTS),
            sync_batch: None,
            outbound: None,
            validated_links: BTreeMap::new(),
            inbound_transfers: Vec::with_capacity(crate::ble::MAX_LINKS),
            mining_peer: None,
            last_synced: None,
            next_sync_at_ms: 0,
            next_maintenance_at_ms: 0,
            next_announce_at_ms: None,
            next_stats_at_ms: 0,
            announce_withheld_logged: false,
            active: None,
            next_active_at_ms: 0,
            too_large: Vec::with_capacity(TOO_LARGE_SLOTS),
        })
    }

    /// The propagation destination hash, for boot banners.
    pub fn destination_hash(&self) -> &DestinationHash {
        &self.dest_hash
    }

    /// One engine-level heap census (#388): estimated bytes per part of
    /// the role's runtime state, walked at the owners — the model is
    /// [`leviculum_core::heap_census`]'s.
    pub fn heap_census(&self) -> EngineHeapCensus {
        use leviculum_core::heap_census as hc;
        let mut work = self.work.capacity() * core::mem::size_of::<Work>();
        for item in &self.work {
            work += match item {
                Work::Upload { data, .. } | Work::Get { data, .. } | Work::Offer { data, .. } => {
                    data.capacity()
                }
            };
        }
        let batch = self.sync_batch.as_ref().map_or(0, |batch| {
            hc::vec_deque_bytes(&batch.messages)
                + batch
                    .messages
                    .iter()
                    .map(|message| message.capacity())
                    .sum::<usize>()
        });
        let outbound = self
            .outbound
            .as_ref()
            .map_or(0, |sync| hc::vec_bytes(&sync.plan.ids));
        let link_maps = hc::btree_map_bytes(&self.pending_proofs)
            + self
                .pending_proofs
                .values()
                .map(hc::vec_deque_bytes)
                .sum::<usize>()
            + hc::btree_map_bytes(&self.validated_links)
            + hc::vec_bytes(&self.inbound_transfers)
            + hc::vec_bytes(&self.too_large);
        EngineHeapCensus {
            peer_count: self.peers.len(),
            peers: self.peers.heap_bytes(),
            work,
            batch,
            outbound,
            link_maps,
            role: self.role.heap_bytes(),
            flush: self.role.store().queued_heap_bytes() + self.peer_store.queued_heap_bytes(),
        }
    }

    /// When the loop should call [`Engine::settle`] again.
    pub fn next_deadline_ms(&self, now_ms: u64) -> u64 {
        if !self.work.is_empty()
            || self.sync_batch.is_some()
            || self.role.store().pending_ops() > 0
            || self.peer_store.pending_ops() > 0
        {
            return now_ms.saturating_add(WORK_POLL_MS);
        }
        let mut due = self
            .next_announce_at_ms
            .unwrap_or(now_ms.saturating_add(ANNOUNCE_DELAY_SECS * 1000));
        due = due.min(self.next_maintenance_at_ms.max(now_ms + 1));
        due = due.min(self.next_stats_at_ms.max(now_ms + 1));
        due = due.min(self.next_sync_at_ms.max(now_ms + 1));
        if let Some(sync) = &self.outbound {
            due = due.min(sync.deadline_ms);
        }
        due.max(now_ms + 1)
    }

    fn seed_clock<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        unix_secs: u64,
        from: &'static str,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if node.has_plausible_wall_clock() || unix_secs == 0 {
            return;
        }
        if node.set_wall_time_unix_secs(unix_secs, TimeSource::Overheard) {
            crate::set_time_source(TimeSource::Overheard);
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[TIME_SEED] source=overheard via={from} unix={unix_secs}"),
            );
            self.on_clock_seeded(node.emission_secs());
        }
    }

    /// The calendar just jumped from uptime seconds to real time: refresh
    /// every uptime-era peer liveness stamp so the 14-day unreachability
    /// cull does not read the jump as fourteen days of silence. "Heard
    /// this boot" is the honest reading of those stamps.
    fn on_clock_seeded(&mut self, now: u64) {
        let floor = leviculum_core::constants::EMISSION_PLAUSIBLE_MIN_SECS;
        let mut refreshed: Vec<[u8; 16]> = Vec::new();
        for peer in self.peers.iter_mut() {
            if peer.last_heard < floor && now >= floor {
                peer.last_heard = now;
                refreshed.push(peer.destination_hash);
            }
        }
        for destination in refreshed {
            self.persist_peer(&destination);
        }
    }

    // -----------------------------------------------------------------
    // Event ingestion (synchronous)
    // -----------------------------------------------------------------

    /// Digest one dispatch's events. Cheap and synchronous; anything that
    /// needs flash or the validator lands in the work queue.
    pub fn on_events<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        events: &[NodeEvent],
    ) -> TickOutput
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let mut out = TickOutput::default();
        for event in events {
            self.on_event(node, event, &mut out);
        }
        out
    }

    fn on_event<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        event: &NodeEvent,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        match event {
            NodeEvent::AnnounceReceived { announce, .. }
                if announce.name_hash()
                    == &Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT])
                    && announce.destination_hash() != &self.dest_hash =>
            {
                let destination = *announce.destination_hash().as_bytes();
                self.on_pn_announce(node, destination, announce.app_data());
            }
            NodeEvent::AnnounceReceived { announce, .. } => {
                self.trigger_active(node, announce.destination_hash().as_bytes(), out);
            }
            NodeEvent::PathFound {
                destination_hash, ..
            } => {
                self.trigger_active(node, destination_hash.as_bytes(), out);
            }
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator: false,
                destination_hash,
                ..
            } if destination_hash == &self.dest_hash => {
                // Resources gated by the application, as the reference
                // gates them (`propagation_link_established`,
                // `reference/LXMF/LXMF/LXMRouter.py:2188-2193`).
                let _ = node.set_resource_strategy(link_id, ResourceStrategy::AcceptApp);
            }
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator: true,
                ..
            } => {
                self.on_sync_link_up(node, link_id, out);
            }
            NodeEvent::ResponseReceived {
                link_id,
                request_id,
                response_data,
                ..
            } => {
                self.on_offer_response(node, link_id, request_id, response_data, out);
            }
            NodeEvent::RequestTimedOut {
                link_id,
                request_id,
            } => {
                let matches = self
                    .outbound
                    .as_ref()
                    .is_some_and(|s| s.link_id == *link_id && s.request_id == Some(*request_id));
                if matches {
                    self.finish_round("offer_timeout", 0, 0);
                    out.merge(node.close_link(link_id));
                }
            }
            NodeEvent::LinkProofRequested {
                link_id,
                packet_hash,
            } if self.owns_link(node, link_id) => {
                self.pending_proofs
                    .entry(*link_id)
                    .or_default()
                    .push_back(*packet_hash);
            }
            NodeEvent::LinkDataReceived { link_id, data } if self.owns_link(node, link_id) => {
                let proof = self
                    .pending_proofs
                    .get_mut(link_id)
                    .and_then(VecDeque::pop_front);
                self.work.push_back(Work::Upload {
                    link_id: *link_id,
                    data: data.clone(),
                    proof,
                    via: "packet",
                });
            }
            NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } if self.owns_link(node, link_id) => {
                // Refused before it moves when above the announced
                // per-sync limit (`reference/LXMF/LXMF/LXMRouter.py:2220-2224`).
                let accept = self.role.accepts_resource_of(*data_size);
                let verdict = if accept {
                    if self.validated_links.contains_key(link_id) {
                        self.inbound_transfers.push(*link_id);
                    }
                    node.accept_resource(link_id)
                } else {
                    node.reject_resource(link_id)
                };
                if let Ok(send) = verdict {
                    out.merge(send);
                }
            }
            NodeEvent::ResourceCompleted {
                link_id,
                data,
                is_sender: false,
                ..
            } if self.owns_link(node, link_id) => {
                self.on_inbound_resource(node, link_id, data, out);
            }
            NodeEvent::ResourceCompleted {
                link_id,
                is_sender: true,
                segment_index,
                total_segments,
                ..
            } if segment_index == total_segments => {
                self.on_resource_sent(node, link_id, true, out);
            }
            NodeEvent::ResourceFailed {
                link_id,
                is_sender: true,
                ..
            } => {
                self.on_resource_sent(node, link_id, false, out);
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &self.dest_hash && path == MESSAGE_GET_PATH => {
                self.work.push_back(Work::Get {
                    link_id: *link_id,
                    request_id: *request_id,
                    data: data.clone(),
                });
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &self.dest_hash && path == OFFER_REQUEST_PATH => {
                self.work.push_back(Work::Offer {
                    link_id: *link_id,
                    request_id: *request_id,
                    data: data.clone(),
                });
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                self.pending_proofs.remove(link_id);
                self.validated_links.remove(link_id);
                self.inbound_transfers.retain(|held| held != link_id);
                let batch_died = self
                    .sync_batch
                    .as_ref()
                    .is_some_and(|batch| batch.link_id == *link_id);
                if batch_died {
                    // The sender is gone; what was already ingested
                    // stays, the rest of the batch is dropped.
                    self.conclude_sync_batch();
                }
                if self
                    .outbound
                    .as_ref()
                    .is_some_and(|sync| sync.link_id == *link_id)
                {
                    let peer = self.outbound.as_ref().map(|s| s.peer).unwrap_or_default();
                    self.outbound = None;
                    if let Some(held) = self.peers.get_mut(&peer) {
                        held.state = SyncPhase::Idle;
                    }
                }
            }
            NodeEvent::PacketDeliveryConfirmed { packet_hash } => {
                if let Some(active) = &self.active {
                    if &active.packet_hash == packet_hash {
                        let tid = active.transient_id;
                        let dest = active.destination;
                        let _ = self.role.store_mut().purge(&tid);
                        crate::log::log_fmt(
                            "PN_DELIVER ",
                            format_args!("proved tid={} dst={}", Hex(&tid[..8]), Hex(&dest)),
                        );
                        self.active = None;
                        self.next_active_at_ms = node.now_ms().saturating_add(ACTIVE_GAP_MS);
                    }
                }
            }
            NodeEvent::DeliveryFailed { packet_hash, .. } => {
                let failed = self
                    .active
                    .as_ref()
                    .is_some_and(|active| &active.packet_hash == packet_hash);
                if failed {
                    let active = self.active.take();
                    self.next_active_at_ms = node.now_ms().saturating_add(ACTIVE_GAP_MS);
                    if let Some(active) = active {
                        let attempts_left = active.attempts_left.saturating_sub(1);
                        if attempts_left == 0 {
                            crate::log::log_fmt(
                                "PN_DELIVER ",
                                format_args!("gave_up dst={}", Hex(&active.destination)),
                            );
                        } else {
                            self.resend_active(
                                node,
                                active.transient_id,
                                active.destination,
                                attempts_left,
                                out,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Forget per-link state before an engine-initiated close: the
    /// `LinkClosed` that close produces rides the engine's own output and
    /// is never fed back to `on_events`, so the cleanup the remote-close
    /// path does there has to happen here by hand.
    fn drop_link_state(&mut self, link_id: &LinkId) {
        self.pending_proofs.remove(link_id);
        self.validated_links.remove(link_id);
        self.inbound_transfers.retain(|held| held != link_id);
    }

    fn owns_link<R, C, S>(&self, node: &NodeCore<R, C, S>, link_id: &LinkId) -> bool
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        node.link(link_id)
            .is_some_and(|link| link.destination_hash() == &self.dest_hash)
    }

    fn on_pn_announce<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        destination_hash: [u8; 16],
        app_data: &[u8],
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Ok(announce) = PropagationNodeAnnounce::decode(app_data) else {
            return;
        };
        // Item 6: a peer's announce timebase is a clock a clockless
        // board may take, through the same sanity window as every
        // other source.
        self.seed_clock(node, announce.timebase, "pn-announce");
        let hops = node.hops_to(&DestinationHash::new(destination_hash));
        let now = node.emission_secs();
        let change = self
            .peers
            .handle_announce(destination_hash, &announce, hops, now);
        // Capture the announcer's keys with the peer (#388 pass 3): the
        // announce that created or refreshed this peer also put the
        // identity into `known_identities`, but that cache is a rolling
        // 8-slot table — by this peer's sync round the entry may be gone.
        // The copy kept here (and persisted below with the peer record)
        // is what the sync recalls first.
        if let Some(peer) = self.peers.get_mut(&destination_hash) {
            if peer.public_keys.is_none() || peer.identity_hash.is_none() {
                if let Some(identity) = node.storage().get_identity(&destination_hash) {
                    peer.capture_identity(identity);
                }
            }
        }
        match change {
            PeerChange::Added => {
                self.log_peer("add", &destination_hash, "announce");
                self.persist_peer(&destination_hash);
                // An announce heard is a contact window open: schedule
                // the sync pass now rather than at the next interval
                // (item 7's announce-heard trigger).
                self.next_sync_at_ms = 0;
            }
            PeerChange::Updated => {
                self.persist_peer(&destination_hash);
                self.next_sync_at_ms = 0;
            }
            PeerChange::Dropped(reason) => {
                let _ = self.peer_store.remove(&destination_hash);
                self.log_peer(
                    "drop",
                    &destination_hash,
                    match reason {
                        DropReason::Disabled => "disabled",
                        DropReason::OutOfDepth => "out_of_depth",
                        DropReason::CostRaised => "cost_raised",
                        DropReason::Unreachable => "unreachable",
                        DropReason::NoAccess => "no_access",
                    },
                );
            }
            PeerChange::Declined(reason) => {
                if reason == DeclineReason::TableFull {
                    self.log_peer("decline", &destination_hash, "table_full");
                }
            }
        }
        self.role
            .set_compute_stamp_value(self.peers.max_peer_min_cost() > 0);
    }

    fn log_peer(&self, action: &str, destination_hash: &[u8; 16], reason: &str) {
        crate::log::log_fmt(
            "PN_PEER ",
            format_args!(
                "peer={} action={} reason={} peers={}",
                Hex(destination_hash),
                action,
                reason,
                self.peers.len()
            ),
        );
    }

    fn persist_peer(&mut self, destination_hash: &[u8; 16]) {
        if let Some(peer) = self.peers.get(destination_hash) {
            let record = PeerRecord::of(peer);
            let _ = self.peer_store.save(&record);
        }
    }

    // -----------------------------------------------------------------
    // Outbound sync (synchronous halves)
    // -----------------------------------------------------------------

    fn on_sync_link_up<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        link_id: &LinkId,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Some(sync) = self.outbound.as_ref() else {
            return;
        };
        if sync.link_id != *link_id {
            return;
        }
        let peer_hash = sync.peer;
        match node.identify_link(link_id, &self.identity) {
            Ok(send) => out.merge(send),
            Err(_) => {
                self.finish_round("identify_failed", 0, 0);
                out.merge(node.close_link(link_id));
                return;
            }
        }
        let Some(peer) = self.peers.get_mut(&peer_hash) else {
            return;
        };
        peer.sync_backoff_secs = 0;
        let Some((key, _)) = peer.peering_key else {
            self.finish_round("no_key", 0, 0);
            out.merge(node.close_link(link_id));
            return;
        };
        let Some(sync) = self.outbound.as_mut() else {
            return;
        };
        let offer = PeerOffer {
            peering_key: key,
            transient_ids: sync.plan.ids.clone(),
        };
        match node.send_request(
            link_id,
            OFFER_REQUEST_PATH,
            Some(&offer.encode()),
            Some(OFFER_REQUEST_TIMEOUT_MS),
        ) {
            Ok((request_id, send)) => {
                sync.request_id = Some(request_id);
                out.merge(send);
                if let Some(peer) = self.peers.get_mut(&peer_hash) {
                    peer.state = SyncPhase::RequestSent;
                }
            }
            Err(_) => {
                self.finish_round("request_failed", 0, 0);
                out.merge(node.close_link(link_id));
            }
        }
    }

    fn on_offer_response<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        response_data: &[u8],
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Some(sync) = self.outbound.as_mut() else {
            return;
        };
        if sync.link_id != *link_id || sync.request_id != Some(*request_id) {
            return;
        }
        let peer_hash = sync.peer;
        let plan = sync.plan.clone();
        let response = match OfferResponse::decode(response_data) {
            Ok(response) => response,
            Err(_) => {
                self.finish_round("bad_response", 0, 0);
                out.merge(node.close_link(link_id));
                return;
            }
        };
        let action = response_action(&response, &plan);
        let wanted = match &action {
            ResponseAction::SendMessages(ids) => ids.len(),
            _ => 0,
        };
        if !matches!(response, OfferResponse::Error(_)) {
            crate::log::log_fmt(
                "PN_OFFER ",
                format_args!(
                    "peer={} dir=out offered={} wanted={}",
                    Hex(&peer_hash),
                    plan.ids.len(),
                    wanted
                ),
            );
        }
        match action {
            ResponseAction::SendMessages(ids) => {
                let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(ids.len());
                for id in &ids {
                    if let Ok(Some(body)) = self.role.store().read_body(id) {
                        bodies.push(body);
                    }
                }
                if bodies.is_empty() {
                    self.conclude_round(node, &peer_hash, link_id, plan.cursor_target, 0, 0, out);
                    return;
                }
                let envelope = PeerSyncEnvelope {
                    timestamp: node.emission_secs() as f64,
                    messages: bodies,
                };
                let data = envelope.encode();
                let total = data.len() as u64;
                let count = envelope.messages.len();
                match node.send_resource(link_id, &data, None, true) {
                    Ok((_, send)) => {
                        out.merge(send);
                        if let Some(sync) = self.outbound.as_mut() {
                            sync.sending = Some((count, total));
                        }
                        if let Some(peer) = self.peers.get_mut(&peer_hash) {
                            peer.state = SyncPhase::ResourceTransferring;
                        }
                    }
                    Err(_) => {
                        self.finish_round("send_failed", 0, 0);
                        out.merge(node.close_link(link_id));
                    }
                }
            }
            ResponseAction::Concluded => {
                self.conclude_round(node, &peer_hash, link_id, plan.cursor_target, 0, 0, out);
            }
            ResponseAction::Backoff(secs) => {
                let next = node.emission_secs().saturating_add(secs);
                if let Some(peer) = self.peers.get_mut(&peer_hash) {
                    peer.next_sync_attempt = next;
                }
                self.finish_round("throttled", 0, 0);
                out.merge(node.close_link(link_id));
            }
            ResponseAction::Unpeer => {
                self.peers.remove(&peer_hash);
                let _ = self.peer_store.remove(&peer_hash);
                self.log_peer("drop", &peer_hash, "no_access");
                self.outbound = None;
                out.merge(node.close_link(link_id));
            }
            ResponseAction::RemineKey => {
                if let Some(peer) = self.peers.get_mut(&peer_hash) {
                    peer.peering_key = None;
                }
                self.persist_peer(&peer_hash);
                self.finish_round("invalid_key", 0, 0);
                out.merge(node.close_link(link_id));
            }
            ResponseAction::Retry => {
                self.finish_round("retry", 0, 0);
                out.merge(node.close_link(link_id));
            }
        }
    }

    fn on_resource_sent<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        link_id: &LinkId,
        success: bool,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Some(sync) = self.outbound.as_ref() else {
            return;
        };
        if sync.link_id != *link_id || sync.sending.is_none() {
            return;
        }
        let peer_hash = sync.peer;
        let (count, bytes) = sync.sending.unwrap_or((0, 0));
        let cursor_target = sync.plan.cursor_target;
        if success {
            self.conclude_round(node, &peer_hash, link_id, cursor_target, count, bytes, out);
        } else {
            self.finish_round("transfer_failed", 0, 0);
            out.merge(node.close_link(link_id));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn conclude_round<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        peer_hash: &[u8; 16],
        link_id: &LinkId,
        cursor_target: u64,
        transferred: usize,
        bytes: u64,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let now = node.emission_secs();
        if let Some(peer) = self.peers.get_mut(peer_hash) {
            peer.cursor = cursor_target;
            peer.state = SyncPhase::Idle;
            peer.next_sync_attempt = 0;
            peer.last_heard = now;
        }
        self.persist_peer(peer_hash);
        crate::log::log_fmt(
            "PN_SYNC ",
            format_args!(
                "peer={} dir=out transferred={} bytes={} result=ok",
                Hex(peer_hash),
                transferred,
                bytes
            ),
        );
        self.outbound = None;
        out.merge(node.close_link(link_id));
    }

    fn finish_round(&mut self, result: &str, transferred: usize, bytes: u64) {
        if let Some(sync) = &self.outbound {
            crate::log::log_fmt(
                "PN_SYNC ",
                format_args!(
                    "peer={} dir=out transferred={} bytes={} result={}",
                    Hex(&sync.peer),
                    transferred,
                    bytes,
                    result
                ),
            );
            let peer = sync.peer;
            if let Some(held) = self.peers.get_mut(&peer) {
                held.state = SyncPhase::Idle;
            }
        }
        self.outbound = None;
    }

    fn on_inbound_resource<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        link_id: &LinkId,
        data: &[u8],
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        self.inbound_transfers.retain(|held| held != link_id);
        let multi = PeerSyncEnvelope::decode(data)
            .map(|envelope| envelope.messages.len() > 1)
            .unwrap_or(false);
        if !multi {
            // The singleton form is a client upload riding a resource;
            // the resource protocol has its own acknowledgement, so no
            // packet proof exists to send.
            self.work.push_back(Work::Upload {
                link_id: *link_id,
                data: data.to_vec(),
                proof: None,
                via: "resource",
            });
            return;
        }
        let Some(remote) = self.validated_links.get(link_id).copied() else {
            // Multi-message without a validated peering key: torn down
            // (`reference/LXMF/LXMF/LXMRouter.py:2381-2389`).
            self.drop_link_state(link_id);
            out.merge(node.close_link(link_id));
            return;
        };
        let Ok(envelope) = PeerSyncEnvelope::decode(data) else {
            return;
        };
        self.seed_clock(node, envelope.timestamp as u64, "peer-sync");
        if self.sync_batch.is_some() {
            // One batch at a time; the gate throttled `/offer`s, so a
            // second batch here is a peer ignoring the throttle.
            self.drop_link_state(link_id);
            out.merge(node.close_link(link_id));
            return;
        }
        self.sync_batch = Some(SyncBatch {
            link_id: *link_id,
            remote,
            messages: envelope.messages.into(),
            accepted: 0,
            bytes: 0,
            invalid: 0,
        });
    }

    fn conclude_sync_batch(&mut self) {
        if let Some(batch) = self.sync_batch.take() {
            crate::log::log_fmt(
                "PN_SYNC ",
                format_args!(
                    "peer={} dir=in transferred={} bytes={} result={}",
                    Hex(&batch.remote),
                    batch.accepted,
                    batch.bytes,
                    if batch.invalid == 0 {
                        "ok"
                    } else {
                        "invalid_stamps"
                    }
                ),
            );
        }
    }

    // -----------------------------------------------------------------
    // Active delivery (instruction item 4)
    // -----------------------------------------------------------------

    /// A path to `destination` appeared: if the store holds a
    /// packet-sized message for it and nothing else is in flight, send
    /// it as the single opportunistic packet the originator would have
    /// sent (`reference/LXMF/LXMF/LXMessage.py:426-434` — the stored
    /// blob minus its 16-byte prefix IS that packet's ciphertext).
    fn trigger_active<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        destination: &[u8; 16],
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if self.active.is_some() || node.now_ms() < self.next_active_at_ms {
            return;
        }
        let mut candidate: Option<StoredMessage> = None;
        let too_large = &self.too_large;
        let _ = self.role.store().for_each(&mut |meta| {
            if &meta.destination_hash == destination
                && !too_large.contains(&meta.transient_id)
                && candidate.is_none_or(|held| meta.sequence < held.sequence)
            {
                candidate = Some(*meta);
            }
        });
        let Some(meta) = candidate else {
            return;
        };
        self.resend_active(node, meta.transient_id, *destination, ACTIVE_ATTEMPTS, out);
    }

    fn resend_active<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        transient_id: TransientId,
        destination: [u8; 16],
        attempts_left: u8,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Ok(Some(body)) = self.role.store().read_body(&transient_id) else {
            return;
        };
        if body.len() < 16 + STAMP_SIZE {
            return;
        }
        // lxmf_data ‖ stamp: strip the destination prefix and the stamp;
        // what remains is the originator's destination-encrypted payload.
        let ciphertext = &body[16..body.len() - STAMP_SIZE];
        match node.send_raw_single_packet(&DestinationHash::new(destination), ciphertext) {
            Ok((packet_hash, send)) => {
                out.merge(send);
                crate::log::log_fmt(
                    "PN_DELIVER ",
                    format_args!(
                        "sent tid={} dst={} bytes={} attempts_left={}",
                        Hex(&transient_id[..8]),
                        Hex(&destination),
                        ciphertext.len(),
                        attempts_left
                    ),
                );
                self.active = Some(ActiveDelivery {
                    transient_id,
                    destination,
                    packet_hash,
                    attempts_left,
                });
            }
            Err(leviculum_core::SendError::TooLarge) => {
                // Left for /get, never retried actively. Bounded: the
                // list only ever holds ids still in the store.
                if !self.too_large.contains(&transient_id) {
                    self.too_large.push(transient_id);
                    if self.too_large.len() > 64 {
                        self.too_large.remove(0);
                    }
                }
            }
            Err(_) => {
                // No path after all, or pacing: the next trigger retries.
                self.next_active_at_ms = node.now_ms().saturating_add(ACTIVE_GAP_MS);
            }
        }
    }

    // -----------------------------------------------------------------
    // The asynchronous half
    // -----------------------------------------------------------------

    /// Run due periodic jobs, process at most one queued work item, and
    /// flush the store adapters. The only awaiting entry point; call
    /// from a main-loop arm (never inside a `select`).
    pub async fn settle<R, C, S>(&mut self, node: &mut NodeCore<R, C, S>) -> TickOutput
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let mut out = TickOutput::default();
        let now_ms = node.now_ms();

        self.harvest_mining();
        self.tick_announce(node, now_ms, &mut out);
        self.tick_maintenance(node, now_ms);
        self.tick_stats(now_ms);
        self.tick_sync(node, now_ms, &mut out);

        if let Some(work) = self.work.pop_front() {
            self.perform(node, work, &mut out).await;
        } else if self.sync_batch.is_some() {
            self.perform_sync_step(node, &mut out).await;
        }

        self.flush(node).await;
        out
    }

    fn harvest_mining(&mut self) {
        let Ok(result) = MINE_RES.try_receive() else {
            return;
        };
        if self.mining_peer == Some(result.peer) {
            self.mining_peer = None;
        }
        if let Some(peer) = self.peers.get_mut(&result.peer) {
            peer.peering_key = Some((result.key, result.value));
            peer.state = SyncPhase::Idle;
        }
        self.persist_peer(&result.peer);
        self.next_sync_at_ms = 0;
    }

    fn tick_announce<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let due = *self
            .next_announce_at_ms
            .get_or_insert(now_ms + ANNOUNCE_DELAY_SECS * 1000);
        if now_ms < due {
            return;
        }
        if !crate::record_store::mounted() {
            // A role whose store did not mount must not invite uploads
            // it can never prove.
            if !self.announce_withheld_logged {
                self.announce_withheld_logged = true;
                crate::log::log_fmt_critical(
                    "PN ",
                    format_args!("announce withheld reason=store-unmounted"),
                );
            }
            self.next_announce_at_ms = Some(now_ms + 60_000);
            return;
        }
        self.announce_withheld_logged = false;
        // NOT clock-gated (instruction item 6): a clockless board still
        // announces, with its uptime timebase. A peer treats the small
        // timebase as merely old — creation is unconditional, only
        // updates are ordered by it (`peer`,
        // `reference/LXMF/LXMF/LXMRouter.py:2016`) — and the first
        // contact the announce invites is exactly what delivers a seed
        // (an upload's envelope timestamp, a peer's announce timebase).
        // The age-based bookkeeping the jump would break is epoch-guarded
        // where it lives (expiry in `PropagationNode::tick`, the peer
        // cull refresh in `seed_clock`). [TIME_SOURCE] in the periodic
        // banner says which clock stamped any given announce.
        let app_data = self.role.announce_app_data(node.emission_secs());
        if let Ok(send) = node.announce_destination(&self.dest_hash, Some(&app_data)) {
            out.merge(send);
            let d = self.dest_hash.as_bytes();
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!(
                    "[ANNOUNCE] sent dst={:02x}{:02x}{:02x}{:02x} reason=pn-periodic",
                    d[0], d[1], d[2], d[3]
                ),
            );
        }
        self.next_announce_at_ms = Some(now_ms + PN_ANNOUNCE_INTERVAL_SECS * 1000);
    }

    /// A BLE peer finished its identity handshake: announce the role to
    /// it, on its link alone, so a phone that just connected learns this
    /// board is a propagation node without waiting out the cadence.
    pub fn on_ble_peer_up<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        iface: usize,
        peer: [u8; 16],
    ) -> TickOutput
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let mut out = TickOutput::default();
        if !crate::record_store::mounted() {
            return out;
        }
        let app_data = self.role.announce_app_data(node.emission_secs());
        if let Ok(send) =
            node.announce_destination_to_peer(&self.dest_hash, Some(&app_data), iface, peer)
        {
            out.merge(send);
        }
        out
    }

    fn tick_maintenance<R, C, S>(&mut self, node: &mut NodeCore<R, C, S>, now_ms: u64)
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if now_ms < self.next_maintenance_at_ms {
            return;
        }
        self.next_maintenance_at_ms = now_ms + MAINTENANCE_SECS * 1000;
        let now = node.emission_secs();
        let evicted = self.role.tick(now);
        self.log_evictions(&evicted);
        for dropped in self.peers.cull(now) {
            let _ = self.peer_store.remove(&dropped);
            self.log_peer("drop", &dropped, "unreachable");
        }
    }

    fn log_evictions(&self, evicted: &[Eviction]) {
        for eviction in evicted {
            crate::log::log_fmt(
                "PN_EVICT ",
                format_args!(
                    "tid={} bytes={} age_s={} reason={}",
                    Hex(&eviction.transient_id[..8]),
                    eviction.size,
                    eviction.age_secs,
                    match eviction.reason {
                        EvictionReason::Expired => "expired",
                        EvictionReason::Displaced => "displaced",
                    }
                ),
            );
        }
    }

    fn tick_stats(&mut self, now_ms: u64) {
        if now_ms < self.next_stats_at_ms {
            return;
        }
        self.next_stats_at_ms = now_ms + STATS_SECS * 1000;
        let live = self.role.store().count().unwrap_or(0);
        STORE_FILL.store(live.min(u32::MAX as usize) as u32, Ordering::Relaxed);
        crate::log::log_fmt(
            "PN_STATS ",
            format_args!(
                "store={} free_bytes={} capacity={} peers={}",
                live,
                self.role.store().free_space(),
                self.role.store().capacity(),
                self.peers.len()
            ),
        );
    }

    fn tick_sync<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        if let Some(sync) = &self.outbound {
            if now_ms > sync.deadline_ms {
                let link_id = sync.link_id;
                self.finish_round("timeout", 0, 0);
                out.merge(node.close_link(&link_id));
            }
        }
        if self.outbound.is_some() || self.mining_peer.is_some() || now_ms < self.next_sync_at_ms {
            return;
        }
        self.next_sync_at_ms = now_ms + SYNC_INTERVAL_SECS * 1000;

        let now = node.emission_secs();
        let newest = self.role.store().newest_sequence().unwrap_or(0);
        let Some(destination) = self.peers.next_due(now, newest, self.last_synced) else {
            return;
        };
        self.last_synced = Some(destination);
        self.start_round(node, destination, now, now_ms, out);
    }

    fn start_round<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        destination: [u8; 16],
        now: u64,
        now_ms: u64,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        // Recall order (#388 pass 3): the keys kept with the peer first,
        // the node's rolling `known_identities` cache second. A cache hit
        // for a peer that has no keys yet is captured back into the peer
        // record, so the next recall no longer depends on the cache.
        let recalled = self
            .peers
            .get(&destination)
            .and_then(|peer| peer.recall_identity(node.storage()));
        let our_identity_hash = self.identity_hash;
        let Some(peer) = self.peers.get_mut(&destination) else {
            return;
        };
        let captured = match &recalled {
            Some(identity) => peer.capture_identity(identity),
            None => false,
        };
        if captured {
            self.persist_peer(&destination);
        }
        let Some(peer) = self.peers.get_mut(&destination) else {
            return;
        };
        if !peer.peering_key_ready() {
            let Some(peer_identity_hash) = peer.identity_hash else {
                out.merge(node.request_path(&DestinationHash::new(destination)));
                return;
            };
            peer.state = SyncPhase::KeyMining;
            let material = peering_key_material(&peer_identity_hash, &our_identity_hash);
            let cost = peer.peering_cost;
            if MINE_REQ
                .try_send(MineJob {
                    peer: destination,
                    material,
                    cost,
                })
                .is_ok()
            {
                self.mining_peer = Some(destination);
            } else if let Some(peer) = self.peers.get_mut(&destination) {
                peer.state = SyncPhase::Idle;
            }
            return;
        }

        let mut entries: Vec<StoredMessage> = Vec::new();
        let cursor = peer.cursor;
        if self
            .role
            .store()
            .for_each(&mut |meta| {
                if meta.sequence > cursor {
                    entries.push(*meta);
                }
            })
            .is_err()
        {
            return;
        }
        entries.sort_by_key(|meta| meta.sequence);
        let Some(plan) = build_offer(peer, &entries) else {
            return;
        };
        if plan.ids.is_empty() {
            peer.cursor = plan.cursor_target;
            self.persist_peer(&destination);
            return;
        }

        let Some(signing_key) = recalled
            .as_ref()
            .map(|identity| identity.ed25519_verifying().to_bytes())
        else {
            out.merge(node.request_path(&DestinationHash::new(destination)));
            return;
        };
        peer.sync_backoff_secs += SYNC_BACKOFF_STEP_SECS;
        peer.next_sync_attempt = now + peer.sync_backoff_secs;
        peer.state = SyncPhase::LinkEstablishing;
        let (link_id, _, core_out) =
            match node.connect(DestinationHash::new(destination), &signing_key) {
                Ok(connected) => connected,
                Err(_) => {
                    // Link-cap refusal (`MAX_ENDPOINT_LINKS`, #388): the
                    // core has emitted LinkRefused, which the bins render
                    // as a LINK_REFUSED line (`crate::events`); the
                    // backoff above is already booked, so this round
                    // retries on a later sync pass instead of spinning.
                    crate::log::log_fmt(
                        "PN_SYNC ",
                        format_args!("peer={} action=defer reason=link_cap", Hex(&destination)),
                    );
                    if let Some(peer) = self.peers.get_mut(&destination) {
                        peer.state = SyncPhase::Idle;
                    }
                    return;
                }
            };
        out.merge(core_out);
        self.outbound = Some(OutboundSync {
            peer: destination,
            link_id,
            plan,
            request_id: None,
            sending: None,
            deadline_ms: now_ms + OUTBOUND_DEADLINE_MS,
        });
    }

    // -----------------------------------------------------------------
    // Work processing (the awaiting parts)
    // -----------------------------------------------------------------

    /// Validate one stamp with the streaming validator, reporting the
    /// wall-clock cost as `PN_STAMP ms=` — the number instruction item 3
    /// wants measured on this hardware.
    async fn validate(
        &mut self,
        transient_id: &TransientId,
        stamp: &[u8; STAMP_SIZE],
    ) -> Option<u16> {
        let min_cost = self.role.min_accepted_cost();
        let compute = self.role.compute_stamp_value();
        if min_cost == 0 && !compute {
            return Some(0);
        }
        let started = embassy_time::Instant::now();
        let mut stamper = CooperativeStamper::cooperative(crate::rng::RawHwRng::new());
        let result = if min_cost == 0 {
            Some(
                stamper
                    .measure_stamp(transient_id, stamp, WORKBLOCK_EXPAND_ROUNDS_PN)
                    .await,
            )
        } else {
            stamper
                .validate_stamp(transient_id, stamp, min_cost, WORKBLOCK_EXPAND_ROUNDS_PN)
                .await
                .unwrap_or(None)
        };
        crate::log::log_fmt(
            "PN_STAMP ",
            format_args!(
                "ms={} min_cost={} valid={}",
                started.elapsed().as_millis(),
                min_cost,
                result.is_some() as u8
            ),
        );
        result
    }

    async fn perform<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        work: Work,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        match work {
            Work::Upload {
                link_id,
                data,
                proof,
                via,
            } => {
                // Item 6: the envelope timestamp is a phone's clock.
                if let Ok(envelope) = PeerSyncEnvelope::decode(&data) {
                    self.seed_clock(node, envelope.timestamp as u64, "upload");
                }
                // Validate outside the role call: the validator awaits,
                // the role's closure cannot.
                let precomputed =
                    match leviculum_lxmf::propagation::PropagationUpload::decode(&data) {
                        Ok(upload) => {
                            let transient_id = *upload.transient_id();
                            let stamp = *upload.propagation_stamp();
                            Some(self.validate(&transient_id, &stamp).await)
                        }
                        Err(_) => None,
                    };
                let now = node.emission_secs();
                let outcome = self
                    .role
                    .handle_upload(&data, now, |_, _| precomputed.flatten());
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
                        // Persist before you prove: the flush is the
                        // storage statement the proof makes.
                        let durable = self.flush(node).await;
                        crate::log::log_fmt(
                            "PN_ACCEPT ",
                            format_args!(
                                "tid={} dst={} bytes={} value={} dup={} via={} durable={}",
                                Hex(&transient_id[..8]),
                                Hex(&destination_hash),
                                size,
                                stamp_value,
                                duplicate as u8,
                                via,
                                durable as u8
                            ),
                        );
                        if durable {
                            if let Some(packet_hash) = proof {
                                if let Ok(send) = node.send_data_proof(&link_id, &packet_hash) {
                                    out.merge(send);
                                }
                            }
                        }
                    }
                    UploadOutcome::InvalidStamp { reject } => {
                        if let Ok((_, send)) = node.send_packet_on_link(&link_id, &reject) {
                            out.merge(send);
                        }
                        self.drop_link_state(&link_id);
                        out.merge(node.close_link(&link_id));
                    }
                    UploadOutcome::PeerSyncForm => {
                        // Multi-message on the packet path: nonconforming
                        // (`reference/LXMF/LXMF/LXMRouter.py:2382-2385`).
                        self.drop_link_state(&link_id);
                        out.merge(node.close_link(&link_id));
                    }
                    UploadOutcome::Malformed(_) => {}
                    UploadOutcome::StoreFailed(_) => {
                        // No proof leaves; the client keeps its retry.
                    }
                }
            }
            Work::Get {
                link_id,
                request_id,
                data,
            } => {
                let Some(identity) = node.get_remote_identity(&link_id).cloned() else {
                    let response = MessageListResponse::Error(PeerError::NoIdentity)
                        .encode()
                        .unwrap_or_default();
                    self.respond(node, &link_id, &request_id, &response, out);
                    return;
                };
                let name_hash = Destination::compute_name_hash(APP_NAME, &["delivery"]);
                let mailbox =
                    *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes();
                let now = node.emission_secs();
                match self.role.handle_get(&data, &mailbox, now) {
                    Ok(GetOutcome::List { response, count }) => {
                        crate::log::log_fmt(
                            "PN_GET ",
                            format_args!(
                                "dst={} form=list count={} bytes=0 purged=0",
                                Hex(&mailbox),
                                count
                            ),
                        );
                        self.respond(node, &link_id, &request_id, &response, out);
                    }
                    Ok(GetOutcome::Fetch {
                        response,
                        served,
                        served_bytes,
                        purged,
                    }) => {
                        // The purges the client confirmed go to flash
                        // before the response claims anything.
                        let _ = self.flush(node).await;
                        crate::log::log_fmt(
                            "PN_GET ",
                            format_args!(
                                "dst={} form=fetch count={} bytes={} purged={}",
                                Hex(&mailbox),
                                served.len(),
                                served_bytes,
                                purged.len()
                            ),
                        );
                        self.respond(node, &link_id, &request_id, &response, out);
                    }
                    Err(_) => {
                        // msgpack nil, the reference's answer to a
                        // request it could not process.
                        self.respond(node, &link_id, &request_id, &[0xC0], out);
                    }
                }
            }
            Work::Offer {
                link_id,
                request_id,
                data,
            } => {
                let response = self.answer_offer_work(node, &link_id, &data).await;
                self.respond(node, &link_id, &request_id, &response, out);
            }
        }
    }

    /// Answer one inbound `/offer`
    /// (`offer_request`, `reference/LXMF/LXMF/LXMRouter.py:2266-2329`).
    async fn answer_offer_work<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        link_id: &LinkId,
        data: &[u8],
    ) -> Vec<u8>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let now = node.emission_secs();
        let Some(identity) = node.get_remote_identity(link_id).cloned() else {
            return OfferResponse::Error(PeerError::NoIdentity).encode();
        };
        let remote_identity_hash = *identity.hash();
        let name_hash = Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]);
        let remote_hash =
            *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes();

        if let Err(error) = self.gate.admit(
            self.peers.config(),
            &remote_hash,
            now,
            // The sequential-validation gate: while a batch drains, new
            // offers wait (module docs).
            self.sync_batch.is_some(),
            self.inbound_transfers.len(),
        ) {
            return OfferResponse::Error(error).encode();
        }
        let Ok(offer) = PeerOffer::decode(data) else {
            return OfferResponse::Error(PeerError::InvalidData).encode();
        };
        let our_cost = self.peers.config().peering_cost;
        if our_cost > 0 {
            let material = peering_key_material(&self.identity_hash, &remote_identity_hash);
            let mut stamper = CooperativeStamper::cooperative(crate::rng::RawHwRng::new());
            let valid = stamper
                .validate_stamp(
                    &material,
                    &offer.peering_key,
                    our_cost,
                    WORKBLOCK_EXPAND_ROUNDS_PEERING,
                )
                .await
                .unwrap_or(None)
                .is_some();
            if !valid {
                return OfferResponse::Error(PeerError::InvalidKey).encode();
            }
        }
        self.validated_links.insert(*link_id, remote_hash);

        let store = self.role.store();
        let response = answer_offer(&offer.transient_ids, |id| {
            store.contains(id).unwrap_or(false)
        });
        let wanted = match &response {
            OfferResponse::WantNone => 0,
            OfferResponse::WantAll => offer.transient_ids.len(),
            OfferResponse::Wanted(ids) => ids.len(),
            OfferResponse::Error(_) => 0,
        };
        crate::log::log_fmt(
            "PN_OFFER ",
            format_args!(
                "peer={} dir=in offered={} wanted={}",
                Hex(&remote_hash),
                offer.transient_ids.len(),
                wanted
            ),
        );
        response.encode()
    }

    /// Ingest one message of the draining sync batch.
    async fn perform_sync_step<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Some(message) = self
            .sync_batch
            .as_mut()
            .and_then(|batch| batch.messages.pop_front())
        else {
            self.conclude_sync_batch();
            return;
        };
        // The same acceptance path as a client upload
        // (`reference/LXMF/LXMF/LXMRouter.py:2430-2436`).
        let precomputed = if message.len() > STAMP_SIZE {
            let (unstamped, stamp_bytes) = message.split_at(message.len() - STAMP_SIZE);
            let transient_id = leviculum_core::crypto::full_hash(unstamped);
            let mut stamp = [0u8; STAMP_SIZE];
            stamp.copy_from_slice(stamp_bytes);
            Some(self.validate(&transient_id, &stamp).await)
        } else {
            None
        };
        let now = node.emission_secs();
        let outcome = self
            .role
            .accept_stamped(&message, now, |_, _| precomputed.flatten());
        let durable = self.flush(node).await;
        match outcome {
            UploadOutcome::Accepted {
                transient_id,
                destination_hash,
                size,
                stamp_value,
                duplicate,
                ..
            } => {
                crate::log::log_fmt(
                    "PN_ACCEPT ",
                    format_args!(
                        "tid={} dst={} bytes={} value={} dup={} via=sync durable={}",
                        Hex(&transient_id[..8]),
                        Hex(&destination_hash),
                        size,
                        stamp_value,
                        duplicate as u8,
                        durable as u8
                    ),
                );
                if !duplicate && durable {
                    if let Some(batch) = self.sync_batch.as_mut() {
                        batch.accepted += 1;
                        batch.bytes += size as u64;
                    }
                }
            }
            UploadOutcome::InvalidStamp { .. }
            | UploadOutcome::Malformed(_)
            | UploadOutcome::PeerSyncForm => {
                if let Some(batch) = self.sync_batch.as_mut() {
                    batch.invalid += 1;
                }
            }
            UploadOutcome::StoreFailed(_) => {}
        }
        let drained = self.sync_batch.as_ref().map(|batch| {
            (
                batch.messages.is_empty(),
                batch.invalid,
                batch.remote,
                batch.link_id,
            )
        });
        if let Some((true, invalid, remote, link_id)) = drained {
            self.conclude_sync_batch();
            if invalid > 0 {
                // Invalid stamps throttle the sender
                // (`reference/LXMF/LXMF/LXMRouter.py:2440-2450`).
                self.gate.throttle(remote, node.emission_secs());
                self.drop_link_state(&link_id);
                out.merge(node.close_link(&link_id));
            }
        }
    }

    fn respond<R, C, S>(
        &self,
        node: &mut NodeCore<R, C, S>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        response: &[u8],
        out: &mut TickOutput,
    ) where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        match node.send_response(link_id, request_id, response) {
            Ok(send) => out.merge(send),
            Err(RequestError::PayloadTooLarge) => {
                if let Ok((_, send)) = node.send_response_resource(link_id, request_id, response) {
                    out.merge(send);
                }
            }
            Err(_) => {}
        }
    }

    /// Drain both adapters' queued writes through the record-store task.
    /// Returns whether every op landed; a failed append un-remembers its
    /// id from the duplicate cache so the client's retry is stored.
    async fn flush<R, C, S>(&mut self, _node: &mut NodeCore<R, C, S>) -> bool
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let mut all_ok = true;
        while let Some(op) = self.role.store().peek_op().cloned() {
            let ok = execute(&op).await;
            if let Some(free) = ok {
                self.role.store_mut().set_free_bytes(free);
            }
            let done = self.role.store_mut().op_done(ok.is_some());
            if ok.is_none() {
                all_ok = false;
                if let Some(FlushOp::Append { key, .. }) = done {
                    self.role.forget_processed(&key);
                }
            }
        }
        while let Some(op) = self.peer_store.peek_op().cloned() {
            let ok = execute(&op).await;
            if ok.is_none() {
                all_ok = false;
            }
            let _ = self.peer_store.op_done(ok.is_some());
        }
        all_ok
    }
}

/// Execute one [`FlushOp`] on the record-store task; `Some(free_bytes)`
/// on success.
async fn execute(op: &FlushOp) -> Option<u32> {
    use crate::record_store::{pn_execute, PnOp};
    let op = match op {
        FlushOp::Append {
            key,
            time,
            tag,
            body,
        } => PnOp::Append {
            key: *key,
            time: *time,
            tag: *tag,
            body: body.clone(),
        },
        FlushOp::Purge { offset, key } => PnOp::Purge {
            offset: *offset,
            key: *key,
        },
    };
    pn_execute(op).await.ok().map(|done| done.free_bytes)
}
