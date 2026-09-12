//! File-backed [`PeerStore`] (Codeberg #384, part 2): the host half of the
//! peer-table persistence the concept paper's §5 assigns — "the host
//! persists its table in a file, as the reference does"
//! (`LXMRouter.exit_handler` writes its peers with `LXMPeer.to_bytes`,
//! `reference/LXMF/LXMF/LXMRouter.py:1367`). The board's implementation is
//! part 3's tagged-record adapter; see
//! [`leviculum_lxmf::peering::PeerRecord`] for what the trait demands of
//! the record log.
//!
//! One msgpack file, rewritten whole on every change: the table is at most
//! `max_peers` (default 20) records of ~100 B, and peer-state churn is
//! announce-cadence slow, so atomic-rewrite is simpler than any journal
//! and always consistent on reopen. Written via the durable temp+rename
//! path for the same reason the message store is: a peering key can cost
//! minutes of mining (§5's cost table), and losing it to a power cut costs
//! that again.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use leviculum_lxmf::msgpack;
use leviculum_lxmf::peering::{PeerRecord, PeerStore};
use leviculum_lxmf::storage::StorageError;

/// A peer table in one msgpack file.
pub struct FilePeerStore {
    path: PathBuf,
    records: Vec<PeerRecord>,
}

impl FilePeerStore {
    /// Open (or create) the store at `path`. An unreadable or corrupt
    /// file starts empty rather than failing the daemon: the table
    /// re-forms from announces, one bounded re-offer per re-formed peer
    /// (§5's reboot case).
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StorageError::Io)?;
        }
        let records = match std::fs::read(&path) {
            Ok(bytes) => decode_records(&bytes).unwrap_or_else(|| {
                tracing::warn!(
                    "peer store at {} did not parse; starting with an empty table",
                    path.display()
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Ok(Self { path, records })
    }

    fn persist(&self) -> Result<(), StorageError> {
        let bytes = encode_records(&self.records);
        let temp = self.path.with_extension("tmp");
        let mut file = std::fs::File::create(&temp).map_err(|_| StorageError::Io)?;
        file.write_all(&bytes).map_err(|_| StorageError::Io)?;
        file.sync_all().map_err(|_| StorageError::Io)?;
        drop(file);
        std::fs::rename(&temp, &self.path).map_err(|_| StorageError::Io)?;
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }
}

impl PeerStore for FilePeerStore {
    fn save(&mut self, record: &PeerRecord) -> Result<(), StorageError> {
        match self
            .records
            .iter_mut()
            .find(|known| known.destination_hash == record.destination_hash)
        {
            Some(existing) => *existing = record.clone(),
            None => self.records.push(record.clone()),
        }
        self.persist()
    }

    fn remove(&mut self, destination_hash: &[u8; 16]) -> Result<(), StorageError> {
        let before = self.records.len();
        self.records
            .retain(|record| &record.destination_hash != destination_hash);
        if self.records.len() != before {
            self.persist()?;
        }
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<PeerRecord>, StorageError> {
        Ok(self.records.clone())
    }
}

const RECORD_FIELDS: usize = 13;

/// Field count before the peer's public keys were persisted (#388 pass
/// 3); records of this arity still decode, keys absent, so an upgrade
/// keeps its peer table.
const RECORD_FIELDS_LEGACY: usize = 12;

fn encode_records(records: &[PeerRecord]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, records.len());
    for record in records {
        msgpack::array(&mut out, RECORD_FIELDS);
        msgpack::bin(&mut out, &record.destination_hash);
        match &record.identity_hash {
            Some(hash) => msgpack::bin(&mut out, hash),
            None => msgpack::nil(&mut out),
        }
        match &record.peering_key {
            Some((key, value)) => {
                msgpack::array(&mut out, 2);
                msgpack::bin(&mut out, key);
                msgpack::uint(&mut out, *value as u64);
            }
            None => msgpack::nil(&mut out),
        }
        msgpack::uint(&mut out, record.transfer_limit_kb);
        msgpack::uint(&mut out, record.sync_limit_kb);
        msgpack::uint(&mut out, record.stamp_cost as u64);
        msgpack::uint(&mut out, record.stamp_cost_flexibility as u64);
        msgpack::uint(&mut out, record.peering_cost as u64);
        msgpack::uint(&mut out, record.peering_timebase);
        msgpack::uint(&mut out, record.last_heard);
        msgpack::uint(&mut out, record.cursor);
        msgpack::bool(&mut out, record.is_static);
        match &record.public_keys {
            Some(keys) => msgpack::bin(&mut out, keys),
            None => msgpack::nil(&mut out),
        }
    }
    out
}

fn decode_records(bytes: &[u8]) -> Option<Vec<PeerRecord>> {
    let mut position = 0;
    let count = msgpack::array_len(bytes, &mut position).ok()?;
    if count > bytes.len() {
        return None;
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let fields = msgpack::array_len(bytes, &mut position).ok()?;
        if fields != RECORD_FIELDS && fields != RECORD_FIELDS_LEGACY {
            return None;
        }
        let destination_hash: [u8; 16] = msgpack::read_bin(bytes, &mut position)
            .ok()?
            .try_into()
            .ok()?;
        let identity_hash = if msgpack::peek_kind(bytes, position).ok()? == msgpack::Kind::Nil {
            msgpack::read_nil(bytes, &mut position).ok()?;
            None
        } else {
            Some(
                msgpack::read_bin(bytes, &mut position)
                    .ok()?
                    .try_into()
                    .ok()?,
            )
        };
        let peering_key = if msgpack::peek_kind(bytes, position).ok()? == msgpack::Kind::Nil {
            msgpack::read_nil(bytes, &mut position).ok()?;
            None
        } else {
            if msgpack::array_len(bytes, &mut position).ok()? != 2 {
                return None;
            }
            let key: [u8; 32] = msgpack::read_bin(bytes, &mut position)
                .ok()?
                .try_into()
                .ok()?;
            let value = msgpack::read_uint(bytes, &mut position).ok()?;
            Some((key, value.min(u16::MAX as u64) as u16))
        };
        let transfer_limit_kb = msgpack::read_uint(bytes, &mut position).ok()?;
        let sync_limit_kb = msgpack::read_uint(bytes, &mut position).ok()?;
        let stamp_cost = msgpack::read_uint(bytes, &mut position).ok()? as u8;
        let stamp_cost_flexibility = msgpack::read_uint(bytes, &mut position).ok()? as u8;
        let peering_cost = msgpack::read_uint(bytes, &mut position).ok()? as u8;
        let peering_timebase = msgpack::read_uint(bytes, &mut position).ok()?;
        let last_heard = msgpack::read_uint(bytes, &mut position).ok()?;
        let cursor = msgpack::read_uint(bytes, &mut position).ok()?;
        let is_static = msgpack::read_bool(bytes, &mut position).ok()?;
        let public_keys = if fields == RECORD_FIELDS_LEGACY
            || msgpack::peek_kind(bytes, position).ok()? == msgpack::Kind::Nil
        {
            if fields != RECORD_FIELDS_LEGACY {
                msgpack::read_nil(bytes, &mut position).ok()?;
            }
            None
        } else {
            Some(
                msgpack::read_bin(bytes, &mut position)
                    .ok()?
                    .try_into()
                    .ok()?,
            )
        };
        records.push(PeerRecord {
            destination_hash,
            identity_hash,
            public_keys,
            peering_key,
            transfer_limit_kb,
            sync_limit_kb,
            stamp_cost,
            stamp_cost_flexibility,
            peering_cost,
            peering_timebase,
            last_heard,
            cursor,
            is_static,
        });
    }
    Some(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seed: u8) -> PeerRecord {
        PeerRecord {
            destination_hash: [seed; 16],
            identity_hash: seed.is_multiple_of(2).then(|| [seed + 1; 16]),
            public_keys: seed.is_multiple_of(2).then(|| [seed + 2; 64]),
            peering_key: Some(([seed; 32], 18)),
            transfer_limit_kb: 4,
            sync_limit_kb: 32,
            stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 18,
            peering_timebase: 1000 + seed as u64,
            last_heard: 2000,
            cursor: 7,
            is_static: seed == 3,
        }
    }

    #[test]
    fn records_survive_a_reopen_and_upsert_replaces() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("peers");
        let mut store = FilePeerStore::open(&path).expect("open");
        store.save(&record(2)).unwrap();
        store.save(&record(3)).unwrap();
        let mut changed = record(2);
        changed.cursor = 99;
        store.save(&changed).unwrap();
        drop(store);

        let store = FilePeerStore::open(&path).expect("reopen");
        let mut records = store.load_all().unwrap();
        records.sort_by_key(|record| record.destination_hash);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cursor, 99);
        assert_eq!(records[0].peering_key, Some(([2; 32], 18)));
        assert!(records[1].is_static);
    }

    #[test]
    fn remove_persists_and_a_corrupt_file_starts_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("peers");
        let mut store = FilePeerStore::open(&path).expect("open");
        store.save(&record(2)).unwrap();
        store.remove(&[2; 16]).unwrap();
        drop(store);
        let store = FilePeerStore::open(&path).expect("reopen");
        assert!(store.load_all().unwrap().is_empty());

        std::fs::write(&path, b"not msgpack at all").unwrap();
        let store = FilePeerStore::open(&path).expect("open corrupt");
        assert!(store.load_all().unwrap().is_empty());
    }
}
