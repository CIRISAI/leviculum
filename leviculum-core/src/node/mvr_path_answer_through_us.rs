//! mvr: order 1672 — a path answer that runs back through us, and a dead
//! next hop nothing ever leaves (Refs #230, #308, #24).
//!
//! ## The field case (miauhaus, 2026-11-11 09:14 to 09:26 UTC)
//!
//! miauhaus held rns.moscow at 3 hops via the autoconnect interface. A link
//! request's proof came back at 6 hops and `PATH_REBALANCE from=3 to=6` lifted
//! the stored count. The next path request was answered with a 4-hop copy of
//! the SAME emission from another neighbour, and the same-emission deviation
//! (`SameEmissionRule::FewerHopsWins`, 4 < 6) adopted it:
//!
//! ```text
//! PATH_REBALANCE dst=9d8f681f… from=3 to=6
//! PATH_ADD dst=9d8f681f… hops=4 iface=auto/end0/fe31d1bd next_hop=ac905dfe… reason=same_emission_fewer_hops
//! ```
//!
//! Every link request after it went to that next hop and expired unproven,
//! five in a row. The requests came from a local client (lnomad on
//! miauhaus), and Python's rule for a local client's expired request only
//! asks the path again (Transport.py:715-717), so the path was never marked
//! unresponsive and every answer — the same emission at 4, 4 and 6 hops —
//! was refused. The 6-hop route that had just carried a page stayed unused
//! until rns.moscow announced again.
//!
//! ## What the reference does
//!
//! Python never lets a second copy of an emission replace a path in the
//! equal-or-fewer branch: it wants an unseen random blob AND a strictly newer
//! emission (reference/Reticulum/RNS/Transport.py:1772). Its only same-emission
//! acceptance is the unresponsive one, in the more-hops branch
//! (Transport.py:1817-1824). The 1.5 rebalance moves the stored count, but
//! with no hop count in the same-emission test the rebalance cannot turn an
//! echo into a better route there. Ours has a hop count in it.
//!
//! ## What is pinned
//!
//! 1. The loop, with three nodes: alpha (miauhaus) learns the destination at
//!    3 hops from an upstream relay, bravo learns it from alpha's rebroadcast
//!    at 4, alpha's count is rebalanced to 6, bravo's rebroadcast reaches
//!    alpha at 5 and the field's 4-hop copy arrives too. Before the fix alpha
//!    adopted bravo's copy and a client's link request went alpha -> bravo ->
//!    alpha -> bravo. After it both copies are refused (`PATH_REFUSED
//!    reason=longer_than_installed`) and the request goes upstream.
//! 2. The heal the deviation exists for still works after a rebalance: a copy
//!    shorter than the installed count is taken.
//! 3. The dead next hop of fetches 2 and 3: five local-client link requests
//!    to one next hop, each expiring unproven, each answered by the same
//!    three copies. Before the fix the path never moved; after it the first
//!    expiry moves it to the other next hop's 6-hop copy, and the dead hop's
//!    4-hop copies do not move it back.
//! 4. The race of fetch 1: a request expiring after a LATER request over the
//!    same next hop was proven marks nothing.
//!
//! Sans-I/O: two or three nodes, mock interfaces, deterministic, sub-second.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketContext, PacketType, TransportType};
use crate::storage_types::LinkEntry;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::{Action, InterfaceId, PathEntry, TickOutput};

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The upstream relay alpha learned the destination from (miauhaus's
/// `e54d52a6` on `autoconnect/Dark_Doodad_23`).
const UPSTREAM: [u8; TRUNCATED_HASHBYTES] = [0xE5; TRUNCATED_HASHBYTES];
/// The neighbour whose 4-hop answers took the path (`ac905dfe`).
const NEIGHBOUR: [u8; TRUNCATED_HASHBYTES] = [0xAC; TRUNCATED_HASHBYTES];

fn relay() -> Box<Node> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

fn add_iface(node: &mut Node, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// rns.moscow: a link-accepting destination and its one announce, wire hops 0.
struct Origin {
    dest_hash: crate::DestinationHash,
    signing_key: [u8; 32],
    raw: Vec<u8>,
}

fn origin() -> Origin {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["moscow"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    Origin {
        dest_hash: *dest.hash(),
        signing_key,
        raw: buf[..len].to_vec(),
    }
}

impl Origin {
    fn dest(&self) -> [u8; TRUNCATED_HASHBYTES] {
        *self.dest_hash.as_bytes()
    }

    /// The same emission as a relay `via` hands it on: only the header moves,
    /// so the random blob, and with it the emission, is the original's.
    /// Processed at `wire_hops + 1`.
    fn copy(&self, wire_hops: u8, via: [u8; TRUNCATED_HASHBYTES], response: bool) -> Vec<u8> {
        let mut p = Packet::unpack(&self.raw).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = wire_hops;
        p.transport_id = Some(via);
        if response {
            p.context = PacketContext::PathResponse;
        }
        let mut buf = [0u8; MTU];
        let len = p.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }
}

/// What `out` puts on interface `iface`: its sends there and its broadcasts
/// not excluding it.
fn sent_on(out: &TickOutput, iface: usize) -> Vec<Vec<u8>> {
    out.actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface: i, data, .. } if i.0 == iface => Some(data.clone()),
            Action::Broadcast {
                data,
                exclude_iface,
                ..
            } if exclude_iface.map(|e| e.0) != Some(iface) => Some(data.clone()),
            _ => None,
        })
        .collect()
}

