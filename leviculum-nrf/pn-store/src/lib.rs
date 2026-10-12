//! The board's propagation-node store adapters (Codeberg #384, part 3).
//!
//! Two adapters over one region of `leviculum-record-log`-formatted
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
//! understates value, never invents it. [`TAG_PEER`] marks peer records,
//! [`TAG_PRICED`] the one marker record [`PnStore::stamp_values_true`]
//! reads, and `TAG_BENCH` (0xB0, `leviculum-nrf/src/record_store.rs`) marks
//! the bench instrument's synthetic records; all three are above the clamp,
//! so none is ever visible to the message role and bench records survive
//! in place, invisible, until their page is reclaimed.
//!
//! # Re-pricing a store at value 0 (audit 2026-10-12 D10)
//!
//! A board built before the audit stored value 0 for everything it took at
//! cost 0 before its first demanding peer, and a stock peer never takes an
//! id below its minimum (`reference/LXMF/LXMF/LXMPeer.py:340`). Because the
//! value IS the record's tag and the log is append-only, the true value is
//! written by re-appending the record under its new tag and purging the old
//! ([`PnStore::set_stamp_value`]): same key, same receive time, same body, a
//! new and higher sequence, which every peer's cursor stands below. The
//! records to price are found one at a time ([`PnStore::next_unpriced`]),
//! so the walk holds a cursor and never a list.

#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use leviculum_lxmf::constants::DESTINATION_LENGTH;
use leviculum_lxmf::peering::{PeerRecord, PeerStore};
use leviculum_lxmf::propagation_store::{copy_from, PropagationStore, StoredMessage, MIN_BODY_LEN};
use leviculum_lxmf::storage::StorageError;
use leviculum_lxmf::PropagationNode;
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

/// Tag of the marker record that says every message record in the region
/// carries its true stamp value ([`PnStore::stamp_values_true`]).
pub const TAG_PRICED: u8 = 0xA1;

/// Key of the [`TAG_PRICED`] marker: no transient id, because the tag
/// already keeps it out of every message read.
const PRICED_KEY: [u8; KEY_LEN] = *b"leviculum stamp values are true.";

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

/// Estimated heap bytes a [`FlushOp`] queue pins (#388 census): the
/// ring's slot spine plus every queued append's body allocation.
fn queue_heap_bytes(queue: &VecDeque<FlushOp>) -> usize {
    let mut bytes = queue.capacity() * core::mem::size_of::<FlushOp>();
    for op in queue {
        if let FlushOp::Append { body, .. } = op {
            bytes += body.capacity();
        }
    }
    bytes
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

/// The record-store task answered the message store's front op: `ok` is
/// whether it landed. Pops the op and returns it.
///
/// **An append that failed is not processed.** The role remembered the
/// message's transient id when it accepted the upload, before the deferred
/// write ran; left there, the client's retry of a message that never
/// reached flash would be answered as a proven duplicate and lost, the
/// exact lie "persist before you prove" exists to prevent. So a failed
/// append un-remembers its id ([`PropagationNode::forget_processed`]). A
/// failed purge needs nothing from the role: popping it drops its mask and
/// the record reappears. Every way the task can fail an op (the store not
/// mounted, the flash refusing past its retries, a page sealed mid-record)
/// arrives here as the same `false`.
///
/// This is where the two crates meet: the queue is this crate's, the
/// duplicate cache the role's. The board's engine (`leviculum-nrf/src/pn.rs`,
/// `flush`) calls it; no rig cell makes a board's flash refuse an append
/// (leviculum#413).
pub fn settle_message_op<R: Region>(
    role: &mut PropagationNode<PnStore<R>>,
    ok: bool,
) -> Option<FlushOp> {
    let done = role.store_mut().op_done(ok);
    if !ok {
        if let Some(FlushOp::Append { key, .. }) = &done {
            role.forget_processed(key);
        }
    }
    done
}

/// One message record [`PnStore::next_unpriced`] found at value 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unpriced {
    pub transient_id: [u8; 32],
    /// The record's [`sequence`]: where the walk's cursor moves once it is
    /// priced.
    pub sequence: u64,
    /// The propagation stamp, the body's last [`STAMP_LEN`] bytes.
    pub stamp: [u8; STAMP_LEN],
}

