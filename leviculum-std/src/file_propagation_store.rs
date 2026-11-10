//! File-backed [`PropagationStore`]: one file per stored message
//! (Codeberg #384, part 1).
//!
//! The layout is the reference's message store, one directory of files named
//! `<transient_id_hex>_<received_secs>_<stamp_value>_<sequence>` whose
//! content is `lxmf_data || stamp` (`lxmf_propagation`,
//! `reference/LXMF/LXMF/LXMRouter.py:2512-2515`; re-indexed at startup by
//! `enable_propagation`, `:565-592`). Three local differences: the timestamp
//! is written as integer seconds rather than a float, the stamp-value
//! component is always present (the reference omits it at value 0 and then
//! *skips such files entirely* when re-indexing, `:568` requires three
//! components — a quirk, not a behaviour worth importing), and a fourth
//! component carries the append-order sequence the per-peer sync cursors
//! index (`docs/src/concepts/propagation-node-on-a-board.md` §5) so cursors
//! stay valid across a reopen.
//!
//! The directory is shared with `lxmd` in one direction. `lnpnd` started on
//! an `lxmd` config directory opens the store `lxmd` left there, so [`open`]
//! reads the reference's names as well: `<hex>_<received float>` and
//! `<hex>_<received float>_<stamp_value>`, the receive time truncated to
//! whole seconds and a missing stamp value read as 0 (two-component files
//! are served here although `lxmd` itself would never index them again; a
//! part-1 store of ours is the three-component case too). Such entries are
//! assigned fresh sequences above every known one, in receive-time order,
//! which at worst re-offers them once, and are renamed into our scheme on
//! that first open, so after it the directory is ours. What we write is
//! always our own four-component scheme; `lxmd` would index it unchanged,
//! since it reads only the first three components, and a torn temp file of
//! ours is invisible to both indexers.
//!
//! [`open`]: FilePropagationStore::open
//!
//! # Power-cut safety, and why this store fsyncs
//!
//! The role proves an upload packet only after [`PropagationStore::append`]
//! returns ("persist before you prove",
//! `docs/src/concepts/propagation-node-on-a-board.md` §3): a proof written
//! before the record is durable converts a power cut into a silently lost
//! message, because the client stops retrying. So `append` here is
//! write-to-temp, `fsync`, rename-into-place, `fsync` the directory — after
//! it returns, the message survives the plug being pulled, and a cut at any
//! earlier point leaves only a `.tmp-` file the reopen scan ignores. The
//! generic `atomic_write` helper in `crate::storage` deliberately skips the
//! fsyncs (announce caches and ratchet files tolerate losing the last
//! seconds); this store must not.
//!
//! # Write-behind, and where "persist before you prove" holds then
//!
//! Those two `fsync`s cost 1.0 to 1.8 ms an append on the coder host and 3
//! to 9 ms on the field node's Odroid (order 1009), and `lnpnd` appends from
//! a hook that holds the core lock under a 5 ms budget. So a store switched
//! to [`write_behind`] hands every filesystem change to a writer thread of
//! its own and returns: `append` queues the durable write and hands back a
//! ticket ([`writes_issued`]), and [`settle_writes`] says how far the writer
//! has got. The caller proves an upload once the writer has passed its
//! ticket and the message is still in the store, which moves the boundary
//! the trait documents from `append` to the writer's confirmation, as the
//! board's flush queue does (`leviculum-nrf/pn-store`, "How a synchronous
//! trait meets an asynchronous flash"). The visibility rule is the board's
//! too: a queued append is visible to `contains` and the body reads, so a
//! retry is a duplicate and a `/get` could serve it, but not to `for_each`,
//! so nothing offers, lists or hands a peer cursor a message that a power
//! cut could still take. A write the writer could not make is reported in
//! [`SettledWrites::failed`] and leaves the store, so the caller can forget
//! the id and the client's retry is stored rather than answered as proven.
//!
//! Purges and the removal of a replaced file go through the same queue, in
//! order, so a purge of a queued message runs after its write.
//!
//! # The sequence index
//!
//! Beside the directory index keyed by transient ID the store keeps the same
//! entries ordered by sequence, so [`PropagationStore::for_each_above`] reads
//! only what an offer reads and [`PropagationStore::newest_sequence`] is the
//! last key, and [`PropagationStore::meta`] is a lookup. Without it every
//! outbound sync tick walked the whole store: 170 ms a tick on the field
//! node's Odroid at 300 000 messages (leviculum#478).
//! A third copy ordered by receive time serves the expiry sweep
//! ([`PropagationStore::for_each_received_before`]): it reads what expired,
//! where it walked the whole store every maintenance tick, 24.6 ms on the
//! Odroid at about 300 000 messages (order 1171's field profile,
//! leviculum#478). A fourth counts, by sequence, the entries a peer may be
//! offered ([`PropagationStore::count_above_cursors`], `SequenceCounts`):
//! a peer's unhandled count is a prefix sum, where it walked the store once
//! per peer per `--status` request, 94 ms a request at 320 000 messages and
//! 20 peers on the coder host (order 1370). All four indexes change
//! together, through `index_insert` and `index_remove` only.
//!
//! # What the index costs
//!
//! The index is the store's whole RAM, and it grows with every message the
//! store holds, up to the store limit. An entry held its file's full path
//! until LV-1000 (order 1515): 457 bytes resident a message at 326 000
//! messages, 149 MB, and the largest table by far in lnpnd's growth soak
//! at that size (miauhaus holds 326 000, and the OOM killer took lnpnd
//! there at 398 MB). The path is the entry's name under the store's
//! directory (`entry_name`, the module docs' first section), so it is now
//! derived where a file is opened, renamed or removed, and only a file under
//! another name keeps its path, in `odd_names`: a legacy name whose rename
//! at open failed. 289 bytes resident a message at the same count, 94 MB.
//!
//! The census ([`FilePropagationStore::heap_census`]) prices the four
//! indexes as `alloc` lays them out, the live entries and the room the
//! containers hold beyond them named apart (order 1648). How full a
//! B-tree's nodes run depends on how it was built: the id-keyed map fills
//! as random inserts do, about 8 entries a node of 11, but the sequence and
//! receive-time sets are collected full from a sorted run at the open and
//! then grow at their right end, 7 a node. A store just opened costs 254
//! bytes a message, one appended to from empty 334 (20 000 messages,
//! `lnpnd/tests/store_index_census.rs`). The census before priced every
//! tree at 7 a node, which overstated a reopened store by a fifth.
//!
//! # The index file, and why a start reads no message
//!
//! The reference re-indexes its store at every start by listing the
//! directory and opening every file for its size and destination hash
//! (`enable_propagation`, `reference/LXMF/LXMF/LXMRouter.py:565-592`), and
//! so did this store: 130 s on the field node's Odroid at 323 597 messages,
//! during which the daemon answered nothing (order 1376). 36 s of it on the
//! coder host's cold cache at 320 000, 2.1 s warm: the cost is the two
//! system calls a file, `stat` and `open`+`read`, each a random read on a
//! cold disk, and not the listing or the indexes built from it.
//!
//! So the store keeps what those calls return in [`INDEX_FILE`] inside the
//! directory, one record per message under the name it was read for, and a
//! start lists the directory and reads only the files the record does not
//! name: the ones `lxmd` or a crash left behind since the record was
//! written. A name carries the transient ID, the receive time, the stamp
//! value and the sequence, and a file under a given name is never rewritten
//! (an append writes a new name, a re-pricing renames), so a record for the
//! same name is the file's own size and destination. A record whose file is
//! gone is dropped by the listing. The record is not trusted beyond that:
//! it ends in a SHA-256 over everything before it, and a file that fails it
//! is ignored and rebuilt by the walk it replaces. It is written when the
//! start found it out of date, and when the store is dropped; a crash in
//! between costs the next start a read of what arrived since.
//!
//! [`write_behind`]: FilePropagationStore::write_behind
//! [`writes_issued`]: FilePropagationStore::writes_issued
//! [`settle_writes`]: FilePropagationStore::settle_writes

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Bound, ControlFlow};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use leviculum_lxmf::propagation_store::{
    copy_from, count_above_cursors_by_walk, PropagationBodyStore, PropagationStore, StoredMessage,
    MIN_BODY_LEN,
};
use leviculum_lxmf::storage::StorageError;
use leviculum_lxmf::TransientId;

use sha2::{Digest, Sha256};

use crate::durable::{device_fsync, write_durably, Fsync};
use crate::sequence_counts::{Inserted, SequenceCounts};
use crate::storage::{hex_decode, hex_encode, note_unreadable_entry};

/// A directory of propagation-store messages with a hard byte capacity.
///
/// The capacity counts the messages' own byte lengths, so
/// [`PropagationStore::space_required`] keeps its default: a file costs what
/// it holds, and an eviction pass frees exactly that.
pub struct FilePropagationStore {
    dir: PathBuf,
    capacity: u64,
    used: u64,
    index: BTreeMap<TransientId, IndexEntry>,
    /// The files whose name is not the one [`Self::entry_name`] gives their
    /// entry: a legacy name whose rename at open failed, or a name in
    /// another spelling. Every other file's path is derived from its entry
    /// ([`Self::path_of`]), so the index holds no path of its own (module
    /// docs, "What the index costs").
    odd_names: BTreeMap<TransientId, PathBuf>,
    /// `index` ordered by `(sequence, transient_id)` (module docs, "The
    /// sequence index").
    by_sequence: BTreeSet<(u64, TransientId)>,
    /// `index` ordered by `(received_at, transient_id)`, what the expiry
    /// sweep reads (module docs, "The sequence index").
    by_received: BTreeSet<(u64, TransientId)>,
    /// `index`'s entries of at most the counted size, counted by sequence
    /// (module docs, "The sequence index").
    counts: SequenceCounts,
    /// Last assigned append sequence; recovered as the maximum on disk.
    last_sequence: u64,
    fsync: Arc<dyn Fsync>,
    /// `Some` once [`Self::write_behind`] switched the store to its writer.
    writer: Option<Writer>,
    /// The index moved since [`INDEX_FILE`] was last written (module docs,
    /// "The index file").
    index_dirty: bool,
    opened: OpenProfile,
    /// The last sequence the open indexed: `by_sequence` and `by_received`
    /// were built from a sorted run at the open, which fills their nodes,
    /// and an entry inserted since goes in one by one (module docs, "What
    /// the index costs").
    bulk_through: u64,
    /// Entries of the bulk-built run still in the store.
    bulk_live: usize,
}

/// [`FilePropagationStore::heap_census`]: what the store holds in RAM.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoreHeapCensus {
    /// Messages indexed.
    pub messages: usize,
    /// The four indexes (module docs, "The sequence index" and "What the
    /// index costs"): `index_entry_bytes` plus `index_spare_bytes`.
    pub index_bytes: usize,
    /// The live entries' own sizes across the four indexes, an odd name's
    /// path included: what the index would cost in containers with no
    /// room to spare.
    pub index_entry_bytes: usize,
    /// What the containers allocate beyond their live entries: the B-tree
    /// nodes' empty slots and headers and the inner nodes' edges, and the
    /// counting index's vectors' capacity beyond their length.
    pub index_spare_bytes: usize,
    /// Appends and purges the writer has not confirmed, bodies included.
    pub writer_bytes: usize,
}

