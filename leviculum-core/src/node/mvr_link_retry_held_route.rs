//! mvr: #308 — a link request follows the last announce (#230 at the link
//! layer). Host-side reproduction of the `pathchoice_loss30_lnsd` mechanism
//! (periculum pass 305, 2026-09-26): a destination reachable both directly
//! (1 hop, lossy) and via a relay (2 hops), the direct route installed, and
//! every link-request attempt — the initial send and each establishment-
//! timeout retry — leaving on the SAME direct route, because nothing between
//! a failed attempt and the next one touches the path table:
//!
//!   * the retry re-reads the path table (`link_management.rs::check_timeouts`)
//!     but never modifies it — no re-request, no unresponsive marking, no
//!     drop (`transport.rs::clean_link_table` sub-case 2, the
//!     `lr_taken_hops == 0` arm, is the only rediscovery and it fires on the
//!     TRANSPORT'S link entry, not here — and it too marks nothing);
//!   * the 2-hop copy of the SAME announce emission loses the acceptance
//!     comparison in `handle_announce` (worse hops + same emission + path
//!     not Unresponsive ⇒ rejected, `transport.rs` accept_reason `None`
//!     arm), so re-hearing the alternative while the link is pending
//!     changes nothing either.
//!
//! Python 1.5.2 behaves the same at every one of these points (RNS.Link never
//! retries at all; Transport.py:884-955 rediscovers but marks nothing for
//! lr_taken_hops == 0; Transport.py:2289-2294 accepts a worse-hop copy only
//! when the path is marked unresponsive), so this is a shared design property
//! (#230), not a port bug. The fix direction is undecided (Lew's call); these
//! assertions state TODAY'S behaviour so any chosen fix has a red to turn
//! green: after a fix that suppresses or demotes the direct route between
//! failed attempts, the retry assertions below (same interface, still 1 hop,
//! never Unresponsive) are exactly the ones that must flip.
//!
//! The "in-process lossy medium" here is the sans-I/O scripted-delivery
//! harness every mvr uses (same pattern as `mvr_establishment_loss`): loss is
//! deterministic — a packet is "lost on the direct route" by not handing it
//! to the peer. No RF, no Docker, sub-second wall clock.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{LinkCloseReason, LinkId};
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{HeaderType, Packet, PacketType, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, TickOutput};

// NOT NoStorage: the scenario's whole subject is the path table, and
// NoStorage drops every write.
type EndpointNode = NodeCore<OsRng, MockClock, MemoryStorage>;

fn add_iface(node: &mut EndpointNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults())
}

