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
//! returns the survivors sorted by `(status_code, value, last_heard)` desc.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        other => {
            tracing::warn!("discovery: interface type {other} is not discoverable, skipping");
            return None;
        }
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

    let filename = {
        let mut s = String::with_capacity(di.discovery_hash.len() * 2);
        use std::fmt::Write as _;
        for b in di.discovery_hash.iter() {
            let _ = write!(s, "{b:02x}");
        }
        s
    };
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
    Ok(())
}

/// List persisted discovered interfaces, dropping expired/unknown records from
/// disk, sorted by `(status_code, value, last_heard)` descending (Python
/// `list_discovered_interfaces`). `now` is the query wall-clock (Unix seconds).
pub(crate) fn list_discovered_interfaces(
    storage_root: &Path,
    now: f64,
) -> Vec<DiscoveredInterfaceRecord> {
    let dir = discovery_dir(storage_root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut records: Vec<DiscoveredInterfaceRecord> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip our own `.tmp` scratch files and any subdirectories.
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) == Some("tmp") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let record = match DiscoveredInterfaceRecord::decode_msgpack(&bytes) {
            Some(r) => r,
            None => {
                tracing::warn!("discovery: corrupt record {}, skipping", path.display());
                continue;
            }
        };

        if record.should_remove(now) {
            // Prune stale/unknown-type records from disk, matching Python.
            let _ = std::fs::remove_file(&path);
            continue;
        }
        records.push(record);
    }

    // Sort by (status_code, value, last_heard) descending. status_code derives
    // from the last_heard age at query time.
    records.sort_by(|a, b| {
        let ka = (a.status(now).code(), a.value, a.last_heard);
        let kb = (b.status(now).code(), b.value, b.last_heard);
        kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
    });
    records
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
}
