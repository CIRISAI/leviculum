//! mvr: an MTU that cannot carry a link is refused at both adoption sites,
//! rather than producing derived sizes that are silently degenerate.
//! Codeberg #392.
//!
//! ## Mechanism
//!
//! Since #390 the negotiated MTU is whatever the far end confirms
//! (`self.mtu = confirmed_mtu or RNS.Reticulum.MTU`,
//! `reference/Reticulum/RNS/Link.py`, `validate_proof`), so the number that
//! sizes every buffer on a link now arrives from the other end of the wire
//! instead of from our own constants. Two sizes are derived from it by
//! subtraction, and both run out of bytes:
//!
//! * `compute_link_mdu` (`link/mod.rs`) computes
//!   `floor((mtu - 1 - 19 - 48) / 16) * 16 - 1`. At an MTU of 83 or less the
//!   product is 0 and the trailing `- 1` underflows: a panic in debug, and in
//!   release a `usize::MAX` MDU, which passes every "does this payload still
//!   fit" check instead of failing one.
//! * `resource_sdu` (`resource/mod.rs`) saturates to 0 at an MTU of 36 or
//!   less, and the sender then evaluates `encrypted.len().div_ceil(sdu)`
//!   (`resource/outgoing.rs`) — a divide by zero.
//!
//! [`crate::constants::LINK_MTU_MIN`] is the one stated floor for both, and
//! it is enforced where the peer's value is adopted: `Link::new_incoming` on
//! the responder, `Link::process_proof` on the initiator. The readers stay as
//! they are; a floor per reader is a floor that drifts.
//!
//! Unreachable in the field today — the smallest interface we ship declares
//! `hw_mtu = 508` — which is why this is a guard and not a field fix.
//!
//! Sans-I/O: two `NodeCore`s over `MockInterface`, no radio, no timers.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::LINK_MTU_MIN;
use crate::destination::{Destination, DestinationHash, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{Link, LinkCloseReason, LinkId, LinkState};
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Header of a LINK_REQUEST / PROOF packet: flags(1) + hops(1) +
/// link or destination hash(16) + context(1).
const HEADER_1_LEN: usize = 19;

fn add_iface(node: &mut EndpointNode, name: &'static str, hw_mtu: u32) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    node.set_interface_hw_mtu(idx, hw_mtu);
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

/// A node that has sent its link request and heard nothing back yet, plus
/// everything needed to answer it: the responder node, and the destination
/// identity the initiator learned from its announce.
struct Pending {
    sender: EndpointNode,
    s_iface: usize,
    sender_link: LinkId,
    receiver: EndpointNode,
    r_iface: usize,
    identity: Identity,
    dest_hash: DestinationHash,
    request_packet: Vec<u8>,
}

/// What one full establishment attempt produced.
struct Attempt {
    sender: EndpointNode,
    sender_link: LinkId,
    receiver: EndpointNode,
    receiver_link: Option<LinkId>,
    /// Packets the responder emitted in answer to the request — a proof, or
    /// nothing at all when it refuses the link.
    from_receiver: usize,
}

/// Set up two nodes whose shared medium declares `hw_mtu` and get the
/// initiator as far as a sent link request, the way
/// [`super::mvr_link_mtu_asymmetry`] does: the peer is learned from its
/// announce, so the initiator has a next hop to read an MTU off.
fn pending_link_over(hw_mtu: u32) -> Pending {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();

    let mut receiver = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);
    let r_iface = add_iface(&mut receiver, "R_lora", hw_mtu);

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_iface = add_iface(&mut sender, "S_lora", hw_mtu);

    let announce = receiver
        .announce_destination(&dest_hash, None)
        .expect("announce");
    for pkt in action_data(&announce) {
        let _ = sender.handle_packet(InterfaceId(s_iface), &pkt);
    }

    let (sender_link, _routed, out) = sender.connect(dest_hash, &signing_key).expect("connect");
    let request_packet = action_data(&out)
        .into_iter()
        .next()
        .expect("connect emits the link request");

    Pending {
        sender,
        s_iface,
        sender_link,
        receiver,
        r_iface,
        identity,
        dest_hash,
        request_packet,
    }
}

/// Drive the handshake to wherever it ends up.
fn attempt_link_over(hw_mtu: u32) -> Attempt {
    let Pending {
        mut sender,
        s_iface,
        sender_link,
        mut receiver,
        r_iface,
        request_packet,
        ..
    } = pending_link_over(hw_mtu);

    let mut receiver_link = None;
    let mut from_receiver = 0usize;
    let mut for_receiver = std::vec![request_packet];
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
            let emitted = action_data(&o);
            from_receiver += emitted.len();
            back.extend(emitted);
        }
        let mut next = Vec::new();
        for pkt in back {
            next.extend(action_data(
                &sender.handle_packet(InterfaceId(s_iface), &pkt),
            ));
        }
        for_receiver = next;
    }

    Attempt {
        sender,
        sender_link,
        receiver,
        receiver_link,
        from_receiver,
    }
}