/// How full a B-tree's nodes run, by how the tree was built (order 1648).
///
/// Entries a node, the separator above it counted with it, fitted to a
/// counting allocator over 200 000 entries of each kind: random inserts 8.4, ascending inserts 7 (a full node splits six to the
/// left, four and the new entry to the right, and the left one is never
/// touched again), a sorted run collected at once 12 (`collect` fills every
/// node). `lnpnd/tests/store_index_census.rs` holds the store's sum to the
/// bytes a counting allocator sees it give back.
#[derive(Debug, Clone, Copy)]
enum NodeFill {
    Random,
    Ascending,
    Bulk,
}

impl NodeFill {
    /// Entries a node, in tenths.
    fn tenths(self) -> usize {
        match self {
            NodeFill::Random => 84,
            NodeFill::Ascending => 70,
            NodeFill::Bulk => 120,
        }
    }
}

/// Bytes `alloc`'s B-tree allocates for `len` entries of `K` to `V` filled
/// as `fill` says: its leaves (a parent pointer, two `u16`, eleven key and
/// eleven value slots) and the level above them (a leaf's layout and twelve
/// edges). The levels above that are under one part in fifty and left out.
fn btree_allocated<K, V>(len: usize, fill: NodeFill) -> usize {
    use std::mem::{align_of, size_of};
    if len == 0 {
        return 0;
    }
    let align = align_of::<usize>()
        .max(align_of::<K>())
        .max(align_of::<V>());
    let leaf =
        (size_of::<usize>() + 4 + 11 * (size_of::<K>() + size_of::<V>())).next_multiple_of(align);
    let inner = leaf + 12 * size_of::<usize>();
    let tenths = fill.tenths();
    let leaves = (len * 10).div_ceil(tenths);
    let inners = (leaves * 10).div_ceil(tenths);
    leaves * leaf + inners * inner
}

/// What [`FilePropagationStore::open`] found and spent its time on (module
/// docs, "The index file").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OpenProfile {
    /// What became of [`INDEX_FILE`].
    pub index_file: IndexFileState,
    /// Messages indexed.
    pub messages: usize,
    /// Of those, the ones whose size and destination came from the index
    /// file.
    pub from_index_file: usize,
    /// The ones whose file was read for them, the walk's per-file cost.
    pub files_read: usize,
    /// Reading and checking the index file.
    pub load_index_us: u64,
    /// The directory listing and the name parse, file reads excluded.
    pub list_us: u64,
    /// `stat` of the files read.
    pub stat_us: u64,
    /// `open` and the 16-byte destination read of the files read.
    pub read_us: u64,
    /// The sequence, receive-time and count indexes.
    pub build_us: u64,
    /// Writing the index file afresh; 0 when it was current.
    pub save_index_us: u64,
    /// The whole open.
    pub total_us: u64,
}

/// What [`FilePropagationStore::open`] made of [`INDEX_FILE`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum IndexFileState {
    /// Read and checked.
    Loaded,
    /// Not there: a first start, or a store written before the index file.
    #[default]
    Missing,
    /// There, but cut short or failing its checksum; ignored, and rebuilt.
    Corrupt,
}

impl IndexFileState {
    /// The word a start line prints for it.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexFileState::Loaded => "loaded",
            IndexFileState::Missing => "missing",
            IndexFileState::Corrupt => "corrupt",
        }
    }
}

#[derive(Debug, Clone)]
struct IndexEntry {
    meta: StoredMessage,
}

/// How far the writer has got, as [`FilePropagationStore::settle_writes`]
/// reports it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettledWrites {
    /// Every append whose ticket is at most this is settled: durable and in
    /// the index, or failed and gone. 0 for a store without a writer, whose
    /// appends hand out no tickets because they are durable on return.
    pub through: u64,
    /// The messages whose write failed since the last report. They are no
    /// longer in the store.
    pub failed: Vec<TransientId>,
}

/// The write-behind half of the store: the queue to the writer thread and
/// the appends it has not confirmed yet.
struct Writer {
    /// `None` once the writer is gone (dropped, or dead): appends fail then.
    jobs: Option<mpsc::Sender<Job>>,
    done: mpsc::Receiver<(u64, bool)>,
    thread: Option<JoinHandle<()>>,
    /// The last ticket handed out, and the last one settled.
    issued: u64,
    through: u64,
    /// Unsettled appends, ticket to message.
    tickets: BTreeMap<u64, TransientId>,
    /// What those appends stored, until the writer confirms them: visible to
    /// `contains` and the body reads, not to `for_each` (module docs).
    queued: BTreeMap<TransientId, Queued>,
    failed: Vec<TransientId>,
}

struct Queued {
    ticket: u64,
    entry: IndexEntry,
    body: Arc<[u8]>,
}

/// One filesystem change, in the order the store made it.
enum Job {
    Write {
        ticket: u64,
        path: PathBuf,
        body: Arc<[u8]>,
        /// The file this message lived in before, removed once the new one
        /// is durable.
        supersedes: Option<PathBuf>,
    },
    Remove(PathBuf),
}