fn link_requests(frames: &[Vec<u8>]) -> Vec<Vec<u8>> {
    frames
        .iter()
        .filter(|f| Packet::unpack(f).is_ok_and(|p| p.flags.packet_type == PacketType::LinkRequest))
        .cloned()
        .collect()
}

fn advance(node: &mut Node, ms: u64) -> TickOutput {
    let now = node.transport().clock().now_ms();
    node.transport().clock().set(now + ms);
    node.handle_timeout()
}

fn next_hop(node: &Node, dest: &[u8; TRUNCATED_HASHBYTES]) -> Option<[u8; TRUNCATED_HASHBYTES]> {
    node.transport().path(dest).and_then(|p| p.next_hop)
}

fn hops(node: &Node, dest: &[u8; TRUNCATED_HASHBYTES]) -> Option<u8> {
    node.transport().path(dest).map(|p| p.hops)
}

/// A shared-instance client of alpha that reaches the destination through
/// alpha (lnomad on miauhaus): its link requests arrive on alpha's local
/// interface at hops 0.
fn client_of(alpha_id: [u8; TRUNCATED_HASHBYTES], o: &Origin) -> (Box<Node>, usize) {
    let mut client = NodeCoreBuilder::new().build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let uplink = add_iface(&mut client, "uplink", 9);
    client.transport.insert_path(
        o.dest(),
        PathEntry {
            hops: 4,
            expires_ms: u64::MAX,
            interface_index: uplink,
            random_blobs: Vec::new(),
            next_hop: Some(alpha_id),
            via_peer: None,
        },
    );
    (client, uplink)
}

fn one_link_request(client: &mut Node, uplink: usize, o: &Origin) -> Vec<u8> {
    let (_link, _routed, out) = client.connect(o.dest_hash, &o.signing_key).unwrap();
    let mut lrs = link_requests(&sent_on(&out, uplink));
    assert_eq!(lrs.len(), 1, "the client sends one link request");
    lrs.remove(0)
}

// ---------------------------------------------------------------------------
// 1. THE LOOP
// ---------------------------------------------------------------------------