/// The responder's side of the floor: an interface that cannot carry a link
/// gets no link, and the requester gets no proof — which is what every
/// initiator's establishment timeout already handles.
///
/// Before the fix the responder accepted the request at MTU 83, echoed 83 in
/// its proof, and both ends then carried an MDU of `usize::MAX` (a panic in
/// debug the first time `mdu()` was read).
#[test]
fn a_path_one_byte_below_the_floor_yields_no_link() {
    let attempt = attempt_link_over(LINK_MTU_MIN - 1);

    std::eprintln!(
        "hw_mtu={} receiver_link={:?} from_receiver={}",
        LINK_MTU_MIN - 1,
        attempt.receiver_link,
        attempt.from_receiver
    );
    assert!(
        attempt.receiver_link.is_none(),
        "a responder must not establish a link it cannot size"
    );
    assert_eq!(
        attempt.from_receiver, 0,
        "a refused link request is answered with nothing, not with a proof"
    );
    assert!(
        attempt.receiver.links.is_empty(),
        "the refused request must leave no link in the responder's table"
    );
    assert_eq!(
        attempt
            .sender
            .link(&attempt.sender_link)
            .expect("the initiator keeps its pending link until it times out")
            .state(),
        LinkState::Pending,
        "without a proof the initiator stays pending"
    );
}

/// The floor itself is admitted, and every size derived from it is usable:
/// an MDU of 15 real bytes rather than 0 or `usize::MAX`, and a nonzero
/// resource SDU, which is the divisor the sender's part count needs.
#[test]
fn the_floor_itself_establishes_with_usable_derived_sizes() {
    let attempt = attempt_link_over(LINK_MTU_MIN);

    let receiver_link = attempt
        .receiver_link
        .expect("the floor must still establish a link");
    let initiator = attempt
        .sender
        .link(&attempt.sender_link)
        .expect("initiator link");
    let responder = attempt
        .receiver
        .link(&receiver_link)
        .expect("responder link");

    let mdu = initiator.mdu();
    let sdu = crate::resource::resource_sdu(initiator.negotiated_mtu());
    std::eprintln!(
        "hw_mtu={LINK_MTU_MIN} initiator_mtu={} responder_mtu={} mdu={mdu} sdu={sdu}",
        initiator.negotiated_mtu(),
        responder.negotiated_mtu()
    );

    assert_eq!(initiator.state(), LinkState::Active);
    assert_eq!(
        initiator.negotiated_mtu(),
        responder.negotiated_mtu(),
        "both ends stay on one MTU (#390)"
    );
    assert_eq!(initiator.negotiated_mtu(), LINK_MTU_MIN);
    assert_eq!(mdu, 15, "floor(16/16) * 16 - 1");
    assert_eq!(sdu, LINK_MTU_MIN as usize - 36);

    // The MDU is a real length the crypto path can serve, not an artefact of
    // the subtraction: a full-MDU packet builds and stays inside the MTU.
    let mut rng = OsRng;
    let pkt = initiator
        .build_data_packet_with_context(
            &std::vec![0xA5u8; mdu],
            crate::packet::PacketContext::None,
            &mut rng,
        )
        .expect("a full-MDU packet must build at the floor");
    std::eprintln!("floor packet_len={}", pkt.len());
    assert!(
        pkt.len() <= LINK_MTU_MIN as usize,
        "a full-MDU packet must fit the negotiated MTU; got {}",
        pkt.len()
    );
}

/// The initiator's side of the floor, which is the one #390 made reachable:
/// the confirmed MTU comes from the peer, so a peer that confirms below the
/// floor must get no link — even though our own responder can no longer
/// produce such a proof. The proof here is therefore built by hand, signed
/// by the destination identity the initiator learned from the announce, which
/// is exactly what a foreign or buggy responder puts on the wire.
#[test]
fn a_peer_that_confirms_below_the_floor_gets_no_link() {
    for (confirmed, expect_link) in [(LINK_MTU_MIN - 1, false), (LINK_MTU_MIN, true)] {
        // A non-clamping medium, so the only value under test is the one the
        // hand-built proof confirms. The real responder node never sees the
        // request: the initiator has to still be pending when the hand-built
        // proof arrives, which is the state the field case arrives in.
        let mut attempt = pending_link_over(508);
        let request_data = &attempt.request_packet[HEADER_1_LEN..];
        let link_id = Link::calculate_link_id(&attempt.request_packet);
        assert_eq!(
            link_id, attempt.sender_link,
            "the harness must address the initiator's own pending link"
        );

        let mut responder = Link::new_incoming(
            request_data,
            link_id,
            attempt.dest_hash,
            &mut OsRng,
            Some(508),
        )
        .expect("the hand-built responder is at the base MTU");
        let proof = responder
            .build_proof_packet(&attempt.identity, confirmed, 1)
            .expect("proof");

        let out = attempt
            .sender
            .handle_packet(InterfaceId(attempt.s_iface), &proof);
        let established = out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
        let closed = out.events.iter().any(|e| {
            matches!(
                e,
                NodeEvent::LinkClosed {
                    reason: LinkCloseReason::InvalidProof,
                    ..
                }
            )
        });
        std::eprintln!("confirmed={confirmed} established={established} closed={closed}");

        if expect_link {
            assert!(established, "a proof at the floor must establish the link");
            assert_eq!(
                attempt
                    .sender
                    .link(&attempt.sender_link)
                    .expect("link")
                    .negotiated_mtu(),
                confirmed
            );
        } else {
            assert!(
                !established,
                "a proof confirming {confirmed} must not establish a link"
            );
            assert!(
                closed,
                "the refused proof must close the pending link, the way an \
                 unverifiable signature does"
            );
            assert!(
                attempt.sender.link(&attempt.sender_link).is_none(),
                "no link may survive a refused proof"
            );
        }
    }
}