/// Responder owning a link-accepting destination, plus ONE packed direct
/// (wire hops 0) announce. One emission only: the whole point is that the
/// direct and the relayed copy carry the SAME random blob, so the acceptance
/// comparison runs its same-emission arms.
fn make_responder() -> (EndpointNode, crate::DestinationHash, [u8; 32], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["heldroute"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    let raw = buf[..len].to_vec();

    node.register_destination(dest);
    (node, dest_hash, signing_key, raw)
}

/// The same announce one relay later: HEADER_2, wire hops 1, the relay's
/// identity hash as `transport_id` — byte-exact what a transport node puts
/// on the air when it rebroadcasts (same construction as
/// `mvr_ble_peer_loss_reroute`). Same emission, same random blob.
fn relayed_copy(direct_raw: &[u8], via: [u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    let mut p = Packet::unpack(direct_raw).unwrap();
    p.flags.header_type = HeaderType::Type2;
    p.flags.transport_type = TransportType::Transport;
    p.hops = 1;
    p.transport_id = Some(via);
    let mut buf = [0u8; MTU];
    let len = p.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// Every LINKREQUEST this output puts on the wire, as its send target
/// (`Some(iface)` for a routed send, `None` for a broadcast). Filtering by
/// packet type keeps the count clean of whatever else a maintenance tick
/// emits (path requests after the final close, for instance).
fn link_request_sends(out: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> Vec<Option<usize>> {
    out.actions
        .iter()
        .filter_map(|action| {
            let (data, target) = match action {
                Action::SendPacket { iface, data, .. } => (data, Some(iface.0)),
                Action::Broadcast { data, .. } => (data, None),
            };
            Packet::unpack(data)
                .ok()
                .filter(|p| {
                    p.flags.packet_type == PacketType::LinkRequest && &p.destination_hash == dest
                })
                .map(|_| target)
        })
        .collect()
}

fn has_timeout_close(output: &TickOutput) -> bool {
    output.events.iter().any(|e| {
        matches!(
            e,
            NodeEvent::LinkClosed {
                reason: LinkCloseReason::Timeout,
                ..
            }
        )
    })
}

/// Advance the initiator's virtual clock just past the (possibly re-keyed)
/// link's current establishment timeout and run one maintenance tick.
fn tick_past_establishment_timeout(
    initiator: &mut EndpointNode,
    caller_link_id: &LinkId,
) -> TickOutput {
    let timeout_ms = initiator
        .link(caller_link_id)
        .expect("link must still be pending")
        .establishment_timeout_ms();
    initiator.transport().clock().advance(timeout_ms + 1);
    initiator.handle_timeout()
}

/// POSITIVE CONTROL. The relayed 2-hop copy is a genuinely installable
/// alternative: a node that hears ONLY it installs a 2-hop path over the
/// relay interface. So the rejections asserted in the main test below are
/// the hop-count comparison at work, not a malformed packet.
#[test]
fn relayed_copy_alone_installs_a_two_hop_path() {
    let (_responder, dest_hash, _signing_key, direct_raw) = make_responder();
    let relay_id = *Identity::generate(&mut OsRng).hash();
    let relayed = relayed_copy(&direct_raw, relay_id);

    let mut node = make_initiator();
    let relay_iface = add_iface(&mut node, "I_relay", 2);

    let _ = node.handle_packet(InterfaceId(relay_iface), &relayed);

    let path = node
        .transport()
        .path(dest_hash.as_bytes())
        .expect("the relayed copy alone must install a path");
    assert_eq!(path.hops, 2, "wire hops 1 + receipt increment = 2");
    assert_eq!(path.interface_index, relay_iface);
    assert_eq!(
        path.next_hop,
        Some(relay_id),
        "the relayed copy names the relay as next hop"
    );
}

/// THE #308 REPRODUCTION. Direct route installed, the 2-hop alternative of
/// the same emission known and re-offered, a link request lost on the direct
/// route — and every retry leaves on the same direct route with the path
/// entry untouched, until the whole attempt budget is spent.
#[test]
fn every_link_request_retry_leaves_on_the_held_direct_route() {
    let (_responder, dest_hash, signing_key, direct_raw) = make_responder();
    let relay_id = *Identity::generate(&mut OsRng).hash();
    let relayed = relayed_copy(&direct_raw, relay_id);
    let dest = *dest_hash.as_bytes();

    let mut initiator = make_initiator();
    let direct_iface = add_iface(&mut initiator, "I_direct", 1);
    let relay_iface = add_iface(&mut initiator, "I_relay", 2);

    // The direct copy installs the 1-hop route (the soak's 13-of-36 case:
    // the direct copy of the epoch won the race).
    let _ = initiator.handle_packet(InterfaceId(direct_iface), &direct_raw);
    let path = initiator.transport().path(&dest).expect("path installed");
    assert_eq!(path.hops, 1);
    assert_eq!(path.interface_index, direct_iface);

    // The relayed copy of the SAME emission arrives too — and is rejected:
    // worse hops, same emission, path not Unresponsive (the #230 acceptance
    // property, `handle_announce` accept_reason `None` arm).
    let _ = initiator.handle_packet(InterfaceId(relay_iface), &relayed);
    let path = initiator.transport().path(&dest).expect("path still there");
    assert_eq!(
        (path.hops, path.interface_index),
        (1, direct_iface),
        "a worse-hop copy of the same emission must not displace the held route"
    );

    // Attempt 1: the link request leaves routed, on the direct route. It is
    // "lost" — never handed to the responder.
    let (link_id, routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    assert!(
        routed,
        "with a held path the request must route, not broadcast"
    );
    assert_eq!(
        link_request_sends(&out, &dest),
        std::vec![Some(direct_iface)],
        "attempt 1 leaves on the held direct route"
    );

    // While the link is pending, the 2-hop alternative is heard AGAIN (the
    // path response that arrives between attempts in the emulated cell).
    // Announces are dedup-exempt, so it reaches the acceptance comparison —
    // and is rejected the same way. A newer copy of the alternative changes
    // neither the pending link nor the next attempt's route.
    let _ = initiator.handle_packet(InterfaceId(relay_iface), &relayed);
    let path = initiator.transport().path(&dest).expect("path still there");
    assert_eq!((path.hops, path.interface_index), (1, direct_iface));

    // Attempts 2..n: each establishment timeout re-keys and resends. The
    // retry re-reads the path table (`check_timeouts`) and the table still
    // holds the direct route — TODAY'S behaviour under test: every retry
    // leaves on the SAME route, and nothing ever marks it Unresponsive.
    let mut attempts_on_direct = 1;
    let mut died = false;
    for _ in 0..16 {
        if initiator.link(&link_id).is_none() {
            break;
        }
        let out = tick_past_establishment_timeout(&mut initiator, &link_id);
        if has_timeout_close(&out) {
            died = true;
            break;
        }
        assert_eq!(
            link_request_sends(&out, &dest),
            std::vec![Some(direct_iface)],
            "a retry after a lost request must (today) leave on the held direct route"
        );
        attempts_on_direct += 1;
        assert!(
            !initiator.transport().path_is_unresponsive(&dest),
            "no initiator-side attempt failure marks the path Unresponsive today"
        );
        let path = initiator.transport().path(&dest).expect("path held");
        assert_eq!(
            (path.hops, path.interface_index),
            (1, direct_iface),
            "the path entry survives every failed attempt unchanged"
        );
    }

    assert!(died, "persistent loss must end in LinkClosed(Timeout)");
    assert_eq!(
        attempts_on_direct, 3,
        "the whole budget (1 + max(LINK_REQUEST_MAX_RETRIES, hops=1) = 3 attempts) \
         is spent on the one lossy route — the periculum pass-305 log shape"
    );

    // Post-mortem (today's only recovery, and it is too late for this
    // caller): the non-transport initiator expires the path and asks again
    // (`emit_link_closed`, the Transport.py:696-721 parity arm). lncp has
    // already errored out on the LinkClosed event by now.
    assert!(
        initiator.transport().path(&dest).is_none(),
        "after the final timeout the initiator expires the held path"
    );
}
