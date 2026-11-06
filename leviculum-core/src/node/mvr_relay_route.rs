//! Steering a link through a chosen relay (+ciris, leviculum#91).
//!
//! `A` reaches `B` through two transport nodes, `C1` and `C2`, each on its
//! own interface of `A`. Both relay `B`'s announce; `A`'s path table keeps
//! one of them, and its relay table keeps both. `connect_via` opens a link
//! through the one asked for, two such links can be up at once, and a link
//! opened with `connect` still follows the learned path.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::memory_storage::MemoryStorage;
use crate::node::{ConnectViaError, NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};
use crate::DestinationHash;

type Relay = NodeCore<OsRng, MockClock, MemoryStorage>;
type Endpoint = NodeCore<OsRng, MockClock, MemoryStorage>;

fn add_iface<C: Clock, S: crate::traits::Storage>(
    node: &mut NodeCore<OsRng, C, S>,
    name: &'static str,
) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// Each outbound packet with the interface it is for (`None`: broadcast).
fn sent(output: &TickOutput) -> Vec<(Option<usize>, Vec<u8>)> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::SendPacket { iface, data, .. } => (Some(iface.0), data.clone()),
            Action::Broadcast { data, .. } => (None, data.clone()),
        })
        .collect()
}

fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
    sent(output).into_iter().map(|(_, d)| d).collect()
}

struct Mesh {
    a: Endpoint,
    b: Endpoint,
    c: [Relay; 2],
    /// A's interface to each relay.
    a_to: [usize; 2],
    /// Each relay's interface to A, then to B.
    c_a: [usize; 2],
    c_b: [usize; 2],
    /// B's interface to each relay.
    b_to: [usize; 2],
    dest: DestinationHash,
    key: [u8; 32],
    a_events: Vec<NodeEvent>,
}

impl Mesh {
    fn new() -> Self {
        let identity = Identity::generate(&mut OsRng);
        let key = identity.ed25519_verifying().to_bytes();
        let endpoint = || -> Endpoint {
            NodeCoreBuilder::new().build(
                OsRng,
                MockClock::new(TEST_TIME_MS),
                MemoryStorage::with_defaults(),
            )
        };
        let mut b = endpoint();
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "mvrapp",
            &["via"],
        )
        .unwrap();
        dest.set_accepts_links(true);
        let dest_hash = *dest.hash();
        let announce = dest
            .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
            .unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = announce.pack(&mut buf).unwrap();
        let announce_raw = buf[..len].to_vec();
        b.register_destination(dest);

        let relay = || -> Relay {
            NodeCoreBuilder::new().enable_transport(true).build(
                OsRng,
                MockClock::new(TEST_TIME_MS),
                MemoryStorage::with_defaults(),
            )
        };
        let mut c = [relay(), relay()];
        let mut a = endpoint();
        a.set_relay_route_bounds(
            crate::relay_routes::DEFAULT_RELAYS_PER_DESTINATION,
            crate::relay_routes::DEFAULT_RELAY_DESTINATIONS,
        );
        let a_to = [add_iface(&mut a, "A_C1"), add_iface(&mut a, "A_C2")];
        let c_a = [add_iface(&mut c[0], "C1_A"), add_iface(&mut c[1], "C2_A")];
        let c_b = [add_iface(&mut c[0], "C1_B"), add_iface(&mut c[1], "C2_B")];
        let b_to = [add_iface(&mut b, "B_C1"), add_iface(&mut b, "B_C2")];

        let mut mesh = Mesh {
            a,
            b,
            c,
            a_to,
            c_a,
            c_b,
            b_to,
            dest: dest_hash,
            key,
            a_events: Vec::new(),
        };
        // B announces to both relays; each rebroadcasts it toward A.
        for i in 0..2 {
            let _ = mesh.c[i].handle_packet(InterfaceId(mesh.c_b[i]), &announce_raw);
            let now = mesh.c[i].transport().clock().now_ms();
            mesh.c[i].transport().clock().set(now + 100_000);
            let rebroadcast = packets(&mesh.c[i].handle_timeout());
            assert!(!rebroadcast.is_empty(), "relay {i} rebroadcasts B");
            let out = mesh
                .a
                .handle_packet(InterfaceId(mesh.a_to[i]), &rebroadcast[0]);
            mesh.a_events.extend(out.events);
        }
        mesh
    }

    fn relay_id(&self, i: usize) -> [u8; 16] {
        self.a
            .relay_routes(&self.dest)
            .into_iter()
            .find(|r| r.interface_index == self.a_to[i])
            .expect("a row per relay")
            .relay
    }

    /// Carry a link request from A out over relay `i` to B, B's proof back,
    /// and A's RTT on to B. Returns whether A established.
    fn establish_over(&mut self, i: usize, request: &[u8]) -> bool {
        let fwd = packets(&self.c[i].handle_packet(InterfaceId(self.c_a[i]), request));
        assert_eq!(fwd.len(), 1, "relay {i} forwards the request");
        let proof = packets(&self.b.handle_packet(InterfaceId(self.b_to[i]), &fwd[0]));
        assert_eq!(proof.len(), 1, "B proves");
        let back = packets(&self.c[i].handle_packet(InterfaceId(self.c_b[i]), &proof[0]));
        assert_eq!(back.len(), 1, "relay {i} returns the proof");
        let out = self.a.handle_packet(InterfaceId(self.a_to[i]), &back[0]);
        let established = out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
        for rtt in packets(&out) {
            for p in packets(&self.c[i].handle_packet(InterfaceId(self.c_a[i]), &rtt)) {
                let _ = self.b.handle_packet(InterfaceId(self.b_to[i]), &p);
            }
        }
        established
    }
}