/// The one-lap walk that prices a store's value-0 records at boot (see
/// the module docs), as `lnpnd` re-prices its store before it starts
/// (`reprice_stored_values`, `lnpnd/src/validation.rs`).
///
/// It holds a cursor and the record in hand, never a list: the next record
/// is found by one [`scan`] ([`PnStore::next_unpriced`]) when the previous
/// one is priced, so the walk costs the same few dozen bytes whether the
/// store holds one record or a full region. The workblock itself is the
/// caller's (on the board, the engine's sliced grind, which already exists
/// for uploads); the walk only says which stamp to measure next and what to
/// do with the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepriceWalk {
    /// The sequence of the last record priced, or `None` before the first.
    after: Option<u64>,
    /// The newest sequence when the walk began: its one lap ends there.
    upto: u64,
    /// The record whose stamp is being measured, and its sequence.
    current: Option<([u8; 32], u64)>,
}

impl RepriceWalk {
    /// The walk this boot owes, or `None` when the region already carries
    /// the [`TAG_PRICED`] marker.
    pub fn at_boot<R: Region>(store: &PnStore<R>) -> Option<Self> {
        if store.stamp_values_true() {
            return None;
        }
        Some(Self {
            after: None,
            upto: store.newest_sequence().unwrap_or(0),
            current: None,
        })
    }

    /// The next record to measure, remembered as the one in hand; `None`
    /// once the lap is done, after which [`Self::finish`] is owed.
    pub fn next<R: Region>(&mut self, store: &PnStore<R>) -> Option<Unpriced> {
        let next = store.next_unpriced(self.after, self.upto)?;
        self.current = Some((next.transient_id, next.sequence));
        Some(next)
    }

    /// The record in hand measured `value`: queue its re-write when the
    /// value is above 0 and move the cursor past it either way. Returns
    /// whether a re-write was queued.
    pub fn priced<R: Region>(&mut self, store: &mut PnStore<R>, value: u16) -> bool {
        let Some((transient_id, sequence)) = self.current.take() else {
            return false;
        };
        self.after = Some(sequence);
        value > 0 && store.set_stamp_value(&transient_id, value.min(u8::MAX as u16) as u8)
    }

    /// The lap is done: queue the marker, so the next boot does not walk
    /// again.
    pub fn finish<R: Region>(self, store: &mut PnStore<R>) {
        store.mark_stamp_values_true();
    }
}

/// Length of the propagation stamp that ends every stored message body
/// (`lxmf_data ‖ stamp`).
pub const STAMP_LEN: usize = leviculum_lxmf::constants::STAMP_SIZE;

