//! The propagation node's message store boundary (Codeberg #384, part 1).
//!
//! A propagation node keeps exactly three protocol facts per message — the
//! transient ID, the destination hash, and the stored bytes — plus two local
//! policy fields, the receive timestamp and the stamp value
//! (`docs/src/concepts/propagation-node-on-a-board.md` §1, "What is
//! protocol"). This trait is that record and nothing more: no per-peer sets,
//! no index requirement, no assumption that iteration is cheap or that the
//! medium is a filesystem.
//!
//! # The record's shape
//!
//! * **Key**: the 32-byte transient ID, `SHA-256(lxmf_data)`
//!   (`lxmf_propagation`, `reference/LXMF/LXMF/LXMRouter.py:2494`).
//! * **Body**: `lxmf_data || stamp` — the destination-encrypted message with
//!   the 32-byte propagation stamp appended, exactly the bytes the reference
//!   writes to its message file (`LXMRouter.py:2512-2515`). The body's first
//!   16 bytes are therefore the destination hash; the reference reads it back
//!   from the head of its file the same way (`LXMRouter.py:578`).
//! * **Timestamp**: seconds. Local policy only (expiry and cull weight); no
//!   peer ever sees it.
//! * **Stamp value**: the validated stamp's leading-zero-bit count, one byte.
//!   Protocol-adjacent: a peer drops offers below its own requirement
//!   (`sync`, `reference/LXMF/LXMF/LXMPeer.py:340`).
//!
//! # Why the verbs look like `leviculum-record-log`
//!
//! Part 3 of #384 puts this store on the boards' 16-page internal-flash
//! record log, and the trait is shaped so that adapter is mechanical rather
//! than a redesign. Verb by verb (`leviculum-nrf/record-log/src/lib.rs`):
//!
//! * [`PropagationStore::append`] ↔ `RecordLog::append(key, time, tag, body)`
//!   (`lib.rs:429`) — key = transient ID, `time` = received-at (truncated to
//!   `u32` on the board), `tag` = stamp value.
//! * [`PropagationStore::for_each`] ↔ `RecordLog::for_each` (`lib.rs:541`).
//!   The visitor sees directory data only, never a full body; the record log
//!   yields `Record { key, time, tag, len }` and the adapter fills
//!   `destination_hash` from the record's own destination — its first 16
//!   body bytes — with one `memcpy` from memory-mapped flash per record (the
//!   concept page's §"Scan" is why that is free of the radio). That copy is
//!   the adapter's own business, not a body read through this boundary; why
//!   the read is a separate trait is the section below.
//! * [`PropagationBodyStore::read_body`] ↔ `RecordLog::read_body`
//!   (`lib.rs:574`).
//! * [`PropagationStore::purge`] ↔ `RecordLog::purge` (`lib.rs:709`).
//! * [`PropagationStore::free_space`] ↔ `RecordLog::free_bytes` (`lib.rs:683`)
//!   and [`PropagationStore::capacity`] ↔ `sectors() × SECTOR_PAYLOAD`.
//! * [`PropagationStore::space_required`] is the one verb with no record-log
//!   counterpart: that adapter answers [`StorageError::Full`] only for a body
//!   the log cannot hold at all, because the log's own round-robin reclaim is
//!   the board's eviction, so the default (the body length) is never spent
//!   there. A store whose eviction the role does drive overrides it, and the
//!   eviction section below says why.
//!
//! The record log's verbs are `async` (every flash program waits on the
//! SoftDevice's scheduler); these traits are synchronous because the host
//! implementations complete immediately and the role logic runs inside the
//! driver's core lock, where an `await` point has nothing to yield to. The
//! board adapter owns the async plumbing: each trait verb maps to exactly one
//! record-log call, so wrapping it in the firmware's executor is part 3's
//! adapter work, not a redesign of this boundary.
//!
//! # Why the body read is a separate trait
//!
//! Part 5 of #384 moves the RP2350's records onto a microSD card, and a card
//! is not memory-mapped. One body read there is an asynchronous block fetch
//! through the storage owner, and the role cannot block inside it, so `/get`
//! hands the caller a work list instead of bytes
//! ([`crate::propagation_node::PropagationNode::prepare_get`], then
//! [`crate::propagation_node::PropagationNode::finish_get`]). That fetch is
//! bounded from the directory before any body is read. Metadata verbs —
//! duplicate detection, counts, cursors and free space — never need the
//! card. A card caller retains the bodies it fetched and streams them
//! through [`crate::propagation_node::FetchSource`] without assembling a
//! response buffer; memory-mapped stores can stream directly from the
//! medium. Both sources implement [`PropagationBodyStore`], separately
//! from [`PropagationStore`]. Even the synchronous
//! [`crate::propagation_node::PropagationNode::handle_get`] only plans;
//! encoding or streaming its result is what needs the body trait.
//!
//! # Eviction, and where it deliberately is NOT
//!
//! The trait has no eviction verb. On the host the role evicts by the
//! reference's weight — `age × size`, `clean_message_store`
//! (`LXMRouter.py:1144`, weight at `:1056-1067`) — implemented in
//! [`crate::propagation_node`] on top of `for_each` + `purge`. The bytes it
//! must free are the ones a store would charge the incoming body
//! ([`PropagationStore::space_required`]), not `body.len()`: on a block-backed
//! store one body costs whole blocks plus its descriptor and commit, and
//! evicting by raw body bytes would loop forever against a
//! [`PropagationStore::free_space`] that frees blocks. The weight itself stays
//! `age × size`, the reference's own number. On the board
//! the record log reclaims round-robin: when the active page cannot fit the
//! next record, the oldest page is erased wholesale. That is a **deviation**
//! from the reference's per-message weighted cull, and it stands under the
//! deviation rule: (1) wire format is untouched — eviction is invisible on
//! the wire, a dropped message is simply absent from later `/get` lists,
//! which is exactly how the reference's own expiry and cull present
//! themselves (`docs/src/concepts/propagation-node-on-a-board.md` §1, "What a
//! peer expects when a node forgets"); (2) semantics are preserved —
//! forgetting is normal and unsignalled in this protocol; (3) it measurably
//! serves Priority 1 on that hardware: a fixed metadata page for weighted
//! eviction state would spend the whole 10 000-cycle erase budget in 18.6
//! days at measured field duty (concept page §2, "Endurance"), while
//! oldest-page-first is the log's native order and costs nothing. Oldest-page
//! reclaim is also close to the reference's weight in practice: `age` is the
//! dominant factor of `age × size` once messages are hours apart, and the log
//! is append-ordered.