#[test]
fn both_relays_are_on_file_and_each_was_announced() {
    let mesh = Mesh::new();
    let routes = mesh.a.relay_routes(&mesh.dest);
    assert_eq!(routes.len(), 2, "{routes:?}");
    let mut ifaces: Vec<usize> = routes.iter().map(|r| r.interface_index).collect();
    ifaces.sort_unstable();
    assert_eq!(ifaces, mesh.a_to.to_vec());
    let learned = mesh
        .a_events
        .iter()
        .filter(|e| matches!(e, NodeEvent::RelayRouteLearned { .. }))
        .count();
    assert_eq!(learned, 2, "one event per relay row");
}

/// The witness from the issue: whichever relay the path table keeps, a link
/// asked for through the other goes through the other, on its interface;
/// a second link through the first relay comes up beside it.
#[test]
fn a_link_goes_through_the_relay_asked_for_and_two_can_be_up_at_once() {
    let mut mesh = Mesh::new();
    let learned_iface = mesh
        .a
        .transport()
        .path(mesh.dest.as_bytes())
        .expect("a learned path")
        .interface_index;
    let other = if learned_iface == mesh.a_to[0] { 1 } else { 0 };
    let kept = 1 - other;

    let relay = mesh.relay_id(other);
    let key = mesh.key;
    let dest = mesh.dest;
    let (first, out) = mesh
        .a
        .connect_via(dest, &key, relay, None)
        .expect("a row for that relay");
    let out = sent(&out);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].0,
        Some(mesh.a_to[other]),
        "the request leaves on the chosen relay's interface only"
    );
    assert!(mesh.establish_over(other, &out[0].1));

    // Make-before-break: a second link to B through the other relay.
    let relay_kept = mesh.relay_id(kept);
    let (second, out) = mesh
        .a
        .connect_via(dest, &key, relay_kept, None)
        .expect("a row for the kept relay");
    let out = sent(&out);
    assert_eq!(out[0].0, Some(mesh.a_to[kept]));
    assert!(mesh.establish_over(kept, &out[0].1));
    assert_eq!(mesh.a.active_link_count(), 2, "both links are up");

    let via = |id: &LinkId| {
        mesh.a
            .link_list()
            .into_iter()
            .find(|l| l.link_id == *id)
            .and_then(|l| l.via_relay)
    };
    assert_eq!(via(&first), Some(relay));
    assert_eq!(via(&second), Some(relay_kept));
    // The responder reads the relay off its interface: one relay per side.
    assert_eq!(mesh.b.link_list().len(), 2);
}

#[test]
fn an_unknown_relay_is_refused_and_connect_still_follows_the_learned_path() {
    let mut mesh = Mesh::new();
    let key = mesh.key;
    let dest = mesh.dest;
    assert_eq!(
        mesh.a
            .connect_via(dest, &key, [0xEE; 16], None)
            .unwrap_err(),
        ConnectViaError::NoRouteViaRelay
    );
    let learned_iface = mesh
        .a
        .transport()
        .path(dest.as_bytes())
        .unwrap()
        .interface_index;
    let (_, _, out) = mesh.a.connect(dest, &key).expect("connect");
    assert_eq!(sent(&out)[0].0, Some(learned_iface));
}

