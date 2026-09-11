//! The board's propagation-node store adapters (Codeberg #384, part 3).
//!
//! Two adapters over one region of [`leviculum-record-log`]-formatted
//! flash: [`PnStore`] implements
//! [`leviculum_lxmf::PropagationStore`] for message records, and
//! [`PnPeerStore`] implements [`leviculum_lxmf::peering::PeerStore`] for
//! the peer table, as tagged records in the same region
//! (`PeerRecord`'s trait contract, `leviculum-lxmf/src/peering.rs`).
//!
//! # How a synchronous trait meets an asynchronous flash
//!
//! The traits are synchronous; the flash behind them is
//! `nrf_softdevice::Flash`, which is async, lock-held per operation, and
//! whose in-flight futures must never be dropped (the `DropBomb`,
//! `leviculum-nrf/src/record_store.rs` module docs). The adapters split
//! the verbs by what they actually touch:
//!
//! * **Reads never touch the async flash at all.** The nRF52840's
//!   internal flash is memory-mapped and a read is a `memcpy` that cannot
//!   fail or be refused (`Device::read`,
//!   `leviculum-nrf/src/record_store.rs`), so `for_each`, `read_body`,
//!   `contains`, `count` and `newest_sequence` are one synchronous pass
//!   over the region bytes ([`scan`]) — the concept page's §2 "Scan"
//!   arithmetic, 8-33 ms for the full 64 KiB, none of it on the
//!   SoftDevice's flash scheduler.
//! * **Writes are queued, not performed.** `append`, `purge`, `save` and
//!   `remove` push a [`FlushOp`] onto the adapter's queue and return; the
//!   engine drains the queue after every role call by sending each op to
//!   the record-store task over its channel and awaiting the reply —
//!   channel + reply, exactly as that task already works — and only then
//!   performs the wire action the write gated (the upload proof, the
//!   `/get` response). "Persist before you prove" therefore holds at the
//!   flush boundary, not at the trait boundary.
//!
//! What each verb costs: a read verb is a scan of at most the 64 KiB
//! region at RAM speed; an `append` flush is one channel round trip and
//! three program runs (3.6 ms of NVMC per median record, §2) plus one
//! 85 ms page erase every eleventh record; a `purge` flush is one round
//! trip and a single word write; a peer `save` is an append plus one
//! purge per superseded record.
//!
//! # Visibility rules, and why they make the cursor sound
//!
//! A queued append is visible to `contains` and `read_body` (duplicate
//! detection must see it) but **not** to `for_each`. `for_each` is what
//! `build_offer` and the `/get` list are answered from, and a record that
//! is not yet durable must not be offered, listed, or — above all — have
//! a provisional sequence leak into a peer's sync cursor. Once flushed,
//! the record appears in the scan with its real sequence,
//! `page_sequence << 16 | offset` ([`sequence`]), which is the mapping
//! `StoredMessage::sequence` documents. A queued purge masks its record
//! from every read; if its flush fails the mask is dropped and the record
//! honestly reappears.
//!
//! # Record tagging
//!
//! The record log's tag byte is opaque; this crate assigns it. A message
//! record's tag is its stamp value, clamped to [`MESSAGE_TAG_MAX`] — a
//! stamp value above 127 has probability 2^-127 and the clamp only ever
//! understates value, never invents it. [`TAG_PEER`] marks peer records
//! and `TAG_BENCH` (0xB0, `leviculum-nrf/src/record_store.rs`) marks the
//! bench instrument's synthetic records; both are above the clamp, so
//! neither is ever visible to the message role and bench records survive
//! in place, invisible, until their page is reclaimed.

#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use leviculum_lxmf::constants::DESTINATION_LENGTH;
use leviculum_lxmf::peering::{PeerRecord, PeerStore};
use leviculum_lxmf::propagation_store::{PropagationStore, StoredMessage, MIN_BODY_LEN};
use leviculum_lxmf::storage::StorageError;
use leviculum_record_log::{
    crc16_update, record_stride, FLAG_LIVE, FLAG_PURGED, HEADER_LEN, KEY_LEN, MIN_STRIDE,
    SECTOR_HEADER_LEN, SECTOR_PAYLOAD, SECTOR_SIZE,
};

