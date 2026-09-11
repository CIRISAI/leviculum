//! File-backed ratchet store (msgpack per file, Python-compatible).
//!
//! Known ratchets: one file per destination in `{storage}/ratchets/`
//! Dest ratchet keys: one file per destination in `{storage}/ratchetkeys/`
//! Filenames are hex-encoded truncated destination hashes.

use std::path::{Path, PathBuf};

use leviculum_core::constants::{RATCHET_SIZE, TRUNCATED_HASHBYTES};
use leviculum_core::ratchet_store::{KnownRatchetEntry, RatchetStore};

use crate::error::Error;
use crate::storage::{atomic_write, hex_decode, hex_encode, note_unreadable_entry};

pub(crate) const RATCHETS_DIR: &str = "ratchets";
pub(crate) const RATCHETKEYS_DIR: &str = "ratchetkeys";

fn encode_known_ratchet(ratchet_pub: &[u8; RATCHET_SIZE], received_secs: f64) -> Vec<u8> {
    let map = rmpv::Value::Map(vec![
        (
            rmpv::Value::String("ratchet".into()),
            rmpv::Value::Binary(ratchet_pub.to_vec()),
        ),
        (
            rmpv::Value::String("received".into()),
            rmpv::Value::F64(received_secs),
        ),
    ]);
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &map).expect("Vec write cannot fail");
    buf
}

fn decode_known_ratchet(data: &[u8]) -> Option<([u8; RATCHET_SIZE], f64)> {
    let value = rmpv::decode::read_value(&mut &data[..]).ok()?;
    let map = value.as_map()?;

    let mut ratchet: Option<[u8; RATCHET_SIZE]> = None;
    let mut received: Option<f64> = None;

    for (k, v) in map {
        let key_str = k.as_str()?;
        match key_str {
            "ratchet" => {
                let bytes = v.as_slice()?;
                if bytes.len() != RATCHET_SIZE {
                    return None;
                }
                let mut arr = [0u8; RATCHET_SIZE];
                arr.copy_from_slice(bytes);
                ratchet = Some(arr);
            }
            "received" => {
                received = Some(v.as_f64()?);
            }
            _ => {}
        }
    }

    Some((ratchet?, received?))
}

pub(crate) struct FileRatchetStore {
    ratchets_dir: PathBuf,
    ratchetkeys_dir: PathBuf,
}

impl FileRatchetStore {
    pub(crate) fn new(storage_dir: &Path) -> Self {
        Self {
            ratchets_dir: storage_dir.join(RATCHETS_DIR),
            ratchetkeys_dir: storage_dir.join(RATCHETKEYS_DIR),
        }
    }

    /// Delete a known ratchet file (for expiry). Not part of the trait.
    pub(crate) fn delete_known_ratchet(&self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) {
        let hex_name = hex_encode(dest_hash);
        let path = self.ratchets_dir.join(&hex_name);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }

    fn ensure_dir(dir: &Path) {
        if !dir.exists() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
}

impl RatchetStore for FileRatchetStore {
    type Error = Error;

    fn load_known_ratchets(
        &mut self,
    ) -> core::result::Result<Vec<([u8; TRUNCATED_HASHBYTES], KnownRatchetEntry)>, Error> {
        let dir = match std::fs::read_dir(&self.ratchets_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Storage(format!("ratchets dir: {e}"))),
        };

        let mut entries = Vec::new();
        for entry in dir.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => {
                    note_unreadable_entry(&self.ratchets_dir, &entry.file_name());
                    continue;
                }
            };
            // Skip temp files
            if name.ends_with(".tmp") || name.ends_with(".out") {
                continue;
            }

            let hash_bytes = match hex_decode(&name) {
                Some(b) if b.len() == TRUNCATED_HASHBYTES => {
                    let mut arr = [0u8; TRUNCATED_HASHBYTES];
                    arr.copy_from_slice(&b);
                    arr
                }
                _ => {
                    note_unreadable_entry(&self.ratchets_dir, &entry.file_name());
                    continue;
                }
            };

            let data = match std::fs::read(entry.path()) {
                Ok(d) => d,
                Err(_) => continue,
            };

