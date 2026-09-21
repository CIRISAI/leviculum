//! mvr for Codeberg #420 — what a destination last said about itself is
//! forgotten by a restart.
//!
//! The #417 fix peers with a node that syncs into us by recalling that
//! node's propagation announce (`NodeCore::recall_app_data`, the reference's
//! `Identity.recall_app_data`, `reference/Reticulum/RNS/Identity.py:162-172`).
//! The reference answers that question from `Identity.known_destinations`,
//! which is written to storage with the `app_data` field filled
//! (`remember`, Identity.py:101-113; `save_known_destinations`,
//! Identity.py:177-239) and read back on start
//! (`load_known_destinations`, Identity.py:242-265). Ours answers from the
//! announce cache, which is memory-only, and our `known_destinations` writer
//! puts `app_data: None` on every destination learned at runtime
//! (`leviculum-std/src/storage.rs`, `take_flush_snapshot`).
//!
//! So every restart opens a window — up to a full announce interval, six
//! hours at the reference cadence — in which a syncing neighbour reads as
//! "not a node" and our store fills without ever syncing back, which is
//! exactly #417 again.
//!
//! **Acceptance**: red before the change (the recall after the restart is
//! `None`), green after, with the pre-restart recall asserted first so a
//! green cannot come from the premise having quietly broken.

use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, InterfaceId};
use leviculum_std::driver::{StdClock, StdNodeCore, StdStorage};

/// A node core on `dir`, the way the daemon builds one: file storage, so a
/// second core on the same path is the same node after a restart.
fn core(dir: &std::path::Path) -> StdNodeCore {
    NodeCoreBuilder::new().enable_transport(false).build(
        rand_core::OsRng,
        StdClock::new(),
        StdStorage::new(dir).expect("storage under a fresh temp dir"),
    )
}

/// A neighbour's propagation announce: its destination hash, the raw packet
/// as it arrives off an interface, and the `app_data` it carries — the
/// bytes #417's peering decision is made of.
fn propagation_announce(now_secs: u64) -> ([u8; 16], Vec<u8>, Vec<u8>) {
    let mut destination = Destination::new(
        Some(leviculum_std::generate_identity()),
        Direction::In,
        DestinationType::Single,
        leviculum_lxmf::node::APP_NAME,
        &[leviculum_lxmf::propagation_client::PROPAGATION_ASPECT],
    )
    .expect("a propagation destination for the neighbour");
    let hash = *destination.hash().as_bytes();
    let app_data = leviculum_lxmf::PropagationNodeAnnounce {
        legacy_support: false,
        timebase: now_secs,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 1024,
        stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 0,
        metadata: Vec::new(),
    }
    .encode()
    .expect("propagation announce app data");
    let packet = destination
        .announce(Some(&app_data), &mut rand_core::OsRng, 0, now_secs)
        .expect("announce packet");
    let mut buf = vec![0u8; 600];
    let len = packet.pack(&mut buf).expect("pack the announce");
    buf.truncate(len);
    (hash, buf, app_data)
}

#[test]
fn a_neighbours_recalled_announce_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");

    let (remote, packet, app_data) = {
        let mut core = core(dir.path());
        let (remote, packet, app_data) = propagation_announce(core.emission_secs());
        let _ = core.handle_packet(InterfaceId(0), &packet);
        assert_eq!(
            core.recall_app_data(&DestinationHash::new(remote))
                .as_deref(),
            Some(app_data.as_slice()),
            "the running node must recall the announce, or this test proves nothing"
        );
        // The daemon's shutdown persist (driver/mod.rs: core.storage_mut().flush()).
        leviculum_core::traits::Storage::flush(core.storage_mut());
        (remote, packet, app_data)
    };
    let _ = packet;

    // The restart: a second core over the same storage directory.
    let core = core(dir.path());
    assert_eq!(
        core.recall_app_data(&DestinationHash::new(remote))
            .as_deref(),
        Some(app_data.as_slice()),
        "after a restart the neighbour's app_data must still be recallable, \
         as Python's known_destinations makes it (Identity.py:162-172)"
    );
}
