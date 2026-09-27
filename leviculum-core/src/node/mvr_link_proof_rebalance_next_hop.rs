//! mvr: #332 — a link-request proof that took the SHORT way back must not
//! strand the relayed route it was answering (#330's guard, Leviculum #230).
//!
//! Host-side reproduction of the `pathchoice_*_lnsd` mechanism measured by
//! periculum pass 327 (2026-09-27,
//! the periculum tree's own report `2026-09-10-pathchoice-sweep`, section 10):
//! twelve arms under `measure`, `rnsd` carrying 8/8 transfers on every relayed
//! arm while `lnsd` read 7/8, 3/8, 4/8, 8/8, 8/8 at L = 0.3/0.5/0.7/0.9/1.0.
//! Ten of ten failed `lnsd` attempts had sent their link request over the
//! direct lossy pair, and all 21 relayed requests in the run belonged to
//! attempts that succeeded. The route moved without an announce:
//!
//! ```text
//! PATH_ADD hops=2 next_hop=<bravo> reason="new_destination"
//! LINK_ENTRY_SET remaining_hops=2            <- attempt 1, relayed, ok
//! LRPROOF arrived dest=… iface=serial_0 hops=1
//! event="PATH_REBALANCE" dst=… from=2 to=1
//! LINK_ENTRY_SET remaining_hops=1            <- every later attempt, direct
//! ```
//!
//! ## The mechanism, end to end
//!
//! The link request left over bravo (2 hops), charlie's proof came back across
//! the direct pair (1 hop), and the #330 rebalance
//! (`transport.rs::rebalance_path_hops`, terminus adoption in
//! `node/link_management.rs`) wrote `hops = 1` into the path entry while
//! `next_hop` still named bravo. `PathEntry::needs_relay()` is
//! `hops > 1 && next_hop.is_some()` (`storage_types.rs:60`), so from that
//! moment on the entry claims a direct neighbour: `connect` stops putting a
//! transport header on the request (`link_management.rs::connect`, the
//! `needs_relay()` arm) and `send_to_destination` sends it as a final-hop
//! packet (`transport.rs`, `route_via_transport`). Bravo, no longer named in
//! any header, forwards nothing ever again, and every later attempt is a coin
//! toss on the pair that lost the first one. At L >= 0.9 no proof crosses the
//! pair at all, nothing rebalances, and the arm reads 8/8 — the damage is
//! done by the ONE frame that gets through.
//!
//! ## What the guard is
//!
//! `rebalance_path_hops` refuses a count that turns `needs_relay()` false
//! while `next_hop` still names a transport peer, and reports the refusal as
//! `PathRebalance::HeldForNextHop` (`transport.rs::rebalance_path_hops` states
//! the rule and the deviation-rule check; Python 1.5.2 has the same hole and
//! does not guard it). The assertions below are written for the guard; each
//! one names what it read before the guard landed, so the pair reads as the
//! red and the green of one change.
//!
//! ## The topology
//!
//! ```text
//!         bravo             alpha-bravo clean, bravo-charlie clean
//!        /     \            alpha-charlie audible but lossy
//!   alpha - - - charlie     all three on one carrier
//! ```
//!
//! Same shape as the emulated cell, which puts all three nodes on ONE serial
//! medium: alpha therefore has exactly ONE interface here, and both bravo's
//! rebroadcast and charlie's direct proof arrive on it. That is what makes the
//! entry's `hops` the only thing that decides whether bravo stays in the
//! route — the interface index cannot tell the two routes apart, and the
//! rebalance never touches it or the next hop anyway. Bravo is given its own
//! pair of interfaces so nothing here depends on same-carrier announce
//! rebroadcast; what is under test is what ALPHA does.
//!
//! Loss is deterministic, as in every mvr: a frame is "lost on the direct
//! pair" by not handing it to the peer. Sans-I/O, no RF, no Docker,
//! sub-second wall clock.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{HeaderType, Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, PathEntry, PathRebalance, TickOutput};

// NOT NoStorage: the subject is the path table, and NoStorage drops every
// write to it.
type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

