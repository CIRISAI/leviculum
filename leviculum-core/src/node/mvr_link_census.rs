//! mvr: the link census counts live links by role and destination, and
//! links leave it when they close (CIRISEdge#819, leviculum#57).
//!
//! Sans-I/O: two endpoints over one mock mesh hop.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::link::{LinkCloseReason, LinkState};
use crate::node::{LinkCensus, LinkRole, NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

type Node = NodeCore<OsRng, MockClock, crate::traits::NoStorage>;

fn add_iface(node: &mut Node, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

fn deliver(target: &mut Node, iface: usize, pkts: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for pkt in pkts {
        out.extend(packets(&target.handle_packet(InterfaceId(iface), &pkt)));
    }
    out
}

#[test]
fn the_census_counts_live_links_by_role_and_destination() {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut b = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        crate::traits::NoStorage,
    );
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["census"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let dest_hash = *dest.hash();
    b.register_destination(dest);
    let mut a = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        crate::traits::NoStorage,
    );
    let b_mesh = add_iface(&mut b, "B_mesh");
    let a_mesh = add_iface(&mut a, "A_mesh");
    assert_eq!(a.link_census(), LinkCensus::default());

    // Three links from A to B's destination; the last stays pending.
    let mut ids = Vec::new();
    for i in 0..3 {
        let (link_id, _, out) = a.connect(dest_hash, &signing_key).expect("connect");
        ids.push(link_id);
        if i == 2 {
            break;
        }
        let mut to_b = packets(&out);
        for _ in 0..8 {
            if to_b.is_empty() {
                break;
            }
            let back = deliver(&mut b, b_mesh, to_b);
            to_b = deliver(&mut a, a_mesh, back);
        }
    }

    let census = a.link_census();
    assert_eq!(
        (census.initiator, census.responder, census.pending),
        (2, 0, 1)
    );
    assert_eq!(census.by_destination.len(), 1);
    assert_eq!(census.by_destination[0].destination_hash, dest_hash);
    assert_eq!(census.by_destination[0].initiator, 2);

    let census = b.link_census();
    assert_eq!((census.initiator, census.responder), (0, 2));
    assert_eq!(census.by_destination[0].responder, 2);

    // The list names each link with its role and state; idle and age are
    // known only once established.
    let list = a.link_list();
    assert_eq!(list.len(), 3);
    let live: Vec<_> = list.iter().filter(|l| l.age_secs.is_some()).collect();
    assert_eq!(live.len(), 2);
    assert!(live
        .iter()
        .all(|l| l.role == LinkRole::Initiator && l.state == LinkState::Active));
    assert!(b.link_list().iter().all(|l| l.role == LinkRole::Responder));
    a.transport.clock().advance(7_000);
    assert!(a
        .link_list()
        .iter()
        .filter(|l| l.age_secs.is_some())
        .all(|l| l.idle_secs == Some(7) && l.age_secs == Some(7)));

    let lc = a.link_lifecycle();
    assert_eq!((lc.established_initiator, lc.established_responder), (2, 0));
    assert_eq!(b.link_lifecycle().established_responder, 2);

    // A closed link leaves the census and is counted by reason, on both ends.
    let close = a.close_link(&ids[0]);
    assert_eq!(a.link_census().initiator, 1);
    assert_eq!(a.link_lifecycle().closed(LinkCloseReason::Normal), 1);
    deliver(&mut b, b_mesh, packets(&close));
    assert_eq!(b.link_lifecycle().closed(LinkCloseReason::PeerClosed), 1);

    // A request B refuses is counted as rejected on B.
    let (refused, _, out) = a.connect(dest_hash, &signing_key).expect("connect");
    let _ = deliver(&mut b, b_mesh, packets(&out));
    let pending_on_b = b
        .link_list()
        .into_iter()
        .find(|l| l.age_secs.is_none())
        .expect("B holds the pending request");
    b.reject_link(&pending_on_b.link_id);
    assert_eq!(b.link_lifecycle().rejected, 1);
    assert_eq!(b.link_lifecycle().handshake_failed_total(), 0);
    // Rejecting a link that is already live is a close, not a refusal: the
    // counters keep agreeing with the census.
    let live_on_b = b
        .link_list()
        .into_iter()
        .find(|l| l.age_secs.is_some())
        .expect("an established link on B");
    b.reject_link(&live_on_b.link_id);
    let lc = b.link_lifecycle();
    assert_eq!(lc.rejected, 1, "not counted as a refusal");
    assert_eq!(lc.closed(LinkCloseReason::Normal), 1);
    let census = b.link_census();
    assert_eq!(
        lc.established() - lc.closed_total(),
        (census.initiator + census.responder) as u64
    );
    let _ = a.close_link(&refused);

    // A link that never established is a handshake failure, not a close.
    let _ = a.close_link(&ids[2]);
    let lc = a.link_lifecycle();
    assert_eq!(lc.handshake_failed(LinkCloseReason::Normal), 2);
    assert_eq!(lc.closed_total(), 1);
    // And the counters agree with the census.
    let census = a.link_census();
    assert_eq!(
        lc.established() - lc.closed_total(),
        (census.initiator + census.responder) as u64
    );
}
