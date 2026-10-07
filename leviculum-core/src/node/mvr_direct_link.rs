//! mvr: a link upgrades onto a direct interface, falls back off it, and
//! a refused or stalled upgrade leaves the link exactly as it was
//! (+ciris, leviculum#70).
//!
//! Sans-I/O: the two endpoints share one mock mesh interface standing in
//! for the relayed path; the driver's probe and punch are played by hand,
//! each side gaining a second mock interface as its punched socket.

extern crate std;

use std::string::String;
use std::vec::Vec;

use core::net::SocketAddr;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::direct_link::wire::{self, REJECT_POLICY};
use crate::direct_link::{Failure, ProbeProtocol};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{
    DirectLinkConfig, DirectLinkError, DirectLinkJob, DirectLinkPolicy, NodeCore, NodeCoreBuilder,
    NodeEvent,
};
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

/// Packets the tick put on the wire, with the interface each left on
/// (`None` for a broadcast).
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

/// Deliver, collecting what comes back and the events raised.
fn deliver(target: &mut Node, iface: usize, pkts: Vec<Vec<u8>>) -> (Vec<Vec<u8>>, Vec<NodeEvent>) {
    let mut out = Vec::new();
    let mut events = Vec::new();
    for pkt in pkts {
        let tick = target.handle_packet(InterfaceId(iface), &pkt);
        out.extend(packets(&tick));
        events.extend(tick.events);
    }
    (out, events)
}

struct Pair {
    a: Node,
    b: Node,
    a_mesh: usize,
    b_mesh: usize,
    link: LinkId,
    dest: crate::destination::DestinationHash,
    signing_key: [u8; 32],
    a_events: Vec<NodeEvent>,
    b_events: Vec<NodeEvent>,
}

impl Pair {
    fn new() -> Self {
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
            &["directlink"],
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
        let (link, _, out) = a.connect(dest_hash, &signing_key).expect("connect");
        let mut pair = Pair {
            a,
            b,
            a_mesh,
            b_mesh,
            link,
            dest: dest_hash,
            signing_key,
            a_events: Vec::new(),
            b_events: Vec::new(),
        };
        pair.pump_from_a(packets(&out));
        assert_eq!(pair.a.active_link_count(), 1);
        assert_eq!(pair.b.active_link_count(), 1);
        pair
    }

    /// Carry packets back and forth over the mesh until both go quiet.
    fn pump_from_a(&mut self, mut to_b: Vec<Vec<u8>>) {
        for _ in 0..16 {
            if to_b.is_empty() {
                return;
            }
            let (to_a, ev) = deliver(&mut self.b, self.b_mesh, to_b);
            self.b_events.extend(ev);
            let (next, ev) = deliver(&mut self.a, self.a_mesh, to_a);
            self.a_events.extend(ev);
            to_b = next;
        }
    }

    fn pump_from_b(&mut self, to_a: Vec<Vec<u8>>) {
        let (to_b, ev) = deliver(&mut self.a, self.a_mesh, to_a);
        self.a_events.extend(ev);
        self.pump_from_a(to_b);
    }

    fn configure(&mut self, b_policy: DirectLinkPolicy) {
        self.a.set_direct_link_config(DirectLinkConfig {
            policy: DirectLinkPolicy::Reject,
            facilitator: Some(facilitator()),
            protocol: ProbeProtocol::Rnsp,
        });
        self.b.set_direct_link_config(DirectLinkConfig {
            policy: b_policy,
            facilitator: None,
            protocol: ProbeProtocol::Rnsp,
        });
    }

    fn propose(&mut self) {
        let out = self
            .a
            .propose_direct_link(&self.link)
            .expect("proposal starts");
        self.a_events.extend(out.events.clone());
        self.pump_from_a(packets(&out));
    }

    /// Let `ms` pass a second at a time, running both nodes' timers and
    /// carrying what they send, so keepalives hold the link up.
    fn advance(&mut self, ms: u64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(1_000);
            left -= step;
            self.a.transport.clock().advance(step);
            self.b.transport.clock().advance(step);
            let out = self.a.handle_timeout();
            self.a_events.extend(out.events.clone());
            let a_out = packets(&out);
            let out = self.b.handle_timeout();
            self.b_events.extend(out.events.clone());
            let b_out = packets(&out);
            self.pump_from_a(a_out);
            self.pump_from_b(b_out);
        }
    }
}

