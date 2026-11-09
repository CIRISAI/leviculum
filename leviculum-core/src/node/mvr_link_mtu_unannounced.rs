//! mvr: a link request with no path (+ciris, leviculum#104).
//!
//! ## Mechanism
//!
//! A destination nobody announced has no path, so the request goes out on
//! every interface and signals the base MTU, as the reference does
//! (`Link.py:310-314`). That held a derived one-hop address (an A/V leg in
//! CIRISEdge) at 500 on a punched path that carries 1400. The fix adds one
//! copy at its own MTU on each direct interface a DirectLink upgrade
//! attached, and leaves the broadcast exactly as it was everywhere else
//! (`Transport::send_link_request_on_all_interfaces`; the direct case is
//! witnessed in `mvr_direct_link`). Copies on shared media would hand a
//! Python responder one link per copy: its dedup keys on the full packet
//! hash, signalling bytes included (`Transport.py:1376`), and
//! `Destination.incoming_link_request` never looks the link id up
//! (`Destination.py:420-424`).
//!
//! A copy and the broadcast are one request (signalling is not part of the
//! link id) that a leviculum responder or relay may hear twice. The tests
//! here pin that it then makes one link, proves once and forwards once,
//! and that a same-interface repeat is still treated as a retry.
//!
//! Sans-I/O: `NodeCore`s over `MockInterface`, no radio, no timers.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::MTU;
use crate::destination::{Destination, DestinationHash, DestinationType, Direction};
use crate::identity::Identity;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{resignal_link_request, Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The punched path's MTU (`crate::direct_link::DIRECT_LINK_MTU`).
const DIRECT: u32 = crate::direct_link::DIRECT_LINK_MTU;
/// What a TCP interface signals (leviculum-std's `TCP_HW_MTU`).
const TCP: u32 = 16_384;
/// What an AutoInterface signals (leviculum-std's `AUTO_HW_MTU`).
const AUTO: u32 = 1_196;

fn add_iface<C: crate::traits::Clock, S: crate::traits::Storage>(
    node: &mut NodeCore<OsRng, C, S>,
    name: &'static str,
    hw_mtu: Option<u32>,
) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    if let Some(mtu) = hw_mtu {
        node.set_interface_hw_mtu(idx, mtu);
    }
    idx
}

fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

/// The MTU a request signals.
fn signalled(request: &[u8]) -> u32 {
    let tail: [u8; 3] = request[request.len() - 3..].try_into().unwrap();
    crate::link::decode_signaling_bytes(&tail).0
}

/// A destination that accepts links and is never announced, with its
/// signing key and its announce (for a relay that must learn it).
fn derived() -> (Destination, DestinationHash, [u8; 32], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let key = identity.ed25519_verifying().to_bytes();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "av",
        &["derived"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    let announce = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).unwrap();
    (dest, hash, key, buf[..len].to_vec())
}

fn endpoint() -> EndpointNode {
    NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage)
}

/// Without a direct interface the request is the one broadcast it always
/// was, at the base MTU, whatever the shared media signal: Auto, plain UDP
/// and TCP each get exactly one copy, the same one.
#[test]
fn without_a_direct_interface_it_is_one_broadcast_at_the_base() {
    let mut sender = endpoint();
    add_iface(&mut sender, "S_auto", Some(AUTO));
    add_iface(&mut sender, "S_udp", None);
    add_iface(&mut sender, "S_tcp", Some(TCP));
    let (_, hash, key, _) = derived();
    let (_, routed, out) = sender.connect(hash, &key).unwrap();
    assert!(!routed);
    match out.actions.as_slice() {
        [Action::Broadcast {
            data,
            exclude_ifaces,
            exclude_iface: None,
            ..
        }] => {
            assert!(exclude_ifaces.is_empty(), "every medium, one copy");
            assert_eq!(signalled(data), MTU as u32);
        }
        other => panic!("expected one broadcast, got {other:?}"),
    }
}

