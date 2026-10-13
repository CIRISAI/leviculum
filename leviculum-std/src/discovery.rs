//! Discovered-interface registry persistence (Codeberg #32, sub-task c).
//!
//! Stores validated discovery announces as one msgpack file per record under
//! `<storage>/discovery/interfaces/<discovery_hash>`, byte-compatible with
//! Python `RNS.Discovery.InterfaceDiscovery` so the two stacks share a storage
//! directory drop-in. The record shape, `config_entry` derivation, and msgpack
//! codec live in [`leviculum_core::discovery`]; this layer owns the filesystem
//! read/modify/write lifecycle (the core is `no_std` and has no storage handle).
//!
//! `persist_discovered` mirrors Python `interface_discovered`: a first sighting
//! writes `discovered == last_heard == received` with `heard_count = 0`; a
//! re-sighting preserves the original `discovered` and increments `heard_count`.
//! `list_discovered_interfaces` mirrors Python `list_discovered_interfaces`: it
//! drops records past `THRESHOLD_REMOVE` (and unknown types) from disk, then
//! returns the survivors sorted by `(status_code, value, last_heard)` desc. An
//! undecodable record is skipped and warned about once per record identity
//! rather than on every pass (Codeberg #157). Unlike Python it does not read
//! an unchanged registry again: the decoded list is cached per directory and
//! only a record file whose stamp moved is decoded anew (Codeberg #437).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use leviculum_core::discovery::DiscoveredInterface;
use leviculum_core::discovery::DiscoveredInterfaceRecord;
use leviculum_core::discovery::InterfaceDescriptor;

use crate::config::InterfaceConfig;

/// Python default per-interface discovery announce interval: 6 hours
/// (Reticulum.py:856).
pub(crate) const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Python floor on a configured `announce_interval`: 5 minutes
/// (Reticulum.py:854).
pub(crate) const MIN_ANNOUNCE_INTERVAL_SECS: u64 = 5 * 60;

/// Resolve the per-interface discovery announce interval in seconds
/// (Reticulum.py:852-856), with a Leviculum direct-seconds override for fast
/// tests. Priority: `discovery_announce_interval_secs` (raw seconds, no floor) >
/// `announce_interval` (minutes, 5-minute floor) > 6-hour default.
pub(crate) fn resolve_announce_interval_secs(cfg: &InterfaceConfig) -> u64 {
    if let Some(secs) = cfg.discovery_announce_interval_secs {
        secs
    } else if let Some(mins) = cfg.announce_interval {
        mins.saturating_mul(60).max(MIN_ANNOUNCE_INTERVAL_SECS)
    } else {
        DEFAULT_ANNOUNCE_INTERVAL_SECS
    }
}

/// Build the discovery [`InterfaceDescriptor`] for a discoverable interface from
/// its own configuration.
///
/// Interface-isolation rule: the descriptor is sourced from the interface's own
/// declaration, never synthesised by transport. Returns `None` for a
/// non-discoverable interface, an unsupported type, or a descriptor missing a
/// field its type requires (matching Python `get_interface_announce_data`, which
/// aborts the announce in that case).
pub(crate) fn descriptor_from_config(cfg: &InterfaceConfig) -> Option<InterfaceDescriptor> {
    if !cfg.discoverable {
        return None;
    }
    let mut desc = InterfaceDescriptor {
        interface_type: cfg.interface_type.clone(),
        name: cfg.discovery_name.clone(),
        // Every type publishes its position, nil when unset, and stamps at its
        // own cost (Reticulum.py:857-864, Discovery.py:98-109).
        latitude: cfg.latitude,
        longitude: cfg.longitude,
        height: cfg.height,
        stamp_value: cfg.discovery_stamp_value,
        ..Default::default()
    };
    match cfg.interface_type.as_str() {
        "TCPServerInterface" | "BackboneInterface" => {
            desc.reachable_on = Some(cfg.reachable_on.clone().or_else(|| cfg.listen_ip.clone())?);
            desc.port = Some(cfg.listen_port? as u64);
        }
        "RNodeInterface" => {
            desc.frequency = Some(cfg.frequency?);
            desc.bandwidth = Some(cfg.bandwidth? as u64);
            desc.spreadingfactor = Some(cfg.spreading_factor? as u64);
            desc.codingrate = Some(cfg.coding_rate? as u64);
        }
        // The TNC's radio settings are not readable through KISS, so they come
        // from the `discovery_*` keys and none is required (Discovery.py:158-162).
        "KISSInterface" => {
            desc.frequency = cfg.discovery_frequency;
            desc.bandwidth = cfg.discovery_bandwidth;
            desc.modulation = cfg.discovery_modulation;
        }
        other => {
            tracing::warn!("discovery: interface type {other} is not discoverable, skipping");
            return None;
        }
    }
    // Python `publish_ifac`: the announce carries this interface's own IFAC
    // credentials, which are its `networkname`/`passphrase`
    // (Reticulum.py:752-766, 861). Off by default -- the passphrase goes on the
    // air only on an explicit opt-in (Codeberg #162).
    if cfg.publish_ifac {
        desc.publish_ifac = true;
        desc.ifac_netname = cfg.networkname.clone();
        desc.ifac_netkey = cfg.passphrase.clone();
    }
    Some(desc)
}

