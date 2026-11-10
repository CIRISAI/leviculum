//! Order 1584: a renamed client's announce reaches the network whichever
//! order the instance hears the new client and the old one's departure in.
//!
//! Two `loquax send` runs back to back, one identity, the second with
//! `--from`: each is a short-lived local client that announces once. On an
//! idle host the instance handles the first client's disconnect before the
//! second connects; under load it may not. Every ordering below must put the
//! second (renamed) announce on the network interface.
//!
//! Three of the five orderings (the second announce while the first
//! client's hops-0 path still stands) are red without order 1488's
//! `local_client_same_second` arm in `handle_announce` and green with it;
//! the two where the departure comes first are green either way. The
//! land-gate red that order 1584 chased (Python heard one announce in
//! 40 s) did not reproduce in 42 loaded runs, and no ordering here loses
//! the announce, so the instance's acceptance rule is not where it went.
//!
//! Sans-I/O: 1 node, 3 mock interfaces, deterministic, sub-second.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

const NEW_NAME: &[u8] = b"lew@schneckenschreck";

struct Instance {
    node: Node,
    net: usize,
    first: usize,
    second: usize,
}

fn make_instance() -> Instance {
    let mut node: Node = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let net = node
        .transport
        .register_interface(Box::new(MockInterface::new("net", 0)));
    node.set_interface_name(net, String::from("tcp_client_0"));
    let first = node
        .transport
        .register_interface(Box::new(MockInterface::new("first", 1)));
    node.set_interface_name(first, String::from("Local[rns/x]/0"));
    node.transport.set_local_client(first, true);
    let second = node
        .transport
        .register_interface(Box::new(MockInterface::new("second", 2)));
    node.set_interface_name(second, String::from("Local[rns/x]/1"));
    node.transport.set_local_client(second, true);
    Instance {
        node,
        net,
        first,
        second,
    }
}

fn announce(dest: &mut Destination, name: &[u8], secs: u64) -> Vec<u8> {
    let packet = dest
        .announce(Some(name), &mut OsRng, secs * 1000, secs)
        .expect("announce");
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack");
    buf[..len].to_vec()
}

/// `raw` as a Python transport node rebroadcasts it back to us: header
/// type 2 with its own transport id, one hop on the wire.
fn echo(raw: &[u8]) -> Vec<u8> {
    let mut packet = Packet::unpack(raw).expect("unpack");
    packet.flags.header_type = HeaderType::Type2;
    packet.flags.transport_type = TransportType::Transport;
    packet.transport_id = Some([0x77; TRUNCATED_HASHBYTES]);
    packet.hops = 1;
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack");
    buf[..len].to_vec()
}

/// Whether an action in `out` puts a packet carrying `needle` on `iface`.
fn reaches(out: &TickOutput, iface: usize, needle: &[u8]) -> bool {
    out.actions.iter().any(|a| {
        let (data, to_iface) = match a {
            Action::SendPacket { iface: i, data, .. } => (data, i.0 == iface),
            Action::Broadcast {
                data,
                exclude_iface,
                exclude_ifaces,
                ..
            } => (
                data,
                *exclude_iface != Some(InterfaceId(iface))
                    && !exclude_ifaces.contains(&InterfaceId(iface)),
            ),
        };
        to_iface && data.windows(needle.len()).any(|w| w == needle)
    })
}

/// Drive the timers for two seconds; true once the renamed announce left
/// on the network interface.
fn renamed_leaves(i: &mut Instance) -> bool {
    let mut left = false;
    for _ in 0..20 {
        let out = i.node.handle_timeout();
        left |= reaches(&out, i.net, NEW_NAME);
        i.node.transport.clock().advance(100);
    }
    left
}

enum Order {
    /// The first client's departure is handled before the second arrives.
    DownThenSecond,
    /// The second client's announce arrives while the first still stands.
    SecondThenDown,
    /// As above, and the departure is handled before the scheduler pass.
    SecondDownThenPoll,
    /// The departure, then the network's echo of the first announce, then
    /// the second client.
    DownEchoSecond,
    /// The network's echo of the first announce, the second client, the
    /// departure.
    EchoSecondDown,
}

fn run(order: Order, gap_ms: u64) -> bool {
    let mut i = make_instance();
    let mut dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .expect("destination");
    let secs = TEST_TIME_MS / 1000;
    let first = announce(&mut dest, b"lew", secs);
    let _ = i.node.handle_packet(InterfaceId(i.first), &first);
    let out = i.node.handle_timeout();
    assert!(reaches(&out, i.net, b"lew"), "the first announce leaves");
    i.node.transport.clock().advance(gap_ms);
    let second = announce(&mut dest, NEW_NAME, secs);
    let mut left = false;
    match order {
        Order::DownThenSecond => {
            let _ = i.node.handle_interface_down(InterfaceId(i.first));
            let out = i.node.handle_packet(InterfaceId(i.second), &second);
            left |= reaches(&out, i.net, NEW_NAME);
        }
        Order::SecondThenDown => {
            let out = i.node.handle_packet(InterfaceId(i.second), &second);
            left |= reaches(&out, i.net, NEW_NAME);
            let out = i.node.handle_timeout();
            left |= reaches(&out, i.net, NEW_NAME);
            let out = i.node.handle_interface_down(InterfaceId(i.first));
            left |= reaches(&out, i.net, NEW_NAME);
        }
        Order::SecondDownThenPoll => {
            let out = i.node.handle_packet(InterfaceId(i.second), &second);
            left |= reaches(&out, i.net, NEW_NAME);
            let out = i.node.handle_interface_down(InterfaceId(i.first));
            left |= reaches(&out, i.net, NEW_NAME);
        }
        Order::DownEchoSecond => {
            let _ = i.node.handle_interface_down(InterfaceId(i.first));
            let _ = i.node.handle_packet(InterfaceId(i.net), &echo(&first));
            let _ = i.node.handle_timeout();
            let out = i.node.handle_packet(InterfaceId(i.second), &second);
            left |= reaches(&out, i.net, NEW_NAME);
        }
        Order::EchoSecondDown => {
            let _ = i.node.handle_packet(InterfaceId(i.net), &echo(&first));
            let _ = i.node.handle_timeout();
            let out = i.node.handle_packet(InterfaceId(i.second), &second);
            left |= reaches(&out, i.net, NEW_NAME);
            let out = i.node.handle_interface_down(InterfaceId(i.first));
            left |= reaches(&out, i.net, NEW_NAME);
        }
    }
    left | renamed_leaves(&mut i)
}

#[test]
fn renamed_after_the_departure_reaches_the_network() {
    assert!(run(Order::DownThenSecond, 100));
    assert!(run(Order::DownThenSecond, 700));
}

#[test]
fn renamed_before_the_departure_reaches_the_network() {
    assert!(run(Order::SecondThenDown, 100));
    assert!(run(Order::SecondThenDown, 700));
}

#[test]
fn renamed_before_the_departure_and_the_pass_reaches_the_network() {
    assert!(run(Order::SecondDownThenPoll, 100));
    assert!(run(Order::SecondDownThenPoll, 700));
}

#[test]
fn renamed_after_the_departure_and_the_echo_reaches_the_network() {
    assert!(run(Order::DownEchoSecond, 450), "450");
    assert!(run(Order::DownEchoSecond, 700), "700");
}

#[test]
fn renamed_after_the_echo_before_the_departure_reaches_the_network() {
    assert!(run(Order::EchoSecondDown, 450), "450");
    assert!(run(Order::EchoSecondDown, 700), "700");
}