#[cfg(test)]
mod tests;

/// Highest tag byte a message record may carry: the stamp value clamp.
/// Everything above is a non-message record kind.
pub const MESSAGE_TAG_MAX: u8 = 0x7F;

/// Tag of a persisted peer record ([`PnPeerStore`]).
pub const TAG_PEER: u8 = 0xA0;

/// `LVR1`, the page-header magic, restated from the record log's layout
/// (`leviculum-nrf/record-log/src/lib.rs`) because the log does not
/// export it; the golden-bytes test in this crate pins the two together
/// by scanning a region the log itself wrote.
const MAGIC: u32 = 0x3152_564C;
const VERSION: u8 = 1;
const CRC_INIT: u16 = 0xFFFF;

/// The store's append-order position of a record: monotone over the
/// store's lifetime because page sequences increase by one per erase and
/// offsets increase within a page. The domain of the per-peer sync
/// cursor (`StoredMessage::sequence`,
/// `leviculum-lxmf/src/propagation_store.rs`).
pub const fn sequence(page_seq: u32, offset_in_page: u16) -> u64 {
    ((page_seq as u64) << 16) | offset_in_page as u64
}

/// Access to the region's bytes. On the board this is a
/// `core::slice::from_raw_parts` view of the memory-mapped store region;
/// in tests it is the simulated part's byte array. Callback-shaped so a
/// test region can hand out a borrow of shared, mutating storage.
pub trait Region {
    fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T;
}

impl Region for &[u8] {
    fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        f(self)
    }
}

/// One committed record as the synchronous scan sees it.
#[derive(Debug, Clone, Copy)]
pub struct RawRecord<'a> {
    pub key: &'a [u8; KEY_LEN],
    pub time: u32,
    pub tag: u8,
    pub live: bool,
    /// Offset of the record header from the region base.
    pub offset: u32,
    pub page_seq: u32,
    pub body: &'a [u8],
}

impl RawRecord<'_> {
    /// The record's [`sequence`].
    pub fn sequence(&self) -> u64 {
        sequence(self.page_seq, (self.offset % SECTOR_SIZE) as u16)
    }
}

/// Visit every intact committed record in `region`, page by page.
///
/// The read-only twin of `RecordLog::for_each_seq`
/// (`leviculum-nrf/record-log/src/lib.rs`): same header layout, same
/// commit-flag rule (a record exists iff its flags byte reads live or
/// purged and its CRC checks), same stop-at-first-gap walk per page. The
/// crate's tests hold the two together by scanning regions the log
/// itself wrote, reclaim, purge and torn tails included.
///
/// Safe against a concurrent writer on the board: an in-flight append's
/// record still reads flags `0xFF` (the commit word is written last) and
/// the walk stops before it; a half-erased page fails its header CRC and
/// is skipped whole.
pub fn scan(region: &[u8], mut visit: impl FnMut(&RawRecord<'_>)) {
    let pages = region.len() / SECTOR_SIZE as usize;
    for page in 0..pages {
        let base = page * SECTOR_SIZE as usize;
        let header = &region[base..base + SECTOR_HEADER_LEN as usize];
        if u32::from_le_bytes([header[0], header[1], header[2], header[3]]) != MAGIC
            || header[8] != VERSION
        {
            continue;
        }
        let stored = u16::from_le_bytes([header[10], header[11]]);
        if crc16_update(CRC_INIT, &header[0..10]) != stored {
            continue;
        }
        let page_seq = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);

        let mut off = SECTOR_HEADER_LEN as usize;
        while SECTOR_SIZE as usize - off >= MIN_STRIDE as usize {
            let at = base + off;
            let head = &region[at..at + HEADER_LEN];
            let flags = head[39];
            if flags != FLAG_LIVE && flags != FLAG_PURGED {
                break;
            }
            let len = u16::from_le_bytes([head[0], head[1]]) as usize;
            let stride = record_stride(len);
            if stride > SECTOR_SIZE as usize - off {
                break;
            }
            let body = &region[at + HEADER_LEN..at + HEADER_LEN + len];
            let stored = u16::from_le_bytes([head[40], head[41]]);
            let crc = crc16_update(crc16_update(CRC_INIT, &head[0..39]), body);
            if crc == stored {
                let key: &[u8; KEY_LEN] = region[at + 2..at + 2 + KEY_LEN]
                    .try_into()
                    .expect("slice length is KEY_LEN");
                visit(&RawRecord {
                    key,
                    time: u32::from_le_bytes([head[34], head[35], head[36], head[37]]),
                    tag: head[38],
                    live: flags == FLAG_LIVE,
                    offset: at as u32,
                    page_seq,
                    body,
                });
            }
            off += stride;
        }
    }
}