#[test]
fn a_rebalanced_count_does_not_make_an_echo_of_us_a_shorter_route() {
    let o = origin();
    let dest = o.dest();
    let mut alpha = relay();
    let mut bravo = relay();
    let a_up = add_iface(&mut alpha, "autoconnect", 1);
    let a_end0 = add_iface(&mut alpha, "end0", 2);
    let a_tcp = add_iface(&mut alpha, "tcp_client_5", 3);
    let a_local = add_iface(&mut alpha, "local", 4);
    alpha.transport.set_local_client(a_local, true);
    let b_end0 = add_iface(&mut bravo, "end0", 5);
    let alpha_id = *alpha.identity().hash();
    let bravo_id = *bravo.identity().hash();

    let ((), logs) = crate::test_log_capture::with_captured_logs(|| {
        // alpha learns the destination at 3 hops from upstream.
        let _ = alpha.handle_packet(InterfaceId(a_up), &o.copy(2, UPSTREAM, false));
        assert_eq!(
            (hops(&alpha, &dest), next_hop(&alpha, &dest)),
            (Some(3), Some(UPSTREAM))
        );

        // bravo learns it from alpha's rebroadcast: through us, at 4.
        let out = advance(&mut alpha, 100_000);
        for f in sent_on(&out, a_end0) {
            let _ = bravo.handle_packet(InterfaceId(b_end0), &f);
        }
        assert_eq!(
            (hops(&bravo, &dest), next_hop(&bravo, &dest)),
            (Some(4), Some(alpha_id)),
            "precondition: bravo routes the destination through alpha"
        );

        // A proof came back at 6 hops: alpha's count is rebalanced.
        alpha.transport.rebalance_path_hops(&dest, 6);
        assert_eq!(hops(&alpha, &dest), Some(6));

        // bravo's own rebroadcast of the emission reaches alpha at 5. (bravo
        // does not answer alpha's path request: alpha is its next hop,
        // `next_hop_is_requestor`.)
        let out = advance(&mut bravo, 100_000);
        for f in sent_on(&out, b_end0) {
            let _ = alpha.handle_packet(InterfaceId(a_end0), &f);
        }
        // And the field's shape: a 4-hop answer of the same emission.
        let _ = alpha.handle_packet(InterfaceId(a_tcp), &o.copy(3, NEIGHBOUR, true));
    });

    assert_eq!(
        next_hop(&alpha, &dest),
        Some(UPSTREAM),
        "before order 1672 alpha took bravo's 5-hop copy, 5 < 6 \
         (reason=same_emission_fewer_hops), and routed the destination \
         through a neighbour that routes it through alpha; logs={logs}"
    );
    assert_eq!(hops(&alpha, &dest), Some(6), "the rebalanced count stands");
    assert!(
        !logs.contains("reason=\"same_emission_fewer_hops\""),
        "no copy may win on hops against the rebalanced count"
    );
    let refused = logs
        .lines()
        .filter(|l| {
            l.contains("event=\"PATH_REFUSED\"") && l.contains("reason=\"longer_than_installed\"")
        })
        .count();
    assert_eq!(
        refused, 2,
        "both copies, bravo's 5 and the 4, are refused by name; logs={logs}"
    );
    let _ = bravo_id;

    // The client's link request now goes upstream, not into the loop.
    let (mut client, uplink) = client_of(alpha_id, &o);
    let lr = one_link_request(&mut client, uplink, &o);
    let out = alpha.handle_packet(InterfaceId(a_local), &lr);
    assert_eq!(link_requests(&sent_on(&out, a_end0)).len(), 0);
    assert_eq!(link_requests(&sent_on(&out, a_up)).len(), 1);
}

/// The loop before the fix, from bravo's side: what alpha would have handed
/// bravo comes straight back. Pinned so the claim in the report is a
/// measurement: bravo forwards a request it gets from alpha back to alpha.
#[test]
fn bravo_returns_a_request_for_the_destination_to_alpha() {
    let o = origin();
    let mut alpha = relay();
    let mut bravo = relay();
    let a_up = add_iface(&mut alpha, "autoconnect", 1);
    let a_end0 = add_iface(&mut alpha, "end0", 2);
    let b_end0 = add_iface(&mut bravo, "end0", 5);
    let alpha_id = *alpha.identity().hash();
    let bravo_id = *bravo.identity().hash();

    let _ = alpha.handle_packet(InterfaceId(a_up), &o.copy(2, UPSTREAM, false));
    let out = advance(&mut alpha, 100_000);
    for f in sent_on(&out, a_end0) {
        let _ = bravo.handle_packet(InterfaceId(b_end0), &f);
    }

    // A request addressed to bravo for the destination (what alpha sent
    // while it routed through bravo).
    let (mut sender, uplink) = client_of(bravo_id, &o);
    let lr = one_link_request(&mut sender, uplink, &o);
    let out = bravo.handle_packet(InterfaceId(b_end0), &lr);
    let back = link_requests(&sent_on(&out, b_end0));
    assert_eq!(back.len(), 1, "bravo forwards the request");
    let p = Packet::unpack(&back[0]).unwrap();
    assert_eq!(
        p.transport_id,
        Some(alpha_id),
        "and names alpha as its next hop: the two-node loop"
    );
}

