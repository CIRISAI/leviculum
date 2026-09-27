//! mvr: #358 — a link request that waits behind the duty budget is dead on
//! arrival, and the returning proof dies without a counter.
//!
//! Field measurement 2026-09-27 (feld-pocket, 16:18–16:35 CEST): the Pocket's
//! LoRa interface sat at its lawful 10 % long-term airtime cap
//! (`[LORA_AIRTIME_LOCK] limits lt=1000`, `lt=360205 holding`) and held each
//! queued frame until the ledger dipped below the cap. Three link requests
//! from the phone (BLE side) reached the air 145.6 s / 137.7 s / 144.0 s
//! after they were relayed. The relay's link-table entry, set at forward time
//! with `proof_timeout_ms = (packet.hops + path_hops + 2) * 6 s`
//! (`transport.rs`, the `LINK_ENTRY_SET` site — 30 s in the field topology),
//! expired long before the proof returned. The reverse table cannot catch an
//! LRPROOF by construction: its key is the request's full packet hash, while
//! the link id strips the request's signalling bytes
//! (`link/mod.rs::calculate_link_id`), so the lookup misses for every modern
//! link request. The proof then fell through to the node layer
//! (`link_management.rs::handle_link_proof`), matched no local link, and was
//! dropped with a debug trace only — no counter moved. 75 proofs received
//! over LoRa that day, 2 routed onward.
//!
//! Host model (sans-I/O, 3 NodeCores, deterministic, < 5 s): the duty hold
//! is, at core level, nothing but TIME between the relay forwarding the
//! request (entry set) and the proof coming back. The queue itself stays in
//! the interface (interface isolation), so the mvr advances the relay's
//! clock by the field median hold and runs maintenance — exactly what a real
//! hold does to the entry.
//!
//! Two tests:
//!   * `duty_held_request_late_lrproof_is_counted` — the reproduction. RED
//!     before the `lrproof-no-link` counter existed: the proof vanished with
//!     `packets_dropped` unchanged. GREEN with the counter.
//!   * `prompt_lrproof_establishes_control` — same topology without the
//!     hold: the proof is forwarded, the link establishes, the new counter
//!     stays at zero.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::ESTABLISHMENT_TIMEOUT_PER_HOP_MS;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage, Storage};
use crate::transport::{Action, DropReason, InterfaceId, TickOutput};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;
type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The field median of the three measured holds (145.6 / 137.7 / 144.0 s).
const HOLD_MS: u64 = 144_000;

fn add_iface<C, S>(
    node: &mut NodeCore<OsRng, C, S>,
    name: &'static str,
    local_client: bool,
) -> usize
where
    C: crate::traits::Clock,
    S: crate::traits::Storage,
{
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    if local_client {
        node.set_interface_local_client(idx, true);
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

/// Responder owning a link-accepting destination, plus its direct announce.
fn make_responder() -> (EndpointNode, crate::DestinationHash, [u8; 32], Vec<u8>) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["dutyhold"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    let announce_packet = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; crate::constants::MTU];
    let len = announce_packet.pack(&mut buf).unwrap();
    let announce_raw = buf[..len].to_vec();

    node.register_destination(dest);
    (node, dest_hash, signing_key, announce_raw)
}

fn make_transport_node() -> TransportNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    )
}

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, NoStorage)
}