/// A relay that drops the request: with the host's budget the link fails
/// once the budget is spent, with no retry, and frees the row's pin.
#[test]
fn a_host_budget_is_one_attempt_and_no_retry() {
    let mut mesh = Mesh::new();
    let relay = mesh.relay_id(0);
    let key = mesh.key;
    let dest = mesh.dest;
    let (_link, out) = mesh
        .a
        .connect_via(dest, &key, relay, Some(2_000))
        .expect("connect_via");
    assert_eq!(packets(&out).len(), 1);
    // The relay never answers.
    let now = mesh.a.transport().clock().now_ms();
    mesh.a.transport().clock().set(now + 4_000);
    let out = mesh.a.handle_timeout();
    let retried = packets(&out).iter().any(|raw| {
        crate::packet::Packet::unpack(raw)
            .is_ok_and(|p| p.flags.packet_type == crate::packet::PacketType::LinkRequest)
    });
    assert!(!retried, "no retried request");
    assert_eq!(mesh.a.link_list().len(), 0, "the link is gone");
}

// leviculum#93: what a relay forwards, metered, refused and capped.

/// A link request the relay's rule refuses is never forwarded: the link
/// cannot establish through it, and the refusal is counted.
#[test]
fn a_refused_request_is_not_forwarded() {
    let mut mesh = Mesh::new();
    mesh.c[0].set_forward_admission(Some(std::boxed::Box::new(
        |_: &crate::forward_meter::ForwardRequest| crate::forward_meter::ForwardDecision::Refuse,
    )));
    let relay = mesh.relay_id(0);
    let (key, dest) = (mesh.key, mesh.dest);
    let (_, out) = mesh.a.connect_via(dest, &key, relay, Some(2_000)).unwrap();
    let fwd = packets(&mesh.c[0].handle_packet(InterfaceId(mesh.c_a[0]), &packets(&out)[0]));
    assert!(fwd.is_empty(), "not forwarded: {fwd:?}");
    assert_eq!(mesh.c[0].forward_refusals(), 1);
    assert!(mesh.c[0].forwarded_links().is_empty());
}

/// Admitted links are metered each way, and a byte cap drops above it.
#[test]
fn forwarded_links_are_metered_and_a_cap_drops_above_it() {
    let mut mesh = Mesh::new();
    mesh.c[1].set_forward_admission(Some(std::boxed::Box::new(
        |_: &crate::forward_meter::ForwardRequest| crate::forward_meter::ForwardDecision::Admit {
            max_bytes_per_s: Some(400),
            max_packets_per_s: None,
        },
    )));
    let relay = mesh.relay_id(1);
    let (key, dest) = (mesh.key, mesh.dest);
    let (link, out) = mesh.a.connect_via(dest, &key, relay, None).unwrap();
    assert!(mesh.establish_over(1, &packets(&out)[0]));
    let before = mesh.c[1].forwarded_links()[0];
    assert!(
        before.toward_destination.packets >= 1,
        "the RTT packet went through: {before:?}"
    );

    // Five 200-byte messages at once: a 400 B/s cap passes about two.
    let mut forwarded = 0;
    for _ in 0..5 {
        let Ok(out) = mesh.a.send_on_link(&link, &[7u8; 200]) else {
            let now = mesh.a.transport().clock().now_ms();
            mesh.a.transport().clock().set(now + 50);
            continue;
        };
        for p in packets(&out) {
            forwarded += packets(&mesh.c[1].handle_packet(InterfaceId(mesh.c_a[1]), &p)).len();
        }
    }
    let after = mesh.c[1].forwarded_links()[0];
    assert!(after.toward_destination.dropped_over_cap > 0, "{after:?}");
    assert_eq!(
        after.toward_destination.packets - before.toward_destination.packets,
        forwarded as u64,
        "counted what it forwarded"
    );
    assert!(after.toward_destination.bytes > before.toward_destination.bytes);
    assert_eq!(after.link_id, *link.as_bytes());
    assert_eq!(after.destination_hash, *dest.as_bytes());
}

/// leviculum#97: closing everything takes a link still establishing too, the
/// one a host's own close pass cannot have seen.
#[test]
fn close_all_links_takes_one_still_establishing() {
    let mut mesh = Mesh::new();
    let relay = mesh.relay_id(0);
    let (key, dest) = (mesh.key, mesh.dest);
    let (_, out) = mesh.a.connect_via(dest, &key, relay, None).unwrap();
    let fwd = packets(&mesh.c[0].handle_packet(InterfaceId(mesh.c_a[0]), &packets(&out)[0]));
    let _ = mesh.b.handle_packet(InterfaceId(mesh.b_to[0]), &fwd[0]);
    assert_eq!(mesh.b.link_list().len(), 1, "landing on B");
    assert_eq!(mesh.a.link_list().len(), 1, "pending on A");
    let closed = mesh.b.close_all_links();
    assert!(closed
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkClosed { .. })));
    assert!(mesh.b.link_list().is_empty());
    let _ = mesh.a.close_all_links();
    assert!(mesh.a.link_list().is_empty());
}