// ---------------------------------------------------------------------------
// 2. THE HEAL STILL WORKS
// ---------------------------------------------------------------------------

#[test]
fn a_copy_shorter_than_the_installed_count_still_wins_after_a_rebalance() {
    let o = origin();
    let dest = o.dest();
    let mut alpha = relay();
    let a_up = add_iface(&mut alpha, "autoconnect", 1);
    let a_end0 = add_iface(&mut alpha, "end0", 2);

    let _ = alpha.handle_packet(InterfaceId(a_up), &o.copy(2, UPSTREAM, false));
    alpha.transport.rebalance_path_hops(&dest, 6);

    let ((), logs) = crate::test_log_capture::with_captured_logs(|| {
        let _ = alpha.handle_packet(InterfaceId(a_end0), &o.copy(1, NEIGHBOUR, true));
    });
    assert_eq!(
        (hops(&alpha, &dest), next_hop(&alpha, &dest)),
        (Some(2), Some(NEIGHBOUR)),
        "2 < 3: a genuinely shorter copy of the emission is the heal \
         `FewerHopsWins` exists for (Codeberg #231)"
    );
    assert!(logs.contains("reason=\"same_emission_fewer_hops\""));
}

// ---------------------------------------------------------------------------
// 3. THE DEAD NEXT HOP
// ---------------------------------------------------------------------------

