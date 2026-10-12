//! Propagation-stamp validation off the core lock (Codeberg #384 part 4,
//! deliverable 5).
//!
//! Part 1 validated inline in the engine's event hook. At the default
//! costs the Lead set for the boards (stamp cost 13) that is ~10 ms of
//! SHA-256 under the core mutex for every accepted message — the
//! reference keeps exactly this work off its router thread: inbound
//! batches validate on the resource callback's own thread, one at a
//! time under `sequential_validation_lock`
//! (`reference/LXMF/LXMF/LXMRouter.py:2395-2402`). Here a single worker
//! thread does the hash grinding; the engine's hook only queues.
//!
//! # What stays ordered
//!
//! The upload proof correlates to the packet by per-link FIFO (engine
//! module docs). Jobs enter one channel in event order, the worker is a
//! single thread, and results are drained in completion order, so the
//! per-link order of (validate → append → prove) is exactly the order
//! the packets arrived in. One worker also *is* the reference's
//! `sequential_pn_stamp_validation = yes` behaviour: a second batch
//! waits for the first.
//!
//! # What moves and what does not
//!
//! Only the workblock expansion and hashing move. Decoding is cheap and
//! done twice (once here to find the stamps, once in the role's own
//! accept path); the store append and the proof stay under the lock,
//! because "persist before you prove" is anchored to the store and the
//! link, both of which live there.
//!
//! Staying under the lock is not the same as running all at once under it.
//! A durable append is 1.1 to 1.5 ms of disk on the host store, so the
//! engine takes the drained results one message at a time
//! (`PERSIST_PER_HOOK`, `lnpnd/src/engine.rs`) — a 105-message batch
//! applied in a single hook held the core for 239 ms
//! (`docs/src/concepts/core-lock-budget.md`, "A durable store append is
//! inside the budget too").

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use leviculum_core::crypto::full_hash;
use leviculum_core::LinkId;
use leviculum_lxmf::constants::{LXMF_OVERHEAD, STAMP_SIZE, WORKBLOCK_EXPAND_ROUNDS_PN};
use leviculum_lxmf::{CooperativeStamper, PeerSyncEnvelope, PropagationUpload, TransientId};

use crate::peering::SyncSender;

/// Validate (or measure) one inbound message's propagation stamp.
///
/// At a minimum cost above 0 this is the reference's `validate_pn_stamp`
/// (`reference/LXMF/LXMF/LXStamper.py:84-96`); at cost 0 with peers that
/// filter by value, the true value is computed the way the reference
/// always does (concept paper §5's accept-path consequence).
pub(crate) fn validate_stamp_value(
    transient_id: &TransientId,
    stamp: &[u8; 32],
    min_cost: u8,
    compute_value: bool,
) -> Option<u16> {
    let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
    if min_cost == 0 {
        if compute_value {
            Some(futures::executor::block_on(stamper.measure_stamp(
                transient_id,
                stamp,
                WORKBLOCK_EXPAND_ROUNDS_PN,
            )))
        } else {
            Some(0)
        }
    } else {
        futures::executor::block_on(stamper.validate_stamp(
            transient_id,
            stamp,
            min_cost,
            WORKBLOCK_EXPAND_ROUNDS_PN,
        ))
        .unwrap_or(None)
    }
}