fn run_writer(jobs: mpsc::Receiver<Job>, done: mpsc::Sender<(u64, bool)>, fsync: Arc<dyn Fsync>) {
    // Ends when the store drops its sender, after everything queued before
    // that: dropping the store is a flush.
    for job in jobs {
        match job {
            Job::Write {
                ticket,
                path,
                body,
                supersedes,
            } => {
                let temp = temp_path(&path);
                let ok = write_durably(&temp, &path, &body, &*fsync).is_ok();
                if ok {
                    if let Some(previous) = supersedes {
                        let _ = std::fs::remove_file(previous);
                    }
                } else {
                    let _ = std::fs::remove_file(&temp);
                }
                // A store that is gone no longer asks; the write still
                // happened, and the next open finds it.
                let _ = done.send((ticket, ok));
            }
            Job::Remove(path) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl FilePropagationStore {
    /// Open the store, creating the directory if needed, and rebuild the
    /// index from the files on disk — the same startup scan the reference
    /// performs (`enable_propagation`,
    /// `reference/LXMF/LXMF/LXMRouter.py:565-592`). Files whose names or
    /// contents do not parse are noted and skipped, never deleted.
    pub fn open<P: AsRef<Path>>(dir: P, capacity: u64) -> Result<Self, StorageError> {
        Self::open_with(dir, capacity, device_fsync())
    }

    /// [`Self::open`] over another [`Fsync`]: the seam a test makes the
    /// device slow through, or kills the writer through.
    pub fn open_with<P: AsRef<Path>>(
        dir: P,
        capacity: u64,
        fsync: Arc<dyn Fsync>,
    ) -> Result<Self, StorageError> {
        let started = Instant::now();
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|_| StorageError::Io)?;
        let mut opened = OpenProfile::default();
        let (recorded, index_file) = load_index(&dir.join(INDEX_FILE));
        opened.index_file = index_file;
        opened.load_index_us = micros(started);
        let listed = Instant::now();
        let mut file_us = 0u64;
        let mut index = BTreeMap::new();
        let mut odd_names: BTreeMap<TransientId, PathBuf> = BTreeMap::new();
        let mut used = 0u64;
        let entries = std::fs::read_dir(&dir).map_err(|_| StorageError::Io)?;
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
            };
            // An interrupted append's temp file: not a message, not noise
            // worth a warning either.
            if name.starts_with(TEMP_PREFIX) || name == STAMP_VALUES_MARKER || name == INDEX_FILE {
                continue;
            }
            let Some((transient_id, received_at, stamp_value, sequence)) = parse_name(&name) else {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
            };
            let path = entry.path();
            // Whether the file has a name other than the one this store
            // writes for it.
            let odd = match sequence {
                Some(sequence) => {
                    name != Self::entry_name(&transient_id, received_at, stamp_value, sequence)
                }
                None => true,
            };
            // The record for this very name, if the index file has one: a
            // legacy name has no sequence and is never recorded under it.
            let record = recorded.get(&transient_id).filter(|record| {
                record.received_at == received_at
                    && record.stamp_value == stamp_value
                    && Some(record.sequence) == sequence
            });
            let (size, destination_hash) = match record {
                Some(record) => {
                    opened.from_index_file += 1;
                    (record.size as u64, record.destination_hash)
                }
                None => {
                    opened.files_read += 1;
                    let stat = Instant::now();
                    let metadata = std::fs::metadata(&path);
                    let stat_us = micros(stat);
                    opened.stat_us += stat_us;
                    let Ok(metadata) = metadata else {
                        file_us += stat_us;
                        note_unreadable_entry(&dir, &entry.file_name());
                        continue;
                    };
                    let size = metadata.len();
                    if size < MIN_BODY_LEN as u64 || size > u32::MAX as u64 {
                        file_us += stat_us;
                        note_unreadable_entry(&dir, &entry.file_name());
                        continue;
                    }
                    // The destination hash lives in the first 16 bytes of
                    // the body, as the reference reads it back
                    // (`LXMRouter.py:578`).
                    let read = Instant::now();
                    let destination_hash = read_destination(&path);
                    let read_us = micros(read);
                    opened.read_us += read_us;
                    file_us += stat_us + read_us;
                    let Some(destination_hash) = destination_hash else {
                        note_unreadable_entry(&dir, &entry.file_name());
                        continue;
                    };
                    (size, destination_hash)
                }
            };
            used += size;
            index.insert(
                transient_id,
                IndexEntry {
                    meta: StoredMessage {
                        transient_id,
                        destination_hash,
                        size: size as u32,
                        received_at,
                        stamp_value,
                        // Names without a sequence (part-1 or the
                        // reference's) get a real one after the scan, below.
                        sequence: sequence.unwrap_or(0),
                    },
                },
            );
            // The last file listed for an id is the one indexed, so its
            // name decides whether the id has an odd one.
            if odd {
                odd_names.insert(transient_id, path);
            } else {
                odd_names.remove(&transient_id);
            }
        }
        opened.list_us = micros(listed).saturating_sub(file_us);
        let built = Instant::now();
        let mut last_sequence = index
            .values()
            .map(|entry| entry.meta.sequence)
            .max()
            .unwrap_or(0);
        // Sequence legacy entries above everything known, oldest first, so
        // relative order is kept and cursors below them stay sound (a
        // too-high foreign cursor is the bounded full re-offer case, §5).
        let mut legacy: Vec<TransientId> = index
            .values()
            .filter(|entry| entry.meta.sequence == 0)
            .map(|entry| entry.meta.transient_id)
            .collect();
        legacy.sort_by_key(|id| {
            let meta = index[id].meta;
            (meta.received_at, meta.transient_id)
        });
        for id in legacy {
            last_sequence += 1;
            if let Some(entry) = index.get_mut(&id) {
                entry.meta.sequence = last_sequence;
                // Persist the assignment so it is one-time, not
                // per-reopen: a later open must not renumber.
                let renamed = dir.join(Self::entry_name(
                    &entry.meta.transient_id,
                    entry.meta.received_at,
                    entry.meta.stamp_value,
                    last_sequence,
                ));
                // A failed rename leaves the old name in `odd_names`, where
                // the scan put it; the next open tries again.
                let renamed_ok = odd_names
                    .get(&id)
                    .is_some_and(|old| std::fs::rename(old, &renamed).is_ok());
                if renamed_ok {
                    odd_names.remove(&id);
                }
            }
        }
        let by_sequence = index
            .values()
            .map(|entry| (entry.meta.sequence, entry.meta.transient_id))
            .collect();
        let by_received = index
            .values()
            .map(|entry| (entry.meta.received_at, entry.meta.transient_id))
            .collect();
        opened.messages = index.len();
        // Out of date when anything was read past it, or it names a file
        // the listing no longer has.
        let index_dirty = index_file != IndexFileState::Loaded
            || opened.files_read > 0
            || opened.from_index_file != recorded.len();
        let mut store = Self {
            dir,
            capacity,
            used,
            index,
            odd_names,
            by_sequence,
            by_received,
            counts: SequenceCounts::new(u64::MAX),
            last_sequence,
            fsync,
            writer: None,
            index_dirty,
            opened: OpenProfile::default(),
            bulk_through: last_sequence,
            bulk_live: opened.messages,
        };
        store.rebuild_counts(u64::MAX);
        opened.build_us = micros(built);
        if store.index_dirty {
            let saved = Instant::now();
            store.save_index();
            opened.save_index_us = micros(saved);
        }
        opened.total_us = micros(started);
        store.opened = opened;
        Ok(store)
    }

    /// Heap bytes the store pins in RAM, for lnpnd's `PN_CENSUS` line
    /// (order 1515): the four indexes over the stored messages, and what the
    /// writer still holds. Walks the index once for the paths' own heap, so
    /// it is a census call, not a per-hook one.
    pub fn heap_census(&self) -> StoreHeapCensus {
        use leviculum_core::heap_census::btree_map_bytes;
        use std::mem::size_of;
        let len = self.index.len();
        let odd = self.odd_names.len();
        let odd_paths: usize = self.odd_names.values().map(PathBuf::capacity).sum();
        // Every transient id is a hash, so the id-keyed maps fill as random
        // inserts do. The sequence and receive-time sets are a sorted run
        // from the open plus what was appended since, in ascending order.
        let appended = len.saturating_sub(self.bulk_live);
        let ordered = |fill: NodeFill, n: usize| btree_allocated::<(u64, TransientId), ()>(n, fill);
        let index_alloc = btree_allocated::<TransientId, IndexEntry>(len, NodeFill::Random)
            + btree_allocated::<TransientId, PathBuf>(odd, NodeFill::Random)
            + odd_paths
            + 2 * (ordered(NodeFill::Bulk, self.bulk_live)
                + ordered(NodeFill::Ascending, appended));
        let (counts_live, counts_alloc) = self.counts.heap_bytes_live_and_allocated();
        let index_entry_bytes = len
            * (size_of::<TransientId>()
                + size_of::<IndexEntry>()
                + 2 * size_of::<(u64, TransientId)>())
            + odd * (size_of::<TransientId>() + size_of::<PathBuf>())
            + odd_paths
            + counts_live;
        let index_bytes = index_alloc + counts_alloc;
        let writer_bytes = self.writer.as_ref().map_or(0, |writer| {
            btree_map_bytes(&writer.tickets)
                + btree_map_bytes(&writer.queued)
                + writer
                    .queued
                    .values()
                    .map(|queued| queued.body.len())
                    .sum::<usize>()
                + writer.failed.capacity() * std::mem::size_of::<TransientId>()
        });
        StoreHeapCensus {
            messages: len,
            index_bytes,
            index_entry_bytes,
            index_spare_bytes: index_bytes.saturating_sub(index_entry_bytes),
            writer_bytes,
        }
    }

    /// What the open found and what it cost (module docs, "The index
    /// file").
    pub fn open_profile(&self) -> &OpenProfile {
        &self.opened
    }

    /// Write [`INDEX_FILE`] from the index, if the index moved since it was
    /// last written or the file is gone. A failure is a warning: the file is
    /// a cache, and the next start reads what it is missing.
    pub fn save_index(&mut self) {
        if !self.index_dirty && self.dir.join(INDEX_FILE).exists() {
            return;
        }
        let mut bytes =
            Vec::with_capacity(INDEX_HEADER_LEN + self.index.len() * INDEX_RECORD_LEN + 32);
        bytes.extend_from_slice(INDEX_MAGIC);
        bytes.extend_from_slice(&(self.index.len() as u64).to_le_bytes());
        for entry in self.index.values() {
            let meta = &entry.meta;
            bytes.extend_from_slice(&meta.transient_id);
            bytes.extend_from_slice(&meta.received_at.to_le_bytes());
            bytes.push(meta.stamp_value);
            bytes.extend_from_slice(&meta.sequence.to_le_bytes());
            bytes.extend_from_slice(&meta.size.to_le_bytes());
            bytes.extend_from_slice(&meta.destination_hash);
        }
        let checksum = Sha256::digest(&bytes);
        bytes.extend_from_slice(&checksum);
        let path = self.dir.join(INDEX_FILE);
        let temp = temp_path(&path);
        // No fsync: a torn or lost file fails its checksum or is missing,
        // and either costs one walk, never a wrong answer.
        let written = std::fs::write(&temp, &bytes).and_then(|()| std::fs::rename(&temp, &path));
        match written {
            Ok(()) => self.index_dirty = false,
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                tracing::warn!("message store index {}: {error}", path.display());
            }
        }
    }

    /// Hand every filesystem change from here on to a writer thread, so an
    /// append costs its caller a queue push instead of two `fsync`s (module
    /// docs, "Write-behind"). Fails only when the thread cannot be spawned.
    pub fn write_behind(mut self) -> Result<Self, StorageError> {
        if self.writer.is_some() {
            return Ok(self);
        }
        let (jobs, inbox) = mpsc::channel();
        let (outbox, done) = mpsc::channel();
        let fsync = Arc::clone(&self.fsync);
        let thread = std::thread::Builder::new()
            .name("pn-store-writer".into())
            .spawn(move || run_writer(inbox, outbox, fsync))
            .map_err(|_| StorageError::Io)?;
        self.writer = Some(Writer {
            jobs: Some(jobs),
            done,
            thread: Some(thread),
            issued: 0,
            through: 0,
            tickets: BTreeMap::new(),
            queued: BTreeMap::new(),
            failed: Vec::new(),
        });
        Ok(self)
    }

    /// The ticket of the last append handed to the writer; 0 without one.
    /// An upload accepted now is durable once [`SettledWrites::through`]
    /// reaches this.
    pub fn writes_issued(&self) -> u64 {
        self.writer.as_ref().map_or(0, |writer| writer.issued)
    }

    /// Whether an append is still with the writer.
    pub fn writes_outstanding(&self) -> bool {
        self.writer
            .as_ref()
            .is_some_and(|writer| writer.through < writer.issued)
    }

    /// Take in what the writer has finished, without waiting: confirmed
    /// appends join the index, failed ones leave the store and are reported.
    pub fn settle_writes(&mut self) -> SettledWrites {
        self.collect(false);
        self.report()
    }

    /// [`Self::settle_writes`], after waiting for every queued write.
    pub fn flush_writes(&mut self) -> SettledWrites {
        self.collect(true);
        self.report()
    }

    fn report(&mut self) -> SettledWrites {
        match self.writer.as_mut() {
            Some(writer) => SettledWrites {
                through: writer.through,
                failed: std::mem::take(&mut writer.failed),
            },
            None => SettledWrites::default(),
        }
    }

    /// Settle what the writer reported; with `wait`, until nothing is
    /// outstanding.
    fn collect(&mut self, wait: bool) {
        loop {
            let Some(writer) = self.writer.as_mut() else {
                return;
            };
            let next = if wait && writer.through < writer.issued {
                writer.done.recv().map_err(|_| TryRecvError::Disconnected)
            } else {
                writer.done.try_recv()
            };
            match next {
                Ok((ticket, ok)) => self.settle(ticket, ok),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    // The writer died (or panicked) with appends unconfirmed:
                    // none of them may be proven, so all of them failed.
                    writer.jobs = None;
                    let open: Vec<u64> = writer.tickets.keys().copied().collect();
                    let issued = writer.issued;
                    for ticket in open {
                        self.settle(ticket, false);
                    }
                    if let Some(writer) = self.writer.as_mut() {
                        writer.through = issued;
                    }
                    return;
                }
            }
        }
    }

    fn settle(&mut self, ticket: u64, ok: bool) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        writer.through = writer.through.max(ticket);
        let Some(transient_id) = writer.tickets.remove(&ticket) else {
            return;
        };
        // Purged, or appended again, while the write ran: the later state
        // is the store's, and its own job settles it.
        if writer
            .queued
            .get(&transient_id)
            .is_none_or(|queued| queued.ticket != ticket)
        {
            return;
        }
        let Some(queued) = writer.queued.remove(&transient_id) else {
            return;
        };
        if ok {
            self.index_insert(transient_id, queued.entry);
        } else {
            self.used -= queued.entry.meta.size as u64;
            writer.failed.push(transient_id);
        }
    }

    /// Put one entry in all four indexes, replacing whatever the ID held.
    fn index_insert(&mut self, transient_id: TransientId, entry: IndexEntry) {
        let sequence = entry.meta.sequence;
        let received_at = entry.meta.received_at;
        let size = entry.meta.size;
        let mut rebuild = false;
        self.index_dirty = true;
        // Every entry inserted after the open is under the name this store
        // writes; an odd name it replaces is gone with the old file.
        self.odd_names.remove(&transient_id);
        if let Some(previous) = self.index.insert(transient_id, entry) {
            self.left_bulk(previous.meta.sequence);
            self.by_sequence
                .remove(&(previous.meta.sequence, transient_id));
            self.by_received
                .remove(&(previous.meta.received_at, transient_id));
            rebuild |= self.counts.remove(previous.meta.sequence);
        }
        self.by_sequence.insert((sequence, transient_id));
        self.by_received.insert((received_at, transient_id));
        rebuild |= self.counts.insert(sequence, size) == Inserted::Rebuild;
        if rebuild {
            self.rebuild_counts(self.counts.largest_size());
        }
    }

    /// An entry left the sequence and receive-time indexes: one fewer of
    /// the run the open built, if it was one of them.
    fn left_bulk(&mut self, sequence: u64) {
        if sequence <= self.bulk_through {
            self.bulk_live = self.bulk_live.saturating_sub(1);
        }
    }

    /// Take one entry out of all four indexes.
    fn index_remove(&mut self, transient_id: &TransientId) -> Option<IndexEntry> {
        let entry = self.index.remove(transient_id)?;
        self.left_bulk(entry.meta.sequence);
        self.odd_names.remove(transient_id);
        self.index_dirty = true;
        self.by_sequence
            .remove(&(entry.meta.sequence, *transient_id));
        self.by_received
            .remove(&(entry.meta.received_at, *transient_id));
        if self.counts.remove(entry.meta.sequence) {
            self.rebuild_counts(self.counts.largest_size());
        }
        Some(entry)
    }

    /// The counting index afresh from the sequence index, weighing entries
    /// of at most `largest_size`: at open, when the role names its size,
    /// and when dead slots outnumber live ones (`SequenceCounts`).
    fn rebuild_counts(&mut self, largest_size: u64) {
        let index = &self.index;
        self.counts = SequenceCounts::rebuilt(
            largest_size,
            index.len(),
            self.by_sequence
                .iter()
                .filter_map(|(sequence, transient_id)| {
                    index
                        .get(transient_id)
                        .map(|entry| (*sequence, entry.meta.size))
                }),
        );
    }

    /// Heap bytes of the counting index (`SequenceCounts`).
    pub fn count_index_bytes(&self) -> usize {
        self.counts.heap_bytes()
    }

    /// Queue a removal behind the writes before it, or remove now when no
    /// writer runs.
    fn remove_file(&self, path: PathBuf) -> std::io::Result<()> {
        if let Some(jobs) = self.writer.as_ref().and_then(|writer| writer.jobs.as_ref()) {
            if jobs.send(Job::Remove(path.clone())).is_ok() {
                return Ok(());
            }
        }
        std::fs::remove_file(path)
    }

    /// The directory the messages live in.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Whether every stored stamp value is the stamp's true value. False on
    /// a store written before leviculum#442 D6 (8f470aa7), which stored 0
    /// for whatever it took at cost 0 before its first demanding peer, and
    /// on one `lxmd` left behind; [`Self::mark_stamp_values_true`] records
    /// that a re-pricing pass has run over it.
    pub fn stamp_values_true(&self) -> bool {
        self.dir.join(STAMP_VALUES_MARKER).exists()
    }

    /// Record that every stored value is true from here on: what this
    /// daemon appends already is.
    pub fn mark_stamp_values_true(&mut self) -> Result<(), StorageError> {
        let path = self.dir.join(STAMP_VALUES_MARKER);
        write_durably(&temp_path(&path), &path, b"", &*self.fsync)
    }

    /// Replace one message's stored stamp value, keeping its receive time
    /// and its sequence: a re-priced message must not move past the peer
    /// cursors that already stand below it. `Ok(false)` when the message
    /// is not in the store.
    pub fn set_stamp_value(
        &mut self,
        transient_id: &TransientId,
        stamp_value: u8,
    ) -> Result<bool, StorageError> {
        // A rename of a file the writer has not placed yet would fail.
        let _ = self.flush_writes();
        let Some(meta) = self.index.get(transient_id).map(|entry| entry.meta) else {
            return Ok(false);
        };
        let renamed = self.dir.join(Self::entry_name(
            transient_id,
            meta.received_at,
            stamp_value,
            meta.sequence,
        ));
        std::fs::rename(self.path_of(&meta), &renamed).map_err(|_| StorageError::Io)?;
        if let Ok(dir_handle) = std::fs::File::open(&self.dir) {
            let _ = self.fsync.sync(&dir_handle);
        }
        self.odd_names.remove(transient_id);
        let Some(entry) = self.index.get_mut(transient_id) else {
            return Ok(false);
        };
        entry.meta.stamp_value = stamp_value;
        self.index_dirty = true;
        Ok(true)
    }

    /// Number of stored messages, queued appends included.
    pub fn len(&self) -> usize {
        self.index.len() + self.writer.as_ref().map_or(0, |writer| writer.queued.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Where the file of an indexed message is: its odd name if it has one,
    /// else the name its entry gives it ([`Self::entry_name`]).
    fn path_of(&self, meta: &StoredMessage) -> PathBuf {
        match self.odd_names.get(&meta.transient_id) {
            Some(path) => path.clone(),
            None => self.canonical_path(meta),
        }
    }

    /// The path this store writes `meta` under.
    fn canonical_path(&self, meta: &StoredMessage) -> PathBuf {
        self.dir.join(Self::entry_name(
            &meta.transient_id,
            meta.received_at,
            meta.stamp_value,
            meta.sequence,
        ))
    }

    fn entry_name(
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        sequence: u64,
    ) -> String {
        format!(
            "{}_{received_at}_{stamp_value}_{sequence}",
            hex_encode(transient_id.as_slice())
        )
    }
}

fn parse_name(name: &str) -> Option<(TransientId, u64, u8, Option<u64>)> {
    let mut parts = name.split('_');
    let id: TransientId = hex_decode(parts.next()?)?.try_into().ok()?;
    let received_at = parse_received(parts.next()?)?;
    let stamp_value: u8 = match parts.next() {
        // The reference's name at stamp value 0: it omits the component
        // (`value_component`, `reference/LXMF/LXMF/LXMRouter.py:2513`).
        None => return Some((id, received_at, 0, None)),
        Some(raw) => raw.parse().ok()?,
    };
    let sequence = match parts.next() {
        // Part-1 layout, or the reference's name with a stamp value: no
        // sequence component yet.
        None => None,
        Some(raw) => {
            let sequence: u64 = raw.parse().ok()?;
            // 0 is reserved for "not yet assigned".
            if sequence == 0 {
                return None;
            }
            Some(sequence)
        }
    };
    if parts.next().is_some() {
        return None;
    }
    Some((id, received_at, stamp_value, sequence))
}

/// The receive-time component of a store filename, as whole seconds.
///
/// Our own writer emits integer seconds. The reference writes Python's
/// `time.time()` — a FLOAT, `1789656258.9376912`
/// (`lxmf_propagation`, `reference/LXMF/LXMF/LXMRouter.py:2514`) — and an
/// integer-only parse rejects every file in a store `lxmd` left behind,
/// which is the whole store when a node is taken over. Truncating toward
/// the second is the reference's own resolution for the value: it re-reads
/// the component as a float and compares it against `> 0` and against
/// message age in seconds (`enable_propagation`, `:568-571`).
fn parse_received(raw: &str) -> Option<u64> {
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(seconds);
    }
    let seconds = raw.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 || seconds >= u64::MAX as f64 {
        return None;
    }
    Some(seconds.trunc() as u64)
}