/// Current wall-clock as Unix-epoch seconds (matches Python `time.time()`).
pub(crate) fn now_unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The per-record storage directory: `<storage>/discovery/interfaces`.
pub(crate) fn discovery_dir(storage_root: &Path) -> PathBuf {
    storage_root.join("discovery").join("interfaces")
}

/// Persist a validated discovery announce, mirroring Python `interface_discovered`.
///
/// On the first sighting of a `discovery_hash`, writes a fresh record. On a
/// re-sighting, preserves the original `discovered` timestamp and increments
/// `heard_count`. `hops` is the reception-time distance; `now` is the reception
/// wall-clock (Unix seconds).
pub(crate) fn persist_discovered(
    storage_root: &Path,
    di: &DiscoveredInterface,
    hops: u32,
    now: f64,
) -> std::io::Result<()> {
    let dir = discovery_dir(storage_root);
    std::fs::create_dir_all(&dir)?;

    let filename = crate::storage::hex_encode(&di.discovery_hash);
    let path = dir.join(&filename);

    // Preserve `discovered` + `heard_count` across re-announces (Python reads
    // the prior record, keeps its first-seen time, and bumps the counter). A
    // corrupt/unreadable prior record re-creates fresh, like Python.
    let (discovered, heard_count) = match std::fs::read(&path) {
        Ok(bytes) => match DiscoveredInterfaceRecord::decode_msgpack(&bytes) {
            Some(prev) => (prev.discovered, prev.heard_count + 1),
            None => (now, 0),
        },
        Err(_) => (now, 0),
    };

    let record =
        DiscoveredInterfaceRecord::from_discovered(di, hops, now, discovered, now, heard_count);
    let encoded = record.encode_msgpack();

    // Atomic-ish write: temp file then rename, so a concurrent reader never sees
    // a half-written record (Python writes in place; rename is strictly safer).
    let tmp = dir.join(format!("{filename}.tmp"));
    std::fs::write(&tmp, &encoded)?;
    std::fs::rename(&tmp, &path)?;
    let mut generations = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    match generations.get_mut(&dir) {
        Some(generation) => *generation = generation.wrapping_add(1),
        None => {
            generations.insert(dir, 1);
        }
    }
    Ok(())
}

/// Corrupt records already warned about, keyed by path, holding a fingerprint
/// of the bytes that were warned about (Codeberg #157).
///
/// A scan runs on every `rnstatus`-style query and on the autoconnect cadence,
/// so an undecodable record used to emit one warning per pass and fill the log
/// indefinitely. The key is the record's identity, not a global flag: a second,
/// different corrupt record still gets its own warning, and a record whose
/// bytes change (rewritten by a peer stack, repaired, or corrupted a different
/// way) warns again because the fingerprint no longer matches.
static WARNED_CORRUPT: Mutex<BTreeMap<PathBuf, u64>> = Mutex::new(BTreeMap::new());

/// Fingerprint of a record's raw bytes, used only to detect that a
/// still-undecodable file has *changed* since the warning. Not a security
/// property, so the std hasher is enough.
fn record_fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The registry as a listing returns it: shared, so a read of an unchanged
/// registry hands out the list it already holds instead of a copy of 773
/// records (Codeberg #437).
pub(crate) type DiscoveredList = Arc<Vec<Arc<DiscoveredInterfaceRecord>>>;

