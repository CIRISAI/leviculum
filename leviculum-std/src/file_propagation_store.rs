//! File-backed [`PropagationStore`]: one file per stored message
//! (Codeberg #384, part 1).
//!
//! The layout is the reference's message store, one directory of files named
//! `<transient_id_hex>_<received_secs>_<stamp_value>_<sequence>` whose
//! content is `lxmf_data || stamp` (`lxmf_propagation`,
//! `reference/LXMF/LXMF/LXMRouter.py:2512-2515`; re-indexed at startup by
//! `enable_propagation`, `:565-592`). Three local differences: the timestamp
//! is written as integer seconds rather than a float (a float IS read back,
//! which is what a store inherited from `lxmd` carries), the stamp-value
//! component is
//! always present (the reference omits it at value 0 and then *skips such
//! files entirely* when re-indexing, `:568` requires three components — a
//! quirk, not a behaviour worth importing), and a fourth component carries
//! the append-order sequence the per-peer sync cursors index
//! (`docs/src/concepts/propagation-node-on-a-board.md` §5) so cursors stay
//! valid across a reopen. A three-component name (a part-1 store, or a
//! directory copied from a reference node) is still read; such entries are
//! assigned fresh sequences above every known one, in receive-time order,
//! which at worst re-offers them once. The directory is not a wire format;
//! nothing reads it but us.
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
//! earlier point leaves only a `.tmp` file the reopen scan ignores. The
//! generic `atomic_write` helper in `crate::storage` deliberately skips the
//! fsyncs (announce caches and ratchet files tolerate losing the last
//! seconds); this store must not.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use leviculum_lxmf::propagation_store::{PropagationStore, StoredMessage, MIN_BODY_LEN};
use leviculum_lxmf::storage::StorageError;
use leviculum_lxmf::TransientId;

use crate::storage::{hex_decode, hex_encode, note_unreadable_entry};

/// A directory of propagation-store messages with a hard byte capacity.
pub struct FilePropagationStore {
    dir: PathBuf,
    capacity: u64,
    used: u64,
    index: BTreeMap<TransientId, IndexEntry>,
    /// Last assigned append sequence; recovered as the maximum on disk.
    last_sequence: u64,
}

#[derive(Debug, Clone)]
struct IndexEntry {
    meta: StoredMessage,
    path: PathBuf,
}

impl FilePropagationStore {
    /// Open the store, creating the directory if needed, and rebuild the
    /// index from the files on disk — the same startup scan the reference
    /// performs (`enable_propagation`,
    /// `reference/LXMF/LXMF/LXMRouter.py:565-592`). Files whose names or
    /// contents do not parse are noted and skipped, never deleted.
    pub fn open<P: AsRef<Path>>(dir: P, capacity: u64) -> Result<Self, StorageError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|_| StorageError::Io)?;
        let mut index = BTreeMap::new();
        let mut used = 0u64;
        let entries = std::fs::read_dir(&dir).map_err(|_| StorageError::Io)?;
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
            };
            // An interrupted append's temp file: not a message, not noise
            // worth a warning either.
            if name.ends_with(".tmp") {
                continue;
            }
            let Some((transient_id, received_at, stamp_value, sequence)) = parse_name(&name) else {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
            };
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
            };
            let size = metadata.len();
            if size < MIN_BODY_LEN as u64 || size > u32::MAX as u64 {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
            }
            // The destination hash lives in the first 16 bytes of the body,
            // as the reference reads it back (`LXMRouter.py:578`).
            let Some(destination_hash) = read_destination(&path) else {
                note_unreadable_entry(&dir, &entry.file_name());
                continue;
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
                        // Legacy three-component names get a real sequence
                        // after the scan, below.
                        sequence: sequence.unwrap_or(0),
                    },
                    path,
                },
            );
        }
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
                if std::fs::rename(&entry.path, &renamed).is_ok() {
                    entry.path = renamed;
                }
            }
        }
        Ok(Self {
            dir,
            capacity,
            used,
            index,
            last_sequence,
        })
    }

    /// The directory the messages live in.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Number of stored messages.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
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
    let stamp_value: u8 = parts.next()?.parse().ok()?;
    let sequence = match parts.next() {
        // Part-1 layout: no sequence component yet.
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

/// Write `body` durably: temp file, fsync, rename, fsync the directory.
fn durable_write(dir: &Path, path: &Path, body: &[u8]) -> Result<(), StorageError> {
    let temp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temp).map_err(|_| StorageError::Io)?;
    file.write_all(body).map_err(|_| StorageError::Io)?;
    file.sync_all().map_err(|_| StorageError::Io)?;
    drop(file);
    std::fs::rename(&temp, path).map_err(|_| StorageError::Io)?;
    // The rename itself must survive the power cut, not just the bytes.
    if let Ok(dir_handle) = std::fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
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
        let replaced = self
            .index
            .get(transient_id)
            .map_or(0, |entry| entry.meta.size as u64);
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
        durable_write(&self.dir, &path, body)?;
        self.last_sequence = sequence;
        // Replacing an entry under a different timestamp leaves the old
        // file behind; remove it after the new one is durable.
        if let Some(previous) = self.index.get(transient_id) {
            if previous.path != path {
                let _ = std::fs::remove_file(&previous.path);
            }
        }
        let mut destination_hash = [0u8; 16];
        destination_hash.copy_from_slice(&body[..16]);
        self.index.insert(
            *transient_id,
            IndexEntry {
                meta: StoredMessage {
                    transient_id: *transient_id,
                    destination_hash,
                    size: body.len() as u32,
                    received_at,
                    stamp_value,
                    sequence,
                },
                path,
            },
        );
        self.used = next;
        Ok(())
    }

    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        for entry in self.index.values() {
            visit(&entry.meta);
        }
        Ok(())
    }

    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(entry) = self.index.get(transient_id) else {
            return Ok(None);
        };
        match std::fs::read(&entry.path) {
            Ok(body) => Ok(Some(body)),
            Err(_) => Err(StorageError::Io),
        }
    }

    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        let Some(entry) = self.index.remove(transient_id) else {
            return Ok(false);
        };
        self.used -= entry.meta.size as u64;
        match std::fs::remove_file(&entry.path) {
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
        Ok(self.index.contains_key(transient_id))
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

    /// The acceptance case from the batch instruction: a power cut during
    /// an upload leaves a store that reopens with every completed message
    /// and no partial one. A cut before the rename is a stray `.tmp` file;
    /// a cut mid-`write(2)` before the rename is a shorter `.tmp` file.
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
        let torn = dir
            .path()
            .join(FilePropagationStore::entry_name(&[2; 32], 999, 0, 2))
            .with_extension("tmp");
        std::fs::write(&torn, body_for(9, 60)).expect("torn temp file");
        let torn_short = dir
            .path()
            .join(FilePropagationStore::entry_name(&[3; 32], 999, 0, 3))
            .with_extension("tmp");
        std::fs::write(&torn_short, [9u8; 7]).expect("short torn temp file");

        let store = FilePropagationStore::open(dir.path(), 4096).expect("reopen");
        assert_eq!(store.len(), 1, "only the completed message may reopen");
        assert!(store.contains(&[1; 32]).unwrap());
        assert!(!store.contains(&[2; 32]).unwrap());
        assert!(!store.contains(&[3; 32]).unwrap());
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
}
