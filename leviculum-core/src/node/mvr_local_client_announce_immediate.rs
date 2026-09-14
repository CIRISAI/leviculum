//! A local client's announce goes on the air in the pass that received it,
//! with no hold added by the core.
//!
//! ## The reference
//!
//! Python-RNS treats an announce from a local client as the one case that
//! is not held at all: "If the announce is from a local client, it is
//! announced immediately, but only one time" — `retransmit_timeout = now`
//! and `retries = Transport.PATHFINDER_R`
//! (`reference/Reticulum/RNS/Transport.py:1890-1894`). Every other
//! announce it inserts gets the random rebroadcast window
//! (`retransmit_timeout = now + (RNS.rand() * Transport.PATHFINDER_RW)`,
//! `reference/Reticulum/RNS/Transport.py:1873`), which
//! [`super::mvr_announce_rebroadcast_window`] pins.
//!
//! ## Ours, until 2026-09-14
//!
//! `handle_announce` added `LOCAL_CLIENT_ANNOUNCE_DELAY_MS` (250 ms) to the
//! first announce of each local-client destination, to batch a burst of
//! registrations at instance start-up. Wire format and semantics were
//! untouched by it, but the deviation rule's third condition — a measurable
//! improvement of priority 1 — was never met: no measurement was ever taken
//! for it. And since `ead1632` ("nrf: every LoRa key-up jitters and listens
//! before it talks") the interface spaces its own transmissions, so the hold
//! was both duplicated and sitting one layer below where the
//! interface-isolation rule puts collision avoidance. It is gone.
//!
//! ## What this pins
//!
//! The client's first announce is due at `now` — not "soon", not "within a
//! window". A caller that registers a destination and immediately asks the
//! mesh for it must find it there, and a re-introduced hold at this layer
//! (however small, however well-argued) makes that a race. `retries` stays
//! at `PATHFINDER_RETRIES`, so it still fires exactly once, which is the
//! other half of the reference's sentence.
//!
//! Sans-I/O: 1 node, 2 mock interfaces, deterministic, sub-second.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, PATHFINDER_RETRIES, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::{Action, InterfaceId, TickOutput};

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// A shared instance: one network interface, one local-client IPC
/// interface. `enable_transport` is off deliberately — rebroadcasting a
/// local client's announce is what makes our own registered destination
/// reachable at all, and is orthogonal to the transit role.
fn make_shared_instance() -> (Node, usize, usize) {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: Node = NodeCoreBuilder::new().enable_transport(false).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    );
    let net = node
        .transport
        .register_interface(Box::new(MockInterface::new("net", 0)));
    node.set_interface_name(net, String::from("tcp_server/0.0.0.0:4242"));
    let client = node
        .transport
        .register_interface(Box::new(MockInterface::new("client", 1)));
    node.set_interface_name(client, String::from("Local[rns/default]/0"));
    node.transport.set_local_client(client, true);
    (node, net, client)
}

/// A destination the client hosts, and the raw announce it sends over the
/// IPC to register it.
fn client_registration() -> ([u8; TRUNCATED_HASHBYTES], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["localimmediate"],
    )
    .unwrap();
    let dest_hash = *dest.hash().as_bytes();
    let announce = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).unwrap();
    (dest_hash, buf[..len].to_vec())
}

/// Announce transmissions for `dest`, in any form.
fn announce_tx_count(out: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> usize {
    out.actions
        .iter()
        .filter(|a| {
            let data = match a {
                Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
            };
            Packet::unpack(data)
                .map(|p| p.flags.packet_type == PacketType::Announce && &p.destination_hash == dest)
                .unwrap_or(false)
        })
        .count()
}

/// THE pin: the first announce of a local-client destination is scheduled
/// for `now`, and goes out on the next scheduler pass without the clock
/// having to move.
#[test]
fn a_local_clients_first_announce_is_due_now() {
    let (mut instance, _net, client) = make_shared_instance();
    let (dest, announce_raw) = client_registration();

    let t0 = instance.transport().clock().now_ms();

    let out = instance.handle_packet(InterfaceId(client), &announce_raw);
    assert_eq!(
        announce_tx_count(&out, &dest),
        0,
        "the receiving pass inserts the entry; the scheduler transmits \
         (Python parity, Transport.py:1896-1906 + :589-622)"
    );

    let entry = instance
        .transport()
        .storage()
        .get_announce(&dest)
        .expect("the registration is queued for rebroadcast");
    assert_eq!(
        entry.retransmit_at_ms,
        Some(t0),
        "a local client's announce is due now, with no hold added by the \
         core (`retransmit_timeout = now`, Transport.py:1894)"
    );
    assert_eq!(
        entry.retries, PATHFINDER_RETRIES,
        "and only one time: starting at PATHFINDER_R is what removes the \
         entry after a single fire (Transport.py:1895)"
    );

    // The clock does not move. If anything at this layer still held the
    // announce back, this pass would be empty.
    let out = instance.handle_timeout();
    assert_eq!(
        announce_tx_count(&out, &dest),
        1,
        "the registration goes on the air at t0, in the first scheduler \
         pass after it was received"
    );
}