/// One write the engine owes the record-store task, in queue order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushOp {
    /// `RecordLog::append(key, time, tag, body)`.
    Append {
        key: [u8; KEY_LEN],
        time: u32,
        tag: u8,
        body: Vec<u8>,
    },
    /// `RecordLog::purge` of the record at this region offset, after
    /// re-validating that its key still matches (the page may have been
    /// reclaimed since the mark).
    Purge { offset: u32, key: [u8; KEY_LEN] },
}

/// The message-store half: [`PropagationStore`] over the region.
pub struct PnStore<R> {
    region: R,
    /// Pages in the region, for [`PropagationStore::capacity`].
    pages: u32,
    /// Mirror of the log's `free_bytes`, updated from flush replies.
    free_bytes: u32,
    queue: VecDeque<FlushOp>,
}

impl<R: Region> PnStore<R> {
    /// `pages` and `free_bytes` come from the mount line's own numbers.
    pub fn new(region: R, pages: u32, free_bytes: u32) -> Self {
        Self {
            region,
            pages,
            free_bytes,
            queue: VecDeque::new(),
        }
    }

    /// Update the free-bytes mirror from a flush reply.
    pub fn set_free_bytes(&mut self, free_bytes: u32) {
        self.free_bytes = free_bytes;
    }

    /// Writes waiting to be flushed.
    pub fn pending_ops(&self) -> usize {
        self.queue.len()
    }

    /// The next op to flush, if any. The op stays queued (and keeps its
    /// visibility effects) until [`Self::op_done`].
    pub fn peek_op(&self) -> Option<&FlushOp> {
        self.queue.front()
    }

    /// The front op's flush concluded. Returns the op; on failure an
    /// append's key is what the engine must un-remember from the role's
    /// duplicate cache, and a purge's mask is dropped so the record
    /// reappears.
    pub fn op_done(&mut self, _ok: bool) -> Option<FlushOp> {
        self.queue.pop_front()
    }

    fn is_purge_masked(&self, offset: u32) -> bool {
        self.queue
            .iter()
            .any(|op| matches!(op, FlushOp::Purge { offset: o, .. } if *o == offset))
    }

    fn pending_append_body(&self, key: &[u8; KEY_LEN]) -> Option<&[u8]> {
        self.queue.iter().rev().find_map(|op| match op {
            FlushOp::Append { key: k, body, .. } if k == key => Some(body.as_slice()),
            _ => None,
        })
    }