fn add_iface(node: &mut Node, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn make_endpoint() -> Node {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults())
}

fn make_relay() -> Node {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    )
}

/// The responder: a link-accepting destination plus one packed direct
/// (wire hops 0) announce.
fn make_responder() -> (Node, crate::DestinationHash, [u8; 32], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["rebalance"],
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

fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

fn one_packet(output: &TickOutput) -> Vec<u8> {
    let data = action_data(output);
    assert_eq!(
        data.len(),
        1,
        "expected exactly one outbound packet, got {}",
        data.len()
    );
    data.into_iter().next().unwrap()
}

/// Feed an announce into a relay, advance its clock past the rebroadcast
/// delay, and collect what it put on the wire.
fn forward_announce(relay: &mut Node, in_iface: usize, raw: &[u8]) -> Vec<Vec<u8>> {
    let _ = relay.handle_packet(InterfaceId(in_iface), raw);
    let now = relay.transport().clock().now_ms();
    relay.transport().clock().set(now + 100_000);
    let out = relay.handle_timeout();
    action_data(&out)
}

/// One LINKREQUEST on the wire, read as the three things that decide whether a
/// relay is still in the route: the interface it left on, whether it carries a
/// transport header, and whom that header names.
struct SentRequest {
    raw: Vec<u8>,
    iface: Option<usize>,
    header_type: HeaderType,
    transport_id: Option<[u8; TRUNCATED_HASHBYTES]>,
}

fn link_requests(output: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> Vec<SentRequest> {
    output
        .actions
        .iter()
        .filter_map(|action| {
            let (data, iface) = match action {
                Action::SendPacket { iface, data, .. } => (data, Some(iface.0)),
                Action::Broadcast { data, .. } => (data, None),
            };
            let packet = Packet::unpack(data).ok()?;
            if packet.flags.packet_type != PacketType::LinkRequest
                || &packet.destination_hash != dest
            {
                return None;
            }
            Some(SentRequest {
                raw: data.clone(),
                iface,
                header_type: packet.flags.header_type,
                transport_id: packet.transport_id,
            })
        })
        .collect()
}

fn one_link_request(output: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> SentRequest {
    let mut reqs = link_requests(output, dest);
    assert_eq!(
        reqs.len(),
        1,
        "expected exactly one link request, got {}",
        reqs.len()
    );
    reqs.remove(0)
}

fn has_link_established(output: &TickOutput) -> bool {
    output
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }))
}

// ---------------------------------------------------------------------------
// POSITIVE CONTROL
// ---------------------------------------------------------------------------

/// The relayed announce really does install a relay route: 2 hops, bravo named
/// as next hop, `needs_relay()` true. Every assertion below about what the
/// rebalance destroys rests on this.
#[test]
fn the_relayed_announce_installs_a_two_hop_route_over_bravo() {
    let (_charlie, dest_hash, _signing_key, announce) = make_responder();
    let mut bravo = make_relay();
    let mut alpha = make_endpoint();

    let b_from_charlie = add_iface(&mut bravo, "B_from_charlie", 2);
    let _b_to_alpha = add_iface(&mut bravo, "B_to_alpha", 3);
    let a_medium = add_iface(&mut alpha, "A_medium", 1);
    let bravo_id = *bravo.identity().hash();

    let relayed = forward_announce(&mut bravo, b_from_charlie, &announce);
    assert_eq!(relayed.len(), 1, "bravo must rebroadcast exactly once");
    let _ = alpha.handle_packet(InterfaceId(a_medium), &relayed[0]);

    let path = alpha
        .transport()
        .path(dest_hash.as_bytes())
        .expect("the relayed announce must install a path");
    assert_eq!(path.hops, 2, "wire hops 1 + receipt increment");
    assert_eq!(path.next_hop, Some(bravo_id), "bravo is the next hop");
    assert_eq!(path.interface_index, a_medium);
    assert!(
        path.needs_relay(),
        "a 2-hop entry naming a next hop is a relay route"
    );
}

// ---------------------------------------------------------------------------
// THE #332 REPRODUCTION
// ---------------------------------------------------------------------------