use alloc::{collections::BTreeMap, vec::Vec};
use core::ops::ControlFlow;

use crate::{
    constants::{DESTINATION_LENGTH, STAMP_SIZE},
    propagation::TransientId,
    storage::StorageError,
};

/// Directory entry for one stored message: everything a `/get` list, an
/// eviction pass, or a future peering offer needs, without the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredMessage {
    /// `SHA-256(lxmf_data)` — the key of every exchange.
    pub transient_id: TransientId,
    /// First 16 bytes of the body; decides who may `/get` the message.
    pub destination_hash: [u8; DESTINATION_LENGTH],
    /// Body length in bytes (`lxmf_data || stamp`).
    pub size: u32,
    /// Local receive time, seconds. Feeds expiry and the cull weight.
    pub received_at: u64,
    /// Leading-zero-bit value of the validated propagation stamp.
    pub stamp_value: u8,
    /// Position in the store's append order, strictly monotone over the
    /// store's lifetime: the domain of the per-peer sync cursor
    /// (`docs/src/concepts/propagation-node-on-a-board.md` §5). On the
    /// board this is `page_sequence(u32) << 16 | offset(u16)` — the record
    /// log's page sequences are monotone, so the mapping is too. A store
    /// may renumber only in ways that keep relative order; a peer cursor
    /// that no longer matches any live sequence is answered by
    /// [`crate::peering::build_offer`] with a bounded full re-offer.
    pub sequence: u64,
}

/// Outcome of an explicit client confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmedPurge {
    Absent,
    /// The deletion has completed at the store's durability boundary.
    Committed,
    /// A deferred writer owns the exact deletion and its confirmation ID.
    Queued,
}

