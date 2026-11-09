//! mvr: a link to a destination that is never announced negotiates the MTU
//! its interfaces carry, not the base 500 (+ciris, leviculum#104).
//!
//! ## Mechanism
//!
//! The initiator reads the MTU it signals off the learned path's next-hop
//! interface (`Transport::next_hop_interface_hw_mtu`). A destination nobody
//! announced has no path, so the read returned `None` and the request
//! signalled the base MTU, exactly as the reference does there
//! (`Link.py:310-314`). The request itself then went out on every interface
//! (`send_on_all_interfaces`), and the responder clamps to what arrives: it
//! can lower an offer, never raise one (`Link::new_incoming`). A derived
//! one-hop address (an A/V leg in CIRISEdge) is never announced, so its link
//! sat at a Channel MDU of 425 on a punched path that carries 1400.
//!
//! With no path the request now signals the smallest MTU any interface on
//! the node signals, an unsignalled interface counting as the base
//! (`Transport::broadcast_hw_mtu`). No interface the request crosses is
//! offered more than it signals, which is the invariant every per-hop clamp
//! in the reference keeps, and a node with any unsignalled interface still
//! offers 500.
//!
//! Sans-I/O: two `NodeCore`s over `MockInterface`, no radio, no timers.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{CHANNEL_ENVELOPE_HEADER_SIZE, MTU};
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The punched path's MTU (`crate::direct_link::DIRECT_LINK_MTU`).
const DIRECT: u32 = crate::direct_link::DIRECT_LINK_MTU;
/// What a TCP interface signals (leviculum-std's `TCP_HW_MTU`).
const TCP: u32 = 16_384;

fn add_iface(node: &mut EndpointNode, name: &'static str, hw_mtu: Option<u32>) -> usize {
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

/// Both ends' negotiated MTU and the initiator's link MDU.
struct Negotiated {
    initiator: u32,
    responder: u32,
    initiator_mdu: usize,
}

/// The initiator carries `initiator_ifaces`; the responder hears the request
/// on one interface signalling `responder_iface`. The destination is never
/// announced: the initiator has its signing key out of band, as a derived
/// address's dialler does.
fn negotiate(initiator_ifaces: &[(&'static str, Option<u32>)], responder_iface: u32) -> Negotiated {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();

    let mut receiver = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "av",
        &["derived"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);
    let r_iface = add_iface(&mut receiver, "R_direct", Some(responder_iface));

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_ifaces: Vec<usize> = initiator_ifaces
        .iter()
        .map(|(name, mtu)| add_iface(&mut sender, name, *mtu))
        .collect();
    // The punched interface is the last one listed.
    let s_iface = *s_ifaces.last().expect("an interface");

    let (sender_link, routed, out) = sender.connect(dest_hash, &signing_key).expect("connect");
    assert!(!routed, "never announced: the request has no path to take");

    let mut receiver_link: Option<LinkId> = None;
    let mut for_receiver = action_data(&out);
    assert_eq!(for_receiver.len(), 1, "one request, broadcast");
    for _ in 0..8 {
        if for_receiver.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for pkt in for_receiver {
            let o = receiver.handle_packet(InterfaceId(r_iface), &pkt);
            for ev in &o.events {
                if let NodeEvent::LinkEstablished { link_id, .. } = ev {
                    receiver_link = Some(*link_id);
                }
            }
            back.extend(action_data(&o));
        }
        let mut next = Vec::new();
        for pkt in back {
            next.extend(action_data(
                &sender.handle_packet(InterfaceId(s_iface), &pkt),
            ));
        }
        for_receiver = next;
    }

    let receiver_link = receiver_link.expect("the responder establishes");
    let initiator = sender.link(&sender_link).expect("initiator link");
    assert!(initiator.is_active(), "the proof arrived");
    Negotiated {
        initiator: initiator.negotiated_mtu(),
        responder: receiver.link(&receiver_link).unwrap().negotiated_mtu(),
        initiator_mdu: initiator.mdu(),
    }
}

/// The field case: TCP to a transport node plus a punched direct path, and a
/// link to a derived address the peer answers on the direct path. Before the
/// fix both ends settled on 500 (Channel MDU 425, edge's 400-byte frame
/// budget); now on the punched path's 1400 (Channel MDU 1321).
#[test]
fn a_never_announced_destination_negotiates_its_carriers_mtu() {
    let n = negotiate(&[("S_tcp", Some(TCP)), ("S_direct", Some(DIRECT))], DIRECT);
    std::eprintln!(
        "initiator={} responder={} link_mdu={}",
        n.initiator,
        n.responder,
        n.initiator_mdu
    );
    assert_eq!(n.responder, DIRECT, "the responder sees the punched MTU");
    assert_eq!(n.initiator, DIRECT, "and confirms it to the initiator");
    // floor((1400 - 1 - 19 - 48) / 16) * 16 - 1
    assert_eq!(n.initiator_mdu, 1327);
    assert_eq!(n.initiator_mdu - CHANNEL_ENVELOPE_HEADER_SIZE, 1321);
}

/// The offer is the smallest any interface signals: the responder's larger
/// interface does not lift a link past the initiator's narrowest carrier.
#[test]
fn the_offer_is_the_narrowest_interface() {
    let n = negotiate(&[("S_tcp", Some(TCP)), ("S_direct", Some(DIRECT))], TCP);
    assert_eq!((n.initiator, n.responder), (DIRECT, DIRECT));
}

/// Compatibility: an interface that signals no MTU (a UDPInterface, Codeberg
/// #357) anywhere on the initiator keeps the reference's base offer.
#[test]
fn an_unsignalled_interface_keeps_the_base_offer() {
    let n = negotiate(
        &[
            ("S_udp", None),
            ("S_tcp", Some(TCP)),
            ("S_direct", Some(DIRECT)),
        ],
        DIRECT,
    );
    assert_eq!((n.initiator, n.responder), (MTU as u32, MTU as u32));
}