/// The role's configuration on a board: the announced costs and name from
/// the persisted config, the limits from the heap budget
/// (`leviculum-heap-budget`), and `serve_cap_bytes` as the boot plan funds
/// it. `leviculum_nrf::pn::Engine::new` builds its role from this, so the
/// host tests below run the configuration the board runs.
pub fn board_role_config(
    stamp_cost: u8,
    peering_cost: u8,
    name: Vec<u8>,
    serve_cap_bytes: usize,
) -> leviculum_lxmf::PropagationNodeConfig {
    use leviculum_heap_budget::{
        BOARD_SERVE_LIMIT_BYTES, BOARD_SYNC_LIMIT_KB, BOARD_TRANSFER_LIMIT_KB,
    };
    leviculum_lxmf::PropagationNodeConfig {
        stamp_cost,
        peering_cost,
        name: Some(name),
        transfer_limit_kb: BOARD_TRANSFER_LIMIT_KB,
        // The announced sync appetite must fit both the resource cap and
        // the heap budget (#388); derived from what the board can afford to
        // answer (order 511) — see BOARD_SYNC_LIMIT_KB.
        sync_limit_kb: BOARD_SYNC_LIMIT_KB,
        // The fetch is bounded apart from the announcement, so the smaller
        // derived limit does not shrink what a client gets.
        serve_limit_bytes: Some(BOARD_SERVE_LIMIT_BYTES),
        // What one fetch may SERVE, from the same boot arithmetic the
        // `HEAP_BUDGET serve_cap=` line prints. Below the announced sync
        // limit whenever the plan funds less than it announces, which is
        // today, on every board. It is the FLOOR: every `/get` re-reads it
        // against the heap the board has at that moment (`SERVE_CAP`).
        serve_cap_bytes: Some(serve_cap_bytes),
        // Every accepted message is priced, at cost 0 too, as the host does
        // (leviculum#442 D6). Until the audit of 2026-10-12 (D10) the board
        // skipped the workblock while no peer demanded a value and stored 0,
        // which every stock peer that peered later skipped for good
        // (`reference/LXMF/LXMF/LXMPeer.py:340`): a fresh board's whole early
        // store never propagated. The price is the 1000-round workblock per
        // accept, ~3.7 s of CPU in sliced settle passes, after the upload's
        // proof has already left (#397), so no round trip waits on it.
        always_compute_stamp_value: true,
        ..leviculum_lxmf::PropagationNodeConfig::default()
    }
}

/// The message-store half: [`PropagationStore`] over the region.
pub struct PnStore<R> {
    region: R,
    /// Pages in the region, for [`PropagationStore::capacity`].
    pages: u32,
    queue: VecDeque<FlushOp>,
}

impl<R: Region> PnStore<R> {
    /// `pages` is the region's page count, as the mount line reports it.
    pub fn new(region: R, pages: u32) -> Self {
        Self {
            region,
            pages,
            queue: VecDeque::new(),
        }
    }

    /// Writes waiting to be flushed.
    pub fn pending_ops(&self) -> usize {
        self.queue.len()
    }