    fn scan_messages(&self, mut visit: impl FnMut(&RawRecord<'_>)) {
        self.region.with_bytes(|bytes| {
            scan(bytes, |record| {
                if record.live
                    && record.tag <= MESSAGE_TAG_MAX
                    && !self.is_purge_masked(record.offset)
                {
                    visit(record);
                }
            })
        });
    }
}

impl<R: Region> PropagationStore for PnStore<R> {
    /// Queue the append. Never [`StorageError::Full`]: the record log
    /// reclaims its oldest page round-robin when the region is full, which
    /// is the board's eviction (the deviation argued in
    /// `leviculum-lxmf/src/propagation_store.rs` module docs), so the
    /// role's weighted `make_room` never runs here.
    fn append(
        &mut self,
        transient_id: &[u8; 32],
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError> {
        if body.len() < MIN_BODY_LEN {
            return Err(StorageError::Corrupt);
        }
        if body.len() > leviculum_record_log::MAX_BODY {
            // A record never straddles a page; the announced transfer
            // limit (field 3 = 4 kB) keeps conforming peers below this.
            return Err(StorageError::Full);
        }
        self.queue.push_back(FlushOp::Append {
            key: *transient_id,
            time: received_at.min(u32::MAX as u64) as u32,
            tag: stamp_value.min(MESSAGE_TAG_MAX),
            body: body.to_vec(),
        });
        Ok(())
    }

    /// Durable records only — a queued append is deliberately absent (see
    /// the module docs' visibility rules).
    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        self.scan_messages(|record| {
            if record.body.len() < MIN_BODY_LEN {
                return;
            }
            let mut destination_hash = [0u8; DESTINATION_LENGTH];
            destination_hash.copy_from_slice(&record.body[..DESTINATION_LENGTH]);
            visit(&StoredMessage {
                transient_id: *record.key,
                destination_hash,
                size: record.body.len() as u32,
                received_at: record.time as u64,
                stamp_value: record.tag,
                sequence: record.sequence(),
            });
        });
        Ok(())
    }

    fn read_body(&self, transient_id: &[u8; 32]) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(body) = self.pending_append_body(transient_id) {
            return Ok(Some(body.to_vec()));
        }
        let mut found = None;
        self.scan_messages(|record| {
            if record.key == transient_id && found.is_none() {
                found = Some(record.body.to_vec());
            }
        });
        Ok(found)
    }