/// Bumped by every [`persist_discovered`], per discovery directory, so a
/// listing in this process sees its own writes at once, whatever the
/// filesystem's timestamp granularity.
///
/// Per directory, so a write to one registry leaves another one cached (the
/// test binary keeps hundreds side by side). A mutex rather than an atomic
/// both for the map and because leviculum-std builds for triples without
/// 64-bit atomics (`just no-atomic64-gate`). It is held for one map lookup
/// only, never across file I/O.
static GENERATION: Mutex<BTreeMap<PathBuf, u64>> = Mutex::new(BTreeMap::new());

/// The write count of `dir` so far, 0 for a directory never written here.
fn generation_of(dir: &Path) -> u64 {
    let generations = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    generations.get(dir).copied().unwrap_or(0)
}

/// What [`list_discovered_interfaces`] saw last, per discovery directory
/// (Codeberg #437).
///
/// The autoconnect branch lists the registry once a second. Re-reading and
/// re-decoding every record each time was, on the miauhaus soak with 773
/// records, 72 percent of the node's allocations: about 14 000 a second to
/// learn that nothing had changed. Python re-reads too
/// (`Discovery.py:402-447`); nothing on the wire or in the
/// listing depends on that, so the read is skipped when the directory says
/// nothing moved.
static REGISTRY_CACHE: Mutex<BTreeMap<PathBuf, DirCache>> = Mutex::new(BTreeMap::new());

/// How long a cached listing is trusted without looking at the files. The
/// directory's mtime moves when a record is created, renamed into place or
/// removed, which is every write this stack makes. It does not move when
/// another process rewrites a record in place, which is how Python writes
/// (`open(filepath, "wb")`, `Discovery.py:465` and `:491`); a stack sharing the storage
/// directory is seen within this interval.
const REVALIDATE_INTERVAL: Duration = Duration::from_secs(60);

/// A timestamp younger than this, measured from the scan that read it, may
/// share its clock tick with a change made just after the scan (ext3 and FAT
/// stamp whole seconds, FAT two). Until every stamp in the directory is older
/// than this, the next listing looks at the files again.
const SETTLE: Duration = Duration::from_secs(2);

/// The identity of a record file's contents as the directory reports it. A
/// record rewritten through a rename is a new inode; one rewritten in place
/// changes its mtime or length.
#[derive(Clone, Copy, PartialEq)]
struct FileStamp {
    mtime: Option<SystemTime>,
    len: u64,
    ino: u64,
}

impl FileStamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(meta);
        #[cfg(not(unix))]
        let ino = 0;
        Self {
            mtime: meta.modified().ok(),
            len: meta.len(),
            ino,
        }
    }
}

/// One record file as last decoded.
enum CachedEntry {
    Record(Arc<DiscoveredInterfaceRecord>),
    /// Undecodable, with the fingerprint of the bytes (Codeberg #157).
    Corrupt(u64),
}

struct CachedFile {
    stamp: FileStamp,
    entry: CachedEntry,
}

struct DirCache {
    /// The directory's mtime when the files were last looked at.
    dir_mtime: Option<SystemTime>,
    /// [`GENERATION`] when the files were last looked at.
    generation: u64,
    /// Every stamp read was older than [`SETTLE`] at the time of the scan.
    settled: bool,
    validated_at: Instant,
    files: BTreeMap<OsString, CachedFile>,
    sorted: DiscoveredList,
}

impl DirCache {
    fn new() -> Self {
        Self {
            dir_mtime: None,
            generation: 0,
            settled: false,
            validated_at: Instant::now(),
            files: BTreeMap::new(),
            sorted: Arc::new(Vec::new()),
        }
    }

    /// Whether the files must be looked at again before the cached list is
    /// handed out.
    fn stale(&self, dir_mtime: Option<SystemTime>, generation: u64) -> bool {
        !self.settled
            || dir_mtime.is_none()
            || dir_mtime != self.dir_mtime
            || generation != self.generation
            || self.validated_at.elapsed() >= REVALIDATE_INTERVAL
    }