fn facilitator() -> SocketAddr {
    "203.0.113.7:4343".parse().unwrap()
}
fn a_public() -> SocketAddr {
    "198.51.100.1:40001".parse().unwrap()
}
fn b_public() -> SocketAddr {
    "192.0.2.9:50002".parse().unwrap()
}

fn only_probe(jobs: &[DirectLinkJob]) -> [u8; 16] {
    match jobs {
        [DirectLinkJob::Probe {
            session,
            server,
            protocol: ProbeProtocol::Rnsp,
        }] if *server == facilitator() => *session,
        other => panic!("expected one probe of the facilitator, got {other:?}"),
    }
}

fn only_punch(jobs: &[DirectLinkJob]) -> ([u8; 16], SocketAddr, [u8; 32]) {
    match jobs {
        [DirectLinkJob::Punch {
            session,
            peer,
            token,
        }] => (*session, *peer, *token),
        other => panic!("expected one punch, got {other:?}"),
    }
}

/// Run an upgrade to the point where both sides are punching.
fn to_punch(pair: &mut Pair) -> ([u8; 16], [u8; 32]) {
    pair.configure(DirectLinkPolicy::AcceptAll);
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());

    // A learns its address; REQUEST goes out and B accepts.
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));
    assert_eq!(only_probe(&pair.b.take_direct_link_jobs()), session);

    // B learns its address; READY goes out and both punch.
    let out = pair.b.direct_link_probed(&session, Some(b_public()));
    let (b_session, b_peer, b_token) = only_punch(&pair.b.take_direct_link_jobs());
    pair.pump_from_b(packets(&out));
    let (a_session, a_peer, a_token) = only_punch(&pair.a.take_direct_link_jobs());

    assert_eq!((a_session, b_session), (session, session));
    assert_eq!(a_peer, b_public(), "A punches toward B's reflexive address");
    assert_eq!(b_peer, a_public(), "B punches toward A's reflexive address");
    assert_eq!(
        a_token, b_token,
        "both ends derive one token from the link key"
    );
    (session, a_token)
}

#[test]
fn an_upgrade_moves_the_link_and_a_lost_interface_moves_it_back() {
    let mut pair = Pair::new();
    let (session, _) = to_punch(&mut pair);

    let a_direct = add_iface(&mut pair.a, "A_direct");
    let b_direct = add_iface(&mut pair.b, "B_direct");
    let out = pair.a.direct_link_punched(&session, Some(a_direct));
    pair.a_events.extend(out.events);
    let out = pair.b.direct_link_punched(&session, Some(b_direct));
    pair.b_events.extend(out.events);

    assert!(pair.a_events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkEstablished { link_id, interface_index, proposed: true }
            if *link_id == pair.link && *interface_index == a_direct
    )));
    assert!(pair.b_events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkEstablished { interface_index, proposed: false, .. }
            if *interface_index == b_direct
    )));
    assert_eq!(pair.a.direct_link_interface(&pair.link), Some(a_direct));
    assert_eq!(pair.b.direct_link_interface(&pair.link), Some(b_direct));

    // Link traffic now leaves on the direct interface (once the channel's
    // pacing after the signals has passed).
    pair.a.transport.clock().advance(1_000);
    let out = pair
        .a
        .send_on_link(&pair.link, b"over the punched path")
        .unwrap();
    let wire = sent(&out);
    assert!(!wire.is_empty());
    assert!(
        wire.iter().all(|(iface, _)| *iface == Some(a_direct)),
        "link packets leave on the direct interface: {wire:?}"
    );
    // And B, receiving them there, delivers them.
    let (_, ev) = deliver(
        &mut pair.b,
        b_direct,
        wire.into_iter().map(|(_, d)| d).collect(),
    );
    assert!(ev.iter().any(|e| matches!(
        e,
        NodeEvent::MessageReceived { data, .. } if data == b"over the punched path"
    )));

    // The direct interface dies: the link goes back to the mesh.
    let out = pair.a.handle_interface_down(InterfaceId(a_direct));
    assert!(out.events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkLost { interface_index, .. } if *interface_index == a_direct
    )));
    assert_eq!(pair.a.direct_link_interface(&pair.link), None);
    pair.a.transport.clock().advance(1_000);
    let out = pair.a.send_on_link(&pair.link, b"relayed again").unwrap();
    assert!(sent(&out)
        .iter()
        .all(|(iface, _)| *iface == Some(pair.a_mesh)));
}