fn read_destination(path: &Path) -> Option<[u8; 16]> {
    use std::io::Read as _;
    let mut head = [0u8; 16];
    let mut file = std::fs::File::open(path).ok()?;
    file.read_exact(&mut head).ok()?;
    Some(head)
}

/// The index file's name inside the store directory (module docs, "The
/// index file"). A dot name with no `_`, so `lxmd`'s indexer
/// (`reference/LXMF/LXMF/LXMRouter.py:566-569`) never reads it as a
/// message.
pub const INDEX_FILE: &str = ".index";

/// The index file's first eight bytes; the last names the layout.
const INDEX_MAGIC: &[u8; 8] = b"LVPNIDX\x01";

/// Magic and record count.
const INDEX_HEADER_LEN: usize = 16;

/// Transient ID, receive time, stamp value, sequence, size, destination.
const INDEX_RECORD_LEN: usize = 32 + 8 + 1 + 8 + 4 + 16;

/// One record of the index file.
struct Recorded {
    received_at: u64,
    stamp_value: u8,
    sequence: u64,
    size: u32,
    destination_hash: [u8; 16],
}

/// The index file's records by transient ID, and what became of the file.
/// Anything short of a whole file with a matching checksum is no records.
fn load_index(
    path: &Path,
) -> (
    std::collections::HashMap<TransientId, Recorded>,
    IndexFileState,
) {
    let mut records = std::collections::HashMap::new();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (records, IndexFileState::Missing)
        }
        Err(_) => return (records, IndexFileState::Corrupt),
    };
    let Some((body, checksum)) = bytes.split_at_checked(bytes.len().saturating_sub(32)) else {
        return (records, IndexFileState::Corrupt);
    };
    let count = body
        .get(8..INDEX_HEADER_LEN)
        .and_then(|count| <[u8; 8]>::try_from(count).ok())
        .map(u64::from_le_bytes);
    let whole = body.len() >= INDEX_HEADER_LEN
        && &body[..8] == INDEX_MAGIC
        && count.is_some_and(|count| {
            (count as u128) * INDEX_RECORD_LEN as u128 == (body.len() - INDEX_HEADER_LEN) as u128
        })
        && checksum.len() == 32
        && Sha256::digest(body).as_slice() == checksum;
    if !whole {
        return (records, IndexFileState::Corrupt);
    }
    records.reserve(count.unwrap_or(0) as usize);
    for record in body[INDEX_HEADER_LEN..].chunks_exact(INDEX_RECORD_LEN) {
        let (id, rest) = record.split_at(32);
        let (received, rest) = rest.split_at(8);
        let (stamp, rest) = rest.split_at(1);
        let (sequence, rest) = rest.split_at(8);
        let (size, destination) = rest.split_at(4);
        let mut transient_id = [0u8; 32];
        transient_id.copy_from_slice(id);
        let mut destination_hash = [0u8; 16];
        destination_hash.copy_from_slice(destination);
        let mut u64_of = [0u8; 8];
        u64_of.copy_from_slice(received);
        let received_at = u64::from_le_bytes(u64_of);
        u64_of.copy_from_slice(sequence);
        let sequence = u64::from_le_bytes(u64_of);
        let mut u32_of = [0u8; 4];
        u32_of.copy_from_slice(size);
        records.insert(
            transient_id,
            Recorded {
                received_at,
                stamp_value: stamp[0],
                sequence,
                size: u32::from_le_bytes(u32_of),
                destination_hash,
            },
        );
    }
    (records, IndexFileState::Loaded)
}

fn micros(since: Instant) -> u64 {
    since.elapsed().as_micros() as u64
}

/// Prefix of the temp file an append writes before the rename.
const TEMP_PREFIX: &str = ".tmp-";

/// The empty file [`FilePropagationStore::mark_stamp_values_true`] leaves.
/// A dot name, so `lxmd`'s indexer (first `_`-component 64 hex characters,
/// `reference/LXMF/LXMF/LXMRouter.py:566-575`) never reads it as a message.
const STAMP_VALUES_MARKER: &str = ".stamp-values-true";

/// The temp file an append writes before renaming it to `path`:
/// `.tmp-<hex>_<secs>_<stamp>_<seq>`. The prefix, not a suffix, is the point.
/// `lxmd` indexes any name whose first `_`-component is 64 hex characters and
/// never looks at an extension (`enable_propagation`,
/// `reference/LXMF/LXMF/LXMRouter.py:566-575`), so a torn `<name>.tmp` would
/// be served by `lxmd` as a message; `.tmp-<hex>` is 69 characters and is not.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!("{TEMP_PREFIX}{name}"))
}

impl Drop for FilePropagationStore {
    /// Close the writer's queue and wait for it: what was handed over is
    /// written before the store is gone. Then the index file, so the next
    /// start reads no message (module docs, "The index file").
    fn drop(&mut self) {
        if let Some(writer) = self.writer.as_mut() {
            writer.jobs = None;
            if let Some(thread) = writer.thread.take() {
                let _ = thread.join();
            }
        }
        // What the writer confirmed joins the index, so the file names it.
        self.collect(false);
        self.save_index();
    }
}