    /// Stat every record file; decode only the ones whose stamp moved.
    fn revalidate(&mut self, dir: &Path, dir_mtime: Option<SystemTime>, generation: u64) {
        let scan_start = SystemTime::now();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                *self = Self::new();
                return;
            }
        };
        let mut newest = dir_mtime;
        let mut seen: BTreeMap<OsString, CachedFile> = BTreeMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip our own `.tmp` scratch files and any subdirectories.
            if path.extension().and_then(|e| e.to_str()) == Some("tmp") {
                continue;
            }
            let meta = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let stamp = FileStamp::of(&meta);
            newest = newest.max(stamp.mtime);
            let name = entry.file_name();
            let file = match self.files.remove(&name) {
                Some(cached) if cached.stamp == stamp => cached,
                _ => match decode_record_file(&path) {
                    Some(entry) => CachedFile { stamp, entry },
                    // Unreadable: not cached, so the next look tries again.
                    None => continue,
                },
            };
            seen.insert(name, file);
        }
        self.files = seen;
        self.dir_mtime = dir_mtime;
        self.generation = generation;
        self.validated_at = Instant::now();
        self.settled =
            newest.is_some_and(|t| t.checked_add(SETTLE).is_some_and(|t| t <= scan_start));

        // Replace the suppression entries for THIS directory with the corrupt
        // records it holds now. Scoped to `dir` so a concurrent scan of another
        // storage root (the unit tests, a second daemon instance) cannot silence
        // or re-arm someone else's records.
        if let Ok(mut warned) = WARNED_CORRUPT.lock() {
            warned.retain(|p, _| p.parent() != Some(dir));
            for (name, file) in &self.files {
                if let CachedEntry::Corrupt(fingerprint) = file.entry {
                    warned.insert(dir.join(name), fingerprint);
                }
            }
        }
    }

    /// Unlink records past `THRESHOLD_REMOVE` (and unknown types), and rebuild
    /// the sorted list if anything was revalidated or removed. Otherwise only
    /// re-sort when the age-derived status moved a record out of order.
    fn assemble(&mut self, dir: &Path, now: f64, rebuild: bool) {
        let expired: Vec<OsString> = self
            .files
            .iter()
            .filter(|(_, f)| matches!(&f.entry, CachedEntry::Record(r) if r.should_remove(now)))
            .map(|(name, _)| name.clone())
            .collect();
        for name in &expired {
            // Prune stale/unknown-type records from disk, matching Python.
            let _ = std::fs::remove_file(dir.join(name));
            self.files.remove(name);
        }

        if rebuild || !expired.is_empty() {
            let mut records: Vec<Arc<DiscoveredInterfaceRecord>> = self
                .files
                .values()
                .filter_map(|f| match &f.entry {
                    CachedEntry::Record(r) => Some(Arc::clone(r)),
                    CachedEntry::Corrupt(_) => None,
                })
                .collect();
            records.sort_by(|a, b| listing_order(a, b, now));
            self.sorted = Arc::new(records);
        } else if !self
            .sorted
            .is_sorted_by(|a, b| listing_order(a, b, now) != std::cmp::Ordering::Greater)
        {
            Arc::make_mut(&mut self.sorted).sort_by(|a, b| listing_order(a, b, now));
        }
    }
}

/// `(status_code, value, last_heard)` descending. status_code derives from
/// the last_heard age at query time, so an order can go stale by the clock
/// alone.
fn listing_order(
    a: &DiscoveredInterfaceRecord,
    b: &DiscoveredInterfaceRecord,
    now: f64,
) -> std::cmp::Ordering {
    let ka = (a.status(now).code(), a.value, a.last_heard);
    let kb = (b.status(now).code(), b.value, b.last_heard);
    kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
}

/// Read and decode one record file; `None` when it cannot be read.
///
/// An undecodable record is skipped, not unlinked. The reference logs and
/// continues too (`Discovery.py:442-445`); only its explicit `should_remove`
/// cases unlink (`:423-425`). Our decoder is the stricter of the two — it
/// requires fields Python's `msgpack.unpackb` never looks at — so deleting
/// here would destroy records a Python `rnsd` still serves out of the storage
/// directory both stacks share (Codeberg #157).
fn decode_record_file(path: &Path) -> Option<CachedEntry> {
    let bytes = std::fs::read(path).ok()?;
    if let Some(record) = DiscoveredInterfaceRecord::decode_msgpack(&bytes) {
        return Some(CachedEntry::Record(Arc::new(record)));
    }
    let fingerprint = record_fingerprint(&bytes);
    let already_warned = WARNED_CORRUPT
        .lock()
        .map(|w| w.get(path) == Some(&fingerprint))
        .unwrap_or(false);
    if !already_warned {
        tracing::warn!("discovery: corrupt record {}, skipping", path.display());
    }
    Some(CachedEntry::Corrupt(fingerprint))
}

