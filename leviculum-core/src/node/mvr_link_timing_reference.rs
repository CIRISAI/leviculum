//! mvr: the link timing class of the 2026-10-12 audit (#664), D4, D5 and D8,
//! each pinned against the reference's arithmetic.
//!
//! - D4: the responder believes `max(measured, wire)` for its RTT, where the
//!   measurement runs from the proof send to the LRRTT's arrival
//!   (`request_time` set beside `prove()`, `Link.py:215`; the max at
//!   `rtt_packet`, `Link.py:540`).
//! - D5: the keepalive, stale and stale-close windows keep the reference's
//!   fractional seconds (`__update_keepalive`, `Link.py:844-846`; close at
//!   `rtt * KEEPALIVE_TIMEOUT_FACTOR + STALE_GRACE` past stale,
//!   `Link.py:797`), to the millisecond.
//! - D8: an initiator never counts its own 0xFF as inbound, not even for stale
//!   recovery (`Link.py:974`), and a keepalive that fails validation does not
//!   feed the watchdog.
//!
//! Sans-I/O: initiator <-> responder over one hop, each with its own
//! `MockClock`, so the gap each side measures is set by hand.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::{LinkId, LinkState};
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::PacketContext;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Byte offset of the context field in a link packet:
/// `[flags(1)][hops(1)][link_id(16)][context(1)][payload]`.
const CONTEXT_OFFSET: usize = 18;

fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
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

fn deliver_all(target: &mut EndpointNode, iface: usize, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for pkt in packets {
        out.extend(action_data(&target.handle_packet(InterfaceId(iface), &pkt)));
    }
    out
}

fn with_context(packets: &[Vec<u8>], context: PacketContext) -> Vec<Vec<u8>> {
    packets
        .iter()
        .filter(|p| {
            p.len() > CONTEXT_OFFSET && PacketContext::from_byte(p[CONTEXT_OFFSET]) == context
        })
        .cloned()
        .collect()
}

struct Pair {
    initiator: EndpointNode,
    responder: EndpointNode,
    i_iface: usize,
    r_iface: usize,
    link_id: LinkId,
}

/// Establish a link with the proof reaching the initiator `initiator_gap_ms`
/// after the request and the LRRTT reaching the responder `responder_gap_ms`
/// after its proof send. Both clocks start at `TEST_TIME_MS`.
fn establish(initiator_gap_ms: u64, responder_gap_ms: u64) -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["linktiming"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);
    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let proof = deliver_all(&mut responder, r_iface, action_data(&out));
    assert!(
        !proof.is_empty(),
        "precondition: the responder proves the request"
    );

    initiator
        .transport()
        .clock()
        .set(TEST_TIME_MS + initiator_gap_ms);
    let rtt = deliver_all(&mut initiator, i_iface, proof);
    let rtt = with_context(&rtt, PacketContext::Lrrtt);
    assert_eq!(rtt.len(), 1, "precondition: one LRRTT leaves the initiator");

    responder
        .transport()
        .clock()
        .set(TEST_TIME_MS + responder_gap_ms);
    deliver_all(&mut responder, r_iface, rtt);

    assert_eq!(
        initiator.active_link_count(),
        1,
        "precondition: initiator active"
    );
    assert_eq!(
        responder.active_link_count(),
        1,
        "precondition: responder active"
    );
    Pair {
        initiator,
        responder,
        i_iface,
        r_iface,
        link_id,
    }
}

fn state(node: &EndpointNode, link_id: &LinkId) -> Option<LinkState> {
    node.link(link_id).map(|l| l.state())
}

/// D4: the initiator's wire RTT is the 500 ms floor it puts on a near-zero
/// measurement (`CHANNEL_DEFAULT_RTT_MS`), the responder measured 1500 ms from
/// its proof to the LRRTT. The reference keeps 1.5 s: keepalive
/// 1.5 * 360 / 1.75 = 308.571 s, stale 617.143 s. With the wire value alone
/// the stale window is 205.714 s, and the link would be Stale at 300 s.
#[test]
fn responder_rtt_is_max_of_measured_and_wire() {
    let mut p = establish(0, 1500);
    let responder_link = p.responder.link(&p.link_id).expect("responder link");
    assert_eq!(
        responder_link.rtt_ms(),
        1500,
        "responder must take max(measured 1500 ms, wire 500 ms)"
    );

    let established = TEST_TIME_MS + 1500;
    p.responder.transport().clock().set(established + 300_000);
    let _ = p.responder.handle_timeout();
    assert_eq!(
        state(&p.responder, &p.link_id),
        Some(LinkState::Active),
        "300 s of silence is inside the reference's 617.143 s stale window at rtt 1.5 s"
    );
    p.responder.transport().clock().set(established + 617_143);
    let _ = p.responder.handle_timeout();
    assert_eq!(state(&p.responder, &p.link_id), Some(LinkState::Stale));
}