/// Fetches 2 and 3: the path sits on a next hop that proves nothing. Five
/// local-client link requests, each expiring unproven, each followed by the
/// answers the field saw (4 hops on end0, 4 on tcp, 6 upstream).
#[test]
fn a_dead_next_hop_is_left_by_the_second_expiry_and_not_taken_back() {
    let o = origin();
    let dest = o.dest();
    let mut alpha = relay();
    let a_up = add_iface(&mut alpha, "autoconnect", 1);
    let a_end0 = add_iface(&mut alpha, "end0", 2);
    let a_tcp = add_iface(&mut alpha, "tcp_client_5", 3);
    let a_local = add_iface(&mut alpha, "local", 4);
    alpha.transport.set_local_client(a_local, true);
    let alpha_id = *alpha.identity().hash();

    // The state fetch 1 left: the neighbour's 4-hop copy holds the path, the
    // upstream 6-hop copy of the same emission is refused.
    let _ = alpha.handle_packet(InterfaceId(a_end0), &o.copy(3, NEIGHBOUR, true));
    let _ = alpha.handle_packet(InterfaceId(a_up), &o.copy(5, UPSTREAM, true));
    assert_eq!(
        (hops(&alpha, &dest), next_hop(&alpha, &dest)),
        (Some(4), Some(NEIGHBOUR)),
        "precondition: the 6-hop copy does not displace the 4"
    );

    let (mut client, uplink) = client_of(alpha_id, &o);
    let mut moved_at = None;
    let mut forwarded_to = Vec::new();
    let ((), logs) = crate::test_log_capture::with_captured_logs(|| {
        for round in 1..=5 {
            let lr = one_link_request(&mut client, uplink, &o);
            let out = alpha.handle_packet(InterfaceId(a_local), &lr);
            let to_neighbour = link_requests(&sent_on(&out, a_end0)).len();
            let to_up = link_requests(&sent_on(&out, a_up)).len();
            forwarded_to.push((round, to_neighbour, to_up));
            if to_up == 1 {
                // The upstream route proves; that link is not under test.
                break;
            }
            // Nothing proves: the entry expires ((4 + 2) x 6 s = 36 s).
            let _ = advance(&mut alpha, 40_000);
            // The answers to alpha's path request, in the field's order.
            let _ = alpha.handle_packet(InterfaceId(a_end0), &o.copy(3, NEIGHBOUR, true));
            let _ = alpha.handle_packet(InterfaceId(a_tcp), &o.copy(3, NEIGHBOUR, true));
            let _ = alpha.handle_packet(InterfaceId(a_up), &o.copy(5, UPSTREAM, true));
            if moved_at.is_none() && next_hop(&alpha, &dest) == Some(UPSTREAM) {
                moved_at = Some(round);
            }
            // A later answer from the dead hop must not take the path back.
            let _ = alpha.handle_packet(InterfaceId(a_end0), &o.copy(3, NEIGHBOUR, true));
        }
    });

    let moved = moved_at.unwrap_or_else(|| {
        panic!(
            "the path never left the dead next hop: before order 1672 a local \
             client's expired request only asked the path again \
             (Transport.py:715-717) and every answer was refused; \
             rounds={forwarded_to:?} logs={logs}"
        )
    });
    assert!(
        moved <= 2,
        "moved at round {moved}, want by the second expiry"
    );
    assert_eq!(
        (hops(&alpha, &dest), next_hop(&alpha, &dest)),
        (Some(6), Some(UPSTREAM)),
        "the dead hop's shorter copies do not move the path back; logs={logs}"
    );
    assert!(logs.contains("reason=\"unresponsive_same_emission\""));
    assert!(logs.contains("event=\"PATH_UNRESPONSIVE\""));
    assert!(
        logs.contains("reason=\"next_hop_unresponsive\""),
        "the dead hop's copies are refused by name; logs={logs}"
    );
    let last = forwarded_to.last().copied();
    assert_eq!(
        last.map(|(_, n, u)| (n, u)),
        Some((0, 1)),
        "the next request goes upstream: {forwarded_to:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. A LATER PROOF OUTRANKS AN EARLIER EXPIRY
// ---------------------------------------------------------------------------

fn link_entry(
    dest: [u8; TRUNCATED_HASHBYTES],
    up: usize,
    local: usize,
    at: u64,
    validated: bool,
) -> LinkEntry {
    LinkEntry {
        timestamp_ms: at,
        next_hop_interface_index: up,
        remaining_hops: 3,
        received_interface_index: local,
        hops: 0,
        validated,
        proof_timeout_ms: at + 30_000,
        destination_hash: dest,
        peer_signing_key: None,
        received_peer: None,
        next_hop_peer: None,
    }
}

/// Fetch 1's race: the first request expired 1.6 s after the second one's
/// proof came back over the same next hop. That expiry marks nothing, so the
/// neighbour's longer copy stays refused and the working route stays.
#[test]
fn an_expiry_after_a_later_proof_marks_nothing() {
    let o = origin();
    let dest = o.dest();
    let mut alpha = relay();
    let a_up = add_iface(&mut alpha, "autoconnect", 1);
    let a_end0 = add_iface(&mut alpha, "end0", 2);
    let a_local = add_iface(&mut alpha, "local", 4);
    alpha.transport.set_local_client(a_local, true);
    let _ = alpha.handle_packet(InterfaceId(a_up), &o.copy(2, UPSTREAM, false));

    let t0 = alpha.transport().clock().now_ms();
    alpha.transport.storage_mut().set_link_entry(
        [0x01; TRUNCATED_HASHBYTES],
        link_entry(dest, a_up, a_local, t0, false),
    );
    alpha.transport.storage_mut().set_link_entry(
        [0x02; TRUNCATED_HASHBYTES],
        link_entry(dest, a_up, a_local, t0 + 26_000, true),
    );
    let _ = advance(&mut alpha, 31_000);
    assert!(
        !alpha
            .transport()
            .storage()
            .has_link_entry(&[0x01; TRUNCATED_HASHBYTES]),
        "precondition: the first entry expired"
    );
    assert!(
        !alpha.transport.path_is_unresponsive(&dest),
        "a request proven later over the same destination outranks the expiry"
    );

    let _ = alpha.handle_packet(InterfaceId(a_end0), &o.copy(3, NEIGHBOUR, true));
    assert_eq!(next_hop(&alpha, &dest), Some(UPSTREAM));

    // Positive control: without the later proof the same expiry marks.
    let mut beta = relay();
    let b_up = add_iface(&mut beta, "autoconnect", 1);
    let _ = add_iface(&mut beta, "end0", 2);
    let b_local = add_iface(&mut beta, "local", 4);
    beta.transport.set_local_client(b_local, true);
    let _ = beta.handle_packet(InterfaceId(b_up), &o.copy(2, UPSTREAM, false));
    let t0 = beta.transport().clock().now_ms();
    beta.transport.storage_mut().set_link_entry(
        [0x01; TRUNCATED_HASHBYTES],
        link_entry(dest, b_up, b_local, t0, false),
    );
    let _ = advance(&mut beta, 31_000);
    assert!(
        beta.transport.path_is_unresponsive(&dest),
        "positive control: an unproven local-client request marks the path"
    );
}
