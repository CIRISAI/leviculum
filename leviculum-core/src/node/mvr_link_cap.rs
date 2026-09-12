//! The link-table cap (`TransportConfig::max_links`, #388).
//!
//! The firmware heap budget multiplies a per-link cost by a link count,
//! but until #388 pass 3 nothing bounded `NodeCore.links`: every accepted
//! inbound request and every `connect` inserted unconditionally, so the
//! budget's `links=` term was a claim. This mvr pins the cap on both
//! paths, sans-I/O with `MockClock`:
//!
//! * inbound: with `max_links = Some(2)`, three link requests from three
//!   identities yield two links and two proofs; the third gets no link,
//!   no proof, and exactly one `LINK_REFUSED reason=budget` line;
//! * a closed link frees its slot immediately: after `close_link` the
//!   next request is accepted and proved;
//! * outbound: `connect` at the cap returns `LinkError::TableFull` and
//!   creates nothing;
//! * default (`None`) stays unbounded — the host behaviour.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkError;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_log_capture::with_captured_logs;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type Node = NodeCore<OsRng, MockClock, NoStorage>;

fn add_iface(node: &mut Node, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
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

/// A capped responder accepting links on one destination, plus its iface.
fn make_responder(max_links: Option<usize>) -> (Node, crate::DestinationHash, [u8; 32], usize) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut node = NodeCoreBuilder::new().max_links(max_links).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        NoStorage,
    );
    let iface = add_iface(&mut node, "wire");

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["linkcap"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    (node, dest_hash, signing_key, iface)
}

/// One fresh initiator's link-request bytes toward `dest_hash`.
fn link_request_raw(dest_hash: crate::DestinationHash, signing_key: &[u8; 32]) -> Vec<u8> {
    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    add_iface(&mut initiator, "wire");
    let (_link_id, _routed, out) = initiator
        .connect(dest_hash, signing_key)
        .expect("uncapped initiator connect");
    let data = action_data(&out);
    assert_eq!(data.len(), 1, "connect emits exactly the link request");
    data.into_iter().next().unwrap()
}

#[test]
fn inbound_cap_refuses_third_link_and_frees_on_close() {
    let (mut responder, dest_hash, signing_key, iface) = make_responder(Some(2));

    let ((), logs) = with_captured_logs(|| {
        // Two requests fill the table; each is answered with a proof.
        for n in 0..2 {
            let raw = link_request_raw(dest_hash, &signing_key);
            let out = responder.handle_packet(InterfaceId(iface), &raw);
            assert_eq!(
                action_data(&out).len(),
                1,
                "request {n} must be answered with exactly one proof"
            );
        }
        assert_eq!(responder.link_count(), 2, "table at the cap");

        // The third gets no link and no proof.
        let raw = link_request_raw(dest_hash, &signing_key);
        let out = responder.handle_packet(InterfaceId(iface), &raw);
        assert!(
            action_data(&out).is_empty(),
            "no proof may leave for the refused request"
        );
        assert_eq!(responder.link_count(), 2, "no third link entry");
    });
    assert_eq!(
        logs.matches("LINK_REFUSED reason=budget links=2 max=2")
            .count(),
        1,
        "exactly one structured refusal line, got:\n{logs}"
    );

    // A closed link frees its slot immediately.
    let victim = crate::LinkId::new(
        responder
            .link_table_entries()
            .first()
            .expect("a live link to close")
            .link_id,
    );
    let _ = responder.close_link(&victim);
    assert_eq!(responder.link_count(), 1, "close frees the slot at once");

    let raw = link_request_raw(dest_hash, &signing_key);
    let out = responder.handle_packet(InterfaceId(iface), &raw);
    assert_eq!(
        action_data(&out).len(),
        1,
        "the next request after a close is accepted and proved"
    );
    assert_eq!(responder.link_count(), 2);
}

#[test]
fn outbound_connect_at_cap_returns_table_full() {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut node = NodeCoreBuilder::new().max_links(Some(1)).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        NoStorage,
    );
    add_iface(&mut node, "wire");
    let dest_hash = *Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["linkcap"],
    )
    .unwrap()
    .hash();

    let (_first, _, _) = node
        .connect(dest_hash, &signing_key)
        .expect("first connect fills the single slot");
    assert_eq!(node.link_count(), 1);

    let refused = node.connect(dest_hash, &signing_key);
    assert_eq!(
        refused.map(|(id, ..)| id),
        Err(LinkError::TableFull { max: 1 }),
        "connect at the cap must refuse"
    );
    assert_eq!(node.link_count(), 1, "the refusal created nothing");
}

#[test]
fn unbounded_default_accepts_past_any_small_count() {
    let (mut responder, dest_hash, signing_key, iface) = make_responder(None);
    for _ in 0..5 {
        let raw = link_request_raw(dest_hash, &signing_key);
        let out = responder.handle_packet(InterfaceId(iface), &raw);
        assert_eq!(
            action_data(&out).len(),
            1,
            "unbounded: every request proved"
        );
    }
    assert_eq!(responder.link_count(), 5);
}