    /// Estimated heap bytes the flush queue pins (#388 census): the ring
    /// spine plus each queued append's body.
    pub fn queued_heap_bytes(&self) -> usize {
        queue_heap_bytes(&self.queue)
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

    /// Whether the region carries the [`TAG_PRICED`] marker: every message
    /// record in it holds its true stamp value, so a value-0 record is worth
    /// 0 and pricing it again would only spend a workblock. Queued writes
    /// count, so a marker appended this boot answers before its flush.
    ///
    /// The marker lives in the log like any record and goes when the
    /// round-robin reclaim reaches its page. Every record older than it goes
    /// first or with it, and it is appended after the last record the
    /// re-pricing walk looked at, so a region that lost its marker holds
    /// nothing written before the walk: only records priced at accept. The
    /// walk then runs once more for the CPU of their value-0 workblocks, and
    /// writes nothing but a new marker.
    pub fn stamp_values_true(&self) -> bool {
        if self.queue.iter().any(|op| {
            matches!(op, FlushOp::Append { tag, key, .. } if *tag == TAG_PRICED && *key == PRICED_KEY)
        }) {
            return true;
        }
        let mut found = false;
        self.region.with_bytes(|bytes| {
            scan(bytes, |record| {
                found |= record.live && record.tag == TAG_PRICED && record.key == &PRICED_KEY;
            })
        });
        found
    }

    /// Queue the [`TAG_PRICED`] marker (see [`Self::stamp_values_true`]).
    pub fn mark_stamp_values_true(&mut self) {
        self.queue.push_back(FlushOp::Append {
            key: PRICED_KEY,
            time: 0,
            tag: TAG_PRICED,
            body: Vec::new(),
        });
    }

    /// The oldest durable message record stored at value 0 whose sequence
    /// lies after `after` (from the start when `None`) and at or below
    /// `upto`, with the stamp its body ends in. `upto` is the newest
    /// sequence when the walk began, which keeps the walk to one lap: what
    /// arrives during it is priced at accept, and a record this walk
    /// re-appended carries its new tag and a sequence above `upto` anyway.
    ///
    /// One [`scan`]; nothing is allocated.
    pub fn next_unpriced(&self, after: Option<u64>, upto: u64) -> Option<Unpriced> {
        let mut next: Option<Unpriced> = None;
        self.scan_messages(|record| {
            let sequence = record.sequence();
            if record.tag != 0
                || after.is_some_and(|after| sequence <= after)
                || sequence > upto
                || record.body.len() < MIN_BODY_LEN
                || next.as_ref().is_some_and(|held| held.sequence <= sequence)
            {
                return;
            }
            let mut stamp = [0u8; STAMP_LEN];
            stamp.copy_from_slice(&record.body[record.body.len() - STAMP_LEN..]);
            next = Some(Unpriced {
                transient_id: *record.key,
                sequence,
                stamp,
            });
        });
        next
    }

    /// Queue the record of `transient_id` re-written under the tag `value`:
    /// an append of the same key, receive time and body under the new tag,
    /// then a purge of every older record of that key, in that order, so a
    /// flush interrupted between the two leaves the message on the part
    /// twice rather than not at all (and the next walk purges the
    /// leftover). `false`, with nothing queued, when the message is no
    /// longer stored or already carries that value.
    ///
    /// The append holds one copy of the body (at most
    /// [`leviculum_record_log::MAX_BODY`]) until its flush, as an upload's
    /// append does.
    pub fn set_stamp_value(&mut self, transient_id: &[u8; 32], value: u8) -> bool {
        let tag = value.min(MESSAGE_TAG_MAX);
        let mut rewrite: Option<FlushOp> = None;
        self.scan_messages(|record| {
            if record.key == transient_id && rewrite.is_none() && record.tag != tag {
                rewrite = Some(FlushOp::Append {
                    key: *transient_id,
                    time: record.time,
                    tag,
                    body: record.body.to_vec(),
                });
            }
        });
        let Some(rewrite) = rewrite else {
            return false;
        };
        let at = self.queue.len();
        self.queue.push_back(rewrite);
        let queue = &mut self.queue;
        self.region.with_bytes(|bytes| {
            scan(bytes, |record| {
                let masked = queue.iter().any(
                    |op| matches!(op, FlushOp::Purge { offset, .. } if *offset == record.offset),
                );
                if record.live
                    && record.tag <= MESSAGE_TAG_MAX
                    && record.key == transient_id
                    && !masked
                {
                    queue.push_back(FlushOp::Purge {
                        offset: record.offset,
                        key: *transient_id,
                    });
                }
            })
        });
        at < self.queue.len()
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

    /// A slice of one body copied straight out of mapped flash (or the
    /// queued append), no allocation: the outbound peer sync streams its
    /// envelope through this a part at a time (leviculum#388).
    fn read_body_at(
        &self,
        transient_id: &[u8; 32],
        offset: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, StorageError> {
        if let Some(body) = self.pending_append_body(transient_id) {
            return Ok(Some(copy_from(body, offset, buf)));
        }
        let mut copied = None;
        self.scan_messages(|record| {
            if record.key == transient_id && copied.is_none() {
                copied = Some(copy_from(record.body, offset, buf));
            }
        });
        Ok(copied)
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

    /// The trait's number: bytes a message body could still use across
    /// the REGION — [`PropagationStore::capacity`] minus what live
    /// records occupy, minus the appends this store has queued.
    ///
    /// Not the log's own `free_bytes`, which this used to mirror. That
    /// one answers a different question — "bytes appendable before
    /// reclaim has to erase a page that still holds records" — and after
    /// the first lap it is only the active page's room, so it reads 0
    /// whenever the active page happens to be full and 0 again on the
    /// next boot of a lapped region. Both boards showed exactly that on
    /// 2026-09-12/13: `PN_STATS store=0 free_bytes=0 capacity=65344`
    /// beside a mount line of `live=2 free_bytes=2600` and 38 successful
    /// appends. A store with two live records in a 16-page region is not
    /// full, and a wrong "full" is worse than none — it is the number a
    /// future store-full bug would hide behind.
    ///
    /// PURGED records do not count as occupied, a queued purge included
    /// (the module's visibility rule: a queued purge masks its record
    /// from every read, and this is a read). Their bytes come back when
    /// the round-robin reclaim reaches their page, and the role's
    /// `make_room` loop purges until `free_space` reaches what it needs —
    /// a purge that did not move this number would spin that loop
    /// through the whole store. PEER and bench records DO count: they
    /// are live records in the same region, and bytes they hold are
    /// bytes a message body cannot have.
    ///
    /// Cost: one [`scan`] of the region — the same pass `for_each` makes,
    /// which the concept page prices at 8 to 33 ms of CPU for a full
    /// 64 KiB (`docs/src/concepts/propagation-node-on-a-board.md`, §Scan)
    /// and no flash-scheduler time at all. The caller that pays it every
    /// 300 s is the `PN_STATS` line, which already pays the same for its
    /// `count()`.
    ///
    /// Page granularity is not modelled: a record never straddles a page,
    /// so a body needing more than the active page's remaining room costs
    /// the next page whole. The number is therefore an upper bound on
    /// what one more body can use, by less than a page.
    fn free_space(&self) -> u64 {
        let live: u64 = self.region.with_bytes(|bytes| {
            let mut used = 0u64;
            scan(bytes, |record| {
                if record.live && !self.is_purge_masked(record.offset) {
                    used += record_stride(record.body.len()) as u64;
                }
            });
            used
        });
        let queued: u64 = self
            .queue
            .iter()
            .map(|op| match op {
                FlushOp::Append { body, .. } => record_stride(body.len()) as u64,
                FlushOp::Purge { .. } => 0,
            })
            .sum();
        self.capacity().saturating_sub(live).saturating_sub(queued)
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
/// key + value, limits, costs, timebase, last-heard, cursor, public keys.
/// The trailing 64 key bytes joined in #388 pass 3; records of the old
/// length ([`PEER_RECORD_LEN_LEGACY`]) still decode (keys absent), so an
/// upgrade keeps its peer table.
pub const PEER_RECORD_LEN: usize = PEER_RECORD_LEN_LEGACY + 64;

/// Body length written before the peer's public keys were persisted.
pub const PEER_RECORD_LEN_LEGACY: usize = 1 + 1 + 16 + 16 + 34 + 4 + 4 + 3 + 8 + 8 + 8;

const PEER_VERSION: u8 = 1;
const PEER_FLAG_IDENTITY: u8 = 0x01;
const PEER_FLAG_KEY: u8 = 0x02;
const PEER_FLAG_STATIC: u8 = 0x04;
const PEER_FLAG_PUBKEYS: u8 = 0x08;

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
    if let Some(keys) = &record.public_keys {
        out[1] |= PEER_FLAG_PUBKEYS;
        out[103..167].copy_from_slice(keys);
    }
    out
}

/// Decode a record-log body back into a [`PeerRecord`], or `None` for a
/// body from a different version or length.
pub fn decode_peer_record(body: &[u8]) -> Option<PeerRecord> {
    if (body.len() != PEER_RECORD_LEN && body.len() != PEER_RECORD_LEN_LEGACY)
        || body[0] != PEER_VERSION
    {
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
    let public_keys =
        (flags & PEER_FLAG_PUBKEYS != 0 && body.len() == PEER_RECORD_LEN).then(|| {
            let mut keys = [0u8; 64];
            keys.copy_from_slice(&body[103..167]);
            keys
        });
    Some(PeerRecord {
        destination_hash,
        identity_hash,
        public_keys,
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
        // The board does not rotate its table, so the log does not keep
        // the host's traffic statistics (`PeerRecord::traffic`).
        traffic: Default::default(),
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

    /// Estimated heap bytes this adapter pins (#388 census): the flush
    /// queue with its append bodies, plus the unflushed save/remove
    /// mirrors.
    pub fn queued_heap_bytes(&self) -> usize {
        queue_heap_bytes(&self.queue)
            + self.pending.capacity() * core::mem::size_of::<PeerRecord>()
            + self.removed.capacity() * DESTINATION_LENGTH
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