/// A responder that hears the request and a re-signalled sibling of it,
/// each on its own interface. Per copy in order: packets sent back; then
/// the link table size and the one link's MTU.
fn responder_hears_both(direct_first: bool) -> (Vec<usize>, usize, u32) {
    let (dest, hash, key, _) = derived();
    let mut receiver = endpoint();
    receiver.register_destination(dest);
    let r_tcp = add_iface(&mut receiver, "R_tcp", Some(TCP));
    let r_direct = add_iface(&mut receiver, "R_direct", Some(DIRECT));
    let mut sender = endpoint();
    add_iface(&mut sender, "S", Some(TCP));
    let (link_id, _, out) = sender.connect(hash, &key).unwrap();
    let request = action_data(&out).pop().unwrap();
    let tcp_copy = (r_tcp, resignal_link_request(&request, TCP));
    let direct_copy = (r_direct, resignal_link_request(&request, DIRECT));
    let order = if direct_first {
        [direct_copy, tcp_copy]
    } else {
        [tcp_copy, direct_copy]
    };
    let back = order
        .iter()
        .map(|(i, c)| action_data(&receiver.handle_packet(InterfaceId(*i), c)).len())
        .collect();
    let mtu = receiver.link(&link_id).expect("the link").negotiated_mtu();
    (back, receiver.links.len(), mtu)
}

/// A responder hearing two copies makes one link and proves once. The first
/// copy wins, at the smaller of its offer and its receiving interface's
/// MTU; the later one changes nothing. The rule is fixed (first arrival);
/// which copy is first is the media's.
#[test]
fn a_responder_hearing_two_copies_makes_one_link_and_proves_once() {
    assert_eq!(responder_hears_both(true), (std::vec![1, 0], 1, DIRECT));
    assert_eq!(responder_hears_both(false), (std::vec![1, 0], 1, TCP));
}

/// A repeat on the interface the link is attached to is still a retry
/// whose proof was lost, and gets the cached proof again.
#[test]
fn a_same_interface_repeat_is_proved_again() {
    let (dest, hash, key, _) = derived();
    let mut receiver = endpoint();
    receiver.register_destination(dest);
    let r = add_iface(&mut receiver, "R", None);
    let mut sender = endpoint();
    add_iface(&mut sender, "S", None);
    let (_, _, out) = sender.connect(hash, &key).unwrap();
    let request = action_data(&out).pop().unwrap();
    let first = action_data(&receiver.handle_packet(InterfaceId(r), &request)).len();
    let again = action_data(&receiver.handle_packet(InterfaceId(r), &request)).len();
    assert_eq!((first, again), (1, 1));
}

/// A relay serving a local client that owns the destination (the one
/// transit case a Type1 request reaches; any other drops it as overheard),
/// hearing the request on `first` and then `second` with the given MTUs.
/// Forwards to the client per copy.
fn relay_forwards(first: usize, second: usize, second_mtu: u32) -> (usize, usize) {
    let (_, hash, key, announce) = derived();
    let mut relay = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        crate::memory_storage::MemoryStorage::with_defaults(),
    );
    let client = add_iface(&mut relay, "A_client", None);
    relay.set_interface_local_client(client, true);
    let ifaces = [
        add_iface(&mut relay, "A_tcp", Some(TCP)),
        add_iface(&mut relay, "A_direct", Some(DIRECT)),
    ];
    let _ = relay.handle_packet(InterfaceId(client), &announce);

    let mut sender = endpoint();
    add_iface(&mut sender, "S", Some(TCP));
    let (_, _, out) = sender.connect(hash, &key).unwrap();
    let request = action_data(&out).pop().unwrap();
    let to_client = |o: &TickOutput| {
        o.actions
            .iter()
            .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == client))
            .count()
    };
    let a = relay.handle_packet(InterfaceId(ifaces[first]), &request);
    let b = relay.handle_packet(
        InterfaceId(ifaces[second]),
        &resignal_link_request(&request, second_mtu),
    );
    (to_client(&a), to_client(&b))
}

/// A relay hearing two copies on two interfaces forwards one. Before, the
/// second overwrote the first's link entry and went to the client too.
#[test]
fn a_relay_hearing_two_copies_forwards_one() {
    assert_eq!(relay_forwards(1, 0, TCP), (1, 0));
}

/// A same-interface repeat of the same bytes (a client older than Codeberg
/// #66 retrying) is still forwarded.
#[test]
fn a_relay_forwards_a_same_interface_repeat() {
    // Re-signalled at the base the request already carries: the same bytes.
    assert_eq!(relay_forwards(0, 0, MTU as u32), (1, 1));
}