/// The store a propagation node keeps its accepted messages in.
///
/// See the module documentation for the record's shape and the record-log
/// mapping. Implementations must be durable at the verb boundary: when
/// [`append`](Self::append) returns `Ok`, the message survives a power cut,
/// because the role proves the upload packet only after this returns
/// ("persist before you prove",
/// `docs/src/concepts/propagation-node-on-a-board.md` §3). A store that
/// defers durability past `append` moves that boundary to its own
/// confirmation, and its caller proves only after it: the board's flush
/// queue (`leviculum-nrf/pn-store`) and the host store's writer
/// (`FilePropagationStore::write_behind` in `leviculum-std`).
pub trait PropagationStore {
    /// Store one message. The body must be `lxmf_data || stamp` and at least
    /// `DESTINATION_LENGTH + STAMP_SIZE` bytes. Returns
    /// [`StorageError::Full`] when the message does not fit; the caller
    /// evicts and retries.
    fn append(
        &mut self,
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError>;

    /// Visit every stored message's directory entry, without reading bodies.
    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError>;

    /// Visit the entries whose [`StoredMessage::sequence`] is above
    /// `cursor`, ascending by sequence, until `visit`
    /// breaks: the walk an offer is built from
    /// ([`OfferBuilder`](crate::peering::OfferBuilder)).
    ///
    /// The default collects one directory pass and sorts it, so it costs the
    /// whole store whatever the visitor reads. A store that keeps its entries
    /// in sequence order overrides it to cost what the visitor reads: an
    /// offer stops after a handful of IDs, and the host node walked 300 000
    /// files for it on every sync tick (leviculum#478).
    fn for_each_above(
        &self,
        cursor: u64,
        visit: &mut dyn FnMut(&StoredMessage) -> ControlFlow<()>,
    ) -> Result<(), StorageError> {
        let mut entries: Vec<StoredMessage> = Vec::new();
        self.for_each(&mut |meta| {
            if meta.sequence > cursor {
                entries.push(*meta);
            }
        })?;
        entries.sort_by_key(|meta| meta.sequence);
        for meta in &entries {
            if visit(meta).is_break() {
                break;
            }
        }
        Ok(())
    }

    /// Visit the entries whose [`StoredMessage::received_at`] is below
    /// `before`, in no promised order, until `visit` breaks: the walk the
    /// role's expiry sweep reads
    /// ([`PropagationNode::tick`](crate::PropagationNode::tick)).
    ///
    /// The default is one directory pass, so it costs the whole store
    /// whatever expired. A store that keeps its entries in receive-time
    /// order overrides it to cost what the visitor reads: the host node
    /// walked 300 000 entries for it every maintenance tick, under the core
    /// lock (leviculum#478).
    fn for_each_received_before(
        &self,
        before: u64,
        visit: &mut dyn FnMut(&StoredMessage) -> ControlFlow<()>,
    ) -> Result<(), StorageError> {
        let mut done = false;
        self.for_each(&mut |meta| {
            if !done && meta.received_at < before {
                done = visit(meta).is_break();
            }
        })
    }

    /// For each of `cursors`, how many entries have a
    /// [`StoredMessage::sequence`] above it and a
    /// [`StoredMessage::size`] of at most `largest_size`: a peer's
    /// unhandled count, what its next sync would look at
    /// ([`PropagationNode::unhandled_above`](crate::PropagationNode::unhandled_above)).
    ///
    /// The default collects one directory pass, sorts it and searches each
    /// cursor, `O(n log n)`. A store that keeps a counting index overrides
    /// it to `O(log n)` a cursor: the host node walked 320 000 entries per
    /// peer for every `--status` request (leviculum#478, order 1370).
    fn count_above_cursors(
        &self,
        cursors: &[u64],
        largest_size: u64,
    ) -> Result<Vec<u64>, StorageError> {
        count_above_cursors_by_walk(self, cursors, largest_size)
    }

    /// The `largest_size` [`Self::count_above_cursors`] will be asked with,
    /// told by the role when it is built so a counting index can weigh its
    /// entries by it. The default keeps no index and ignores it.
    fn count_sizes_up_to(&mut self, _largest_size: u64) {}

    /// Remove one message. Returns whether it was present.
    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError>;

    /// Delete an explicitly confirmed message, retaining its provenance when
    /// an asynchronous writer must complete the operation.
    fn purge_confirmed(
        &mut self,
        transient_id: &TransientId,
    ) -> Result<ConfirmedPurge, StorageError> {
        self.purge(transient_id).map(|present| {
            if present {
                ConfirmedPurge::Committed
            } else {
                ConfirmedPurge::Absent
            }
        })
    }

    /// One message's directory entry, or `None` if absent. One directory
    /// pass by default.
    fn meta(&self, transient_id: &TransientId) -> Result<Option<StoredMessage>, StorageError> {
        let mut found = None;
        self.for_each(&mut |meta| {
            if found.is_none() && meta.transient_id == *transient_id {
                found = Some(*meta);
            }
        })?;
        Ok(found)
    }

    /// Bytes still available for message bodies.
    fn free_space(&self) -> u64;

    /// Total body capacity in bytes.
    fn capacity(&self) -> u64;

    /// Bytes a body of this length consumes: the default is the body itself;
    /// a block-backed store charges whole blocks plus its framing.
    ///
    /// The role asks this before evicting, so the number a store charges on
    /// the way in is the number its [`Self::free_space`] gives back on the
    /// way out (a block-backed store's `free_space` frees physical blocks,
    /// so evicting by raw body bytes would never reach its target).
    /// The role's `make_room` is the one caller; the weight that decides
    /// *which* message goes stays the reference's `age × size` over
    /// [`StoredMessage::size`].
    fn space_required(&self, body_len: usize) -> u64 {
        body_len as u64
    }

    /// Whether a transient ID is stored. Duplicate detection
    /// (`lxmf_propagation`, `reference/LXMF/LXMF/LXMRouter.py:2496`).
    ///
    /// Required of every store: the answer comes from the store's own
    /// directory, not from a body read — a store whose medium is not
    /// memory-mapped ([`PropagationBodyStore`]) has no synchronous body
    /// read to fall back on, and a directory pass is what the record log's
    /// mount already pays for.
    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError>;

    /// Whether the store itself still remembers that a message received at
    /// or after `received_since` left it before it expired, so a restart
    /// does not turn its re-offer into a new copy (the reference persists
    /// its whole duplicate set, `reference/LXMF/LXMF/LXMRouter.py:1018-1024`).
    ///
    /// Default `false`: a host persists its departures beside the store
    /// ([`crate::PropagationNode::restore_departures`]). The board's record
    /// log keeps a purged record on flash until its page is reclaimed, and
    /// answers from that (`leviculum-nrf/pn-store`).
    fn departed(
        &self,
        _transient_id: &TransientId,
        _received_since: u64,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Number of stored messages — one directory pass by default; the record
    /// log's mount already reports the same count
    /// (`leviculum-nrf/record-log/src/lib.rs:420`).
    fn count(&self) -> Result<usize, StorageError> {
        let mut total = 0usize;
        self.for_each(&mut |_| total += 1)?;
        Ok(total)
    }

    /// The highest live [`StoredMessage::sequence`], 0 when empty — what
    /// the sync scheduler compares peer cursors against. One directory
    /// pass by default.
    fn newest_sequence(&self) -> Result<u64, StorageError> {
        let mut newest = 0u64;
        self.for_each(&mut |meta| newest = newest.max(meta.sequence))?;
        Ok(newest)
    }
}

/// [`PropagationStore::count_above_cursors`] over one directory pass, sorted
/// once and searched per cursor: the trait's default, and the answer a
/// counting index falls back to when asked for a size it does not weigh.
pub fn count_above_cursors_by_walk<S: PropagationStore + ?Sized>(
    store: &S,
    cursors: &[u64],
    largest_size: u64,
) -> Result<Vec<u64>, StorageError> {
    let mut sequences: Vec<u64> = Vec::new();
    store.for_each(&mut |meta| {
        if u64::from(meta.size) <= largest_size {
            sequences.push(meta.sequence);
        }
    })?;
    sequences.sort_unstable();
    Ok(cursors
        .iter()
        .map(|cursor| {
            let at_or_below = sequences.partition_point(|sequence| sequence <= cursor);
            (sequences.len() - at_or_below) as u64
        })
        .collect())
}

/// Reading one stored body, separated from the metadata verbs so a store
/// whose medium is not memory-mapped can serve a `/get` through an
/// explicit asynchronous fetch phase.
///
/// The module docs' "Why the body read is a separate trait" section has the
/// full argument. In one line: [`PropagationStore`] answers everything a
/// `/get` *plans* with, this answers what it *serves*, and on a microSD card
/// only the second needs the card.
///
/// Implementations return the stored bytes verbatim — `lxmf_data || stamp`,
/// stamp included; the role strips it — and `Ok(None)` for a transient ID the
/// store does not hold. A card fetch may skip an absent body before it
/// finalizes its plan (`reference/LXMF/LXMF/LXMRouter.py:1535`); a body
/// disappearing after a stream has begun instead fails the resource build,
/// since its advertised length must not change.
pub trait PropagationBodyStore {
    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError>;

    /// Copy one stored body from byte `offset` into the front of `buf`;
    /// returns how many bytes were copied (0 at or past the body's end), or
    /// `None` if the message is absent.
    ///
    /// Streaming fetch and peer-sync sources never hold a whole body.
    /// The default reads the whole body and copies a slice, so media
    /// addressable in place override it (mapped flash, prefetched card
    /// bodies and [`MemoryPropagationStore`]) to avoid that allocation.
    fn read_body_at(
        &self,
        transient_id: &TransientId,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, StorageError> {
        Ok(self
            .read_body(transient_id)?
            .map(|body| copy_from(&body, offset, buf)))
    }
}

/// A card owner's already-read bodies, borrowed by a [`FetchSource`](crate::FetchSource).
/// Partial reads copy directly from the retained buffers; they never clone
/// a body or assemble the complete response.
impl PropagationBodyStore for [(TransientId, Vec<u8>)] {
    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .iter()
            .find(|(id, _)| id == transient_id)
            .map(|(_, body)| body.clone()))
    }

    fn read_body_at(
        &self,
        transient_id: &TransientId,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, StorageError> {
        Ok(self
            .iter()
            .find(|(id, _)| id == transient_id)
            .map(|(_, body)| copy_from(body, offset, buf)))
    }
}

/// The smallest body [`PropagationStore::append`] accepts: a destination hash
/// and a stamp with nothing in between is already malformed one layer up
/// (`PropagationUpload::decode` refuses anything not strictly longer than
/// `LXMF_OVERHEAD + STAMP_SIZE`), so this is a defence against misuse of the
/// trait, not a protocol bound.
pub const MIN_BODY_LEN: usize = DESTINATION_LENGTH + STAMP_SIZE;

/// Copy `body[offset..]` into the front of `buf`, as much as fits; the
/// shape of [`PropagationBodyStore::read_body_at`] over a body in memory.
pub fn copy_from(body: &[u8], offset: usize, buf: &mut [u8]) -> usize {
    let rest = body.get(offset..).unwrap_or(&[]);
    let take = core::cmp::min(rest.len(), buf.len());
    buf[..take].copy_from_slice(&rest[..take]);
    take
}

/// In-memory [`PropagationStore`], for tests and transient embedded use.
///
/// Capacity counts body bytes only, mirroring how the reference's
/// `message_storage_size` sums stored file sizes (`LXMRouter.py:737-741`).
#[derive(Debug, Clone)]
pub struct MemoryPropagationStore {
    entries: BTreeMap<TransientId, MemoryEntry>,
    capacity: u64,
    used: u64,
    next_sequence: u64,
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    received_at: u64,
    stamp_value: u8,
    sequence: u64,
    body: Vec<u8>,
}

impl MemoryPropagationStore {
    pub fn new(capacity: u64) -> Self {
        Self {
            entries: BTreeMap::new(),
            capacity,
            used: 0,
            next_sequence: 0,
        }
    }

    /// Number of stored messages.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl PropagationStore for MemoryPropagationStore {
    fn append(
        &mut self,
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError> {
        if body.len() < MIN_BODY_LEN {
            return Err(StorageError::Corrupt);
        }
        let added = body.len() as u64;
        let replaced = self
            .entries
            .get(transient_id)
            .map_or(0, |entry| entry.body.len() as u64);
        let next = self.used - replaced + added;
        if next > self.capacity {
            return Err(StorageError::Full);
        }
        self.next_sequence += 1;
        self.entries.insert(
            *transient_id,
            MemoryEntry {
                received_at,
                stamp_value,
                sequence: self.next_sequence,
                body: body.to_vec(),
            },
        );
        self.used = next;
        Ok(())
    }

    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        for (transient_id, entry) in &self.entries {
            let mut destination_hash = [0u8; DESTINATION_LENGTH];
            destination_hash.copy_from_slice(&entry.body[..DESTINATION_LENGTH]);
            visit(&StoredMessage {
                transient_id: *transient_id,
                destination_hash,
                size: entry.body.len() as u32,
                received_at: entry.received_at,
                stamp_value: entry.stamp_value,
                sequence: entry.sequence,
            });
        }
        Ok(())
    }

    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        match self.entries.remove(transient_id) {
            Some(entry) => {
                self.used -= entry.body.len() as u64;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn free_space(&self) -> u64 {
        self.capacity - self.used
    }

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        Ok(self.entries.contains_key(transient_id))
    }
}

impl PropagationBodyStore for MemoryPropagationStore {
    /// One `BTreeMap` lookup and one copy of the body; this store's medium is
    /// the heap it already lives in, so the read is synchronous and
    /// [`crate::propagation_node::PropagationNode::handle_get`] is available
    /// to it.
    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .entries
            .get(transient_id)
            .map(|entry| entry.body.clone()))
    }

    fn read_body_at(
        &self,
        transient_id: &TransientId,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, StorageError> {
        Ok(self
            .entries
            .get(transient_id)
            .map(|entry| copy_from(&entry.body, offset, buf)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_for(destination: u8, len: usize) -> Vec<u8> {
        let mut body = alloc::vec![destination; len];
        body[DESTINATION_LENGTH..].fill(0xAB);
        body
    }

    #[test]
    fn append_iterate_read_purge_round_trip() {
        let mut store = MemoryPropagationStore::new(1024);
        let body = body_for(7, 100);
        store.append(&[1; 32], 1000, 3, &body).unwrap();

        let mut seen = Vec::new();
        store.for_each(&mut |meta| seen.push(*meta)).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].transient_id, [1; 32]);
        assert_eq!(seen[0].destination_hash, [7; DESTINATION_LENGTH]);
        assert_eq!(seen[0].size, 100);
        assert_eq!(seen[0].received_at, 1000);
        assert_eq!(seen[0].stamp_value, 3);

        assert_eq!(store.read_body(&[1; 32]).unwrap(), Some(body));
        assert!(store.contains(&[1; 32]).unwrap());
        // The default charge is the body itself — the unit this store's
        // free_space frees.
        assert_eq!(store.space_required(100), 100);
        assert!(store.purge(&[1; 32]).unwrap());
        assert!(!store.purge(&[1; 32]).unwrap());
        assert_eq!(store.read_body(&[1; 32]).unwrap(), None);
        assert_eq!(store.free_space(), 1024);
    }

    #[test]
    fn capacity_refuses_with_full_and_keeps_the_store_intact() {
        let mut store = MemoryPropagationStore::new(120);
        store.append(&[1; 32], 0, 0, &body_for(1, 100)).unwrap();
        assert_eq!(
            store.append(&[2; 32], 0, 0, &body_for(2, 60)),
            Err(StorageError::Full)
        );
        assert!(store.contains(&[1; 32]).unwrap());
        assert_eq!(store.free_space(), 20);
    }

    #[test]
    fn a_body_below_the_minimum_is_refused() {
        let mut store = MemoryPropagationStore::new(1024);
        assert_eq!(
            store.append(&[1; 32], 0, 0, &[0u8; MIN_BODY_LEN - 1]),
            Err(StorageError::Corrupt)
        );
    }
}
