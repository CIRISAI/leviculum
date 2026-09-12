//! The engine's peering half (Codeberg #384, part 2): announce-driven peer
//! table, the `/offer` request handler, inbound sync resources, and the
//! outbound sync state machine over the driver's core.
//!
//! Protocol decisions live in `leviculum_lxmf::peering` (no_std, unit
//! tested); this file is the I/O glue: links, requests, resources, worker
//! threads for key mining, persistence, and the structured `PN_PEER` /
//! `PN_OFFER` / `PN_SYNC` events. Reference pin 795fdaa throughout.
//!
//! # One sync at a time
//!
//! The reference syncs one peer per scheduler pass, picked at random from
//! the fastest pool (`sync_peers`, `reference/LXMF/LXMF/LXMRouter.py:2148-2176`).
//! This engine runs one outbound round *in flight* at a time, selected by
//! deterministic round-robin (`PeerTable::next_due`): the board's §5
//! budget allows exactly one, the host keeps the same shape, and
//! determinism is what the conformance cells can assert against.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, TryRecvError};

use leviculum_core::transport::TickOutput;
use leviculum_core::{Destination, DestinationHash, Identity, LinkId, Storage as _};
use leviculum_lxmf::constants::WORKBLOCK_EXPAND_ROUNDS_PEERING;
use leviculum_lxmf::control::{ControlPeerStats, HOPS_UNKNOWN};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::peering::{
    answer_offer, build_offer, peering_key_material, response_action, DeclineReason, DropReason,
    InboundGate, OfferPlan, OfferResponse, PeerChange, PeerOffer, PeerRecord, PeerStore,
    PeerSyncEnvelope, PeerTable, PeeringConfig, ResponseAction, SyncPhase, OFFER_REQUEST_PATH,
    SYNC_BACKOFF_STEP_SECS, SYNC_INTERVAL_SECS,
};
use leviculum_lxmf::propagation::PeerError;
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_lxmf::propagation_store::StoredMessage;
use leviculum_lxmf::{
    CooperativeStamper, PropagationNode, PropagationNodeAnnounce, PropagationStore, TransientId,
    UploadOutcome,
};

use crate::engine::EngineEvent;

/// Outbound round watchdog: a round that has not concluded in this long is
/// torn down and retried after the backoff already booked at link
/// establishment. Generous against the reference's own request timeouts.
const OUTBOUND_DEADLINE_MS: u64 = 180_000;

/// `/offer` request timeout handed to the core's request tracker.
const OFFER_REQUEST_TIMEOUT_MS: u64 = 60_000;

