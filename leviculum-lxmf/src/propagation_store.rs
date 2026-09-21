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
//!   yields `Record { key, time, tag, len }` and the adapter reads the first
//!   16 body bytes for the destination hash with `read_body` — one `memcpy`
//!   from memory-mapped flash per record (the concept page's §"Scan" is why
//!   that is free of the radio).
//! * [`PropagationStore::read_body`] ↔ `RecordLog::read_body` (`lib.rs:574`).
//! * [`PropagationStore::purge`] ↔ `RecordLog::purge` (`lib.rs:709`).
//! * [`PropagationStore::free_space`] ↔ `RecordLog::free_bytes` (`lib.rs:683`)
//!   and [`PropagationStore::capacity`] ↔ `sectors() × SECTOR_PAYLOAD`.
//!
//! The record log's verbs are `async` (every flash program waits on the
//! SoftDevice's scheduler); this trait is synchronous because both host
//! implementations complete immediately and the role logic runs inside the
//! driver's core lock, where an `await` point has nothing to yield to. The
//! board adapter owns the async plumbing: each trait verb maps to exactly one
//! record-log call, so wrapping it in the firmware's executor is part 3's
//! adapter work, not a redesign of this boundary.
//!
//! # Eviction, and where it deliberately is NOT
//!
//! The trait has no eviction verb. On the host the role evicts by the
//! reference's weight — `age × size`, `clean_message_store`
//! (`LXMRouter.py:1144`, weight at `:1056-1067`) — implemented in
//! [`crate::propagation_node`] on top of `for_each` + `purge`. On the board
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

/// The store a propagation node keeps its accepted messages in.
///
/// See the module documentation for the record's shape and the record-log
/// mapping. Implementations must be durable at the verb boundary: when
/// [`append`](Self::append) returns `Ok`, the message survives a power cut,
/// because the role proves the upload packet only after this returns
/// ("persist before you prove",
/// `docs/src/concepts/propagation-node-on-a-board.md` §3).
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

    /// Read one stored body (`lxmf_data || stamp`), or `None` if absent.
    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError>;

    /// Remove one message. Returns whether it was present.
    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError>;

    /// Bytes still available for message bodies.
    fn free_space(&self) -> u64;

    /// Total body capacity in bytes.
    fn capacity(&self) -> u64;

    /// Whether a transient ID is stored. Duplicate detection
    /// (`lxmf_propagation`, `reference/LXMF/LXMF/LXMRouter.py:2496`).
    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        Ok(self.read_body(transient_id)?.is_some())
    }

    /// Number of stored messages — one directory pass by default; the record
    /// log's mount already reports the same count
    /// (`leviculum-nrf/record-log/src/lib.rs:406`).
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

/// The smallest body [`PropagationStore::append`] accepts: a destination hash
/// and a stamp with nothing in between is already malformed one layer up
/// (`PropagationUpload::decode` refuses anything not strictly longer than
/// `LXMF_OVERHEAD + STAMP_SIZE`), so this is a defence against misuse of the
/// trait, not a protocol bound.
pub const MIN_BODY_LEN: usize = DESTINATION_LENGTH + STAMP_SIZE;

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

    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .entries
            .get(transient_id)
            .map(|entry| entry.body.clone()))
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