    /// Queue the purge (or drop a not-yet-flushed append outright).
    fn purge(&mut self, transient_id: &[u8; 32]) -> Result<bool, StorageError> {
        if let Some(at) = self
            .queue
            .iter()
            .position(|op| matches!(op, FlushOp::Append { key, .. } if key == transient_id))
        {
            self.queue.remove(at);
            return Ok(true);
        }
        let mut offset = None;
        self.scan_messages(|record| {
            if record.key == transient_id && offset.is_none() {
                offset = Some(record.offset);
            }
        });
        match offset {
            Some(offset) => {
                self.queue.push_back(FlushOp::Purge {
                    offset,
                    key: *transient_id,
                });
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn free_space(&self) -> u64 {
        let queued: u64 = self
            .queue
            .iter()
            .map(|op| match op {
                FlushOp::Append { body, .. } => record_stride(body.len()) as u64,
                FlushOp::Purge { .. } => 0,
            })
            .sum();
        (self.free_bytes as u64).saturating_sub(queued)
    }

    fn capacity(&self) -> u64 {
        self.pages as u64 * SECTOR_PAYLOAD as u64
    }

    fn contains(&self, transient_id: &[u8; 32]) -> Result<bool, StorageError> {
        if self.pending_append_body(transient_id).is_some() {
            return Ok(true);
        }
        let mut found = false;
        self.scan_messages(|record| found |= record.key == transient_id);
        Ok(found)
    }
}

/// Encoded size of a [`PeerRecord`] body: version, flags, dest, identity,
/// key + value, limits, costs, timebase, last-heard, cursor.
pub const PEER_RECORD_LEN: usize = 1 + 1 + 16 + 16 + 34 + 4 + 4 + 3 + 8 + 8 + 8;

const PEER_VERSION: u8 = 1;
const PEER_FLAG_IDENTITY: u8 = 0x01;
const PEER_FLAG_KEY: u8 = 0x02;
const PEER_FLAG_STATIC: u8 = 0x04;

/// Encode a [`PeerRecord`] as one record-log body.
pub fn encode_peer_record(record: &PeerRecord) -> [u8; PEER_RECORD_LEN] {
    let mut out = [0u8; PEER_RECORD_LEN];
    out[0] = PEER_VERSION;
    let mut flags = 0u8;
    out[2..18].copy_from_slice(&record.destination_hash);
    if let Some(identity) = &record.identity_hash {
        flags |= PEER_FLAG_IDENTITY;
        out[18..34].copy_from_slice(identity);
    }
    if let Some((key, value)) = &record.peering_key {
        flags |= PEER_FLAG_KEY;
        out[34..66].copy_from_slice(key);
        out[66..68].copy_from_slice(&value.to_le_bytes());
    }
    if record.is_static {
        flags |= PEER_FLAG_STATIC;
    }
    out[1] = flags;
    out[68..72]
        .copy_from_slice(&(record.transfer_limit_kb.min(u32::MAX as u64) as u32).to_le_bytes());
    out[72..76].copy_from_slice(&(record.sync_limit_kb.min(u32::MAX as u64) as u32).to_le_bytes());
    out[76] = record.stamp_cost;
    out[77] = record.stamp_cost_flexibility;
    out[78] = record.peering_cost;
    out[79..87].copy_from_slice(&record.peering_timebase.to_le_bytes());
    out[87..95].copy_from_slice(&record.last_heard.to_le_bytes());
    out[95..103].copy_from_slice(&record.cursor.to_le_bytes());
    out
}

/// Decode a record-log body back into a [`PeerRecord`], or `None` for a
/// body from a different version or length.
pub fn decode_peer_record(body: &[u8]) -> Option<PeerRecord> {
    if body.len() != PEER_RECORD_LEN || body[0] != PEER_VERSION {
        return None;
    }
    let flags = body[1];
    let mut destination_hash = [0u8; DESTINATION_LENGTH];
    destination_hash.copy_from_slice(&body[2..18]);
    let identity_hash = (flags & PEER_FLAG_IDENTITY != 0).then(|| {
        let mut identity = [0u8; DESTINATION_LENGTH];
        identity.copy_from_slice(&body[18..34]);
        identity
    });
    let peering_key = (flags & PEER_FLAG_KEY != 0).then(|| {
        let mut key = [0u8; 32];
        key.copy_from_slice(&body[34..66]);
        (key, u16::from_le_bytes([body[66], body[67]]))
    });
    let le32 = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().ok().unwrap_or_default());
    let le64 = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().ok().unwrap_or_default());
    Some(PeerRecord {
        destination_hash,
        identity_hash,
        peering_key,
        transfer_limit_kb: le32(68) as u64,
        sync_limit_kb: le32(72) as u64,
        stamp_cost: body[76],
        stamp_cost_flexibility: body[77],
        peering_cost: body[78],
        peering_timebase: le64(79),
        last_heard: le64(87),
        cursor: le64(95),
        is_static: flags & PEER_FLAG_STATIC != 0,
    })
}

/// A peer record's log key: the destination hash, zero-padded to
/// [`KEY_LEN`] — the shape `PeerRecord`'s trait contract names.
pub fn peer_key(destination_hash: &[u8; DESTINATION_LENGTH]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    key[..DESTINATION_LENGTH].copy_from_slice(destination_hash);
    key
}

/// The peer-table half: [`PeerStore`] as [`TAG_PEER`] records in the same
/// region. Upsert appends the new record and then purges the old, in that
/// order, so a flush interrupted between the two leaves both on the part
/// and [`PeerStore::load_all`] takes the newer by sequence.
pub struct PnPeerStore<R> {
    region: R,
    queue: VecDeque<FlushOp>,
    /// Saves not yet flushed, newest last — these override the flash view
    /// in `load_all`, because a reboot before the flush would lose them
    /// and the engine must not act on state it believes persisted when it
    /// can still tell the difference.
    pending: Vec<PeerRecord>,
    /// Destinations removed but not yet flushed.
    removed: Vec<[u8; DESTINATION_LENGTH]>,
}

impl<R: Region> PnPeerStore<R> {
    pub fn new(region: R) -> Self {
        Self {
            region,
            queue: VecDeque::new(),
            pending: Vec::new(),
            removed: Vec::new(),
        }
    }

    pub fn pending_ops(&self) -> usize {
        self.queue.len()
    }

    pub fn peek_op(&self) -> Option<&FlushOp> {
        self.queue.front()
    }