impl PropagationStore for FilePropagationStore {
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
        // Free what the writer finished, so the queued bodies held in memory
        // stay bounded by what is actually outstanding.
        self.collect(false);
        let queued_previous = self
            .writer
            .as_ref()
            .and_then(|writer| writer.queued.get(transient_id))
            .map(|queued| queued.entry.meta);
        let (replaced, supersedes) = match queued_previous {
            // A queued append is always under the name this store writes.
            Some(meta) => (meta.size as u64, Some(self.canonical_path(&meta))),
            None => match self.index.get(transient_id) {
                Some(entry) => (entry.meta.size as u64, Some(self.path_of(&entry.meta))),
                None => (0, None),
            },
        };
        let next = self.used - replaced + body.len() as u64;
        if next > self.capacity {
            return Err(StorageError::Full);
        }
        let sequence = self.last_sequence + 1;
        let path = self.dir.join(Self::entry_name(
            transient_id,
            received_at,
            stamp_value,
            sequence,
        ));
        let mut destination_hash = [0u8; 16];
        destination_hash.copy_from_slice(&body[..16]);
        let entry = IndexEntry {
            meta: StoredMessage {
                transient_id: *transient_id,
                destination_hash,
                size: body.len() as u32,
                received_at,
                stamp_value,
                sequence,
            },
        };
        if let Some(writer) = self.writer.as_mut() {
            let Some(jobs) = writer.jobs.as_ref() else {
                return Err(StorageError::Io);
            };
            let ticket = writer.issued + 1;
            let body: Arc<[u8]> = Arc::from(body);
            let supersedes = supersedes.filter(|previous| previous != &path);
            jobs.send(Job::Write {
                ticket,
                path,
                body: Arc::clone(&body),
                supersedes,
            })
            .map_err(|_| StorageError::Io)?;
            writer.issued = ticket;
            writer.tickets.insert(ticket, *transient_id);
            writer.queued.insert(
                *transient_id,
                Queued {
                    ticket,
                    entry,
                    body,
                },
            );
            self.index_remove(transient_id);
            self.last_sequence = sequence;
            self.used = next;
            return Ok(());
        }
        write_durably(&temp_path(&path), &path, body, &*self.fsync)?;
        self.last_sequence = sequence;
        // Replacing an entry under a different timestamp leaves the old
        // file behind; remove it after the new one is durable.
        if let Some(previous) = supersedes.filter(|previous| previous != &path) {
            let _ = std::fs::remove_file(previous);
        }
        self.index_insert(*transient_id, entry);
        self.used = next;
        Ok(())
    }

    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        for entry in self.index.values() {
            visit(&entry.meta);
        }
        Ok(())
    }

    /// The sequence index from just above `cursor`: what the visitor reads,
    /// not the store (module docs, "The sequence index"). Ties come in
    /// transient ID order, as the default's stable sort of `for_each` leaves
    /// them.
    fn for_each_above(
        &self,
        cursor: u64,
        visit: &mut dyn FnMut(&StoredMessage) -> ControlFlow<()>,
    ) -> Result<(), StorageError> {
        let above = (Bound::Excluded((cursor, [0xff; 32])), Bound::Unbounded);
        for (_, transient_id) in self.by_sequence.range(above) {
            let Some(entry) = self.index.get(transient_id) else {
                continue;
            };
            if visit(&entry.meta).is_break() {
                break;
            }
        }
        Ok(())
    }

    /// The receive-time index below `before`, oldest first: what expired,
    /// not the store (module docs, "The sequence index"). Queued, unconfirmed appends are
    /// not visited, as `for_each` does not visit them.
    fn for_each_received_before(
        &self,
        before: u64,
        visit: &mut dyn FnMut(&StoredMessage) -> ControlFlow<()>,
    ) -> Result<(), StorageError> {
        for (_, transient_id) in self.by_received.range(..(before, [0u8; 32])) {
            let Some(entry) = self.index.get(transient_id) else {
                continue;
            };
            if visit(&entry.meta).is_break() {
                break;
            }
        }
        Ok(())
    }

    /// One index lookup; the default walks the store, once per wanted ID of
    /// an outbound sync batch (`plan_sync_batch`, leviculum#478).
    fn meta(&self, transient_id: &TransientId) -> Result<Option<StoredMessage>, StorageError> {
        Ok(self.index.get(transient_id).map(|entry| entry.meta))
    }

    fn count(&self) -> Result<usize, StorageError> {
        Ok(self.index.len())
    }

    /// A prefix sum of the counting index a cursor, `O(log n)`, when asked
    /// for the size it weighs; the walk otherwise (module docs, "The
    /// sequence index"). Queued, unconfirmed appends are not counted, as
    /// `for_each` does not visit them.
    fn count_above_cursors(
        &self,
        cursors: &[u64],
        largest_size: u64,
    ) -> Result<Vec<u64>, StorageError> {
        if largest_size != self.counts.largest_size() {
            return count_above_cursors_by_walk(self, cursors, largest_size);
        }
        Ok(cursors
            .iter()
            .map(|cursor| self.counts.above(*cursor))
            .collect())
    }

    fn count_sizes_up_to(&mut self, largest_size: u64) {
        if largest_size != self.counts.largest_size() {
            self.rebuild_counts(largest_size);
        }
    }

    fn newest_sequence(&self) -> Result<u64, StorageError> {
        Ok(self.by_sequence.last().map_or(0, |(sequence, _)| *sequence))
    }

    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        let queued = self
            .writer
            .as_mut()
            .and_then(|writer| writer.queued.remove(transient_id))
            .map(|queued| queued.entry.meta);
        let (meta, path) = match queued {
            Some(meta) => (meta, self.canonical_path(&meta)),
            None => {
                let Some(meta) = self.index.get(transient_id).map(|entry| entry.meta) else {
                    return Ok(false);
                };
                let path = self.path_of(&meta);
                self.index_remove(transient_id);
                (meta, path)
            }
        };
        self.used -= meta.size as u64;
        match self.remove_file(path) {
            Ok(()) => Ok(true),
            // Already gone is still gone.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(_) => Err(StorageError::Io),
        }
    }

    fn free_space(&self) -> u64 {
        self.capacity - self.used
    }

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        Ok(self.index.contains_key(transient_id)
            || self
                .writer
                .as_ref()
                .is_some_and(|writer| writer.queued.contains_key(transient_id)))
    }
}

impl FilePropagationStore {
    fn queued_body(&self, transient_id: &TransientId) -> Option<&[u8]> {
        self.writer
            .as_ref()
            .and_then(|writer| writer.queued.get(transient_id))
            .map(|queued| &*queued.body)
    }
}

