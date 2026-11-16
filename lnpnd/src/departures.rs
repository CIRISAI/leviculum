//! The duplicate list's persisted part: ids that left the store before
//! they expired, so a restart does not let them come back (leviculum#478,
//! order 1294).
//!
//! The role's duplicate cache is RAM-only
//! (`PropagationNode::track_departures`, `leviculum-lxmf`). Most of it a
//! restart rebuilds for free: a message still in the store is a duplicate
//! by the store's own index. What it cannot rebuild is a message that was
//! collected, delivered to the daemon's own mailbox, or displaced: a peer
//! or a client that offers it again after the restart would have it
//! stored and distributed a second time. miauhaus restarted seven times
//! in ten days. The reference persists its whole set
//! (`locally_processed_transient_ids`,
//! `reference/LXMF/LXMF/LXMRouter.py:1018-1024`); this file holds only
//! the part the store does not, within the expiry window.
//!
//! Since order 1394 the file also keeps why each id left, which
//! `/lev/status` answers from (`leviculum_lxmf::lev_status`), and an
//! expired id too, for the same 30 days: it answers `expired` but is no
//! duplicate.
//!
//! Format `LNPNDDP2`: the 8-byte magic, then one 41-byte record per id,
//! the 32-byte transient id, the departure time as little-endian seconds
//! and the reason byte ([`DepartureReason::code`]). The older `LNPNDDP1`
//! has 40-byte records without the reason byte; it still loads, each id
//! as [`DepartureReason::Unrecorded`]: a duplicate as before, `unknown` to
//! a status query, since that file did not say whether an id was
//! collected or displaced. The next save writes `LNPNDDP2`. Written whole
//! to a temporary file, synced, and renamed over the old one, so a power
//! cut leaves either the old list or the new one. A file that does not
//! parse is reported and ignored: the cost is revenants, not a node that
//! does not start.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use leviculum_lxmf::{Departure, DepartureReason, TransientId};

const MAGIC: &[u8; 8] = b"LNPNDDP2";
const RECORD: usize = 32 + 8 + 1;
const MAGIC_V1: &[u8; 8] = b"LNPNDDP1";
const RECORD_V1: usize = 32 + 8;

/// The departures the file at `path` holds; empty when there is none.
pub fn load(path: &Path) -> Result<Vec<(TransientId, Departure)>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let (body, record_len) = if let Some(body) = bytes.strip_prefix(MAGIC.as_slice()) {
        (body, RECORD)
    } else if let Some(body) = bytes.strip_prefix(MAGIC_V1.as_slice()) {
        (body, RECORD_V1)
    } else {
        return Err(format!("{}: not a departures file", path.display()));
    };
    if body.len() % record_len != 0 {
        return Err(format!("{}: truncated record", path.display()));
    }
    Ok(body
        .chunks_exact(record_len)
        .map(|record| {
            let mut transient_id = [0u8; 32];
            transient_id.copy_from_slice(&record[..32]);
            let mut secs = [0u8; 8];
            secs.copy_from_slice(&record[32..40]);
            let reason = record.get(40).map_or(DepartureReason::Unrecorded, |code| {
                DepartureReason::from_code(*code)
            });
            (
                transient_id,
                Departure {
                    at_secs: u64::from_le_bytes(secs),
                    reason,
                },
            )
        })
        .collect())
}