/// List persisted discovered interfaces, dropping expired/unknown records from
/// disk, sorted by `(status_code, value, last_heard)` descending (Python
/// `list_discovered_interfaces`). `now` is the query wall-clock (Unix seconds).
///
/// A registry whose directory has not moved is not read again: the list comes
/// from [`REGISTRY_CACHE`], and only a record file whose stamp changed is
/// decoded anew (Codeberg #437).
pub(crate) fn list_discovered_interfaces(storage_root: &Path, now: f64) -> DiscoveredList {
    let dir = discovery_dir(storage_root);
    // Read before the directory, so a write racing the scan bumps it past
    // what the cache records and the next listing looks again.
    let generation = generation_of(&dir);
    let mut caches = REGISTRY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let dir_mtime = match std::fs::metadata(&dir) {
        Ok(meta) if meta.is_dir() => meta.modified().ok(),
        _ => {
            caches.remove(&dir);
            return Arc::new(Vec::new());
        }
    };
    if !caches.contains_key(&dir) {
        caches.insert(dir.clone(), DirCache::new());
    }
    let Some(cache) = caches.get_mut(&dir) else {
        return Arc::new(Vec::new());
    };
    let rebuild = cache.stale(dir_mtime, generation);
    if rebuild {
        cache.revalidate(&dir, dir_mtime, generation);
    }
    cache.assemble(&dir, now, rebuild);
    Arc::clone(&cache.sorted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::discovery::STAMP_SIZE;

    fn rnode_di(name: &str, hash_seed: u8) -> DiscoveredInterface {
        DiscoveredInterface {
            interface_type: "RNodeInterface".to_string(),
            transport: true,
            name: name.to_string(),
            transport_id: [0xAB; 16],
            network_id: [0xCD; 16],
            value: 15,
            stamp: [0x11; STAMP_SIZE],
            latitude: None,
            longitude: None,
            height: None,
            reachable_on: None,
            port: None,
            frequency: Some(867_200_000),
            bandwidth: Some(125_000),
            spreadingfactor: Some(8),
            codingrate: Some(5),
            ifac_netname: None,
            ifac_netkey: None,
            discovery_hash: [hash_seed; STAMP_SIZE],
        }
    }

    #[test]
    fn persist_then_list_round_trips() {
        let td = tempfile::tempdir().unwrap();
        let di = rnode_di("Node A", 0x22);
        persist_discovered(td.path(), &di, 2, 1000.0).unwrap();

        let list = list_discovered_interfaces(td.path(), 1000.0);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Node A");
        assert_eq!(list[0].hops, 2);
        assert_eq!(list[0].heard_count, 0);
        assert_eq!(list[0].discovered, 1000.0);
    }

    #[test]
    fn re_announce_preserves_discovered_and_bumps_heard_count() {
        let td = tempfile::tempdir().unwrap();
        let di = rnode_di("Node A", 0x22);
        persist_discovered(td.path(), &di, 1, 1000.0).unwrap();
        persist_discovered(td.path(), &di, 1, 2000.0).unwrap();

        let list = list_discovered_interfaces(td.path(), 2000.0);
        assert_eq!(list.len(), 1, "same discovery_hash stays one record");
        assert_eq!(list[0].discovered, 1000.0, "first-seen preserved");
        assert_eq!(list[0].last_heard, 2000.0, "last_heard updated");
        assert_eq!(list[0].heard_count, 1, "heard_count incremented");
    }

    #[test]
    fn expired_records_are_pruned_from_disk() {
        use leviculum_core::discovery::THRESHOLD_REMOVE;
        let td = tempfile::tempdir().unwrap();
        let di = rnode_di("Old", 0x22);
        persist_discovered(td.path(), &di, 1, 0.0).unwrap();

        let now = THRESHOLD_REMOVE + 10.0;
        let list = list_discovered_interfaces(td.path(), now);
        assert!(list.is_empty(), "expired record dropped from listing");
        let dir = discovery_dir(td.path());
        let remaining: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert!(remaining.is_empty(), "expired record unlinked from disk");
    }

    /// Codeberg #157: a corrupt record used to be re-warned on every scan, so
    /// one bad file filled the log indefinitely. Both states are observed, per
    /// `docs/src/concepts/evidence-and-honesty.md`: the FIRST scan still warns
    /// (a suppression that suppresses everything is worse than the noise), and
    /// a SECOND, different corrupt record still gets its own warning — which a
    /// global "already warned" flag would swallow.
    #[test]
    fn corrupt_record_warns_once_per_record() {
        let capture = crate::test_support::warn_capture::register_warn_capture();
        let td = tempfile::tempdir().unwrap();
        let dir = discovery_dir(td.path());
        std::fs::create_dir_all(&dir).unwrap();

        let first = dir.join("aa00000000000000000000000000000000000000000000000000000000000001");
        std::fs::write(&first, b"this is not msgpack").unwrap();
        let first_str = first.display().to_string();

        let list = list_discovered_interfaces(td.path(), 1000.0);
        assert!(list.is_empty(), "a corrupt record is not listed");
        assert_eq!(
            capture.snapshot().matches(&first_str).count(),
            1,
            "the first sighting of a corrupt record must warn"
        );

        for _ in 0..3 {
            let _ = list_discovered_interfaces(td.path(), 1000.0);
        }
        assert_eq!(
            capture.snapshot().matches(&first_str).count(),
            1,
            "the same corrupt record must not warn again on later scans"
        );

        // A second, different corrupt record still gets its own warning.
        let second = dir.join("bb00000000000000000000000000000000000000000000000000000000000002");
        std::fs::write(&second, b"also not msgpack").unwrap();
        let second_str = second.display().to_string();
        let _ = list_discovered_interfaces(td.path(), 1000.0);
        assert_eq!(
            capture.snapshot().matches(&second_str).count(),
            1,
            "a different corrupt record must warn on its own"
        );
        assert_eq!(
            capture.snapshot().matches(&first_str).count(),
            1,
            "the already-warned record stays silent while the new one warns"
        );
    }

    /// Codeberg #157: the corrupt record is skipped, not unlinked. The
    /// reference leaves it in place too (`Discovery.py:442-445` logs and
    /// continues; only the explicit `should_remove` cases at `:423-425`
    /// unlink), and our decoder is stricter than Python's — a record missing a
    /// field we require but Python does not decodes fine for `rnsd`. Deleting
    /// it would destroy a record the other stack still serves out of the
    /// storage directory the two share.
    #[test]
    fn corrupt_record_is_skipped_not_unlinked() {
        let td = tempfile::tempdir().unwrap();
        let dir = discovery_dir(td.path());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cc00000000000000000000000000000000000000000000000000000000000003");
        std::fs::write(&path, b"not msgpack either").unwrap();

        let list = list_discovered_interfaces(td.path(), 1000.0);
        assert!(list.is_empty());
        assert!(path.exists(), "a corrupt record is left on disk");
    }

    #[test]
    fn list_sorts_by_value_descending() {
        let td = tempfile::tempdir().unwrap();
        let mut low = rnode_di("Low", 0x01);
        low.value = 14;
        let mut high = rnode_di("High", 0x02);
        high.value = 22;
        persist_discovered(td.path(), &low, 1, 1000.0).unwrap();
        persist_discovered(td.path(), &high, 1, 1000.0).unwrap();

        let list = list_discovered_interfaces(td.path(), 1000.0);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "High", "higher stamp value sorts first");
    }

    #[cfg(unix)]
    /// Persist `n` distinct records and [`settle`] the registry.
    fn settled_registry(root: &Path, n: usize) {
        for i in 0..n {
            let mut di = rnode_di(&format!("Node {i}"), 0);
            di.discovery_hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            persist_discovered(root, &di, 1, 1000.0).unwrap();
        }
        settle(root);
    }

    /// Move the directory's and every file's mtime an hour into the past, so
    /// the registry looks the way it does on a node between announces rather
    /// than a moment after a write.
    ///
    /// Unix only: Windows refuses to open a directory as a file to set its
    /// mtime (ERROR_ACCESS_DENIED), so the cache tests that age the registry
    /// run on Unix.
    #[cfg(unix)]
    fn settle(root: &Path) {
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        let dir = discovery_dir(root);
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            std::fs::File::options()
                .write(true)
                .open(entry.path())
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
        std::fs::File::open(&dir)
            .unwrap()
            .set_modified(past)
            .unwrap();
    }

    /// Codeberg #437: the autoconnect branch reads the registry once a second.
    /// On the miauhaus soak (773 records) that read was 72 percent of the
    /// node's allocations, decoding every record to learn that nothing had
    /// changed. A read of an unchanged registry must not decode anything.
    #[cfg(unix)]
    #[test]
    fn unchanged_registry_is_not_decoded_again() {
        use crate::rpc::connection::alloc_probe::measure_allocations;
        let td = tempfile::tempdir().unwrap();
        settled_registry(td.path(), 800);

        let (first, first_allocs) =
            measure_allocations(|| list_discovered_interfaces(td.path(), 1000.0));
        assert_eq!(first.len(), 800);
        drop(first);
        let (second, second_allocs) =
            measure_allocations(|| list_discovered_interfaces(td.path(), 1000.0));
        assert_eq!(second.len(), 800, "the cached read returns every record");
        assert!(
            second_allocs < 16,
            "an unchanged registry of 800 records cost {second_allocs} allocations \
             on the second read (the first read cost {first_allocs})"
        );
    }

    /// A write to one registry does not make another one stale. The write
    /// counter was process-wide, so any `persist_discovered` (in the test
    /// binary, any parallel test) sent every cached directory back to the
    /// files, and the test above went red when a neighbour wrote between its
    /// two reads.
    #[cfg(unix)]
    #[test]
    fn a_write_to_another_registry_leaves_this_one_cached() {
        use crate::rpc::connection::alloc_probe::measure_allocations;
        let here = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        settled_registry(here.path(), 50);
        drop(list_discovered_interfaces(here.path(), 1000.0));

        persist_discovered(elsewhere.path(), &rnode_di("Elsewhere", 0x42), 1, 1000.0).unwrap();

        let (list, allocs) =
            measure_allocations(|| list_discovered_interfaces(here.path(), 1000.0));
        assert_eq!(list.len(), 50);
        assert!(
            allocs < 16,
            "a write to another registry cost this one {allocs} allocations on its next read"
        );
    }

    /// The other half of #437: a record written by someone else (no
    /// `persist_discovered` in this process to bump the generation) is listed
    /// on the next read, and only that record is decoded. The others are
    /// stat'ed, not read.
    #[cfg(unix)]
    #[test]
    fn a_record_added_behind_the_cache_is_listed_and_decoded_alone() {
        use crate::rpc::connection::alloc_probe::measure_allocations;
        let td = tempfile::tempdir().unwrap();
        settled_registry(td.path(), 800);
        let (_, full_read) = measure_allocations(|| list_discovered_interfaces(td.path(), 1000.0));

        let mut di = rnode_di("Newcomer", 0xEE);
        di.value = 30;
        let record = DiscoveredInterfaceRecord::from_discovered(&di, 1, 1000.0, 1000.0, 1000.0, 0);
        let dir = discovery_dir(td.path());
        std::fs::write(
            dir.join(crate::storage::hex_encode(&di.discovery_hash)),
            record.encode_msgpack(),
        )
        .unwrap();

        let (list, revalidation) =
            measure_allocations(|| list_discovered_interfaces(td.path(), 1000.0));
        assert_eq!(list.len(), 801, "the new record is listed");
        assert_eq!(list[0].name, "Newcomer", "and sorted into place");
        assert!(
            revalidation < full_read / 2,
            "a revalidation that decodes one record cost {revalidation} allocations, \
             a full read {full_read}"
        );
    }

    /// Expiry is decided at query time, so a cached record that ages past
    /// `THRESHOLD_REMOVE` is dropped and unlinked on the read that sees it
    /// expire, although nothing on disk changed.
    #[cfg(unix)]
    #[test]
    fn a_cached_record_expires_on_the_read_that_sees_it() {
        use leviculum_core::discovery::THRESHOLD_REMOVE;
        let td = tempfile::tempdir().unwrap();
        settled_registry(td.path(), 2);
        let mut fresh = rnode_di("Fresh", 0x77);
        fresh.discovery_hash[0] = 0xFF;
        let later = 1000.0 + THRESHOLD_REMOVE + 10.0;
        persist_discovered(td.path(), &fresh, 1, later).unwrap();
        settle(td.path());

        assert_eq!(list_discovered_interfaces(td.path(), 1000.0).len(), 3);
        // Served from the cache: neither the directory nor the generation
        // moved since the read above.
        let list = list_discovered_interfaces(td.path(), later);
        assert_eq!(list.len(), 1, "the two old records expired");
        assert_eq!(list[0].name, "Fresh");
        let on_disk = std::fs::read_dir(discovery_dir(td.path())).unwrap().count();
        assert_eq!(on_disk, 1, "expired records are unlinked");
    }
}