impl PropagationBodyStore for FilePropagationStore {
    /// One `open` + `read` of the message's file. The directory index already
    /// holds the path, so the read costs no scan; a file that vanished under
    /// the index (an operator deleted it) is `Io`, not absence — the index is
    /// what this store believes, and it says the message is there.
    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(body) = self.queued_body(transient_id) {
            return Ok(Some(body.to_vec()));
        }
        let Some(entry) = self.index.get(transient_id) else {
            return Ok(None);
        };
        match std::fs::read(self.path_of(&entry.meta)) {
            Ok(body) => Ok(Some(body)),
            Err(_) => Err(StorageError::Io),
        }
    }

    /// Read only the requested range into the caller's buffer, without
    /// allocating a whole message for each streamed resource chunk.
    fn read_body_at(
        &self,
        transient_id: &TransientId,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, StorageError> {
        use std::io::{Read as _, Seek as _, SeekFrom};

        if let Some(body) = self.queued_body(transient_id) {
            return Ok(Some(copy_from(body, offset, buf)));
        }
        let Some(entry) = self.index.get(transient_id) else {
            return Ok(None);
        };
        let mut file =
            std::fs::File::open(self.path_of(&entry.meta)).map_err(|_| StorageError::Io)?;
        let len = file.metadata().map_err(|_| StorageError::Io)?.len();
        let offset = offset as u64;
        if offset >= len || buf.is_empty() {
            return Ok(Some(0));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| StorageError::Io)?;
        let remaining = usize::try_from(len - offset).unwrap_or(usize::MAX);
        let take = buf.len().min(remaining);
        file.read_exact(&mut buf[..take])
            .map_err(|_| StorageError::Io)?;
        Ok(Some(take))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What one `append` costs, on a filesystem the caller names.
    ///
    /// The number matters to a caller that appends under a lock, so it is
    /// measurable from the tree rather than quoted from a report
    /// (`docs/src/concepts/core-lock-budget.md`, "A durable store append is
    /// inside the budget too"). `#[ignore]`d: it is a measurement, and its
    /// result is a property of the disk under it, which is why it prints
    /// the directory it used and why the directory is the caller's to pick.
    ///
    /// ```text
    /// LEVICULUM_APPEND_BENCH_DIR=/var/tmp \
    ///   cargo test -p leviculum-std --lib append_cost -- --ignored --nocapture
    /// ```
    ///
    /// The default is the platform temp directory, and on Linux that is
    /// usually a tmpfs where `fsync` returns without touching a device —
    /// which reads 60x cheaper and answers nothing. Point it at the disk
    /// the store will live on.
    ///
    /// Coder host, ext4 on a virtio disk, 105 bodies of 288 bytes, debug
    /// profile, six runs: **127-189 ms in total, 1.1-1.5 ms per append,
    /// worst single append 6.4 ms**. The memory-store control on the same
    /// bodies is **124 µs for all 105**, so what the file store reports is
    /// the device and not the book-keeping. The same code against the
    /// platform tmpfs reads 5.9 ms in total, 27 µs median — a measurement
    /// of nothing, which is why the directory is an input.
    ///
    /// Nothing runs this: no tier does, and nothing in the release path
    /// re-measures it. Run it by hand when that doc section is re-measured,
    /// or when the write/fsync/rename/fsync path below changes — those are
    /// the two moments its figures can go stale without anyone noticing.
    #[test]
    #[ignore = "a measurement: it asserts nothing, and needs a real disk named"]
    fn append_cost() {
        use std::time::Instant;

        const MESSAGES: usize = 105;
        let base = std::env::var("LEVICULUM_APPEND_BENCH_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let dir = tempfile::tempdir_in(&base).expect("a writable bench directory");
        let mut store = FilePropagationStore::open(dir.path(), 50_000_000).expect("open");
        let body = body_for(0x11, 288);
        let mut each = Vec::with_capacity(MESSAGES);
        let all = Instant::now();
        for index in 0..MESSAGES {
            let mut transient_id = [0u8; 32];
            transient_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
            let one = Instant::now();
            store
                .append(&transient_id, 1_789_000_000 + index as u64, 13, &body)
                .expect("append");
            each.push(one.elapsed().as_micros() as u64);
        }
        let total = all.elapsed().as_micros() as u64;
        each.sort_unstable();
        println!(
            "APPEND dir={} n={MESSAGES} bytes={} total_us={total} min_us={} median_us={} max_us={}",
            dir.path().display(),
            body.len(),
            each[0],
            each[MESSAGES / 2],
            each[MESSAGES - 1],
        );

        // The control that makes the number an attribution: the same verb,
        // the same bodies, the same count, with nothing durable underneath.
        // What separates the two lines is the device.
        let mut memory = leviculum_lxmf::propagation_store::MemoryPropagationStore::new(50_000_000);
        let all = Instant::now();
        for index in 0..MESSAGES {
            let mut transient_id = [0u8; 32];
            transient_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
            memory
                .append(&transient_id, 1_789_000_000 + index as u64, 13, &body)
                .expect("append");
        }
        println!(
            "APPEND store=memory n={MESSAGES} bytes={} total_us={}",
            body.len(),
            all.elapsed().as_micros(),
        );
    }

    /// A store of `count` messages appended without a writer, ids `1..`.
    fn filled(dir: &Path, count: u8) -> FilePropagationStore {
        let mut store = FilePropagationStore::open(dir, 1_000_000).expect("open");
        for n in 1..=count {
            store
                .append(
                    &[n; 32],
                    1_789_000_000 + n as u64,
                    n % 20,
                    &body_for(n, 80 + n as usize),
                )
                .expect("append");
        }
        store
    }

    fn metas(store: &FilePropagationStore) -> Vec<StoredMessage> {
        let mut seen = Vec::new();
        store.for_each(&mut |meta| seen.push(*meta)).unwrap();
        seen.sort_by_key(|meta| meta.transient_id);
        seen
    }

    /// Order 1376: a start over a store that was closed reads no message
    /// file, and indexes exactly what the walk did.
    #[test]
    fn a_reopen_reads_no_file_the_index_file_names() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = filled(dir.path(), 12);
        let before = metas(&store);
        drop(store);

        let store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
        let opened = store.open_profile();
        assert_eq!(opened.index_file, IndexFileState::Loaded);
        assert_eq!((opened.from_index_file, opened.files_read), (12, 0));
        assert_eq!(
            opened.save_index_us, 0,
            "a current index file is not rewritten"
        );
        assert_eq!(metas(&store), before);
        assert_eq!(
            store.free_space(),
            1_000_000 - before.iter().map(|m| m.size as u64).sum::<u64>()
        );
        assert_eq!(store.read_body(&[3; 32]).unwrap(), Some(body_for(3, 83)));
    }

    /// A damaged index file is no index file: ignored, the walk rebuilds the
    /// index, and the next start finds a good one again.
    #[test]
    fn a_corrupt_index_file_is_ignored_and_rebuilt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = filled(dir.path(), 9);
        let before = metas(&store);
        drop(store);
        let path = dir.path().join(INDEX_FILE);
        let good = std::fs::read(&path).expect("the index file is written");

        // One flipped bit in a record, and the file cut short.
        let mut flipped = good.clone();
        flipped[INDEX_HEADER_LEN + 40] ^= 0x01;
        let torn = good[..good.len() - 7].to_vec();
        for damaged in [flipped, torn, b"LVPN".to_vec()] {
            std::fs::write(&path, &damaged).unwrap();
            let store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
            let opened = store.open_profile();
            assert_eq!(opened.index_file, IndexFileState::Corrupt);
            assert_eq!((opened.from_index_file, opened.files_read), (0, 9));
            assert_eq!(metas(&store), before);
            drop(store);
            assert_eq!(std::fs::read(&path).unwrap(), good, "the start rewrote it");
            let store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
            assert_eq!(store.open_profile().index_file, IndexFileState::Loaded);
            assert_eq!(store.open_profile().files_read, 0);
        }
    }

    /// The listing decides what is in the store, the index file only what a
    /// listed name holds: a file added behind its back is read, a removed one
    /// is gone, and a renamed one is read under its new name.
    #[test]
    fn the_listing_corrects_what_the_index_file_says() {
        let dir = tempfile::tempdir().expect("temp dir");
        drop(filled(dir.path(), 6));
        // lxmd's name for a seventh message, one removed, one re-priced.
        let added = format!("{}_1789000100.5_9", hex_encode(&[7u8; 32]));
        std::fs::write(dir.path().join(&added), body_for(7, 77)).unwrap();
        let name = |n: u8, stamp: u8| {
            dir.path().join(FilePropagationStore::entry_name(
                &[n; 32],
                1_789_000_000 + n as u64,
                stamp,
                n as u64,
            ))
        };
        std::fs::remove_file(name(2, 2)).unwrap();
        std::fs::rename(name(4, 4), name(4, 19)).unwrap();

        let store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
        let opened = store.open_profile().clone();
        assert_eq!(opened.index_file, IndexFileState::Loaded);
        assert_eq!((opened.from_index_file, opened.files_read), (4, 2));
        let seen = metas(&store);
        let ids: Vec<u8> = seen.iter().map(|meta| meta.transient_id[0]).collect();
        assert_eq!(ids, vec![1, 3, 4, 5, 6, 7]);
        assert_eq!(seen[2].stamp_value, 19);
        assert_eq!((seen[5].destination_hash, seen[5].size), ([7; 16], 77));
        assert_eq!(
            seen[5].sequence, 7,
            "the legacy name is sequenced above the rest"
        );
        drop(store);

        let store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
        assert_eq!(
            store.open_profile().files_read,
            0,
            "the start wrote what it read"
        );
        assert_eq!(metas(&store), seen);
    }

    /// What the writer confirms before the store is dropped is in the index
    /// file, and a crash, which writes none, costs the next start only what
    /// arrived since the last one was written.
    #[test]
    fn the_index_file_follows_the_writer_and_a_crash_costs_only_the_new() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = filled(dir.path(), 5).write_behind().expect("writer");
        store
            .append(&[6; 32], 1_789_000_006, 6, &body_for(6, 86))
            .unwrap();
        store.purge(&[1; 32]).unwrap();
        drop(store);

        let mut store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
        assert_eq!(store.open_profile().files_read, 0);
        assert_eq!(store.len(), 5);
        store
            .append(&[7; 32], 1_789_000_007, 7, &body_for(7, 87))
            .unwrap();
        let before = metas(&store);
        // The process dies: no drop, no index file.
        std::mem::forget(store);

        let store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
        let opened = store.open_profile();
        assert_eq!((opened.from_index_file, opened.files_read), (5, 1));
        assert_eq!(metas(&store), before);
    }

    /// What [`FilePropagationStore::open`] costs over a store of field size
    /// (order 1376: miauhaus took 130 s to start at 323 597 messages).
    /// `LEVICULUM_OPEN_BENCH_DIR` names the disk, `LEVICULUM_OPEN_BENCH_MESSAGES`
    /// the size (320 000). The fixture is written once, plain writes at the
    /// field's mean of 330 bytes over 2 000 destinations, and reused for
    /// a few weeks; drop the page cache between runs for the cold figure.
    #[test]
    #[ignore = "a measurement over a 320 000-file store on a disk named by env"]
    fn open_cost_at_field_scale() {
        use std::time::Instant;

        let messages: usize = std::env::var("LEVICULUM_OPEN_BENCH_MESSAGES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(320_000);
        let base = std::env::var("LEVICULUM_OPEN_BENCH_DIR")
            .map(std::path::PathBuf::from)
            .expect("LEVICULUM_OPEN_BENCH_DIR names a real disk");
        let dir = base.join(format!("store-{messages}"));
        // Received over the last days, so a daemon started on the fixture
        // (`lnpnd/tests/store_index_at_start.rs`) expires none of it.
        let complete = base.join(format!("store-{messages}.recent"));
        let first_received = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs()
            - 5 * 24 * 60 * 60;
        if !complete.exists() {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("fixture directory");
            let written = Instant::now();
            for index in 0..messages {
                let mut transient_id = [0u8; 32];
                transient_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
                transient_id[8..16].copy_from_slice(&(index as u64 ^ 0x5a5a).to_le_bytes());
                let mut body = vec![0xCD; 330];
                body[..2].copy_from_slice(&((index % 2000) as u16).to_be_bytes());
                let name = FilePropagationStore::entry_name(
                    &transient_id,
                    first_received + index as u64,
                    13,
                    index as u64 + 1,
                );
                std::fs::write(dir.join(name), &body).expect("fixture file");
            }
            std::fs::write(&complete, b"").expect("fixture marker");
            println!(
                "OPEN_FIXTURE n={messages} write_ms={}",
                written.elapsed().as_millis()
            );
        }
        for round in 0..2 {
            let opened = Instant::now();
            let store = FilePropagationStore::open(&dir, u64::MAX).expect("open");
            let open_ms = opened.elapsed().as_millis();
            assert_eq!(store.len(), messages);
            println!(
                "OPEN round={round} n={messages} open_ms={open_ms} {:?}",
                store.open_profile()
            );
        }
    }

    fn body_for(destination: u8, len: usize) -> Vec<u8> {
        let mut body = vec![destination; len];
        body[16..].fill(0xCD);
        body
    }

    #[test]
    fn messages_survive_a_reopen_with_their_directory_data() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        store.append(&[1; 32], 1234, 5, &body_for(7, 100)).unwrap();
        store.append(&[2; 32], 2345, 0, &body_for(9, 200)).unwrap();
        drop(store);

        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.len(), 2);
        let mut seen = Vec::new();
        store.for_each(&mut |meta| seen.push(*meta)).unwrap();
        seen.sort_by_key(|meta| meta.transient_id);
        assert_eq!(seen[0].destination_hash, [7; 16]);
        assert_eq!(seen[0].received_at, 1234);
        assert_eq!(seen[0].stamp_value, 5);
        assert_eq!(seen[0].size, 100);
        assert_eq!(seen[1].destination_hash, [9; 16]);
        assert_eq!(store.read_body(&[1; 32]).unwrap(), Some(body_for(7, 100)));
        assert_eq!(store.free_space(), 4096 - 300);
    }

    #[test]
    fn body_slices_read_only_the_requested_range_and_preserve_eof_semantics() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        let body = body_for(7, 100);
        store.append(&[1; 32], 1234, 0, &body).unwrap();

        let mut buf = [0xA5; 32];
        assert_eq!(
            store.read_body_at(&[1; 32], 12, &mut buf).unwrap(),
            Some(32)
        );
        assert_eq!(&buf, &body[12..44]);

        buf.fill(0xA5);
        assert_eq!(store.read_body_at(&[1; 32], 93, &mut buf).unwrap(), Some(7));
        assert_eq!(&buf[..7], &body[93..]);
        assert_eq!(&buf[7..], &[0xA5; 25]);
        assert_eq!(store.read_body_at(&[1; 32], 5, &mut []).unwrap(), Some(0));
        assert_eq!(
            store.read_body_at(&[1; 32], 100, &mut buf).unwrap(),
            Some(0)
        );
        assert_eq!(
            store.read_body_at(&[1; 32], usize::MAX, &mut buf).unwrap(),
            Some(0)
        );
        assert_eq!(store.read_body_at(&[2; 32], 0, &mut buf).unwrap(), None);
    }

    #[test]
    fn body_slice_of_a_missing_indexed_file_reports_io() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        store.append(&[1; 32], 1234, 0, &body_for(7, 100)).unwrap();
        let meta = store.index.get(&[1; 32]).unwrap().meta;
        std::fs::remove_file(store.path_of(&meta)).unwrap();

        assert_eq!(store.read_body(&[1; 32]), Err(StorageError::Io));
        assert_eq!(
            store.read_body_at(&[1; 32], 0, &mut [0; 32]),
            Err(StorageError::Io)
        );
    }

    /// `lxmd`'s message-store indexer, restated: a file is indexed iff its
    /// name split on `_` has at least three components, the second parses
    /// as a float above zero, the first is 64 characters of hex (the
    /// transient id, `RNS.Identity.HASHLENGTH//8*2`) and the third parses
    /// as an int (`enable_propagation`,
    /// `reference/LXMF/LXMF/LXMRouter.py:566-575`). Nothing else of the
    /// name is looked at, neither a fourth component nor an extension.
    fn lxmd_would_index(name: &str) -> bool {
        let components: Vec<&str> = name.split('_').collect();
        components.len() >= 3
            && components[1]
                .parse::<f64>()
                .is_ok_and(|received| received > 0.0)
            && components[0].len() == 64
            && hex_decode(components[0]).is_some()
            && components[2].parse::<i64>().is_ok()
    }

    /// A directory handed back from `lnpnd` to `lxmd` after a power cut
    /// must not have `lxmd` serve the torn temp file as a message.
    #[test]
    fn lxmd_does_not_index_a_torn_temp_file() {
        let name = FilePropagationStore::entry_name(&[2; 32], 1_789_656_258, 13, 4);
        assert!(
            lxmd_would_index(&name),
            "control: a stored message is indexed"
        );
        let torn = temp_path(Path::new(&name));
        let torn = torn.to_str().expect("utf-8 name");
        assert!(!lxmd_would_index(torn), "lxmd would index the torn {torn}");
    }

    /// The acceptance case from the batch instruction: a power cut during
    /// an upload leaves a store that reopens with every completed message
    /// and no partial one. A cut before the rename is a stray `.tmp-` file;
    /// a cut mid-`write(2)` before the rename is a shorter `.tmp-` file.
    /// Neither may surface as a message or poison the scan.
    #[test]
    fn a_torn_append_is_invisible_after_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        store
            .append(&[1; 32], 1234, 0, &body_for(7, 100))
            .expect("complete message");

        // The moment the power died: a temp file exists, fully or partially
        // written, and was never renamed.
        let torn = temp_path(
            &dir.path()
                .join(FilePropagationStore::entry_name(&[2; 32], 999, 0, 2)),
        );
        std::fs::write(&torn, body_for(9, 60)).expect("torn temp file");
        let torn_short = temp_path(
            &dir.path()
                .join(FilePropagationStore::entry_name(&[3; 32], 999, 0, 3)),
        );
        std::fs::write(&torn_short, [9u8; 7]).expect("short torn temp file");

        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.len(), 1, "only the completed message may reopen");
        assert!(store.contains(&[1; 32]).unwrap());
        assert!(!store.contains(&[2; 32]).unwrap());
        assert!(!store.contains(&[3; 32]).unwrap());
    }

    /// LV-1000 (order 1515): the index held each message's full path, so
    /// a store under a longer directory cost more RAM a message, and the
    /// path was most of an entry (630 B a message at 304 000 messages in
    /// the growth soak, `lnpnd/tests/pn_growth_soak.rs`). A path is derived
    /// from the entry now, so the index costs the same under any directory.
    #[test]
    fn the_index_holds_no_path_per_message() {
        let short = tempfile::tempdir().expect("temp dir");
        let long_base = tempfile::tempdir().expect("temp dir");
        let long = long_base.path().join("a".repeat(200)).join("b".repeat(200));
        std::fs::create_dir_all(&long).expect("long dir");
        let census = |dir: &Path| {
            let mut store = FilePropagationStore::open(dir, 1_000_000).expect("open");
            for n in 1..=40u8 {
                store
                    .append(&[n; 32], 1_789_000_000, 13, &body_for(n, 100))
                    .unwrap();
            }
            store.heap_census()
        };
        let (short, long) = (census(short.path()), census(&long));
        assert_eq!(short.messages, 40);
        assert_eq!(
            short.index_bytes, long.index_bytes,
            "the index must not cost more under a longer directory"
        );
    }

    /// A legacy name the open cannot rename stays the file's name, and the
    /// store still finds the file under it: the one case where a path is
    /// not derived from the entry.
    #[cfg(unix)]
    #[test]
    fn a_legacy_name_the_open_cannot_rename_is_still_served_and_purged() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let legacy = dir.path().join(format!("{}_50_0", hex_encode(&[3u8; 32])));
        std::fs::write(&legacy, body_for(9, 100)).unwrap();
        let read_only = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(dir.path(), read_only).unwrap();
        let store = FilePropagationStore::open(dir.path(), 8192).expect("open");
        // Root renames anyway; then there is no odd name to test.
        let renamed = !legacy.exists();
        assert_eq!(
            store.odd_names.contains_key(&[3; 32]),
            !renamed,
            "a failed rename keeps the old name"
        );
        assert_eq!(
            store.read_body(&[3; 32]).unwrap(),
            Some(body_for(9, 100)),
            "the file is served under the name it has"
        );
        drop(store);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut store = FilePropagationStore::open(dir.path(), 8192).expect("reopen");
        assert!(store.odd_names.is_empty(), "the next open renames it");
        assert_eq!(store.read_body(&[3; 32]).unwrap(), Some(body_for(9, 100)));
        assert!(store.purge(&[3; 32]).unwrap());
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with(&hex_encode(&[3u8; 32])))
            .collect();
        assert!(left.is_empty(), "the purge removed the file: {left:?}");
    }

    #[test]
    fn capacity_is_enforced_and_purge_frees_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 250).expect("open");
        store.append(&[1; 32], 0, 0, &body_for(7, 200)).unwrap();
        assert_eq!(
            store.append(&[2; 32], 0, 0, &body_for(7, 100)),
            Err(StorageError::Full)
        );
        assert!(store.purge(&[1; 32]).unwrap());
        assert!(!store.purge(&[1; 32]).unwrap());
        store.append(&[2; 32], 0, 0, &body_for(7, 100)).unwrap();
        assert_eq!(store.free_space(), 150);

        // The purged file is really gone from disk, not only the index.
        let store = FilePropagationStore::open(dir.path(), 250).expect("reopen");
        assert_eq!(store.len(), 1);
        assert!(store.contains(&[2; 32]).unwrap());
    }

    /// Sequences are the cursor domain (§5): they must survive a reopen,
    /// keep growing from where they were, and a part-1 store without them
    /// is migrated once, oldest first, then never renumbered again.
    #[test]
    fn sequences_persist_grow_and_migrate_legacy_names_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 8192).expect("open");
        store.append(&[1; 32], 100, 0, &body_for(7, 100)).unwrap();
        store.append(&[2; 32], 200, 0, &body_for(7, 100)).unwrap();
        let seq_of = |store: &FilePropagationStore, id: &TransientId| {
            let mut found = 0;
            store
                .for_each(&mut |meta| {
                    if meta.transient_id == *id {
                        found = meta.sequence;
                    }
                })
                .unwrap();
            found
        };
        assert_eq!(seq_of(&store, &[1; 32]), 1);
        assert_eq!(seq_of(&store, &[2; 32]), 2);
        drop(store);

        // A part-1 file: three components, no sequence. Migrated above
        // everything known and renamed on disk.
        std::fs::write(
            dir.path().join(format!("{}_50_0", hex_encode(&[3u8; 32]))),
            body_for(9, 100),
        )
        .unwrap();
        let mut store = FilePropagationStore::open(dir.path(), 8192).expect("reopen");
        assert_eq!(seq_of(&store, &[1; 32]), 1);
        assert_eq!(seq_of(&store, &[2; 32]), 2);
        assert_eq!(seq_of(&store, &[3; 32]), 3);
        // New appends continue above.
        store.append(&[4; 32], 300, 0, &body_for(7, 100)).unwrap();
        assert_eq!(seq_of(&store, &[4; 32]), 4);
        drop(store);

        // The migration was persisted: a third open sees the same numbers.
        let store = FilePropagationStore::open(dir.path(), 8192).expect("reopen 2");
        assert_eq!(seq_of(&store, &[3; 32]), 3);
        assert_eq!(store.newest_sequence().unwrap(), 4);
    }

    /// The names `lxmd` writes (`LXMRouter.py:2512-2514`): a float receive
    /// time, and a stamp-value component only when the value is positive.
    /// Both shapes are indexed and renamed into ours on the first open, so
    /// a second open finds only four-component names.
    #[test]
    fn reference_names_of_both_shapes_are_indexed_and_renamed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let two = format!("{}_1789656258.9376912", hex_encode(&[5u8; 32]));
        let three = format!("{}_1789656259.25_7", hex_encode(&[6u8; 32]));
        std::fs::write(dir.path().join(&two), body_for(9, 100)).unwrap();
        std::fs::write(dir.path().join(&three), body_for(8, 120)).unwrap();

        let store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        assert_eq!(store.len(), 2);
        let mut seen = Vec::new();
        store.for_each(&mut |meta| seen.push(*meta)).unwrap();
        seen.sort_by_key(|meta| meta.transient_id);
        assert_eq!(seen[0].transient_id, [5; 32]);
        assert_eq!(seen[0].received_at, 1_789_656_258);
        assert_eq!(seen[0].stamp_value, 0, "an absent stamp value is 0");
        assert_eq!(seen[0].destination_hash, [9; 16]);
        assert_eq!(seen[1].transient_id, [6; 32]);
        assert_eq!(seen[1].received_at, 1_789_656_259);
        assert_eq!(seen[1].stamp_value, 7);
        // Receive-time order decides the fresh sequences.
        assert_eq!(seen[0].sequence, 1);
        assert_eq!(seen[1].sequence, 2);
        assert_eq!(store.read_body(&[5; 32]).unwrap(), Some(body_for(9, 100)));
        drop(store);

        // The index file beside them is ours and no message (`INDEX_FILE`).
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != INDEX_FILE)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                FilePropagationStore::entry_name(&[5; 32], 1_789_656_258, 0, 1),
                FilePropagationStore::entry_name(&[6; 32], 1_789_656_259, 7, 2),
            ],
            "the reference's names are gone, ours are in their place"
        );

        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.len(), 2);
        assert_eq!(store.newest_sequence().unwrap(), 2);
    }

    /// A name that is neither ours nor the reference's still goes through
    /// `note_unreadable_entry` and stays where it is.
    #[test]
    fn a_name_of_neither_shape_is_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let hex = hex_encode(&[5u8; 32]);
        let odd = [
            hex.clone(),
            format!("{hex}_notatime"),
            format!("{hex}_12.5_x"),
            format!("{hex}_12_1_2_3"),
        ];
        for name in &odd {
            std::fs::write(dir.path().join(name), body_for(9, 100)).unwrap();
        }
        let store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        assert!(store.is_empty());
        for name in &odd {
            assert!(dir.path().join(name).exists(), "{name} must not be moved");
        }
    }

    #[test]
    fn a_foreign_filename_is_skipped_rather_than_fatal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        store.append(&[1; 32], 1, 1, &body_for(7, 100)).unwrap();
        std::fs::write(dir.path().join("not-a-message"), b"junk").unwrap();
        std::fs::write(dir.path().join("deadbeef_1_1"), b"short-id").unwrap();

        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.len(), 1);
        assert!(store.contains(&[1; 32]).unwrap());
    }

    /// A re-priced message keeps its receive time and its sequence across a
    /// reopen, so a peer cursor below it still reaches it (leviculum#442
    /// D6), and the marker is neither a message nor something `lxmd` would
    /// index.
    #[test]
    fn a_repriced_value_survives_a_reopen_in_place() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 4096).expect("open");
        store.append(&[1; 32], 1234, 0, &body_for(7, 100)).unwrap();
        store.append(&[2; 32], 2345, 0, &body_for(9, 100)).unwrap();
        let before = store.meta(&[1; 32]).unwrap().expect("stored");
        assert!(!store.stamp_values_true());

        assert!(store.set_stamp_value(&[1; 32], 14).unwrap());
        assert!(!store.set_stamp_value(&[3; 32], 14).unwrap());
        store.mark_stamp_values_true().unwrap();
        assert!(!lxmd_would_index(STAMP_VALUES_MARKER));
        drop(store);

        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.len(), 2);
        assert!(store.stamp_values_true());
        let after = store.meta(&[1; 32]).unwrap().expect("still stored");
        assert_eq!(after.stamp_value, 14);
        assert_eq!(after.sequence, before.sequence);
        assert_eq!(after.received_at, 1234);
        assert_eq!(store.read_body(&[1; 32]).unwrap(), Some(body_for(7, 100)));
        assert_eq!(
            store.meta(&[2; 32]).unwrap().map(|meta| meta.stamp_value),
            Some(0)
        );
    }

    /// Holds every `fsync` until the test drops the sender.
    struct GatedFsync(std::sync::Mutex<std::sync::mpsc::Receiver<()>>);

    impl Fsync for GatedFsync {
        fn sync(&self, handle: &std::fs::File) -> std::io::Result<()> {
            let gate = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = gate.recv();
            handle.sync_all()
        }
    }

    fn gated(dir: &Path) -> (FilePropagationStore, std::sync::mpsc::Sender<()>) {
        let (release, gate) = std::sync::mpsc::channel();
        let store = FilePropagationStore::open_with(
            dir,
            4096,
            Arc::new(GatedFsync(std::sync::Mutex::new(gate))),
        )
        .expect("open")
        .write_behind()
        .expect("writer");
        (store, release)
    }

    fn files_in(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .expect("list")
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .count()
    }

    /// Order 1081, the board's visibility rule on the host: a queued append
    /// is a duplicate and can be read, but is not listed, so no offer, list
    /// or peer cursor sees it before it is durable.
    #[test]
    fn a_queued_append_is_readable_but_not_listed_until_the_writer_confirms_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (mut store, release) = gated(dir.path());
        store.append(&[1; 32], 1234, 5, &body_for(7, 100)).unwrap();

        assert!(store.contains(&[1; 32]).unwrap());
        assert_eq!(store.read_body(&[1; 32]).unwrap(), Some(body_for(7, 100)));
        let mut slice = [0u8; 4];
        assert_eq!(
            store.read_body_at(&[1; 32], 98, &mut slice).unwrap(),
            Some(2)
        );
        assert_eq!(store.count().unwrap(), 0, "a queued append was listed");
        assert_eq!(store.free_space(), 4096 - 100);
        assert_eq!(store.writes_issued(), 1);
        assert!(store.writes_outstanding());
        assert_eq!(store.settle_writes().through, 0);

        drop(release);
        let settled = store.flush_writes();
        assert_eq!(
            settled,
            SettledWrites {
                through: 1,
                failed: Vec::new()
            }
        );
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(files_in(dir.path()), 1);
        drop(store);
        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.read_body(&[1; 32]).unwrap(), Some(body_for(7, 100)));
    }

    /// Order 1081: a purge of a queued message goes through the same queue,
    /// so its file is removed after the write placed it, not before.
    #[test]
    fn a_purge_of_a_queued_append_runs_after_its_write() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (mut store, release) = gated(dir.path());
        store.append(&[1; 32], 1234, 5, &body_for(7, 100)).unwrap();
        assert!(store.purge(&[1; 32]).unwrap());
        assert!(!store.contains(&[1; 32]).unwrap());
        assert_eq!(store.free_space(), 4096);
        drop(release);
        assert!(store.flush_writes().failed.is_empty());
        drop(store);
        assert_eq!(
            files_in(dir.path()),
            0,
            "the purged message was left on disk"
        );
    }

    /// Order 1081: a write the device refused leaves the store and is
    /// reported, so the caller can forget the id and leave the proof unsent.
    #[test]
    fn a_refused_write_leaves_the_store_and_is_reported() {
        struct Refusing;
        impl Fsync for Refusing {
            fn sync(&self, _handle: &std::fs::File) -> std::io::Result<()> {
                Err(std::io::Error::other("refused"))
            }
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open_with(dir.path(), 4096, Arc::new(Refusing))
            .expect("open")
            .write_behind()
            .expect("writer");
        store.append(&[1; 32], 1234, 5, &body_for(7, 100)).unwrap();
        let settled = store.flush_writes();
        assert_eq!(
            settled,
            SettledWrites {
                through: 1,
                failed: vec![[1; 32]]
            }
        );
        assert!(!store.contains(&[1; 32]).unwrap());
        assert_eq!(store.free_space(), 4096);
        assert_eq!(files_in(dir.path()), 0);
    }

    /// The answers the sequence index gives, recomputed by the trait's own
    /// defaults from `for_each`: what the store said before leviculum#478.
    fn by_walk(store: &FilePropagationStore, cursor: u64) -> (Vec<StoredMessage>, u64, usize) {
        let mut above = Vec::new();
        let mut newest = 0;
        let mut count = 0;
        store
            .for_each(&mut |meta| {
                count += 1;
                newest = newest.max(meta.sequence);
                if meta.sequence > cursor {
                    above.push(*meta);
                }
            })
            .unwrap();
        above.sort_by_key(|meta| meta.sequence);
        (above, newest, count)
    }

    fn by_index(store: &FilePropagationStore, cursor: u64) -> (Vec<StoredMessage>, u64, usize) {
        let mut above = Vec::new();
        store
            .for_each_above(cursor, &mut |meta| {
                above.push(*meta);
                ControlFlow::Continue(())
            })
            .unwrap();
        (
            above,
            store.newest_sequence().unwrap(),
            store.count().unwrap(),
        )
    }

    fn assert_index_matches_walk(store: &FilePropagationStore, ids: &[TransientId], step: usize) {
        let (_, newest, _) = by_walk(store, 0);
        let cursors = [
            0,
            newest / 3,
            newest / 2,
            newest.saturating_sub(1),
            newest,
            newest + 1,
        ];
        for cursor in cursors {
            assert_eq!(
                by_index(store, cursor),
                by_walk(store, cursor),
                "step {step}, cursor {cursor}"
            );
        }
        for largest in [store.counts.largest_size(), 120, u64::MAX] {
            let counted = store.count_above_cursors(&cursors, largest).unwrap();
            let walked: Vec<u64> = cursors
                .iter()
                .map(|cursor| {
                    by_walk(store, *cursor)
                        .0
                        .iter()
                        .filter(|meta| u64::from(meta.size) <= largest)
                        .count() as u64
                })
                .collect();
            assert_eq!(counted, walked, "step {step}, sizes up to {largest}");
        }
        for id in ids {
            let mut walked = None;
            store
                .for_each(&mut |meta| {
                    if meta.transient_id == *id {
                        walked = Some(*meta);
                    }
                })
                .unwrap();
            assert_eq!(store.meta(id).unwrap(), walked, "step {step}");
        }
        let mut received: Vec<u64> = Vec::new();
        store
            .for_each(&mut |meta| received.push(meta.received_at))
            .unwrap();
        received.sort_unstable();
        let middle = received.get(received.len() / 2).copied().unwrap_or(0);
        for before in [0, middle, middle + 1, u64::MAX] {
            let mut walked = Vec::new();
            store
                .for_each(&mut |meta| {
                    if meta.received_at < before {
                        walked.push(*meta);
                    }
                })
                .unwrap();
            let mut indexed = Vec::new();
            store
                .for_each_received_before(before, &mut |meta| {
                    indexed.push(*meta);
                    ControlFlow::Continue(())
                })
                .unwrap();
            walked.sort_by_key(|meta| meta.transient_id);
            indexed.sort_by_key(|meta| meta.transient_id);
            assert_eq!(indexed, walked, "step {step}, received before {before}");
        }
    }

    /// leviculum#478: the sequence, receive-time and counting indexes
    /// answer `for_each_above`, `for_each_received_before`,
    /// `count_above_cursors`, `newest_sequence`, `count` and `meta` exactly
    /// as the whole-store walk did, through appends, re-appends of a stored
    /// ID, purges, queued writes the writer has not confirmed, and a reopen;
    /// and an ordered walk that breaks visits no further.
    #[test]
    fn the_sequence_index_answers_as_the_store_walk_does() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FilePropagationStore::open(dir.path(), 1_000_000)
            .expect("open")
            .write_behind()
            .expect("writer");
        store.count_sizes_up_to(120);
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let ids: Vec<TransientId> = (0..24u8).map(|n| [n; 32]).collect();
        for step in 0..400 {
            let id = ids[(next() % ids.len() as u64) as usize];
            match next() % 4 {
                0 => {
                    store.purge(&id).unwrap();
                }
                3 => {
                    store.flush_writes();
                }
                _ => {
                    let len = 80 + (next() % 80) as usize;
                    store
                        .append(&id, 1_789_000_000 + step as u64, 13, &body_for(3, len))
                        .unwrap();
                }
            }
            assert_index_matches_walk(&store, &ids, step);
        }
        store.flush_writes();
        assert_index_matches_walk(&store, &ids, 400);

        let (above, _, count) = by_walk(&store, 0);
        assert!(count > 3, "the walk left {count} entries to break over");
        let mut visited = Vec::new();
        store
            .for_each_above(0, &mut |meta| {
                visited.push(meta.sequence);
                if visited.len() == 3 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .unwrap();
        let first: Vec<u64> = above.iter().take(3).map(|meta| meta.sequence).collect();
        assert_eq!(visited, first, "a break ends the walk");

        drop(store);
        let mut store = FilePropagationStore::open(dir.path(), 1_000_000).expect("reopen");
        assert_index_matches_walk(&store, &ids, 401);
        store.count_sizes_up_to(120);
        assert_index_matches_walk(&store, &ids, 402);
    }
}