/// Re-price, once per store, every message stored with stamp value 0
/// (leviculum#442 D6). Returns how many changed.
///
/// A store written before 8f470aa7 holds 0 for whatever the node took at
/// cost 0 before its first demanding peer; a store `lxmd` left behind holds
/// 0 for every name without a value component. Stored as 0, such a message
/// is skipped by every peer whose minimum is above 0
/// (`reference/LXMF/LXMF/LXMPeer.py:340`), although its stamp, which the
/// body still carries in its last 32 bytes, may be worth more. The true
/// value is computed as the reference computes it at cost 0
/// (`reference/LXMF/LXMF/LXStamper.py:95`) and written in place; the
/// message keeps its sequence, so a peer whose cursor stands below it now
/// receives it.
///
/// At load, not at offer: building an offer runs under the core lock,
/// where a workblock expansion per stored message would break the lock
/// budget (`docs/src/concepts/core-lock-budget.md`), and before the node
/// is built nothing else is waiting. Once: half of all cost-0 stamps are
/// truly worth 0, and without the store's marker every start would grind
/// them again. What this daemon appends afterwards is priced at accept.
/// The cost is one workblock expansion and one rename per message: 200
/// messages took 229 ms in a release build on the coder host
/// (2026-10-12), so a full 30-day store of ten thousand is seconds, once.
pub fn reprice_stored_values(
    store: &mut leviculum_std::FilePropagationStore,
) -> Result<usize, leviculum_lxmf::storage::StorageError> {
    use leviculum_lxmf::PropagationStore as _;

    if store.stamp_values_true() {
        return Ok(0);
    }
    let mut unpriced = Vec::new();
    store.for_each(&mut |meta| {
        if meta.stamp_value == 0 {
            unpriced.push(meta.transient_id);
        }
    })?;
    let mut repriced = 0;
    for transient_id in unpriced {
        let Some(body) = store.read_body(&transient_id)? else {
            continue;
        };
        if body.len() <= LXMF_OVERHEAD + STAMP_SIZE {
            continue;
        }
        let mut stamp = [0u8; STAMP_SIZE];
        stamp.copy_from_slice(&body[body.len() - STAMP_SIZE..]);
        let value = validate_stamp_value(&transient_id, &stamp, 0, true).unwrap_or(0);
        let value = value.min(u8::MAX as u16) as u8;
        if value > 0 && store.set_stamp_value(&transient_id, value)? {
            repriced += 1;
        }
    }
    store.mark_stamp_values_true()?;
    Ok(repriced)
}

/// How the payload arrived, so the drained result re-enters the engine on
/// the path the event would have taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Carrier {
    /// A raw link packet (`LinkDataReceived`), owing a proof.
    Packet,
    /// A completed inbound resource (`ResourceCompleted`), which may be a
    /// client upload or a peer sync of any message count.
    Resource,
}

/// One inbound payload whose stamps need grinding.
pub(crate) struct ValidationJob {
    pub link_id: LinkId,
    pub carrier: Carrier,
    pub data: Vec<u8>,
    /// The packet's queued proof hash ([`Carrier::Packet`] only).
    pub proof: Option<[u8; 32]>,
    /// The resource's sender at enqueue ([`Carrier::Resource`] only; the
    /// default for a packet): captured when the resource concluded, because
    /// the peer may tear the link down — erasing the peering runtime's link
    /// state and the core's link — before the verdicts drain.
    pub sender: SyncSender,
    /// [`leviculum_lxmf::PropagationNode::min_accepted_cost`] at enqueue.
    pub min_cost: u8,
    /// [`leviculum_lxmf::PropagationNode::compute_stamp_value`] at enqueue.
    pub compute_value: bool,
}

/// The worker's answer: the job back, plus one verdict per transient ID it
/// could parse. The engine re-runs the role's accept path with a lookup
/// closure, so malformed payloads and store outcomes are still classified
/// by the one shared implementation.
pub(crate) struct ValidationDone {
    pub link_id: LinkId,
    pub carrier: Carrier,
    pub data: Vec<u8>,
    pub proof: Option<[u8; 32]>,
    /// [`ValidationJob::sender`], carried through unchanged.
    pub sender: SyncSender,
    pub verdicts: HashMap<TransientId, Option<u16>>,
}

/// The engine's handle: queue in the hook, drain in the tick.
pub(crate) struct ValidationWorker {
    jobs: Sender<ValidationJob>,
    results: Receiver<ValidationDone>,
    in_flight: usize,
}