#[test]
fn signals_never_reach_the_application() {
    let mut pair = Pair::new();
    to_punch(&mut pair);
    let leaked = |events: &[NodeEvent]| {
        events.iter().any(|e| {
            matches!(e, NodeEvent::MessageReceived { msgtype, .. }
                if wire::is_signal_msgtype(*msgtype))
        })
    };
    assert!(!leaked(&pair.a_events), "A saw a signal as a message");
    assert!(!leaked(&pair.b_events), "B saw a signal as a message");
}

#[test]
fn the_default_policy_refuses_and_leaves_the_link_alone() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::default());
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));

    assert!(pair.b.take_direct_link_jobs().is_empty(), "B never probes");
    assert!(pair.a_events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkFailed {
            failure: Failure::Rejected(REJECT_POLICY),
            proposed: true,
            ..
        }
    )));
    assert_eq!(
        pair.a.take_direct_link_jobs(),
        [DirectLinkJob::Release { session }],
        "A drops its probe socket"
    );
    // The link still works on the mesh.
    pair.a.transport.clock().advance(1_000);
    let out = pair.a.send_on_link(&pair.link, b"still here").unwrap();
    let (_, ev) = deliver(&mut pair.b, pair.b_mesh, packets(&out));
    assert!(ev.iter().any(|e| matches!(
        e,
        NodeEvent::MessageReceived { data, .. } if data == b"still here"
    )));
}

#[test]
fn identified_only_accepts_once_the_peer_identifies() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::IdentifiedOnly);

    // A is anonymous on this link: B refuses.
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));
    assert!(pair.b.take_direct_link_jobs().is_empty());

    // A identifies, waits out the cooldown, and B now accepts.
    let identity = Identity::generate(&mut OsRng);
    let out = pair
        .a
        .identify_link(&pair.link, &identity)
        .expect("identify");
    pair.pump_from_a(packets(&out));
    pair.advance(crate::node::PROPOSAL_COOLDOWN_MS);
    let _ = pair.a.take_direct_link_jobs();
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));
    assert_eq!(only_probe(&pair.b.take_direct_link_jobs()), session);
}

#[test]
fn proposals_need_a_facilitator_and_respect_the_cooldown() {
    let mut pair = Pair::new();
    assert_eq!(
        pair.a.propose_direct_link(&pair.link).unwrap_err(),
        DirectLinkError::NoFacilitator
    );
    pair.configure(DirectLinkPolicy::AcceptAll);
    pair.propose();
    assert_eq!(
        pair.a.propose_direct_link(&pair.link).unwrap_err(),
        DirectLinkError::AlreadyUpgrading
    );
    // The facilitator never answers: the session times out on its own.
    pair.advance(crate::direct_link::session::DISCOVER_TIMEOUT_MS);
    assert!(pair.a_events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkFailed {
            failure: Failure::ProbeFailed,
            ..
        }
    )));
    assert_eq!(
        pair.a.propose_direct_link(&pair.link).unwrap_err(),
        DirectLinkError::CoolingDown
    );
    assert_eq!(
        pair.a
            .propose_direct_link(&LinkId::new([9; 16]))
            .unwrap_err(),
        DirectLinkError::NoActiveLink
    );
}

#[test]
fn a_peer_that_goes_quiet_times_the_upgrade_out() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::AcceptAll);
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));
    // B accepted but its facilitator probe never comes back; A gives up
    // waiting for READY, B gives up probing.
    pair.advance(crate::direct_link::session::READY_TIMEOUT_MS);
    assert!(pair.a_events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkFailed {
            failure: Failure::Timeout,
            proposed: true,
            ..
        }
    )));
    assert!(pair.b_events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkFailed {
            failure: Failure::ProbeFailed,
            proposed: false,
            ..
        }
    )));
}

#[test]
fn a_failed_punch_ends_both_sessions_quietly() {
    let mut pair = Pair::new();
    let (session, _) = to_punch(&mut pair);
    let out = pair.a.direct_link_punched(&session, None);
    assert!(out.events.iter().any(|e| matches!(
        e,
        NodeEvent::DirectLinkFailed {
            failure: Failure::PunchFailed,
            ..
        }
    )));
    assert_eq!(
        pair.a.take_direct_link_jobs(),
        [DirectLinkJob::Release { session }]
    );
    assert_eq!(pair.a.direct_link_interface(&pair.link), None);
}

