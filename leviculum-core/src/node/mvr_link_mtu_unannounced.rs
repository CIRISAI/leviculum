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
//! With no path the request now goes out as one copy per interface, each
//! signalling what its interface signals: a copy at the interface's MTU on
//! every signalled interface, and one broadcast at the base MTU on all the
//! others (`Transport::send_link_request_on_all_interfaces`). Signalling is
//! not part of the link id, so the copies are one request, rewritten per hop
//! the way a reference relay rewrites it. No interface is offered more than
//! it signals, and an unsignalled one (a UDPInterface, Codeberg #357) on the
//! same node no longer pulls the punched path down to 500.
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

/// The copy of the request that leaves on `iface`: its own `SendPacket` when
/// the interface signals an MTU, else the base broadcast that does not
/// exclude it.
fn request_on(out: &TickOutput, iface: usize) -> Vec<u8> {
    let mut found: Vec<Vec<u8>> = out
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface: i, data, .. } if i.0 == iface => Some(data.clone()),
            Action::Broadcast {
                data,
                exclude_ifaces,
                exclude_iface,
                ..
            } if !exclude_ifaces.contains(&InterfaceId(iface))
                && *exclude_iface != Some(InterfaceId(iface)) =>
            {
                Some(data.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one copy leaves on interface {iface}"
    );
    found.pop().unwrap()
}

/// The MTU a request copy signals.
fn signalled(request: &[u8]) -> u32 {
    let tail: [u8; 3] = request[request.len() - 3..].try_into().unwrap();
    crate::link::decode_signaling_bytes(&tail).0
}

/// Both ends' negotiated MTU and the initiator's link MDU.
struct Negotiated {
    initiator: u32,
    responder: u32,
    initiator_mdu: usize,
}

/// The initiator carries `initiator_ifaces` and the responder hears the
/// copy that leaves on the LAST of them, on one interface signalling
/// `responder_iface`. The destination is never announced: the initiator has
/// its signing key out of band, as a derived address's dialler does.
fn negotiate(
    initiator_ifaces: &[(&'static str, Option<u32>)],
    responder_iface: Option<u32>,
) -> Negotiated {
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
    let r_iface = add_iface(&mut receiver, "R_leg", responder_iface);

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_ifaces: Vec<usize> = initiator_ifaces
        .iter()
        .map(|(name, mtu)| add_iface(&mut sender, name, *mtu))
        .collect();
    let s_iface = *s_ifaces.last().expect("an interface");

    let (sender_link, routed, out) = sender.connect(dest_hash, &signing_key).expect("connect");
    assert!(!routed, "never announced: the request has no path to take");

    let mut receiver_link: Option<LinkId> = None;
    let mut for_receiver = std::vec![request_on(&out, s_iface)];
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

/// The field case: a plain UDP interface, TCP to a transport node, and a
/// punched direct path, and a link to a derived address the peer answers on
/// the direct path. Before the fix both ends settled on 500 (Channel MDU 425,
/// edge's 244-byte frame budget); now on the punched path's 1400 (Channel
/// MDU 1321), the unsignalled UDP interface notwithstanding.
#[test]
fn a_never_announced_destination_negotiates_its_carriers_mtu() {
    let n = negotiate(
        &[
            ("S_udp", None),
            ("S_tcp", Some(TCP)),
            ("S_direct", Some(DIRECT)),
        ],
        Some(DIRECT),
    );
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

/// Each signalled interface gets a copy at its own MTU, every other one the
/// base broadcast, and all copies are the same request: they differ only in
/// the signalling bytes, which the link id leaves out.
#[test]
fn each_interface_is_offered_what_it_signals() {
    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let udp = add_iface(&mut sender, "S_udp", None);
    let tcp = add_iface(&mut sender, "S_tcp", Some(TCP));
    let direct = add_iface(&mut sender, "S_direct", Some(DIRECT));
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "av",
        &["derived"],
    )
    .unwrap();
    let (link_id, _, out) = sender
        .connect(*dest.hash(), &identity.ed25519_verifying().to_bytes())
        .unwrap();
    let copies = [
        (udp, request_on(&out, udp)),
        (tcp, request_on(&out, tcp)),
        (direct, request_on(&out, direct)),
    ];
    let offered: Vec<(usize, u32)> = copies.iter().map(|(i, c)| (*i, signalled(c))).collect();
    assert_eq!(
        offered,
        std::vec![(udp, MTU as u32), (tcp, TCP), (direct, DIRECT)]
    );
    for (_, copy) in &copies {
        assert_eq!(copy.len(), copies[0].1.len());
        assert_eq!(
            copy[..copy.len() - 3],
            copies[0].1[..copy.len() - 3],
            "one request"
        );
        assert_eq!(crate::link::Link::calculate_link_id(copy), link_id);
    }
}

/// Compatibility: a responder hearing the copy on an interface that signals
/// no MTU (a UDPInterface, Codeberg #357) runs the link at the base.
#[test]
fn an_unsignalled_leg_runs_at_the_base() {
    let n = negotiate(
        &[
            ("S_tcp", Some(TCP)),
            ("S_direct", Some(DIRECT)),
            ("S_udp", None),
        ],
        None,
    );
    assert_eq!((n.initiator, n.responder), (MTU as u32, MTU as u32));
}