/// The reproduction: relay forwards the request, the frame "airs" only after
/// `HOLD_MS`, the proof returns to an expired entry.
#[test]
fn duty_held_request_late_lrproof_is_counted() {
    let (mut responder, dest_hash, signing_key, announce_raw) = make_responder();
    let mut relay = make_transport_node();
    let mut initiator = make_initiator();

    let a_local = add_iface(&mut relay, "A_local_initiator", true); // BLE side
    let a_mesh = add_iface(&mut relay, "A_mesh", false); // LoRa side
    let r_iface = add_iface(&mut responder, "R_mesh", false);
    let _i_iface = add_iface(&mut initiator, "I_to_A", false);

    let _ = relay.handle_packet(InterfaceId(a_mesh), &announce_raw);
    assert_eq!(relay.hops_to(&dest_hash), Some(1));

    // Initiator sends the link request; the relay forwards it and freezes a
    // link-table entry with its proof deadline.
    let (init_link, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let request = one_packet(&out);
    let out = relay.handle_packet(InterfaceId(a_local), &request);
    let relayed_request = one_packet(&out);

    let entry_now = relay.transport().clock().now_ms();
    let entry = relay
        .transport()
        .storage()
        .get_link_entry(init_link.as_bytes())
        .expect("relay must hold a link-table entry after forwarding the request");
    let proof_deadline_ms = entry.proof_timeout_ms.saturating_sub(entry_now);

    // The premise of the field failure, checked rather than assumed: the hold
    // outlives both the relay's proof deadline and the initiator's own
    // per-hop establishment patience (the phone had given up at +16 s).
    assert!(
        HOLD_MS > proof_deadline_ms,
        "hold ({HOLD_MS} ms) must outlive the relay's proof deadline ({proof_deadline_ms} ms)"
    );
    const {
        assert!(
            HOLD_MS > ESTABLISHMENT_TIMEOUT_PER_HOP_MS * 2,
            "hold must outlive the initiator's establishment timeout"
        )
    };

    // The duty hold: the relayed frame sits in the interface queue while the
    // airtime lock is engaged. Sans-I/O that is pure elapsed time at the
    // relay; maintenance reaps the unvalidated entry at its proof deadline.
    let now = relay.transport().clock().now_ms();
    relay.transport().clock().set(now + HOLD_MS);
    let _ = relay.handle_timeout();
    assert!(
        relay
            .transport()
            .storage()
            .get_link_entry(init_link.as_bytes())
            .is_none(),
        "the relay's link-table entry must be gone before the frame even airs"
    );

    // The frame finally airs; the responder accepts and proofs at once
    // (field: base proved within 340 ms of the 145 s-old request arriving).
    let out = responder.handle_packet(InterfaceId(r_iface), &relayed_request);
    let proof = one_packet(&out);

    // The proof returns to the relay. No entry, no reverse hit (the link id
    // is not the request's packet hash), no local link: before the fix it
    // vanished here without touching any counter.
    let dropped_before = relay.transport().stats().packets_dropped();
    let no_link_before = relay.transport().stats().drops_lrproof_no_link();
    let out = relay.handle_packet(InterfaceId(a_mesh), &proof);

    assert!(
        action_data(&out).is_empty(),
        "the relay must not forward a proof it has no entry for"
    );
    assert_eq!(
        relay.transport().stats().drops_lrproof_no_link(),
        no_link_before + 1,
        "the orphaned LRPROOF must be counted under its own named reason \
         (field 2026-09-27: 75 proofs received over LoRa, 2 routed, zero counted)"
    );
    assert_eq!(
        relay.transport().stats().packets_dropped(),
        dropped_before + 1,
        "the named drop must also reach the grand total (record_drop invariant)"
    );
    assert_eq!(
        DropReason::LrproofNoLink.kebab(),
        "lrproof-no-link",
        "the reason must have a stable name for the PKT_DROP event catalogue"
    );
}

/// Control: the same topology with no hold. The proof is forwarded, the link
/// establishes, and the new counter does not move on the healthy path.
#[test]
fn prompt_lrproof_establishes_control() {
    let (mut responder, dest_hash, signing_key, announce_raw) = make_responder();
    let mut relay = make_transport_node();
    let mut initiator = make_initiator();

    let a_local = add_iface(&mut relay, "A_local_initiator", true);
    let a_mesh = add_iface(&mut relay, "A_mesh", false);
    let r_iface = add_iface(&mut responder, "R_mesh", false);
    let i_iface = add_iface(&mut initiator, "I_to_A", false);

    let _ = relay.handle_packet(InterfaceId(a_mesh), &announce_raw);
    assert_eq!(relay.hops_to(&dest_hash), Some(1));

    let (_init_link, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let request = one_packet(&out);
    let out = relay.handle_packet(InterfaceId(a_local), &request);
    let relayed_request = one_packet(&out);

    let out = responder.handle_packet(InterfaceId(r_iface), &relayed_request);
    let proof = one_packet(&out);

    let out = relay.handle_packet(InterfaceId(a_mesh), &proof);
    let to_initiator = one_packet(&out);
    let out = initiator.handle_packet(InterfaceId(i_iface), &to_initiator);
    assert!(
        out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
        "the prompt path must establish the link"
    );
    assert_eq!(
        relay.transport().stats().drops_lrproof_no_link(),
        0,
        "the healthy path must not touch the orphan counter"
    );
}