#[test]
fn closing_the_link_retires_its_direct_interface() {
    let mut pair = Pair::new();
    let (session, _) = to_punch(&mut pair);
    let a_direct = add_iface(&mut pair.a, "A_direct");
    let _ = pair.a.direct_link_punched(&session, Some(a_direct));
    let _ = pair.a.take_direct_link_jobs();

    let _ = pair.a.close_link(&pair.link);
    assert_eq!(
        pair.a.take_direct_link_jobs(),
        [DirectLinkJob::CloseInterface {
            interface_index: a_direct
        }]
    );
}

#[test]
fn a_punch_that_lands_after_the_link_closed_hands_its_interface_back() {
    let mut pair = Pair::new();
    let (session, _) = to_punch(&mut pair);
    let _ = pair.a.close_link(&pair.link);
    assert_eq!(
        pair.a.take_direct_link_jobs(),
        [DirectLinkJob::Release { session }]
    );
    let a_direct = add_iface(&mut pair.a, "A_direct");
    let _ = pair.a.direct_link_punched(&session, Some(a_direct));
    assert_eq!(
        pair.a.take_direct_link_jobs(),
        [
            DirectLinkJob::CloseInterface {
                interface_index: a_direct
            },
            DirectLinkJob::Release { session }
        ]
    );
}

/// Bring both sides of a punched pair onto their direct interfaces.
fn established(pair: &mut Pair) -> (usize, usize) {
    let (session, _) = to_punch(pair);
    let a_direct = add_iface(&mut pair.a, "A_direct");
    let b_direct = add_iface(&mut pair.b, "B_direct");
    let out = pair.a.direct_link_punched(&session, Some(a_direct));
    pair.a_events.extend(out.events);
    let out = pair.b.direct_link_punched(&session, Some(b_direct));
    pair.b_events.extend(out.events);
    (a_direct, b_direct)
}

#[test]
fn a_full_application_sink_does_not_hold_back_signals() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::AcceptAll);
    // B's host has no room for one more message.
    pair.b.set_channel_delivery_budget(Some(0));
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));
    assert_eq!(
        only_probe(&pair.b.take_direct_link_jobs()),
        session,
        "B took the REQUEST although its application sink is full"
    );
    // An application message is still refused.
    pair.a.transport.clock().advance(1_000);
    let out = pair.a.send_on_link(&pair.link, b"waits").unwrap();
    let (_, ev) = deliver(&mut pair.b, pair.b_mesh, packets(&out));
    assert!(!ev
        .iter()
        .any(|e| matches!(e, NodeEvent::MessageReceived { .. })));
}

#[test]
fn a_direct_link_whose_fallback_died_closes_when_the_direct_path_dies() {
    let mut pair = Pair::new();
    let (a_direct, _) = established(&mut pair);
    // The relayed interface goes first; the link carries on directly.
    let _ = pair.a.handle_interface_down(InterfaceId(pair.a_mesh));
    assert_eq!(pair.a.direct_link_interface(&pair.link), Some(a_direct));
    assert_eq!(pair.a.active_link_count(), 1);
    // Then the direct one: nothing left to fall back to.
    let out = pair.a.handle_interface_down(InterfaceId(a_direct));
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::DirectLinkLost { .. })));
    assert!(out.events.iter().any(|e| matches!(
        e,
        NodeEvent::LinkClosed { link_id, .. } if *link_id == pair.link
    )));
    assert_eq!(
        pair.a.active_link_count(),
        0,
        "not pinned to a dead interface"
    );
}

#[test]
fn moving_onto_a_direct_interface_lowers_a_large_link_mtu_on_both_ends() {
    let mut pair = Pair::new();
    for node in [&mut pair.a, &mut pair.b] {
        let link = node.links.get_mut(&pair.link).unwrap();
        link.set_negotiated_mtu_for_test(16_384);
    }
    established(&mut pair);
    let a = pair.a.links.get(&pair.link).unwrap();
    let b = pair.b.links.get(&pair.link).unwrap();
    assert_eq!(a.negotiated_mtu(), crate::direct_link::DIRECT_LINK_MTU);
    assert_eq!(b.negotiated_mtu(), crate::direct_link::DIRECT_LINK_MTU);
    assert_eq!(a.mdu(), b.mdu(), "both ends agree on the MDU");
}

