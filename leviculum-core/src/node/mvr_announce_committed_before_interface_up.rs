//! mvr (order 1317): an announce the core committed before an interface
//! registered, and that the driver dispatches after it, reaches that
//! interface twice.
//!
//! ## The field record
//!
//! `loquax-android/tests/facade.rs` `a_file_travels_both_ways_with_python_lxmf`,
//! red under load: the phone's mailbox (`loquax::home::daemon::run`, a node
//! of its own with a TCP client to a Python peer) sent its home destination
//! twice and then its delivery destination on a connection that had just
//! registered, and the Python peer held the third announce (the other mvr of
//! this order, `leviculum-std/tests/mvr/a_third_announce_on_a_fresh_interface_waits_at_python.rs`).
//! The mailbox's event log of one red run:
//!
//! ```text
//! 03:38:43.491979 PKT_TX ph=616e7fa9 iface="bcast"        len=167   home, committed
//! 03:38:43.497461 New connection: tcp_client_1 (iface:1)             registered
//! 03:38:43.497618 PKT_TX ph=6b6a43ba iface=tcp_client_1  len=167   home, interface-up
//! 03:38:43.498160 PKT_TX ph=ceb05c07 iface="bcast"        len=211   delivery
//! ```
//!
//! and the Python peer counted three announces on that connection, the
//! committed home broadcast reaching it after the interface-up copy.
//!
//! ## The driver order behind it
//!
//! `ReticulumNode::announce_destination` (`leviculum-std/src/driver/mod.rs`)
//! goes through `dispatch_committed`: the core builds the announce and caches
//! it, and the `TickOutput` with its `Broadcast` waits in the action channel
//! for the event loop. When the loop takes the new-interface branch first, it
//! registers the interface and calls `handle_interface_up`, whose Block-D
//! re-announce (`announce_local_destinations`) finds the fresh cache and
//! sends the destination on the new interface; the queued `Broadcast` is
//! dispatched next and reaches the new interface too. Which branch the loop
//! takes first is the scheduler's, which is why the field test went red only
//! beside other load.
//!
//! ## Why the pin is not here (order 1320)
//!
//! Order 1317 pinned the order on one core: commit, register, interface up,
//! dispatch the interface-up output, then the committed one. The control
//! below makes the same three core calls and dispatches in the other order.
//! The core cannot tell the two apart, it sees identical calls, so no core
//! change can make the pin send once without making the control send
//! nothing. The duplicate is the driver's: it dispatched an output after a
//! later interface registered. The fix drains the action channel before a
//! new interface registers (Branch 5 of `run_event_loop`), and the pin moved
//! beside it, `leviculum-std/src/driver/mod.rs`
//! `an_announce_committed_before_a_new_interface_reaches_it_once`.
//!
//! What stays here is the core's half of the contract: an output dispatched
//! before the interface registers reaches nobody new, and the interface-up
//! sends the destination once.

extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use alloc::collections::BTreeMap;
use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{dispatch_actions, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The TCP client the rendezvous dial registers.
const NEW: usize = 1;

/// The phone's mailbox: a node with transport off and one destination.
fn mailbox() -> (Node, DestinationHash) {
    let mut node = NodeCoreBuilder::new().enable_transport(false).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr1317",
        &["home"],
    )
    .unwrap();
    let hash = *dest.hash();
    node.register_destination(dest);
    (node, hash)
}

/// Dispatch `outputs` in order into a driver slice of the new interface
/// alone, and count the announces of `hash` it carried.
fn announces_on_new(outputs: Vec<TickOutput>, hash: &DestinationHash) -> usize {
    let mut new = MockInterface::new("tcp_client_1", NEW as u8);
    for output in outputs {
        let mut slice: Vec<&mut dyn crate::traits::Interface> = vec![&mut new];
        let _ = dispatch_actions(&mut slice, output.actions, &BTreeMap::new());
    }
    new.sent
        .iter()
        .filter_map(|raw| Packet::unpack(raw).ok())
        .filter(|p| {
            p.flags.packet_type == PacketType::Announce && &p.destination_hash == hash.as_bytes()
        })
        .count()
}

/// Control: the committed announce dispatched before the interface
/// registers (the loop took the action channel first) reaches nobody new,
/// and the interface-up sends it once.
#[test]
fn control_an_announce_dispatched_before_the_interface_registers_reaches_it_once() {
    let (mut node, hash) = mailbox();
    let committed = node.announce_destination(&hash, None).unwrap();
    let mut none: Vec<&mut dyn crate::traits::Interface> = Vec::new();
    let _ = dispatch_actions(&mut none, committed.actions, &BTreeMap::new());

    node.set_interface_name(NEW, String::from("tcp_client_1"));
    let up = node.handle_interface_up(NEW);

    assert_eq!(announces_on_new(vec![up], &hash), 1);
}