/// D5: at RTT 500 ms the reference derives keepalive 102.857 s, stale
/// 205.714 s, and closes at stale + 4 * 0.5 + 5 = 212.714 s of silence. The
/// watchdog must flip on those instants to the millisecond, not on whole
/// seconds (204 s and 211 s before this fix).
#[test]
fn derived_windows_keep_the_reference_fractions() {
    let mut p = establish(500, 500);
    let established = TEST_TIME_MS + 500;
    let link = p.initiator.link(&p.link_id).expect("initiator link");
    assert_eq!(link.rtt_ms(), 500);

    // Reference formula, evaluated here so the expectation is not our code.
    let rtt = 0.5_f64;
    let keepalive = (rtt * (360.0 / 1.75)).clamp(5.0, 360.0);
    let stale = keepalive * 2.0;
    let close = stale + rtt * 4.0 + 5.0;
    let stale_ms = (stale * 1000.0) as u64;
    let close_ms = (close * 1000.0) as u64;
    assert_eq!((stale_ms, close_ms), (205_714, 212_714));

    let clock_at = |n: &EndpointNode, off: u64| n.transport().clock().set(established + off);

    clock_at(&p.initiator, stale_ms);
    let _ = p.initiator.handle_timeout();
    assert_eq!(
        state(&p.initiator, &p.link_id),
        Some(LinkState::Active),
        "at exactly {stale_ms} ms of silence the link is not yet stale"
    );
    clock_at(&p.initiator, stale_ms + 1);
    let _ = p.initiator.handle_timeout();
    assert_eq!(state(&p.initiator, &p.link_id), Some(LinkState::Stale));

    clock_at(&p.initiator, close_ms);
    let _ = p.initiator.handle_timeout();
    assert_eq!(
        state(&p.initiator, &p.link_id),
        Some(LinkState::Stale),
        "at exactly {close_ms} ms of silence the stale link is not yet closed"
    );
    clock_at(&p.initiator, close_ms + 1);
    let _ = p.initiator.handle_timeout();
    assert!(
        !matches!(
            state(&p.initiator, &p.link_id),
            Some(LinkState::Active | LinkState::Stale)
        ),
        "past {close_ms} ms the stale link is closed"
    );
}

/// The initiator's next keepalive, taken off the wire at `at_ms` past the
/// handshake. With the mock handshake's near-zero RTT the keepalive interval
/// is the 5 s floor.
fn initiator_keepalive(p: &mut Pair, at_ms: u64) -> Vec<u8> {
    p.initiator.transport().clock().set(TEST_TIME_MS + at_ms);
    let tick = p.initiator.handle_timeout();
    let ka = with_context(&action_data(&tick), PacketContext::Keepalive);
    assert_eq!(ka.len(), 1, "precondition: the initiator sends a keepalive");
    ka.into_iter().next().expect("one keepalive")
}

/// D8: a reflected 0xFF (a medium that echoes frames back) is the
/// initiator's own keepalive; it must neither refresh the inbound clock nor
/// recover a stale link. Positive control: the responder's 0xFE does both.
#[test]
fn initiator_ignores_its_own_keepalive() {
    let mut p = establish(0, 0);
    let own = initiator_keepalive(&mut p, 5_000);
    assert_eq!(own.last().copied(), Some(0xFF));

    p.initiator.transport().clock().set(TEST_TIME_MS + 6_000);
    deliver_all(&mut p.initiator, p.i_iface, std::vec![own.clone()]);
    let age = p
        .initiator
        .link(&p.link_id)
        .and_then(|l| l.inbound_age_ms(TEST_TIME_MS + 6_000));
    assert_eq!(age, Some(6_000), "own 0xFF must not count as inbound");

    p.initiator.transport().clock().set(TEST_TIME_MS + 11_000);
    let _ = p.initiator.handle_timeout();
    assert_eq!(state(&p.initiator, &p.link_id), Some(LinkState::Stale));
    deliver_all(&mut p.initiator, p.i_iface, std::vec![own.clone()]);
    assert_eq!(
        state(&p.initiator, &p.link_id),
        Some(LinkState::Stale),
        "own 0xFF must not recover a stale link"
    );

    // Positive control: the responder's echo is real inbound.
    let echo = deliver_all(&mut p.responder, p.r_iface, std::vec![own]);
    let echo = with_context(&echo, PacketContext::Keepalive);
    assert_eq!(echo.len(), 1, "precondition: the responder echoes");
    deliver_all(&mut p.initiator, p.i_iface, echo);
    assert_eq!(state(&p.initiator, &p.link_id), Some(LinkState::Active));
    let age = p
        .initiator
        .link(&p.link_id)
        .and_then(|l| l.inbound_age_ms(TEST_TIME_MS + 11_000));
    assert_eq!(age, Some(0), "the 0xFE echo refreshes the inbound clock");
}

/// D8: a keepalive whose byte fails validation is not inbound activity. The
/// responder expects 0xFF; a 0x00 must leave its clock where it was.
/// Positive control: the genuine 0xFF refreshes it.
#[test]
fn invalid_keepalive_byte_does_not_refresh_the_clock() {
    let mut p = establish(0, 0);
    let ka = initiator_keepalive(&mut p, 5_000);
    let mut bad = ka.clone();
    if let Some(b) = bad.last_mut() {
        *b = 0x00;
    }

    p.responder.transport().clock().set(TEST_TIME_MS + 6_000);
    deliver_all(&mut p.responder, p.r_iface, std::vec![bad]);
    let age = p
        .responder
        .link(&p.link_id)
        .and_then(|l| l.inbound_age_ms(TEST_TIME_MS + 6_000));
    assert_eq!(
        age,
        Some(6_000),
        "a 0x00 keepalive must not count as inbound"
    );

    deliver_all(&mut p.responder, p.r_iface, std::vec![ka]);
    let age = p
        .responder
        .link(&p.link_id)
        .and_then(|l| l.inbound_age_ms(TEST_TIME_MS + 6_000));
    assert_eq!(age, Some(0), "the genuine 0xFF refreshes the inbound clock");
}