#[test]
fn a_restarted_driver_abandons_every_direct_link_and_session() {
    let mut pair = Pair::new();
    established(&mut pair);
    let out = pair.a.abandon_direct_links();
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::DirectLinkLost { .. })));
    assert_eq!(pair.a.direct_link_interface(&pair.link), None);
    assert!(pair.a.take_direct_link_jobs().is_empty());
    pair.a.transport.clock().advance(1_000);
    let out = pair.a.send_on_link(&pair.link, b"relayed").unwrap();
    assert!(sent(&out)
        .iter()
        .all(|(iface, _)| *iface == Some(pair.a_mesh)));

    // A session in flight fails instead of waiting on a socket that is gone.
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::AcceptAll);
    pair.propose();
    let _ = pair.a.take_direct_link_jobs();
    let out = pair.a.abandon_direct_links();
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::DirectLinkFailed { proposed: true, .. })));
}

#[test]
fn a_busy_channel_delays_signals_instead_of_failing_the_upgrade() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::AcceptAll);
    pair.propose();
    let session = only_probe(&pair.a.take_direct_link_jobs());
    // B's channel is saturated: messages it sent are unproved (lost on the
    // way), so when the REQUEST arrives the ACCEPT cannot go at once.
    let mut saturated = false;
    for _ in 0..64 {
        if pair.b.send_on_link(&pair.link, b"busy").is_err() {
            saturated = true;
            break;
        }
    }
    assert!(saturated, "B's channel pushes back");
    let out = pair.a.direct_link_probed(&session, Some(a_public()));
    pair.pump_from_a(packets(&out));
    assert_eq!(
        only_probe(&pair.b.take_direct_link_jobs()),
        session,
        "B's session survived the backpressure"
    );
    assert_eq!(
        pair.b.direct_link_outbox_len(),
        1,
        "the ACCEPT is held, not dropped"
    );
    // B's earlier messages are retransmitted and proved, its window opens,
    // and the tick flushes the ACCEPT, all inside A's proposal timeout.
    for _ in 0..18 {
        if pair.b.direct_link_outbox_len() == 0 {
            break;
        }
        pair.advance(500);
    }
    assert_eq!(pair.b.direct_link_outbox_len(), 0, "flushed on a tick");
    let out = pair.b.direct_link_probed(&session, Some(b_public()));
    pair.pump_from_b(packets(&out));
    pair.advance(2_000);
    let _ = only_punch(&pair.b.take_direct_link_jobs());
    let _ = only_punch(&pair.a.take_direct_link_jobs());
    assert!(!pair
        .a_events
        .iter()
        .chain(pair.b_events.iter())
        .any(|e| matches!(e, NodeEvent::DirectLinkFailed { .. })));
}

#[test]
fn a_link_gone_stale_on_its_direct_path_falls_back_and_nudges_the_peer() {
    let mut pair = Pair::new();
    let (a_direct, _) = established(&mut pair);
    let _ = pair.a.take_direct_link_jobs();
    // The direct path dies: nothing A sends arrives, nothing arrives at A.
    let mut out_on_mesh = false;
    let mut stale = false;
    for _ in 0..600 {
        pair.a.transport.clock().advance(1_000);
        let out = pair.a.handle_timeout();
        if out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkStale { .. }))
        {
            stale = true;
            assert!(out
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::DirectLinkLost { .. })));
            out_on_mesh = sent(&out)
                .iter()
                .any(|(iface, _)| *iface == Some(pair.a_mesh));
            break;
        }
    }
    assert!(stale, "the link went stale");
    assert!(out_on_mesh, "a keepalive went out over the relay at once");
    assert_eq!(pair.a.direct_link_interface(&pair.link), None);
    assert!(pair
        .a
        .take_direct_link_jobs()
        .contains(&DirectLinkJob::CloseInterface {
            interface_index: a_direct
        }));
    assert_eq!(
        pair.a.link_count(),
        1,
        "stale, waiting on the relay, not closed"
    );
}

