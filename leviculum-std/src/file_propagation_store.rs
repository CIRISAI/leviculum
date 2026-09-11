//! File-backed [`PropagationStore`]: one file per stored message
//! (Codeberg #384, part 1).
//!
//! The layout is the reference's message store, one directory of files named
//! `<transient_id_hex>_<received_secs>_<stamp_value>` whose content is
//! `lxmf_data || stamp` (`lxmf_propagation`,
//! `reference/LXMF/LXMF/LXMRouter.py:2512-2515`; re-indexed at startup by
//! `enable_propagation`, `:565-592`). Two local differences: the timestamp
//! is integer seconds rather than a float, and the stamp-value component is
//! always present (the reference omits it at value 0 and then *skips such
//! files entirely* when re-indexing, `:568` requires three components — a
//! quirk, not a behaviour worth importing). The directory is not a wire
//! format; nothing reads it but us.
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
//! generic [`atomic_write`](crate::storage) helper deliberately skips the
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
            let Some((transient_id, received_at, stamp_value)) = parse_name(&name) else {
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
                    },
                    path,
                },
            );
        }
        Ok(Self {
            dir,
            capacity,
            used,
            index,
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

    fn entry_name(transient_id: &TransientId, received_at: u64, stamp_value: u8) -> String {
        format!(
            "{}_{received_at}_{stamp_value}",
            hex_encode(transient_id.as_slice())
        )
    }
}

fn parse_name(name: &str) -> Option<(TransientId, u64, u8)> {
    let mut parts = name.split('_');
    let id: TransientId = hex_decode(parts.next()?)?.try_into().ok()?;
    let received_at: u64 = parts.next()?.parse().ok()?;
    let stamp_value: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((id, received_at, stamp_value))
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
        let path = self
            .dir
            .join(Self::entry_name(transient_id, received_at, stamp_value));
        durable_write(&self.dir, &path, body)?;
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
            .join(FilePropagationStore::entry_name(&[2; 32], 999, 0))
            .with_extension("tmp");
        std::fs::write(&torn, body_for(9, 60)).expect("torn temp file");
        let torn_short = dir
            .path()
            .join(FilePropagationStore::entry_name(&[3; 32], 999, 0))
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