/// The proof comes back over the direct pair, one hop, while the route it
/// answered is two hops over bravo. The path entry must keep its 2: adopting
/// the 1 would leave bravo named in an entry that no longer routes through
/// him, and the next link request would go out as a final-hop packet nobody
/// forwards.
///
/// Before the guard (commit before this one, measured, all four green as
/// assertions of the defect): `path.hops == 1`, `needs_relay() == false`,
/// attempt 2 `HeaderType::Type1` with `transport_id == None`, and bravo
/// forwarding 0 of the 1 request it heard.
#[test]
fn a_proof_over_the_direct_pair_must_not_strand_the_relayed_route() {
    let (mut charlie, dest_hash, signing_key, announce) = make_responder();
    let mut bravo = make_relay();
    let mut alpha = make_endpoint();
    let dest = *dest_hash.as_bytes();

    let c_medium = add_iface(&mut charlie, "C_medium", 1);
    let b_from_charlie = add_iface(&mut bravo, "B_from_charlie", 2);
    let b_to_alpha = add_iface(&mut bravo, "B_to_alpha", 3);
    let a_medium = add_iface(&mut alpha, "A_medium", 1);
    let bravo_id = *bravo.identity().hash();

    // --- alpha learns charlie over bravo. The direct copy of the announce is
    // the frame the lossy pair ate, so alpha never hears it.
    let relayed = forward_announce(&mut bravo, b_from_charlie, &announce);
    assert_eq!(relayed.len(), 1, "bravo must rebroadcast exactly once");
    let _ = alpha.handle_packet(InterfaceId(a_medium), &relayed[0]);
    let path = alpha.transport().path(&dest).expect("path installed");
    assert_eq!(
        (path.hops, path.next_hop, path.needs_relay()),
        (2, Some(bravo_id), true),
        "precondition: the relay route is installed"
    );

    // --- Attempt 1 leaves over bravo, and bravo forwards it.
    let (_link1, routed, out) = alpha.connect(dest_hash, &signing_key).expect("connect");
    assert!(routed, "with a relay path the request must route");
    let req1 = one_link_request(&out, &dest);
    assert_eq!(
        (req1.iface, req1.header_type, req1.transport_id),
        (Some(a_medium), HeaderType::Type2, Some(bravo_id)),
        "attempt 1 must leave with a transport header naming bravo"
    );

    let out = bravo.handle_packet(InterfaceId(b_to_alpha), &req1.raw);
    let forwarded = one_packet(&out);
    assert_eq!(
        link_requests(&out, &dest).len(),
        1,
        "bravo must forward the relayed request"
    );

    // --- charlie accepts and proves. The proof is originated, wire hops 0.
    let out = charlie.handle_packet(InterfaceId(c_medium), &forwarded);
    let proof = one_packet(&out);
    assert_eq!(proof[1], 0, "charlie originates the proof at wire hops 0");

    // --- THE ONE FRAME THAT GETS THROUGH: the proof crosses the direct pair
    // instead of going back via bravo. alpha counts it as 1 hop (wire 0 +
    // receipt increment) where it froze 2 on the link.
    let out = alpha.handle_packet(InterfaceId(a_medium), &proof);
    assert!(
        has_link_established(&out),
        "the proof is valid and the link must establish"
    );

    // --- The guard: the count is refused, the entry is untouched, and it is
    // still a relay route naming bravo.
    let path = alpha.transport().path(&dest).expect("path still there");
    assert_eq!(
        path.hops, 2,
        "#332: a count that would strand the next hop is not adopted"
    );
    assert_eq!(
        path.next_hop,
        Some(bravo_id),
        "the rebalance does not touch the next hop"
    );
    assert!(
        path.needs_relay(),
        "#332: the entry that names a relay must keep routing through it"
    );

    // --- And that is what the next attempt keeps: a transport header naming
    // bravo, on the carrier both routes share.
    let (_link2, routed2, out) = alpha.connect(dest_hash, &signing_key).expect("connect");
    assert!(routed2, "the entry is still a path, so the send is routed");
    let req2 = one_link_request(&out, &dest);
    assert_eq!(
        (req2.iface, req2.header_type, req2.transport_id),
        (Some(a_medium), HeaderType::Type2, Some(bravo_id)),
        "#332: the next attempt still carries the transport header"
    );

    // Measured, not inferred: bravo hears that request and forwards it.
    let out = bravo.handle_packet(InterfaceId(b_to_alpha), &req2.raw);
    assert_eq!(
        link_requests(&out, &dest).len(),
        1,
        "#332: bravo is still named in the header and stays in the route"
    );
}