#[test]
fn traffic_over_the_relay_after_the_grace_brings_a_direct_end_back() {
    let mut pair = Pair::new();
    let (a_direct, b_direct) = established(&mut pair);
    let _ = pair.a.take_direct_link_jobs();
    // B loses its direct path and goes back to the relay.
    let _ = pair.b.handle_interface_down(InterfaceId(b_direct));
    pair.b.transport.clock().advance(1_000);
    pair.a.transport.clock().advance(1_000);

    // Inside the grace a relayed packet is taken as a straggler.
    let out = pair.b.send_on_link(&pair.link, b"early").unwrap();
    let (_, _) = deliver(&mut pair.a, pair.a_mesh, packets(&out));
    assert_eq!(pair.a.direct_link_interface(&pair.link), Some(a_direct));

    // Past it, relayed traffic means the peer is back on the relay.
    pair.b
        .transport
        .clock()
        .advance(crate::node::FALLBACK_GRACE_MS);
    pair.a
        .transport
        .clock()
        .advance(crate::node::FALLBACK_GRACE_MS);
    let out = pair.b.send_on_link(&pair.link, b"late").unwrap();
    let mut a_out = Vec::new();
    let mut a_ev = Vec::new();
    for pkt in packets(&out) {
        let tick = pair.a.handle_packet(InterfaceId(pair.a_mesh), &pkt);
        a_out.extend(sent(&tick));
        a_ev.extend(tick.events);
    }
    assert!(a_ev
        .iter()
        .any(|e| matches!(e, NodeEvent::DirectLinkLost { .. })));
    assert!(a_ev.iter().any(|e| matches!(
        e,
        NodeEvent::MessageReceived { data, .. } if data == b"late"
    )));
    assert_eq!(pair.a.direct_link_interface(&pair.link), None);
    // The packet is authenticated before A decides, so its own proof may
    // still have left on the direct path; the nudge goes over the relay.
    assert!(
        a_out
            .iter()
            .filter(|(iface, _)| *iface == Some(pair.a_mesh))
            .count()
            >= 1,
        "A answers over the relay now: {a_out:?}"
    );
    assert!(pair
        .a
        .take_direct_link_jobs()
        .contains(&DirectLinkJob::CloseInterface {
            interface_index: a_direct
        }));
}

#[test]
fn the_stale_ends_nudge_alone_brings_the_peer_back_and_a_forged_keepalive_does_not() {
    let mut pair = Pair::new();
    let (a_direct, _) = established(&mut pair);
    let _ = pair.a.take_direct_link_jobs();
    let _ = pair.b.take_direct_link_jobs();

    // A forged keepalive for the link, injected on B's relay past the grace:
    // keepalives are not encrypted, so it proves nothing about the peer.
    pair.b
        .transport
        .clock()
        .advance(crate::node::FALLBACK_GRACE_MS + 1_000);
    let forged = pair
        .a
        .links
        .get(&pair.link)
        .unwrap()
        .build_keepalive_packet()
        .unwrap();
    let _ = pair.b.handle_packet(InterfaceId(pair.b_mesh), &forged);
    assert!(
        pair.b.direct_link_interface(&pair.link).is_some(),
        "an unauthenticated packet must not pull B off its direct path"
    );
    // Nor does a channel packet that fails to decrypt: a real one from A,
    // tampered with on the way.
    pair.a.transport.clock().advance(1_000);
    let mut tampered = packets(&pair.a.send_on_link(&pair.link, b"x").unwrap());
    let last = tampered[0].len() - 1;
    tampered[0][last] ^= 0x55;
    let _ = pair.b.handle_packet(InterfaceId(pair.b_mesh), &tampered[0]);
    assert!(
        pair.b.direct_link_interface(&pair.link).is_some(),
        "a packet that does not decrypt must not pull B off its direct path"
    );

    // A's direct path dies; it goes stale, falls back, and nudges over the
    // relay. Nothing A sends reaches B except over the relay.
    let mut nudge = Vec::new();
    let mut fell_back = false;
    for _ in 0..600 {
        pair.a.transport.clock().advance(1_000);
        let out = pair.a.handle_timeout();
        fell_back |= out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::DirectLinkLost { .. }));
        if fell_back {
            nudge.extend(
                sent(&out)
                    .into_iter()
                    .filter(|(iface, _)| *iface == Some(pair.a_mesh))
                    .map(|(_, d)| d),
            );
            // The signal may wait a tick in the outbox behind channel pacing.
            if nudge.len() >= 2 {
                break;
            }
        }
    }
    assert!(nudge.len() >= 2, "a keepalive and an authenticated signal");
    assert_eq!(pair.a.direct_link_interface(&pair.link), None);
    let _ = a_direct;

    // B, still direct, hears the authenticated nudge over the relay and
    // follows.
    pair.b.transport.clock().advance(1_000);
    let (_, ev) = deliver(&mut pair.b, pair.b_mesh, nudge);
    assert!(ev
        .iter()
        .any(|e| matches!(e, NodeEvent::DirectLinkLost { .. })));
    assert_eq!(pair.b.direct_link_interface(&pair.link), None);
}