/// Replace the file at `path` with `entries`.
pub fn save<'a>(
    path: &Path,
    entries: impl Iterator<Item = (&'a TransientId, &'a Departure)>,
) -> std::io::Result<()> {
    let mut body = MAGIC.to_vec();
    for (transient_id, departure) in entries {
        body.extend_from_slice(transient_id);
        body.extend_from_slice(&departure.at_secs.to_le_bytes());
        body.push(departure.reason.code());
    }
    let temp = path.with_extension("tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)?;
    // The rename itself must survive a power cut, not only the bytes.
    if let Some(dir) = path.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn departure(at_secs: u64, reason: DepartureReason) -> Departure {
        Departure { at_secs, reason }
    }

    #[test]
    fn a_saved_list_loads_back_and_a_missing_one_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("departures");
        assert!(load(&path).expect("absent is fine").is_empty());
        let entries = [
            ([1u8; 32], departure(10, DepartureReason::Collected)),
            ([2u8; 32], departure(20, DepartureReason::Evicted)),
            ([3u8; 32], departure(30, DepartureReason::Expired)),
            ([4u8; 32], departure(40, DepartureReason::Delivered)),
        ];
        save(&path, entries.iter().map(|(id, at)| (id, at))).expect("saves");
        let bytes = std::fs::read(&path).expect("reads");
        assert_eq!(&bytes[..8], b"LNPNDDP2");
        assert_eq!(bytes.len(), 8 + 4 * 41);
        assert_eq!(load(&path).expect("loads"), entries.to_vec());
    }

    /// A node upgraded from the 40-byte format keeps its duplicates: the
    /// old file loads, each id unrecorded, and the next save is the new
    /// format with the same ids.
    #[test]
    fn an_old_format_file_loads_as_unrecorded_and_saves_as_the_new_one() {
        use leviculum_lxmf::{
            MemoryPropagationStore, MessageStatus, PropagationNode, PropagationNodeConfig,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("departures");
        let mut old = b"LNPNDDP1".to_vec();
        for (id, at) in [([1u8; 32], 1_000u64), ([2u8; 32], 2_000)] {
            old.extend_from_slice(&id);
            old.extend_from_slice(&at.to_le_bytes());
        }
        std::fs::write(&path, &old).expect("writes");

        let loaded = load(&path).expect("loads the old format");
        assert_eq!(
            loaded,
            vec![
                ([1u8; 32], departure(1_000, DepartureReason::Unrecorded)),
                ([2u8; 32], departure(2_000, DepartureReason::Unrecorded)),
            ]
        );
        let mut node = PropagationNode::new(
            MemoryPropagationStore::new(64_000),
            PropagationNodeConfig::default(),
        );
        assert_eq!(node.restore_departures(loaded.clone(), 3_000), 2);
        assert_eq!(node.status_of(&[1u8; 32]), MessageStatus::Unknown);

        save(&path, node.departures()).expect("saves");
        assert_eq!(&std::fs::read(&path).expect("reads")[..8], b"LNPNDDP2");
        assert_eq!(load(&path).expect("loads the new format"), loaded);
    }

    /// The order's test: a message is collected, the node restarts, and
    /// the same message offered again is a duplicate, not a revenant. The
    /// node that restarts without the file takes it as new, which is what
    /// miauhaus did after each of its seven restarts in ten days.
    #[test]
    fn a_restart_between_two_identical_offers_keeps_the_second_a_duplicate() {
        use leviculum_lxmf::constants::{LXMF_OVERHEAD, STAMP_SIZE};
        use leviculum_lxmf::{
            MemoryPropagationStore, PropagationNode, PropagationNodeConfig, PropagationStore,
            PropagationUpload, UploadOutcome,
        };

        let config = PropagationNodeConfig {
            stamp_cost: 0,
            ..PropagationNodeConfig::default()
        };
        let upload =
            PropagationUpload::single(1.0, vec![0x37; LXMF_OVERHEAD + 60], [0xEE; STAMP_SIZE])
                .encode();
        let value = |_: &TransientId, _: &[u8; STAMP_SIZE]| Some(0);
        let duplicate = |outcome: UploadOutcome| match outcome {
            UploadOutcome::Accepted { duplicate, .. } => duplicate,
            other => panic!("{other:?}"),
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("departures");
        let mut before = PropagationNode::new(MemoryPropagationStore::new(64_000), config.clone());
        before.track_departures();
        let transient_id = match before.handle_upload(&upload, 1_000, value) {
            UploadOutcome::Accepted { transient_id, .. } => transient_id,
            other => panic!("{other:?}"),
        };
        // Collected: the client confirmed, the store let it go.
        assert!(before.store_mut().purge(&transient_id).expect("purges"));
        before.note_confirmed_deletion(&transient_id, 2_000);
        save(&path, before.departures()).expect("saves");

        let mut after = PropagationNode::new(MemoryPropagationStore::new(64_000), config.clone());
        let restored = after.restore_departures(load(&path).expect("loads"), 3_000);
        assert_eq!(restored, 1);
        assert!(duplicate(after.handle_upload(&upload, 3_000, value)));
        assert!(after.store().is_empty(), "a duplicate is not stored again");

        let mut forgetful = PropagationNode::new(MemoryPropagationStore::new(64_000), config);
        assert!(
            !duplicate(forgetful.handle_upload(&upload, 3_000, value)),
            "without the file the restart takes it as new, or the test proves nothing"
        );
    }

    /// Past the expiry window a departure is not restored: a peer that
    /// offers it then received it after us, and holds a live copy.
    #[test]
    fn a_departure_past_the_expiry_window_is_not_restored() {
        use leviculum_lxmf::{
            MemoryPropagationStore, PropagationNode, PropagationNodeConfig, MESSAGE_EXPIRY_SECS,
        };
        let mut node = PropagationNode::new(
            MemoryPropagationStore::new(64_000),
            PropagationNodeConfig::default(),
        );
        let now = 10 * MESSAGE_EXPIRY_SECS;
        let restored = node.restore_departures(
            [
                (
                    [1u8; 32],
                    departure(now - MESSAGE_EXPIRY_SECS - 1, DepartureReason::Collected),
                ),
                ([2u8; 32], departure(now - 1, DepartureReason::Collected)),
            ],
            now,
        );
        assert_eq!(restored, 1);
        assert_eq!(node.departures().count(), 1);
    }

    #[test]
    fn a_damaged_file_is_an_error_not_a_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("departures");
        std::fs::write(&path, b"LNPNDDP1short").expect("writes");
        assert!(load(&path).is_err());
        std::fs::write(&path, b"LNPNDDP2short").expect("writes");
        assert!(load(&path).is_err());
        std::fs::write(&path, b"something else entirely").expect("writes");
        assert!(load(&path).is_err());
    }
}