// ---------------------------------------------------------------------------
// THE GUARD IN ISOLATION
// ---------------------------------------------------------------------------

fn insert_entry(node: &mut Node, dest: [u8; TRUNCATED_HASHBYTES], hops: u8, relay: bool) {
    node.transport.insert_path(
        dest,
        PathEntry {
            hops,
            expires_ms: u64::MAX,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: if relay {
                Some([0xAB; TRUNCATED_HASHBYTES])
            } else {
                None
            },
            via_peer: None,
        },
    );
}

/// A shorter count that still leaves the entry a relay route is adopted — the
/// #330 behaviour the two `mvr_hop_asymmetry` terminus tests pin (3 -> 2).
#[test]
fn a_shorter_count_that_keeps_the_relay_is_adopted() {
    let mut node = make_endpoint();
    let _ = add_iface(&mut node, "N_medium", 1);
    let dest = [0x11; TRUNCATED_HASHBYTES];
    insert_entry(&mut node, dest, 3, true);

    node.transport.rebalance_path_hops(&dest, 2);

    let path = node.transport().path(&dest).expect("entry");
    assert_eq!(path.hops, 2);
    assert!(
        path.needs_relay(),
        "2 hops over a named next hop still relays"
    );
}

/// An entry that names no next hop has nothing to strand, so its count moves
/// freely.
#[test]
fn a_count_on_an_entry_without_a_next_hop_is_adopted() {
    let mut node = make_endpoint();
    let _ = add_iface(&mut node, "N_medium", 1);
    let dest = [0x22; TRUNCATED_HASHBYTES];
    insert_entry(&mut node, dest, 2, false);

    node.transport.rebalance_path_hops(&dest, 1);

    let path = node.transport().path(&dest).expect("entry");
    assert_eq!(path.hops, 1, "no next hop, nothing to protect");
}

/// THE GUARD, isolated: a count that turns `needs_relay()` false while a next
/// hop is still named is refused, and the refusal says so in the return value
/// rather than only in a log. Before the guard this adopted, leaving
/// `hops = 1` on an entry that still named the relay.
#[test]
fn a_count_that_would_strand_the_next_hop_is_refused() {
    let mut node = make_endpoint();
    let _ = add_iface(&mut node, "N_medium", 1);
    let dest = [0x33; TRUNCATED_HASHBYTES];
    insert_entry(&mut node, dest, 2, true);

    assert_eq!(
        node.transport.rebalance_path_hops(&dest, 1),
        PathRebalance::HeldForNextHop {
            hops: 2,
            refused: 1
        },
    );

    let path = node.transport().path(&dest).expect("entry");
    assert_eq!(path.hops, 2, "the entry is untouched");
    assert!(
        path.needs_relay(),
        "the entry keeps its next hop AND keeps routing through it"
    );
}

/// The same refusal for a 0, which is what a local-client count would do to
/// the same entry: `needs_relay()` is false below 2 hops, whatever the reason.
#[test]
fn a_zero_that_would_strand_the_next_hop_is_refused() {
    let mut node = make_endpoint();
    let _ = add_iface(&mut node, "N_medium", 1);
    let dest = [0x44; TRUNCATED_HASHBYTES];
    insert_entry(&mut node, dest, 2, true);

    assert_eq!(
        node.transport.rebalance_path_hops(&dest, 0),
        PathRebalance::HeldForNextHop {
            hops: 2,
            refused: 0
        },
    );
    assert_eq!(node.transport().path(&dest).expect("entry").hops, 2);
}