#[test]
fn a_session_id_in_use_on_another_link_is_refused() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::AcceptAll);
    // A second link between the same two nodes.
    let (second, _, out) = pair
        .a
        .connect(pair.dest, &pair.signing_key)
        .expect("connect");
    pair.pump_from_a(packets(&out));
    pair.a.transport.clock().advance(1_000);
    pair.b.transport.clock().advance(1_000);

    let request = |session| crate::direct_link::Signal::Request {
        session,
        facilitator: facilitator(),
        initiator_public: a_public(),
        protocol: ProbeProtocol::Rnsp,
    };
    let now = crate::traits::Clock::now_ms(pair.a.transport.clock());
    assert!(pair
        .a
        .send_direct_link_signal(&pair.link, &request([7; 16]), now));
    assert!(pair
        .a
        .send_direct_link_signal(&second, &request([7; 16]), now));
    let out = pair.a.handle_timeout();
    pair.pump_from_a(packets(&out));
    let probes = pair
        .b
        .take_direct_link_jobs()
        .into_iter()
        .filter(|j| matches!(j, DirectLinkJob::Probe { .. }))
        .count();
    assert_eq!(probes, 1, "the reused id must not start a second session");
}

#[test]
fn an_expired_sessions_held_request_is_never_sent() {
    let mut pair = Pair::new();
    pair.configure(DirectLinkPolicy::AcceptAll);
    // A's channel is saturated; its REQUEST will have to wait.
    let mut saturated = false;
    for _ in 0..64 {
        if pair.a.send_on_link(&pair.link, b"busy").is_err() {
            saturated = true;
            break;
        }
    }
    assert!(saturated);
    let _ = pair
        .a
        .propose_direct_link(&pair.link)
        .expect("proposal starts");
    let session = only_probe(&pair.a.take_direct_link_jobs());
    let _ = pair.a.direct_link_probed(&session, Some(a_public()));
    assert_eq!(pair.a.direct_link_outbox_len(), 1, "the REQUEST is held");
    // Nothing gets through; the proposal times out.
    for _ in 0..12 {
        pair.a.transport.clock().advance(1_000);
        let _ = pair.a.handle_timeout();
    }
    assert_eq!(
        pair.a.direct_link_outbox_len(),
        0,
        "the dead session's REQUEST is dropped, not sent later"
    );
}

#[test]
fn a_link_with_no_fallback_that_goes_stale_retires_its_direct_interface() {
    let mut pair = Pair::new();
    let (a_direct, _) = established(&mut pair);
    let _ = pair.a.take_direct_link_jobs();
    let _ = pair.a.handle_interface_down(InterfaceId(pair.a_mesh));
    let mut closed = false;
    for _ in 0..600 {
        pair.a.transport.clock().advance(1_000);
        let out = pair.a.handle_timeout();
        if out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkClosed { .. }))
        {
            closed = true;
            break;
        }
    }
    assert!(closed, "no route left: the link closes");
    assert!(pair
        .a
        .take_direct_link_jobs()
        .contains(&DirectLinkJob::CloseInterface {
            interface_index: a_direct
        }));
}

#[test]
fn a_link_that_went_stale_during_the_punch_is_live_again_when_it_lands() {
    // The relayed link went quiet while the punch ran (the punch window is
    // ten seconds; a fast link's stale time can be shorter).
    let mut pair = Pair::new();
    let (session, _) = to_punch(&mut pair);
    pair.a
        .links
        .get_mut(&pair.link)
        .unwrap()
        .set_state(crate::link::LinkState::Stale);
    let a_direct = add_iface(&mut pair.a, "A_direct");
    let out = pair.a.direct_link_punched(&session, Some(a_direct));
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkRecovered { .. })));
    assert_eq!(
        pair.a.links.get(&pair.link).unwrap().state(),
        crate::link::LinkState::Active
    );
}
