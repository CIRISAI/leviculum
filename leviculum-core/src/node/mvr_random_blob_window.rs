//! Item 3 of the 2026-09-21 hygiene batch: a saturated random-blob
//! window carries twice the heap it needs.
//!
//! ## The leak
//!
//! `handle_announce` rebuilt the window as clone, push, drain: the
//! clone allocates room for exactly the old length, the push finds it
//! full and doubles it, and the drain lowers the length without giving
//! the capacity back. At `MAX_RANDOM_BLOBS` = 64 that is 1280 bytes of
//! blob heap per saturated path where 640 would do — on a table with
//! one entry per destination the node can reach, and the diagnostic
//! dump modelled the smaller number.
//!
//! This pins it where it actually happens: at the stored `PathEntry`
//! after a destination has announced its way past the cap. The pure
//! window function has its own tests in `storage_types`; this is the
//! call site, because a helper that is never called fixes nothing.
//!
//! Sans-I/O: 1 node, 1 mock interface, deterministic, sub-second.

extern crate std;

use std::string::String;

use rand_core::OsRng;

use crate::constants::{MAX_RANDOM_BLOBS, MTU};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::InterfaceId;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// THE pin: however long a destination keeps announcing, its blob
/// window holds the heap it uses and not a byte more.
#[test]
fn a_destination_that_keeps_announcing_does_not_double_its_blob_heap() {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: Node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
    let iface = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new("A_in", 0)));
    node.set_interface_name(iface, String::from("A_in"));

    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["blobwindow"],
    )
    .unwrap();
    let dest_hash = *dest.hash().as_bytes();

    // Well past the cap, so the window saturates and keeps being
    // appended to afterwards — the state a long-lived node's path table
    // is in for every neighbour it can hear.
    let announces = MAX_RANDOM_BLOBS + 16;
    let step = node.transport().config().announce_rate_limit_ms + 1;
    for _ in 0..announces {
        let now = node.transport().clock().now_ms();
        let ann = dest
            .announce(None, &mut OsRng, now, now / 1000)
            .expect("a destination can always announce itself");
        let mut buf = [0u8; MTU];
        let len = ann.pack(&mut buf).unwrap();
        let _ = node.handle_packet(InterfaceId(iface), &buf[..len]);
        node.transport().clock().set(now + step);
    }

    let blobs = &node
        .transport()
        .storage()
        .get_path(&dest_hash)
        .expect("the announces installed a path")
        .random_blobs;

    assert_eq!(
        blobs.len(),
        MAX_RANDOM_BLOBS,
        "the window saturates at the cap"
    );
    assert_eq!(
        blobs.capacity(),
        blobs.len(),
        "a saturated window holds {} blobs in room for {} — that slack is per path, \
         on a table with one entry per reachable destination",
        blobs.len(),
        blobs.capacity(),
    );
}