    /// The front op's flush concluded. A failed append leaves the record
    /// in `pending` semantics only until the next save; peers tolerate a
    /// lost save — the record is rebuilt from the next announce — so no
    /// retry bookkeeping is kept here.
    pub fn op_done(&mut self, _ok: bool) -> Option<FlushOp> {
        let op = self.queue.pop_front();
        if self.queue.is_empty() {
            self.pending.clear();
            self.removed.clear();
        }
        op
    }

    fn is_purge_masked(&self, offset: u32) -> bool {
        self.queue
            .iter()
            .any(|op| matches!(op, FlushOp::Purge { offset: o, .. } if *o == offset))
    }

    /// Live peer records on the part, newest per destination, purge-masked
    /// entries excluded.
    fn flash_records(&self) -> Vec<(u64, PeerRecord)> {
        let mut newest: Vec<(u64, PeerRecord)> = Vec::new();
        self.region.with_bytes(|bytes| {
            scan(bytes, |record| {
                if !record.live || record.tag != TAG_PEER || self.is_purge_masked(record.offset) {
                    return;
                }
                let Some(decoded) = decode_peer_record(record.body) else {
                    return;
                };
                let seq = record.sequence();
                match newest
                    .iter_mut()
                    .find(|(_, held)| held.destination_hash == decoded.destination_hash)
                {
                    Some(slot) if slot.0 < seq => *slot = (seq, decoded),
                    Some(_) => {}
                    None => newest.push((seq, decoded)),
                }
            })
        });
        newest
    }
}

impl<R: Region> PeerStore for PnPeerStore<R> {
    fn save(&mut self, record: &PeerRecord) -> Result<(), StorageError> {
        // Supersede any queued save for the same destination: only the
        // newest value needs flash, and the append-then-purge pair below
        // would otherwise purge the record the earlier pair just wrote.
        self.queue.retain(|op| {
            !matches!(op, FlushOp::Append { key, .. } if *key == peer_key(&record.destination_hash))
        });
        self.pending
            .retain(|held| held.destination_hash != record.destination_hash);
        self.removed.retain(|dest| *dest != record.destination_hash);

        let key = peer_key(&record.destination_hash);
        let mut old = Vec::new();
        self.region.with_bytes(|bytes| {
            scan(bytes, |raw| {
                if raw.live && raw.tag == TAG_PEER && raw.key == &key {
                    old.push(raw.offset);
                }
            })
        });
        self.queue.push_back(FlushOp::Append {
            key,
            time: record.last_heard.min(u32::MAX as u64) as u32,
            tag: TAG_PEER,
            body: encode_peer_record(record).to_vec(),
        });
        for offset in old {
            if !self.is_purge_masked(offset) {
                self.queue.push_back(FlushOp::Purge { offset, key });
            }
        }
        self.pending.push(record.clone());
        Ok(())
    }

    fn remove(&mut self, destination_hash: &[u8; DESTINATION_LENGTH]) -> Result<(), StorageError> {
        let key = peer_key(destination_hash);
        self.queue
            .retain(|op| !matches!(op, FlushOp::Append { key: k, .. } if *k == key));
        self.pending
            .retain(|held| held.destination_hash != *destination_hash);
        let mut offsets = Vec::new();
        self.region.with_bytes(|bytes| {
            scan(bytes, |raw| {
                if raw.live && raw.tag == TAG_PEER && raw.key == &key {
                    offsets.push(raw.offset);
                }
            })
        });
        for offset in offsets {
            if !self.is_purge_masked(offset) {
                self.queue.push_back(FlushOp::Purge { offset, key });
            }
        }
        self.removed.push(*destination_hash);
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<PeerRecord>, StorageError> {
        let mut records: Vec<PeerRecord> = self
            .flash_records()
            .into_iter()
            .map(|(_, record)| record)
            .filter(|record| {
                !self.removed.contains(&record.destination_hash)
                    && !self
                        .pending
                        .iter()
                        .any(|held| held.destination_hash == record.destination_hash)
            })
            .collect();
        records.extend(self.pending.iter().cloned());
        Ok(records)
    }
}
