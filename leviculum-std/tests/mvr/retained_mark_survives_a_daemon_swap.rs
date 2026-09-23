//! mvr for Codeberg #321 — a retained known-destination loses its mark when
//! the file passes through us.
//!
//! The reference stores five elements per known-destination entry
//! (`Identity.remember`, `reference/Reticulum/RNS/Identity.py:101-113`). The
//! fifth is the use-state: `0` never used, a `time.time()` stamp when it was
//! used, and the `-1` sentinel when an application pinned it
//! (`_retain_destination_data`, Identity.py:278-283). The cull reads exactly
//! that field: a `-1` entry is spared, a `0` entry that has not announced
//! within `UNUSED_DESTINATION_LINGER` is dropped together with its ratchet
//! file (Identity.py:336-366).
//!
//! Our writer emitted four elements, so every load-save round-trip through
//! `lnsd` erased the field. The reference's loader fills a short entry with
//! `0` (Identity.py:252-254), so a destination Python had pinned came back
//! as "never used" — an ordinary cull candidate, silently, and only visible
//! one cull cycle later.
//!
//! **Acceptance**: the fixture below is a `known_destinations` file as
//! `rnsd` writes it, carrying one retained, one used and one never-used
//! entry. A daemon-shaped `lnsd` storage opens it, learns one new
//! destination and persists — the same load-modify-save `rnsd` would do —
//! and all three use-states must read back unchanged.

use std::collections::BTreeMap;

use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::traits::Storage as CoreStorage;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, InterfaceId};
use leviculum_std::driver::{StdClock, StdNodeCore, StdStorage};

/// The fifth element as the reference writes it.
const NEVER_USED: i64 = 0;
const RETAINED: i64 = -1;
const LAST_USED_SECS: f64 = 1_756_000_000.0;

fn core(dir: &std::path::Path) -> StdNodeCore {
    NodeCoreBuilder::new().enable_transport(false).build(
        rand_core::OsRng,
        StdClock::new(),
        StdStorage::new(dir).expect("storage under a fresh temp dir"),
    )
}

/// One entry in `rnsd`'s wire shape: `[timestamp, packet_hash, public_key,
/// app_data, use_state]`.
fn rnsd_entry(use_state: rmpv::Value) -> ([u8; 16], rmpv::Value) {
    let identity = leviculum_std::generate_identity();
    let hash = {
        let mut h = [0u8; 16];
        h.copy_from_slice(&identity.hash()[..16]);
        h
    };
    let value = rmpv::Value::Array(vec![
        rmpv::Value::F64(1_755_000_000.0),
        rmpv::Value::Binary(vec![0x5A; 32]),
        rmpv::Value::Binary(identity.public_key_bytes().to_vec()),
        rmpv::Value::Nil,
        use_state,
    ]);
    (hash, value)
}

/// Read the file back the way `rnsd` reads it: the fifth element per entry.
fn use_states(path: &std::path::Path) -> BTreeMap<[u8; 16], rmpv::Value> {
    let bytes = std::fs::read(path).expect("known_destinations must exist after a persist");
    let value = rmpv::decode::read_value(&mut &bytes[..]).expect("msgpack map");
    let mut out = BTreeMap::new();
    for (key, val) in value.as_map().expect("a map, as the reference writes") {
        let slice = key.as_slice().expect("binary key");
        if slice.len() != 16 {
            continue;
        }
        let mut hash = [0u8; 16];
        hash.copy_from_slice(slice);
        let arr = val.as_array().expect("array value");
        assert!(
            arr.len() >= 5,
            "an entry the reference can read carries five elements, got {}",
            arr.len()
        );
        out.insert(hash, arr[4].clone());
    }
    out
}

/// A fresh destination arriving off an interface, so the persist has
/// something to be dirty about — the daemon's ordinary reason to rewrite
/// the file.
fn announce(now_secs: u64) -> Vec<u8> {
    let mut destination = Destination::new(
        Some(leviculum_std::generate_identity()),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["retained"],
    )
    .expect("a destination for the newcomer");
    let packet = destination
        .announce(None, &mut rand_core::OsRng, 0, now_secs)
        .expect("announce packet");
    let mut buf = vec![0u8; 600];
    let len = packet.pack(&mut buf).expect("pack the announce");
    buf.truncate(len);
    buf
}

#[test]
fn a_retained_destination_keeps_its_mark_across_our_persist() {
    let dir = tempfile::tempdir().expect("tempdir");

    let (retained_hash, retained) = rnsd_entry(rmpv::Value::from(RETAINED));
    let (used_hash, used) = rnsd_entry(rmpv::Value::F64(LAST_USED_SECS));
    let (fresh_hash, fresh) = rnsd_entry(rmpv::Value::from(NEVER_USED));
    let map = rmpv::Value::Map(vec![
        (rmpv::Value::Binary(retained_hash.to_vec()), retained),
        (rmpv::Value::Binary(used_hash.to_vec()), used),
        (rmpv::Value::Binary(fresh_hash.to_vec()), fresh),
    ]);
    let mut fixture = Vec::new();
    rmpv::encode::write_value(&mut fixture, &map).expect("encode the rnsd fixture");
    let path = dir.path().join("known_destinations");
    std::fs::write(&path, &fixture).expect("write the rnsd fixture");

    assert_eq!(
        use_states(&path).get(&retained_hash),
        Some(&rmpv::Value::from(RETAINED)),
        "the fixture must start retained, or this test proves nothing"
    );

    // The daemon run: open the file rnsd left, learn one destination, persist.
    {
        let mut core = core(dir.path());
        let packet = announce(core.emission_secs());
        let _ = core.handle_packet(InterfaceId(0), &packet);
        CoreStorage::flush(core.storage_mut());
    }

    let after = use_states(&path);
    assert_eq!(
        after.get(&retained_hash).and_then(rmpv::Value::as_i64),
        Some(RETAINED),
        "a destination Python pinned must still be pinned after our round-trip"
    );
    assert_eq!(
        after.get(&used_hash).and_then(rmpv::Value::as_f64),
        Some(LAST_USED_SECS),
        "a used destination must keep its recency stamp"
    );
    assert_eq!(
        after.get(&fresh_hash).and_then(rmpv::Value::as_i64),
        Some(NEVER_USED),
        "a never-used destination must stay never-used"
    );
    // The newcomer we learned this run is remembered as never used, which is
    // what `Identity.remember` writes (Identity.py:107).
    assert!(
        after.len() >= 4,
        "the destination learned this run must be in the file too, got {} entries",
        after.len()
    );
    let _ = DestinationHash::new(fresh_hash);
}