impl ValidationWorker {
    pub(crate) fn spawn() -> Self {
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<ValidationJob>();
        let (results_tx, results_rx) = std::sync::mpsc::channel::<ValidationDone>();
        std::thread::spawn(move || run_worker(&jobs_rx, &results_tx));
        Self {
            jobs: jobs_tx,
            results: results_rx,
            in_flight: 0,
        }
    }

    /// Whether validating this job would do real work under the lock.
    /// False means the caller should take the inline path: a channel round
    /// trip for a no-op verdict would only add latency.
    pub(crate) fn worth_deferring(min_cost: u8, compute_value: bool) -> bool {
        min_cost > 0 || compute_value
    }

    pub(crate) fn enqueue(&mut self, job: ValidationJob) -> bool {
        match self.jobs.send(job) {
            Ok(()) => {
                self.in_flight += 1;
                true
            }
            Err(_) => false,
        }
    }

    pub(crate) fn try_recv(&mut self) -> Option<ValidationDone> {
        match self.results.try_recv() {
            Ok(done) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                Some(done)
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.in_flight > 0
    }
}

/// Everything (transient_id, stamp) shaped in one inbound payload.
///
/// Mirrors the role's own parsing: the singleton envelope via
/// [`PropagationUpload::decode`], the multi-message sync via
/// [`PeerSyncEnvelope::decode`] with each entry split as
/// `lxmf_data ‖ stamp` (`PropagationNode::accept_stamped`'s length guard,
/// `leviculum-lxmf/src/propagation_node.rs`). A payload neither parser
/// accepts yields no stamps; the engine's re-run classifies it.
fn stamps_in(data: &[u8]) -> Vec<(TransientId, [u8; STAMP_SIZE])> {
    if let Ok(upload) = PropagationUpload::decode(data) {
        return vec![(*upload.transient_id(), *upload.propagation_stamp())];
    }
    let Ok(envelope) = PeerSyncEnvelope::decode(data) else {
        return Vec::new();
    };
    let mut stamps = Vec::with_capacity(envelope.messages.len());
    for stamped in &envelope.messages {
        if stamped.len() <= LXMF_OVERHEAD + STAMP_SIZE {
            continue;
        }
        let (unstamped, stamp) = stamped.split_at(stamped.len() - STAMP_SIZE);
        let mut propagation_stamp = [0u8; STAMP_SIZE];
        propagation_stamp.copy_from_slice(stamp);
        stamps.push((full_hash(unstamped), propagation_stamp));
    }
    stamps
}

fn run_worker(jobs: &Receiver<ValidationJob>, results: &Sender<ValidationDone>) {
    while let Ok(job) = jobs.recv() {
        let mut verdicts = HashMap::new();
        for (transient_id, stamp) in stamps_in(&job.data) {
            let verdict =
                validate_stamp_value(&transient_id, &stamp, job.min_cost, job.compute_value);
            verdicts.insert(transient_id, verdict);
        }
        let done = ValidationDone {
            link_id: job.link_id,
            carrier: job.carrier,
            data: job.data,
            proof: job.proof,
            sender: job.sender,
            verdicts,
        };
        if results.send(done).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The multi-message parser must find every stamped entry, with the
    /// transient ID over the unstamped bytes (the ID the validator and the
    /// store share).
    #[test]
    fn sync_envelope_stamps_are_found_per_message() {
        let body_a = vec![0x11u8; LXMF_OVERHEAD + 8];
        let body_b = vec![0x22u8; LXMF_OVERHEAD + 9];
        let stamp = [0x33u8; STAMP_SIZE];
        let mut stamped_a = body_a.clone();
        stamped_a.extend_from_slice(&stamp);
        let mut stamped_b = body_b.clone();
        stamped_b.extend_from_slice(&stamp);
        let envelope = PeerSyncEnvelope {
            timestamp: 1.0,
            messages: vec![stamped_a, stamped_b],
        };
        let stamps = stamps_in(&envelope.encode());
        assert_eq!(stamps.len(), 2);
        assert_eq!(stamps[0].0, full_hash(&body_a));
        assert_eq!(stamps[1].0, full_hash(&body_b));
    }

    /// A payload neither parser accepts yields no stamps and no panic;
    /// the engine's own re-run classifies it as malformed.
    #[test]
    fn garbage_yields_no_stamps() {
        assert!(stamps_in(b"not msgpack at all").is_empty());
    }

    /// leviculum#442 D6: a message an older lnpnd stored at value 0 is not
    /// offered to a peer demanding 4; after the load-time re-pricing it is,
    /// at its true value and its old sequence, and a second load does no
    /// work.
    #[test]
    fn a_message_stored_at_value_zero_is_offered_once_repriced() {
        use leviculum_lxmf::peering::{build_offer, PeerTable, PeeringConfig, OFFER_BYTES_LIMIT};
        use leviculum_lxmf::propagation::PropagationNodeAnnounce;
        use leviculum_lxmf::{PropagationStore as _, StampCancel, StoredMessage};
        use leviculum_std::FilePropagationStore;

        let mut unstamped = vec![0x42u8; LXMF_OVERHEAD + 40];
        unstamped[16..].fill(0x17);
        let transient_id = full_hash(&unstamped);
        let stamp = futures::executor::block_on(
            CooperativeStamper::cooperative(rand_core::OsRng).generate(
                &transient_id,
                4,
                WORKBLOCK_EXPAND_ROUNDS_PN,
                &StampCancel::new(),
            ),
        )
        .expect("a cost-4 stamp");
        let true_value = validate_stamp_value(&transient_id, &stamp, 0, true).expect("measured");
        assert!(true_value >= 4);
        let mut body = unstamped.clone();
        body.extend_from_slice(&stamp);

        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 1 << 20).expect("open");
        store.append(&transient_id, 100, 0, &body).unwrap();
        let sequence = store.meta(&transient_id).unwrap().expect("stored").sequence;

        let mut table = PeerTable::new(PeeringConfig::default());
        let announce = PropagationNodeAnnounce {
            legacy_support: false,
            timebase: 1,
            enabled: true,
            transfer_limit_kb: 256,
            sync_limit_kb: 10_240,
            stamp_cost: 4,
            stamp_cost_flexibility: 0,
            peering_cost: 18,
            metadata: vec![],
        };
        let _ = table.handle_announce([1; 16], &announce, Some(2), 200, false);
        let peer = table.get(&[1; 16]).expect("peered");
        assert_eq!(peer.min_accepted_cost(), 4);
        let entries = |store: &FilePropagationStore| {
            let mut entries: Vec<StoredMessage> = Vec::new();
            store.for_each(&mut |entry| entries.push(*entry)).unwrap();
            entries
        };

        let before = build_offer(peer, &entries(&store), OFFER_BYTES_LIMIT).expect("a plan");
        assert!(
            before.ids.is_empty(),
            "stored at 0, it is below the peer's minimum"
        );
        assert_eq!(before.skipped_low_value, 1);

        assert_eq!(reprice_stored_values(&mut store).unwrap(), 1);
        let meta = store.meta(&transient_id).unwrap().expect("still stored");
        assert_eq!(meta.stamp_value as u16, true_value);
        assert_eq!(meta.sequence, sequence);
        let plan = build_offer(peer, &entries(&store), OFFER_BYTES_LIMIT).expect("offerable now");
        assert_eq!(plan.ids, vec![transient_id]);

        drop(store);
        let mut store = FilePropagationStore::open(dir.path(), 1 << 20).expect("reopen");
        assert_eq!(
            store.meta(&transient_id).unwrap().unwrap().stamp_value as u16,
            true_value
        );
        assert_eq!(reprice_stored_values(&mut store).unwrap(), 0);
    }
}