/// And the outcomes a caller logs for the cases that are not refusals.
#[test]
fn the_outcomes_a_caller_can_log() {
    let mut node = make_endpoint();
    let _ = add_iface(&mut node, "N_medium", 1);
    let dest = [0x55; TRUNCATED_HASHBYTES];

    assert_eq!(
        node.transport.rebalance_path_hops(&dest, 2),
        PathRebalance::NoEntry,
        "nothing to rebalance before an announce installed anything"
    );

    insert_entry(&mut node, dest, 3, true);
    assert_eq!(
        node.transport.rebalance_path_hops(&dest, 3),
        PathRebalance::Unchanged { hops: 3 },
    );
    assert_eq!(
        node.transport.rebalance_path_hops(&dest, 2),
        PathRebalance::Adopted {
            before: 3,
            after: 2
        },
    );
}

// ---------------------------------------------------------------------------
// THE SAME MECHANISM ON A NODE WITH TWO CARRIERS
// ---------------------------------------------------------------------------

/// The proof arriving on a DIFFERENT carrier than the route it answered, which
/// is where the order that asked for this mvr predicted the stranded request
/// would leave "onto the direct interface". It would not have:
/// `rebalance_path_hops` moves `hops` and nothing else, so `interface_index`
/// still names the carrier the ANNOUNCE came in on — bravo's. Before the guard
/// (measured) the request left that carrier as a final-hop packet, where the
/// destination is two hops away and nobody is addressed to forward it. On the
/// emulated cell the two carriers are one and the same, which is why the field
/// symptom is "a coin toss on the bad pair" rather than "no delivery at all".
///
/// The guard keeps the entry a relay route here too, and the ingress carrier of
/// the proof stays what it always was: not a route.
#[test]
fn a_proof_on_another_carrier_does_not_move_the_route_either() {
    let (mut charlie, dest_hash, signing_key, announce) = make_responder();
    let mut bravo = make_relay();
    let mut alpha = make_endpoint();
    let dest = *dest_hash.as_bytes();

    let c_medium = add_iface(&mut charlie, "C_medium", 1);
    let b_from_charlie = add_iface(&mut bravo, "B_from_charlie", 2);
    let b_to_alpha = add_iface(&mut bravo, "B_to_alpha", 3);
    let a_relay = add_iface(&mut alpha, "A_relay", 4);
    let a_direct = add_iface(&mut alpha, "A_direct", 5);
    let bravo_id = *bravo.identity().hash();

    let relayed = forward_announce(&mut bravo, b_from_charlie, &announce);
    let _ = alpha.handle_packet(InterfaceId(a_relay), &relayed[0]);

    let (_link1, _routed, out) = alpha.connect(dest_hash, &signing_key).expect("connect");
    let req1 = one_link_request(&out, &dest);
    assert_eq!(
        (req1.iface, req1.header_type, req1.transport_id),
        (Some(a_relay), HeaderType::Type2, Some(bravo_id)),
        "attempt 1 leaves over bravo's carrier with bravo in the header"
    );

    let out = bravo.handle_packet(InterfaceId(b_to_alpha), &req1.raw);
    let forwarded = one_packet(&out);
    let out = charlie.handle_packet(InterfaceId(c_medium), &forwarded);
    let proof = one_packet(&out);

    // The proof comes back on the OTHER carrier, one hop.
    let out = alpha.handle_packet(InterfaceId(a_direct), &proof);
    assert!(has_link_established(&out), "the proof must be accepted");

    let path = alpha.transport().path(&dest).expect("path still there");
    assert_eq!(
        path.interface_index, a_relay,
        "the rebalance never moves the interface, whatever it does to hops"
    );

    assert_eq!(
        path.hops, 2,
        "#332: and it never moves the count below 2 here"
    );

    let (_link2, _routed2, out) = alpha.connect(dest_hash, &signing_key).expect("connect");
    let req2 = one_link_request(&out, &dest);
    assert_eq!(
        (req2.iface, req2.header_type, req2.transport_id),
        (Some(a_relay), HeaderType::Type2, Some(bravo_id)),
        "#332: still relayed over bravo's carrier; before the guard this was a \
         final-hop packet on that same carrier — not on the direct one"
    );
}