            match decode_known_ratchet(&data) {
                Some((ratchet, received_secs)) => {
                    entries.push((
                        hash_bytes,
                        KnownRatchetEntry {
                            ratchet,
                            received_at_secs: received_secs,
                        },
                    ));
                }
                None => {
                    // Corrupted file, delete it
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        Ok(entries)
    }

    fn save_known_ratchet(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        entry: &KnownRatchetEntry,
    ) -> core::result::Result<(), Error> {
        Self::ensure_dir(&self.ratchets_dir);
        let hex_name = hex_encode(dest_hash);
        let data = encode_known_ratchet(&entry.ratchet, entry.received_at_secs);
        atomic_write(&self.ratchets_dir.join(&hex_name), &data)
    }

    fn load_dest_ratchet_keys(
        &mut self,
    ) -> core::result::Result<Vec<([u8; TRUNCATED_HASHBYTES], Vec<u8>)>, Error> {
        let dir = match std::fs::read_dir(&self.ratchetkeys_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Storage(format!("ratchetkeys dir: {e}"))),
        };

        let mut entries = Vec::new();
        for entry in dir.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => {
                    note_unreadable_entry(&self.ratchetkeys_dir, &entry.file_name());
                    continue;
                }
            };
            if name.ends_with(".tmp") {
                continue;
            }

            let hash_bytes = match hex_decode(&name) {
                Some(b) if b.len() == TRUNCATED_HASHBYTES => {
                    let mut arr = [0u8; TRUNCATED_HASHBYTES];
                    arr.copy_from_slice(&b);
                    arr
                }
                _ => {
                    note_unreadable_entry(&self.ratchetkeys_dir, &entry.file_name());
                    continue;
                }
            };

            let data = match std::fs::read(entry.path()) {
                Ok(d) if !d.is_empty() => d,
                Ok(_) => {
                    let _ = std::fs::remove_file(entry.path());
                    continue;
                }
                Err(_) => continue,
            };

            entries.push((hash_bytes, data));
        }

        Ok(entries)
    }

    fn save_dest_ratchet_keys(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        serialized: &[u8],
    ) -> core::result::Result<(), Error> {
        Self::ensure_dir(&self.ratchetkeys_dir);
        let hex_name = hex_encode(dest_hash);
        atomic_write(&self.ratchetkeys_dir.join(&hex_name), serialized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codeberg #336: both ratchet directories are enumerated on every
    /// restore, and both used to panic inside the hex decode on a name the
    /// store did not write — a four-byte character has an even length, so
    /// the length check let it through and the slice landed inside it.
    /// Skip it, keep the entries that are ours, and stay up.
    #[test]
    fn a_foreign_filename_is_skipped_rather_than_fatal() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FileRatchetStore::new(dir.path());
        FileRatchetStore::ensure_dir(&store.ratchets_dir);
        FileRatchetStore::ensure_dir(&store.ratchetkeys_dir);

        let dest = [0xA7u8; TRUNCATED_HASHBYTES];
        store
            .save_known_ratchet(
                &dest,
                &KnownRatchetEntry {
                    ratchet: [0x42; RATCHET_SIZE],
                    received_at_secs: 1_700_000_000.0,
                },
            )
            .expect("save ratchet");
        store
            .save_dest_ratchet_keys(&dest, b"private ratchet key")
            .expect("save ratchet key");

        for target in [&store.ratchets_dir, &store.ratchetkeys_dir] {
            std::fs::write(target.join("\u{1F600}"), b"not ours").expect("stray file");
            let raw = std::ffi::OsStr::from_bytes(b"\xff\xfe").to_owned();
            std::fs::write(target.join(raw), b"not ours either").expect("stray file");
        }

        let known = store.load_known_ratchets().expect("load ratchets");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].0, dest);
        assert_eq!(known[0].1.ratchet, [0x42; RATCHET_SIZE]);

        let keys = store.load_dest_ratchet_keys().expect("load ratchet keys");
        assert_eq!(keys, vec![(dest, b"private ratchet key".to_vec())]);
    }
}