fn short_hex(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn full_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// One outbound sync round in flight.
struct OutboundSync {
    peer: [u8; 16],
    link_id: LinkId,
    plan: OfferPlan,
    request_id: Option<[u8; 16]>,
    /// Set once the resource is on the link.
    sending: Option<(usize, u64)>,
    deadline_ms: u64,
    concluded: bool,
}

/// A peering key being mined on its own thread — the reference also mines
/// off its sync thread (`generate_peering_key` spawned from `sync`,
/// `reference/LXMF/LXMF/LXMPeer.py:285-286`). At the reference's default
/// cost 18 the §5 table prices this in seconds on a host; at the maximum
/// 26 it is minutes, which is exactly what may not run under the core
/// lock.
struct MiningJob {
    peer: [u8; 16],
    receiver: Receiver<([u8; 32], u16)>,
}

/// The concrete core type, same alias the engine uses.
type Core = leviculum_std::driver::StdNodeCore;

/// Per-peer traffic counters for the control stats — runtime state, not
/// persisted, exactly like the reference's own counters, which reset with
/// the sync state on restore (`LXMPeer.from_bytes` rebuilds them from the
/// persisted dict; ours restart at zero and the stats say so honestly).
#[derive(Debug, Clone, Copy, Default)]
struct PeerTraffic {
    offered: u64,
    outgoing: u64,
    incoming: u64,
    rx_bytes: u64,
    tx_bytes: u64,
    last_sync_attempt: u64,
    /// The last round's outcome: the reference's `alive` flips true when
    /// a sync link comes up and false when an attempt outlives the last
    /// answer (`reference/LXMF/LXMF/LXMPeer.py:280`, `:328`, `:514`).
    alive: bool,
}

pub(crate) struct PeeringRuntime {
    table: PeerTable,
    gate: InboundGate,
    peer_store: Box<dyn PeerStore + Send>,
    our_identity: Identity,
    our_identity_hash: [u8; 16],
    /// Inbound links whose `/offer` peering key validated, keyed to the
    /// remote's propagation destination hash — the multi-message gate
    /// (`validated_peer_links`, `reference/LXMF/LXMF/LXMRouter.py:2316`,
    /// enforced at `:2381-2389`).
    validated_links: HashMap<LinkId, [u8; 16]>,
    /// Inbound sync resources currently transferring, for
    /// `max_inbound_syncs` (`accepted_offer_links`,
    /// `reference/LXMF/LXMF/LXMRouter.py:2197-2204`).
    inbound_transfers: HashSet<LinkId>,
    outbound: Option<OutboundSync>,
    mining: Option<MiningJob>,
    last_synced: Option<[u8; 16]>,
    next_sync_at_ms: u64,
    /// Runtime traffic counters per peer, for the control stats.
    traffic: HashMap<[u8; 16], PeerTraffic>,
    /// Peer display names from announce metadata (`PN_META_NAME`), for
    /// the control stats; runtime-only, like the counters.
    names: HashMap<[u8; 16], String>,
    /// Sync-form messages accepted from nodes not in the peer table.
    unpeered_incoming: u64,
    unpeered_rx_bytes: u64,
    /// A `--sync <peer>` trigger: sync this peer on the next scheduler
    /// pass, ahead of the round-robin (`peer_sync_request` calls the
    /// peer's own `sync()`, `reference/LXMF/LXMF/LXMRouter.py:852`).
    forced_next: Option<[u8; 16]>,
    events: std::sync::mpsc::Sender<EngineEvent>,
}

impl PeeringRuntime {
    pub(crate) fn new(
        config: PeeringConfig,
        peer_store: Box<dyn PeerStore + Send>,
        our_identity: Identity,
        events: std::sync::mpsc::Sender<EngineEvent>,
    ) -> Self {
        let our_identity_hash = *our_identity.hash();
        let mut table = PeerTable::new(config);
        match peer_store.load_all() {
            Ok(records) => table.restore(records),
            Err(error) => tracing::warn!("lnpnd: peer store load failed: {error}"),
        }
        Self {
            table,
            gate: InboundGate::default(),
            peer_store,
            our_identity,
            our_identity_hash,
            validated_links: HashMap::new(),
            inbound_transfers: HashSet::new(),
            outbound: None,
            mining: None,
            last_synced: None,
            next_sync_at_ms: 0,
            traffic: HashMap::new(),
            names: HashMap::new(),
            unpeered_incoming: 0,
            unpeered_rx_bytes: 0,
            forced_next: None,
            events,
        }
    }

    pub(crate) fn peer_count(&self) -> usize {
        self.table.len()
    }

    pub(crate) fn peers(&self) -> Vec<[u8; 16]> {
        self.table
            .iter()
            .map(|peer| peer.destination_hash)
            .collect()
    }

    /// Whether any peer requires a true stamp value of what we accept —
    /// drives `PropagationNode::set_compute_stamp_value` (§5).
    pub(crate) fn requires_stamp_values(&self) -> bool {
        self.table.max_peer_min_cost() > 0
    }

    /// This node's equivalent of the reference's per-peer unhandled count:
    /// live store entries above the peer's cursor — what the next round
    /// would look at. `None` when the peer is unknown.
    pub(crate) fn unhandled_toward<S: PropagationStore>(
        &self,
        node: &PropagationNode<S>,
        destination_hash: &[u8; 16],
    ) -> Option<usize> {
        let cursor = self.table.get(destination_hash)?.cursor;
        let mut count = 0usize;
        node.store()
            .for_each(&mut |meta| {
                if meta.sequence > cursor {
                    count += 1;
                }
            })
            .ok()?;
        Some(count)
    }

    /// Reset one peer's cursor to 0: the bounded-full-re-offer path (§5's
    /// reboot / reclaimed-page case), exposed so the conformance cells can
    /// drive a second round that a caught-up peer answers with "want
    /// none". Also clears the backoff so the next scheduler pass acts.
    pub(crate) fn reoffer(&mut self, destination_hash: &[u8; 16]) -> bool {
        let Some(peer) = self.table.get_mut(destination_hash) else {
            return false;
        };
        peer.cursor = 0;
        peer.next_sync_attempt = 0;
        peer.sync_backoff_secs = 0;
        let record = PeerRecord::of(peer);
        self.persist(&record);
        // Make the next tick schedule immediately.
        self.next_sync_at_ms = 0;
        true
    }

    /// Sync one peer now, ahead of the round-robin — the
    /// [`leviculum_lxmf::control::SYNC_REQUEST_PATH`] trigger
    /// (`peer_sync_request` calls the peer's own `sync()`,
    /// `reference/LXMF/LXMF/LXMRouter.py:852`). `false` when the peer is
    /// unknown, which the handler answers `ERROR_NOT_FOUND`.
    pub(crate) fn trigger_sync(&mut self, destination_hash: &[u8; 16]) -> bool {
        let Some(peer) = self.table.get_mut(destination_hash) else {
            return false;
        };
        peer.next_sync_attempt = 0;
        peer.sync_backoff_secs = 0;
        self.forced_next = Some(*destination_hash);
        self.next_sync_at_ms = 0;
        true
    }

    /// Break one peering — the
    /// [`leviculum_lxmf::control::UNPEER_REQUEST_PATH`] trigger
    /// (`peer_unpeer_request` calls `unpeer`,
    /// `reference/LXMF/LXMF/LXMRouter.py:864`). `false` when unknown.
    pub(crate) fn unpeer(&mut self, destination_hash: &[u8; 16]) -> bool {
        if !self.table.remove(destination_hash) {
            return false;
        }
        self.forget(destination_hash);
        self.traffic.remove(destination_hash);
        if self.forced_next == Some(*destination_hash) {
            self.forced_next = None;
        }
        self.log_peer("drop", destination_hash, "control");
        true
    }

    /// Messages accepted over sync links from nodes not in the peer table
    /// (`unpeered_propagation_incoming` / `…_rx_bytes` in the stats,
    /// `reference/LXMF/LXMF/LXMRouter.py:827-828`).
    pub(crate) fn unpeered_incoming(&self) -> (u64, u64) {
        (self.unpeered_incoming, self.unpeered_rx_bytes)
    }

    fn traffic_mut(&mut self, destination_hash: &[u8; 16]) -> &mut PeerTraffic {
        self.traffic.entry(*destination_hash).or_default()
    }

    /// Per-peer stats for the control destination — the fields of
    /// `compile_stats`'s peer entries (`reference/LXMF/LXMF/LXMRouter.py:775-803`)
    /// that this engine tracks; byte and message counters are runtime
    /// counters since start, rates are not measured and reported as 0.
    pub(crate) fn control_peer_stats(&self, core: &Core) -> Vec<ControlPeerStats> {
        self.table
            .iter()
            .map(|peer| {
                let traffic = self
                    .traffic
                    .get(&peer.destination_hash)
                    .copied()
                    .unwrap_or_default();
                let acceptance_rate = if traffic.offered > 0 {
                    traffic.outgoing as f64 / traffic.offered as f64
                } else {
                    0.0
                };
                ControlPeerStats {
                    peer_id: peer.destination_hash,
                    is_static: peer.is_static,
                    state: match peer.state {
                        // The reference's ladder (`LXMPeer.py:17-22`); key
                        // mining has no reference state and reports IDLE.
                        SyncPhase::Idle | SyncPhase::KeyMining => 0x00,
                        SyncPhase::LinkEstablishing => 0x01,
                        SyncPhase::RequestSent => 0x03,
                        SyncPhase::ResourceTransferring => 0x05,
                    },
                    alive: traffic.alive,
                    name: self.names.get(&peer.destination_hash).cloned(),
                    last_heard: peer.last_heard,
                    next_sync_attempt: peer.next_sync_attempt,
                    last_sync_attempt: traffic.last_sync_attempt,
                    sync_backoff: peer.sync_backoff_secs,
                    peering_timebase: peer.peering_timebase,
                    ler: 0,
                    str_rate: 0,
                    transfer_limit_kb: Some(peer.transfer_limit_kb),
                    sync_limit_kb: Some(peer.sync_limit_kb),
                    target_stamp_cost: Some(peer.stamp_cost as u64),
                    stamp_cost_flexibility: Some(peer.stamp_cost_flexibility as u64),
                    peering_cost: Some(peer.peering_cost as u64),
                    peering_key_value: peer.peering_key.map(|(_, value)| value as u64),
                    network_distance: core
                        .hops_to(&DestinationHash::new(peer.destination_hash))
                        .map(|hops| hops as u64)
                        .unwrap_or(HOPS_UNKNOWN),
                    rx_bytes: traffic.rx_bytes,
                    tx_bytes: traffic.tx_bytes,
                    acceptance_rate,
                    offered: traffic.offered,
                    outgoing: traffic.outgoing,
                    incoming: traffic.incoming,
                    unhandled: 0,
                }
            })
            .collect()
    }

    /// The peering configuration, for the node-level stats fields.
    pub(crate) fn config(&self) -> &PeeringConfig {
        self.table.config()
    }

    /// Our identity hash, the stats' `identity_hash` field.
    pub(crate) fn our_identity_hash(&self) -> [u8; 16] {
        self.our_identity_hash
    }

    /// Static-list length, for the stats' static/discovered split.
    pub(crate) fn static_peer_count(&self) -> usize {
        self.table.iter().filter(|peer| peer.is_static).count()
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    fn persist(&mut self, record: &PeerRecord) {
        if let Err(error) = self.peer_store.save(record) {
            tracing::warn!("lnpnd: peer store save failed: {error}");
        }
    }

    fn persist_peer(&mut self, destination_hash: &[u8; 16]) {
        if let Some(peer) = self.table.get(destination_hash) {
            let record = PeerRecord::of(peer);
            self.persist(&record);
        }
    }

    fn forget(&mut self, destination_hash: &[u8; 16]) {
        if let Err(error) = self.peer_store.remove(destination_hash) {
            tracing::warn!("lnpnd: peer store remove failed: {error}");
        }
    }

    fn log_peer(&self, action: &'static str, destination_hash: &[u8; 16], reason: &'static str) {
        tracing::debug!(
            event = "PN_PEER",
            peer = full_hex(destination_hash),
            action = action,
            reason = reason,
        );
        self.emit(EngineEvent::Peer {
            action,
            destination_hash: *destination_hash,
            reason,
        });
    }

    /// The remote's `lxmf.propagation` destination hash from its link
    /// identity, derived as the reference derives it
    /// (`offer_request`, `reference/LXMF/LXMF/LXMRouter.py:2269-2271`).
    fn propagation_hash_of(identity: &Identity) -> [u8; 16] {
        let name_hash = Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]);
        *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
    }

    // ------------------------------------------------------------------
    // Announces
    // ------------------------------------------------------------------

    /// Ingest one propagation announce
    /// (`LXMFPropagationAnnounceHandler`, `reference/LXMF/LXMF/Handlers.py:56-99`).
    pub(crate) fn on_announce(
        &mut self,
        core: &mut Core,
        destination_hash: [u8; 16],
        app_data: &[u8],
    ) {
        let Ok(announce) = PropagationNodeAnnounce::decode(app_data) else {
            return;
        };
        // The announced display name (`PN_META_NAME` in the metadata map),
        // kept for the control stats' per-peer `name` field.
        for (key, raw) in &announce.metadata {
            if *key == leviculum_lxmf::PN_META_NAME {
                let mut position = 0;
                if let Ok(bytes) = leviculum_lxmf::msgpack::read_bin(raw, &mut position) {
                    if let Ok(name) = String::from_utf8(bytes.to_vec()) {
                        self.names.insert(destination_hash, name);
                    }
                }
            }
        }
        let hops = core.hops_to(&DestinationHash::new(destination_hash));
        let now = unix_secs();
        let change = self
            .table
            .handle_announce(destination_hash, &announce, hops, now);
        // The identity arrived with the announce; keep its public keys
        // (and hash, for the peering-key material, `LXMPeer.py:258`) with
        // the peer itself (#388 pass 3), so recall does not depend on the
        // node's identity cache at sync time.
        if let Some(peer) = self.table.get_mut(&destination_hash) {
            if peer.public_keys.is_none() || peer.identity_hash.is_none() {
                if let Some(identity) = core.storage().get_identity(&destination_hash) {
                    peer.capture_identity(identity);
                }
            }
        }
        match change {
            PeerChange::Added => {
                self.log_peer("add", &destination_hash, "announce");
                self.persist_peer(&destination_hash);
            }
            PeerChange::Updated => self.persist_peer(&destination_hash),
            PeerChange::Dropped(reason) => {
                self.forget(&destination_hash);
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
    }

    // ------------------------------------------------------------------
    // Inbound: /offer and the sync resource
    // ------------------------------------------------------------------

    /// Answer one `/offer` request
    /// (`offer_request`, `reference/LXMF/LXMF/LXMRouter.py:2266-2329`).
    /// Returns the encoded response body.
    pub(crate) fn answer_offer_request<S: PropagationStore>(
        &mut self,
        core: &mut Core,
        node: &PropagationNode<S>,
        link_id: &LinkId,
        data: &[u8],
    ) -> Vec<u8> {
        let now = unix_secs();
        // Unidentified links get ERROR_NO_IDENTITY (:2267).
        let Some(identity) = core.get_remote_identity(link_id).cloned() else {
            return OfferResponse::Error(PeerError::NoIdentity).encode();
        };
        let remote_identity_hash = *identity.hash();
        let remote_hash = Self::propagation_hash_of(&identity);

        // The gates, in the reference's order (:2273-2295). Sequential
        // stamp validation cannot be observed mid-flight here — this
        // engine validates inside the same lock-held call that ingested
        // the resource — so that gate never fires on this host.
        if let Err(error) = self.gate.admit(
            self.table.config(),
            &remote_hash,
            now,
            false,
            self.inbound_transfers.len(),
        ) {
            return OfferResponse::Error(error).encode();
        }

        let Ok(offer) = PeerOffer::decode(data) else {
            return OfferResponse::Error(PeerError::InvalidData).encode();
        };

        // Peering-key validation at OUR announced cost (:2300-2315). At
        // cost 0 any key passes — the reference's own validator accepts
        // any stamp at target 0 (`stamp_valid`,
        // `reference/LXMF/LXMF/LXStamper.py:73-77`) — so the workblock is
        // skipped entirely.
        let our_cost = self.table.config().peering_cost;
        if our_cost > 0 {
            let material = peering_key_material(&self.our_identity_hash, &remote_identity_hash);
            let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
            let valid = futures::executor::block_on(stamper.validate_stamp(
                &material,
                &offer.peering_key,
                our_cost,
                WORKBLOCK_EXPAND_ROUNDS_PEERING,
            ))
            .unwrap_or(None)
            .is_some();
            if !valid {
                return OfferResponse::Error(PeerError::InvalidKey).encode();
            }
        }
        self.validated_links.insert(*link_id, remote_hash);

        let response = answer_offer(&offer.transient_ids, |id| {
            node.store().contains(id).unwrap_or(false)
        });
        let wanted = match &response {
            OfferResponse::WantNone => 0,
            OfferResponse::WantAll => offer.transient_ids.len(),
            OfferResponse::Wanted(ids) => ids.len(),
            OfferResponse::Error(_) => 0,
        };
        tracing::debug!(
            event = "PN_OFFER",
            peer = full_hex(&remote_hash),
            dir = "in",
            offered = offer.transient_ids.len(),
            wanted = wanted,
        );
        self.emit(EngineEvent::Offer {
            dir: "in",
            peer: remote_hash,
            offered: offer.transient_ids.len(),
            wanted,
        });
        response.encode()
    }

    /// Track an accepted inbound resource for `max_inbound_syncs`.
    pub(crate) fn on_inbound_resource_accepted(&mut self, link_id: &LinkId) {
        if self.validated_links.contains_key(link_id) {
            self.inbound_transfers.insert(*link_id);
        }
    }

    /// The peer behind an inbound link whose `/offer` peering key
    /// validated. The engine captures this the moment a sync resource
    /// concludes — the reference reads `validated_peer_links` inside the
    /// resource callback itself (`reference/LXMF/LXMF/LXMRouter.py:2381`),
    /// so a link torn down while the stamps are still on the validation
    /// worker cannot retroactively orphan the batch.
    pub(crate) fn validated_peer(&self, link_id: &LinkId) -> Option<[u8; 16]> {
        self.validated_links.get(link_id).copied()
    }

    /// Mark a link's peering key as validated, exactly as `/offer` does —
    /// for tests that need the state without a full offer round.
    #[cfg(test)]
    pub(crate) fn validate_link_for_tests(&mut self, link_id: LinkId, peer: [u8; 16]) {
        self.validated_links.insert(link_id, peer);
    }

    /// Handle a completed inbound resource when it is the multi-message
    /// peer-sync form. Returns `true` when handled here; `false` hands the
    /// single-message form back to the engine's client-upload path.
    ///
    /// The multi-message form without a validated peering key tears the
    /// link down (`reference/LXMF/LXMF/LXMRouter.py:2381-2389`); validated
    /// messages are ingested even when others in the batch fail, and any
    /// invalid stamp tears down and throttles the sender for
    /// `PN_STAMP_THROTTLE` (`:2440-2450`).
    ///
    /// `validated_peer` is [`Self::validated_peer`] as of the moment the
    /// resource concluded, captured by the caller: with validation on the
    /// worker, the drain may run after a `LinkClosed` already cleared the
    /// map (the reference peer tears its link down as soon as the transfer
    /// concludes), and a late lookup here would drop the whole batch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_sync_resource<S: PropagationStore>(
        &mut self,
        core: &mut Core,
        node: &mut PropagationNode<S>,
        link_id: &LinkId,
        validated_peer: Option<[u8; 16]>,
        data: &[u8],
        validate: &mut dyn FnMut(&TransientId, &[u8; 32]) -> Option<u16>,
        out: &mut TickOutput,
    ) -> bool {
        let Ok(envelope) = PeerSyncEnvelope::decode(data) else {
            return false;
        };
        if envelope.messages.len() <= 1 {
            self.inbound_transfers.remove(link_id);
            return false;
        }
        self.inbound_transfers.remove(link_id);
        let Some(remote_hash) = validated_peer else {
            tracing::debug!(
                "lnpnd: multi-message transfer without validated peering key; tearing down"
            );
            out.merge(core.close_link(link_id));
            return true;
        };

        let now = unix_secs();
        let mut accepted = 0usize;
        let mut duplicates = 0usize;
        let mut bytes = 0u64;
        let mut invalid = 0usize;
        for message in &envelope.messages {
            let outcome = node.accept_stamped(message, now, &mut *validate);
            match outcome {
                UploadOutcome::Accepted {
                    transient_id,
                    destination_hash,
                    size,
                    stamp_value,
                    duplicate,
                    evicted,
                } => {
                    for eviction in &evicted {
                        tracing::debug!(
                            event = "PN_EVICT",
                            tid = short_hex(&eviction.transient_id),
                            bytes = eviction.size,
                            age_s = eviction.age_secs,
                            reason = "displaced",
                        );
                    }
                    tracing::debug!(
                        event = "PN_ACCEPT",
                        tid = short_hex(&transient_id),
                        dst = full_hex(&destination_hash),
                        bytes = size,
                        value = stamp_value,
                        dup = duplicate,
                        via = "sync",
                    );
                    if duplicate {
                        duplicates += 1;
                    } else {
                        accepted += 1;
                        bytes += size as u64;
                    }
                }
                UploadOutcome::InvalidStamp { .. } => invalid += 1,
                UploadOutcome::Malformed(_) | UploadOutcome::PeerSyncForm => invalid += 1,
                UploadOutcome::StoreFailed(error) => {
                    // Eviction already ran inside accept; a message that
                    // still does not fit is dropped — the protocol's
                    // normal, unsignalled forgetting (§1 of the concept
                    // paper). Complete messages only: the store append is
                    // atomic at the verb boundary.
                    tracing::debug!("lnpnd: sync message dropped, store: {error}");
                }
            }
        }
        if self.table.get(&remote_hash).is_some() {
            let traffic = self.traffic_mut(&remote_hash);
            traffic.incoming += accepted as u64;
            traffic.rx_bytes += bytes;
        } else {
            // A key-validated sender we do not peer back with — the
            // reference's unpeered bucket (`LXMRouter.py:2496-2506`).
            self.unpeered_incoming += accepted as u64;
            self.unpeered_rx_bytes += bytes;
        }
        tracing::debug!(
            event = "PN_SYNC",
            peer = full_hex(&remote_hash),
            dir = "in",
            transferred = accepted,
            bytes = bytes,
            result = if invalid == 0 { "ok" } else { "invalid_stamps" },
        );
        self.emit(EngineEvent::SyncDone {
            dir: "in",
            peer: remote_hash,
            transferred: accepted,
            bytes,
            result: if invalid == 0 { "ok" } else { "invalid_stamps" },
        });
        let _ = duplicates;
        if invalid > 0 {
            self.gate.throttle(remote_hash, now);
            out.merge(core.close_link(link_id));
        }
        true
    }

    // ------------------------------------------------------------------
    // Outbound: the sync state machine
    // ------------------------------------------------------------------

    pub(crate) fn on_tick<S: PropagationStore>(
        &mut self,
        core: &mut Core,
        node: &mut PropagationNode<S>,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        self.harvest_mining();

        if let Some(sync) = &self.outbound {
            if now_ms > sync.deadline_ms {
                let peer = sync.peer;
                let link_id = sync.link_id;
                self.finish_round(&peer, "timeout", 0, 0);
                out.merge(core.close_link(&link_id));
            }
        }

        if self.outbound.is_some() || self.mining.is_some() || now_ms < self.next_sync_at_ms {
            return;
        }
        self.next_sync_at_ms = now_ms + SYNC_INTERVAL_SECS * 1000;

        let now = unix_secs();
        for dropped in self.table.cull(now) {
            self.forget(&dropped);
            self.traffic.remove(&dropped);
            self.names.remove(&dropped);
            self.log_peer("drop", &dropped, "unreachable");
        }
        node.set_compute_stamp_value(self.requires_stamp_values());

        let newest = node.store().newest_sequence().unwrap_or(0);
        // A control-triggered sync outranks the round-robin, like the
        // reference's `peer.sync()` call from `peer_sync_request`
        // (`reference/LXMF/LXMF/LXMRouter.py:852`).
        let forced = self
            .forced_next
            .take()
            .filter(|hash| self.table.get(hash).is_some());
        let Some(destination) =
            forced.or_else(|| self.table.next_due(now, newest, self.last_synced))
        else {
            return;
        };
        self.last_synced = Some(destination);
        self.start_round(core, node, destination, now, now_ms, out);
    }

    fn harvest_mining(&mut self) {
        let Some(job) = &self.mining else { return };
        match job.receiver.try_recv() {
            Ok((key, value)) => {
                let peer_hash = job.peer;
                self.mining = None;
                if let Some(peer) = self.table.get_mut(&peer_hash) {
                    peer.peering_key = Some((key, value));
                    peer.state = SyncPhase::Idle;
                }
                self.persist_peer(&peer_hash);
                tracing::debug!(
                    "lnpnd: peering key mined for {} value={value}",
                    short_hex(&peer_hash)
                );
                self.emit(EngineEvent::KeyMined {
                    peer: peer_hash,
                    value,
                });
                // Sync as soon as the scheduler comes back around.
                self.next_sync_at_ms = 0;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let peer_hash = job.peer;
                self.mining = None;
                if let Some(peer) = self.table.get_mut(&peer_hash) {
                    peer.state = SyncPhase::Idle;
                }
            }
        }
    }

    fn start_round<S: PropagationStore>(
        &mut self,
        core: &mut Core,
        node: &PropagationNode<S>,
        destination: [u8; 16],
        now: u64,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        // Fill in what the round needs: the keys kept with the peer
        // first, the node's identity cache second (#388 pass 3).
        self.traffic_mut(&destination).last_sync_attempt = now;
        let recalled = self
            .table
            .get(&destination)
            .and_then(|peer| peer.recall_identity(core.storage()));
        let Some(peer) = self.table.get_mut(&destination) else {
            return;
        };
        let captured = match &recalled {
            Some(identity) => peer.capture_identity(identity),
            None => false,
        };
        if captured {
            self.persist_peer(&destination);
        }
        let Some(peer) = self.table.get_mut(&destination) else {
            return;
        };

        // Key first: mined at the peer's announced cost, on a worker
        // (`generate_peering_key`, `reference/LXMF/LXMF/LXMPeer.py:242-265`),
        // then persisted so the grind happens once per peer, not per
        // restart (§5).
        if !peer.peering_key_ready() {
            let Some(peer_identity_hash) = peer.identity_hash else {
                out.merge(core.request_path(&DestinationHash::new(destination)));
                return;
            };
            peer.state = SyncPhase::KeyMining;
            let material = peering_key_material(&peer_identity_hash, &self.our_identity_hash);
            let cost = peer.peering_cost;
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
                if let Ok(stamp) = futures::executor::block_on(stamper.generate(
                    &material,
                    cost,
                    WORKBLOCK_EXPAND_ROUNDS_PEERING,
                )) {
                    let value = futures::executor::block_on(stamper.measure_stamp(
                        &material,
                        &stamp,
                        WORKBLOCK_EXPAND_ROUNDS_PEERING,
                    ));
                    let _ = sender.send((stamp, value));
                }
            });
            self.mining = Some(MiningJob {
                peer: destination,
                receiver,
            });
            return;
        }

        // The offer, from the cursor (§5); low-value and oversize entries
        // are stepped past for good, exactly as the reference marks them
        // handled (`LXMPeer.py:340`, `:370-373`).
        let mut entries: Vec<StoredMessage> = Vec::new();
        let cursor = peer.cursor;
        if node
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
            // Nothing offerable, but dead entries to step past.
            peer.cursor = plan.cursor_target;
            self.persist_peer(&destination);
            return;
        }

        // The link. Backoff is booked before establishment and cleared
        // when the link comes up (`LXMPeer.py:321-322`, `:330`, `:541`).
        let Some(signing_key) = recalled
            .as_ref()
            .map(|identity| identity.ed25519_verifying().to_bytes())
        else {
            out.merge(core.request_path(&DestinationHash::new(destination)));
            return;
        };
        peer.sync_backoff_secs += SYNC_BACKOFF_STEP_SECS;
        peer.next_sync_attempt = now + peer.sync_backoff_secs;
        peer.state = SyncPhase::LinkEstablishing;
        let (link_id, _, core_out) =
            match core.connect(DestinationHash::new(destination), &signing_key) {
                Ok(connected) => connected,
                Err(error) => {
                    // Link-cap refusal (`max_links`, #388). The backoff above is
                    // already booked, so the round simply retries on a later
                    // pass instead of spinning.
                    tracing::debug!("lnpnd: sync connect refused: {error}");
                    if let Some(peer) = self.table.get_mut(&destination) {
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
            concluded: false,
        });
    }

    /// Our sync link came up: identify and place the offer
    /// (`link_established`, `reference/LXMF/LXMF/LXMPeer.py:534-542`, then
    /// the request at `:385-390`).
    pub(crate) fn on_link_established(
        &mut self,
        core: &mut Core,
        link_id: &LinkId,
        out: &mut TickOutput,
    ) {
        let Some(sync) = self.outbound.as_mut() else {
            return;
        };
        if sync.link_id != *link_id {
            return;
        }
        let peer_hash = sync.peer;
        match core.identify_link(link_id, &self.our_identity) {
            Ok(send) => out.merge(send),
            Err(error) => {
                tracing::warn!("lnpnd: identify on sync link failed: {error:?}");
                self.finish_round(&peer_hash, "identify_failed", 0, 0);
                out.merge(core.close_link(link_id));
                return;
            }
        }
        self.traffic_mut(&peer_hash).alive = true;
        let Some(peer) = self.table.get_mut(&peer_hash) else {
            return;
        };
        peer.sync_backoff_secs = 0;
        let Some((key, _)) = peer.peering_key else {
            self.finish_round(&peer_hash, "no_key", 0, 0);
            out.merge(core.close_link(link_id));
            return;
        };
        let Some(sync) = self.outbound.as_mut() else {
            return;
        };
        let offer = PeerOffer {
            peering_key: key,
            transient_ids: sync.plan.ids.clone(),
        };
        match core.send_request(
            link_id,
            OFFER_REQUEST_PATH,
            Some(&offer.encode()),
            Some(OFFER_REQUEST_TIMEOUT_MS),
        ) {
            Ok((request_id, send)) => {
                sync.request_id = Some(request_id);
                out.merge(send);
                if let Some(peer) = self.table.get_mut(&peer_hash) {
                    peer.state = SyncPhase::RequestSent;
                }
            }
            Err(error) => {
                tracing::warn!("lnpnd: /offer request failed: {error:?}");
                self.finish_round(&peer_hash, "request_failed", 0, 0);
                out.merge(core.close_link(link_id));
            }
        }
    }

    /// The peer's `/offer` answer
    /// (`offer_response`, `reference/LXMF/LXMF/LXMPeer.py:400-490`).
    pub(crate) fn on_response<S: PropagationStore>(
        &mut self,
        core: &mut Core,
        node: &PropagationNode<S>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        response_data: &[u8],
        out: &mut TickOutput,
    ) {
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
                self.finish_round(&peer_hash, "bad_response", 0, 0);
                out.merge(core.close_link(link_id));
                return;
            }
        };
        let action = response_action(&response, &plan);
        let wanted = match &action {
            ResponseAction::SendMessages(ids) => ids.len(),
            _ => 0,
        };
        if !matches!(response, OfferResponse::Error(_)) {
            // Counted when the peer answered, the reference's own moment
            // (`self.offered += len(self.last_offer)`,
            // `reference/LXMF/LXMF/LXMPeer.py:475`, `:516` on the
            // concluded-resource arm).
            self.traffic_mut(&peer_hash).offered += plan.ids.len() as u64;
            tracing::debug!(
                event = "PN_OFFER",
                peer = full_hex(&peer_hash),
                dir = "out",
                offered = plan.ids.len(),
                wanted = wanted,
            );
            self.emit(EngineEvent::Offer {
                dir: "out",
                peer: peer_hash,
                offered: plan.ids.len(),
                wanted,
            });
        }
        match action {
            ResponseAction::SendMessages(ids) => {
                // Read the wanted bodies; an id purged since the offer is
                // skipped silently, the reference's own disposition for a
                // store that forgot (`LXMPeer.py:459-464` reads what still
                // exists).
                let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(ids.len());
                for id in &ids {
                    if let Ok(Some(body)) = node.store().read_body(id) {
                        bodies.push(body);
                    }
                }
                if bodies.is_empty() {
                    self.conclude_round(core, &peer_hash, link_id, plan.cursor_target, 0, 0, out);
                    return;
                }
                let envelope = PeerSyncEnvelope {
                    timestamp: unix_secs() as f64,
                    messages: bodies,
                };
                let data = envelope.encode();
                let total = data.len() as u64;
                let count = envelope.messages.len();
                match core.send_resource(link_id, &data, None, true) {
                    Ok((_, send)) => {
                        out.merge(send);
                        if let Some(sync) = self.outbound.as_mut() {
                            sync.sending = Some((count, total));
                        }
                        if let Some(peer) = self.table.get_mut(&peer_hash) {
                            peer.state = SyncPhase::ResourceTransferring;
                        }
                    }
                    Err(error) => {
                        tracing::warn!("lnpnd: sync resource send failed: {error:?}");
                        self.finish_round(&peer_hash, "send_failed", 0, 0);
                        out.merge(core.close_link(link_id));
                    }
                }
            }
            ResponseAction::Concluded => {
                self.conclude_round(core, &peer_hash, link_id, plan.cursor_target, 0, 0, out);
            }
            ResponseAction::Backoff(secs) => {
                if let Some(peer) = self.table.get_mut(&peer_hash) {
                    peer.next_sync_attempt = unix_secs() + secs;
                }
                self.finish_round(&peer_hash, "throttled", 0, 0);
                out.merge(core.close_link(link_id));
            }
            ResponseAction::Unpeer => {
                self.table.remove(&peer_hash);
                self.forget(&peer_hash);
                self.log_peer("drop", &peer_hash, "no_access");
                self.outbound = None;
                self.emit(EngineEvent::SyncDone {
                    dir: "out",
                    peer: peer_hash,
                    transferred: 0,
                    bytes: 0,
                    result: "no_access",
                });
                out.merge(core.close_link(link_id));
            }
            ResponseAction::RemineKey => {
                if let Some(peer) = self.table.get_mut(&peer_hash) {
                    peer.peering_key = None;
                }
                self.persist_peer(&peer_hash);
                self.finish_round(&peer_hash, "invalid_key", 0, 0);
                out.merge(core.close_link(link_id));
            }
            ResponseAction::Retry => {
                self.finish_round(&peer_hash, "retry", 0, 0);
                out.merge(core.close_link(link_id));
            }
        }
    }

    /// Our sync resource concluded: only now does the cursor advance
    /// (`resource_concluded`, `reference/LXMF/LXMF/LXMPeer.py:492-521`).
    pub(crate) fn on_resource_sent(
        &mut self,
        core: &mut Core,
        link_id: &LinkId,
        success: bool,
        out: &mut TickOutput,
    ) {
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
            self.conclude_round(core, &peer_hash, link_id, cursor_target, count, bytes, out);
        } else {
            // Cursor untouched: the whole plan is offered again after the
            // backoff, which is what "marks nothing handled until the
            // transfer concludes" means on our side.
            self.finish_round(&peer_hash, "transfer_failed", 0, 0);
            out.merge(core.close_link(link_id));
        }
    }

    pub(crate) fn on_request_timeout(
        &mut self,
        core: &mut Core,
        link_id: &LinkId,
        request_id: &[u8; 16],
        out: &mut TickOutput,
    ) {
        let Some(sync) = self.outbound.as_ref() else {
            return;
        };
        if sync.link_id != *link_id || sync.request_id != Some(*request_id) {
            return;
        }
        let peer_hash = sync.peer;
        self.finish_round(&peer_hash, "offer_timeout", 0, 0);
        out.merge(core.close_link(link_id));
    }

    pub(crate) fn on_link_closed(&mut self, link_id: &LinkId) {
        self.validated_links.remove(link_id);
        self.inbound_transfers.remove(link_id);
        let Some(sync) = self.outbound.as_ref() else {
            return;
        };
        if sync.link_id != *link_id {
            return;
        }
        let peer_hash = sync.peer;
        let concluded = sync.concluded;
        self.outbound = None;
        if let Some(peer) = self.table.get_mut(&peer_hash) {
            peer.state = SyncPhase::Idle;
        }
        if !concluded {
            self.emit(EngineEvent::SyncDone {
                dir: "out",
                peer: peer_hash,
                transferred: 0,
                bytes: 0,
                result: "link_closed",
            });
        }
    }

    /// Successful end of a round: cursor forward, persisted, reported.
    #[allow(clippy::too_many_arguments)]
    fn conclude_round(
        &mut self,
        core: &mut Core,
        peer_hash: &[u8; 16],
        link_id: &LinkId,
        cursor_target: u64,
        transferred: usize,
        bytes: u64,
        out: &mut TickOutput,
    ) {
        if let Some(peer) = self.table.get_mut(peer_hash) {
            peer.cursor = cursor_target;
            peer.state = SyncPhase::Idle;
            peer.next_sync_attempt = 0;
            peer.last_heard = unix_secs();
        }
        // The reference's concluded-transfer accounting
        // (`resource_concluded`, `reference/LXMF/LXMF/LXMPeer.py:514-518`).
        {
            let traffic = self.traffic_mut(peer_hash);
            traffic.alive = true;
            traffic.outgoing += transferred as u64;
            traffic.tx_bytes += bytes;
        }
        self.persist_peer(peer_hash);
        if let Some(sync) = self.outbound.as_mut() {
            sync.concluded = true;
        }
        tracing::debug!(
            event = "PN_SYNC",
            peer = full_hex(peer_hash),
            dir = "out",
            transferred = transferred,
            bytes = bytes,
            result = "ok",
        );
        self.emit(EngineEvent::SyncDone {
            dir: "out",
            peer: *peer_hash,
            transferred,
            bytes,
            result: "ok",
        });
        self.outbound = None;
        if let Some(peer) = self.table.get_mut(peer_hash) {
            peer.state = SyncPhase::Idle;
        }
        out.merge(core.close_link(link_id));
    }

    /// Unsuccessful end of a round: cursor untouched, backoff (already
    /// booked at establishment) stands, reported with its reason.
    fn finish_round(
        &mut self,
        peer_hash: &[u8; 16],
        result: &'static str,
        transferred: usize,
        bytes: u64,
    ) {
        // The attempt outlived the answer: the reference's liveness rule
        // (`reference/LXMF/LXMF/LXMPeer.py:280`).
        self.traffic_mut(peer_hash).alive = false;
        if let Some(sync) = self.outbound.as_mut() {
            sync.concluded = true;
        }
        tracing::debug!(
            event = "PN_SYNC",
            peer = full_hex(peer_hash),
            dir = "out",
            transferred = transferred,
            bytes = bytes,
            result = result,
        );
        self.emit(EngineEvent::SyncDone {
            dir: "out",
            peer: *peer_hash,
            transferred,
            bytes,
            result,
        });
        self.outbound = None;
        if let Some(peer) = self.table.get_mut(peer_hash) {
            peer.state = SyncPhase::Idle;
        }
    }
}
