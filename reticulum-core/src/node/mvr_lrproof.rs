//! mvr: deterministic reproduction of the LRPROOF hop-count-mismatch drop.
//!
//! Field symptom (hardware forensics 2026-06-14, `lora_link_rust`,
//! `rnode_pair`): the link-request proof reaches the relay node but is
//! dropped at `transport.rs` "hop count mismatch (remaining_hops)", so the
//! link never establishes ("Link died (Timeout)"). Identical commit, sometimes
//! green, sometimes red. The relay's node logs showed:
//!
//! ```text
//! 0-alpha: LRPROOF arrived dest=bcd0ff85 iface=rnode_0 hops=2
//! 0-alpha: LRPROOF link_table hit entry(next_hop=rnode_0 remaining=1 recv=tcp_server hops=2)
//! 0-alpha: Dropped LRPROOF, hop count mismatch (remaining_hops) packet_hops=2 remaining_hops=1
//! ```
//!
//! Mechanism this mvr nails down (sans-I/O, no LoRa/Docker/Python, <5 s):
//! A relay freezes `remaining_hops = path.hops` when it forwards a link
//! request (`transport.rs`, the `LINK_ENTRY_SET` instrumentation). If the
//! returning LRPROOF arrives with `packet.hops != remaining_hops` it is
//! dropped from the destination side and never reaches the initiator.
//!
//! For the proof to arrive with MORE hops than the relay's frozen
//! `remaining_hops`, the proof must return over a LONGER path than the relay's
//! stored route: the relay's path table still holds an optimistic 1-hop entry
//! for the responder, while the live request/proof round trip actually
//! traverses a second relay (2 hops). This is the "2-hop path beside the
//! 1-hop path" from Hypothesis 1. The harness controls per-packet delivery so
//! the asymmetry is deterministic instead of race-dependent.
//!
//! Three tests:
//!   * `lrproof_hop_mismatch_drops_proof_characterization` — GREEN now:
//!     deterministically triggers the drop and proves the exact cause from the
//!     structured logs (the reproduction artifact; stays green so the default
//!     suite is unaffected).
//!   * `lrproof_link_should_establish_despite_hop_asymmetry` — `#[ignore]`d,
//!     RED on master: asserts the desired post-fix behaviour (link establishes)
//!     and fails with the captured "Dropped LRPROOF, hop count mismatch" as the
//!     proven cause. Un-ignore in the same commit that lands the fix.
//!   * `lrproof_symmetric_single_hop_relay_establishes` — GREEN control:
//!     identical relay code, symmetric 1-hop path, link establishes. Proves
//!     the drop is the hop asymmetry, not the relay path itself, and that a
//!     clean single-relay topology does NOT trip the check.
//!
//! Note on node count: the acceptance sketch suggested 1-2 nodes, but a proof
//! cannot arrive with `hops=2` against a frozen `remaining_hops=1` unless a
//! SECOND forwarder actually re-transmits it (the field "hops=2"). That second
//! relay (`G`) is the uncovered re-transmit path; two nodes alone cannot
//! produce the asymmetry (see the report and the green control).

extern crate std;

use std::string::String;
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

// ----------------------------------------------------------------------------
// Tracing capture: prove the EXACT drop reason rather than only "no link".
// ----------------------------------------------------------------------------

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `body` with all `reticulum_core` tracing captured into the returned
/// string. Scoped to the current thread so parallel tests don't interfere.
fn with_captured_logs<R>(body: impl FnOnce() -> R) -> (R, String) {
    use tracing_subscriber::util::SubscriberInitExt;

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(buf.clone()))
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_target(true)
        .finish();
    // Thread-local default for the duration of `body` (the `tracing` crate's
    // own `with_default` is gated behind its `std` feature, which this crate
    // does not enable; `tracing-subscriber` provides the guard with std on).
    let guard = subscriber.set_default();
    let out = body();
    drop(guard);
    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    (out, logs)
}

// ----------------------------------------------------------------------------
// Sans-I/O node helpers
// ----------------------------------------------------------------------------

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;
type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Register a named interface on a NodeCore and return its index.
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

/// All bytes the node wants to put on the wire this step (SendPacket + Broadcast).
fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

/// Single outbound packet; panics if not exactly one.
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

fn link_request_link_id(output: &TickOutput) -> LinkId {
    output
        .events
        .iter()
        .find_map(|e| match e {
            NodeEvent::LinkRequest { link_id, .. } => Some(*link_id),
            _ => None,
        })
        .expect("expected LinkRequest event")
}

fn has_link_established(output: &TickOutput) -> bool {
    output
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }))
}

/// Build a responder node owning a link-accepting destination.
/// Returns (node, dest_hash, signing_key, announce_raw).
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
        &["lrproof"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    // Pack a direct (hops=0, no transport_id) announce BEFORE moving dest into
    // the node, so relays that receive it install a 1-hop DIRECT path.
    let announce_packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();
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

// ----------------------------------------------------------------------------
// The reproduction scenario (shared by the characterization and the red test)
// ----------------------------------------------------------------------------

/// Outcome of one run of the asymmetric-return-path scenario.
struct ScenarioOutcome {
    /// Number of packets `A` dropped when the LRPROOF arrived.
    a_drop_delta: u64,
    /// Did `A` forward anything onward when the proof arrived?
    a_forwarded_proof: bool,
    /// Did the initiator establish the link?
    initiator_established: bool,
    /// Initiator's active link count after the round trip.
    initiator_active_links: usize,
    /// Captured structured tracing for the whole run.
    logs: String,
}

/// Topology (sans-I/O; per-packet delivery is scripted, modelling a mesh RF
/// medium where the relay `A` holds an optimistic 1-hop path to the responder
/// `R` that is no longer directly deliverable, so the live route is
/// `A -> G -> R`):
///
/// ```text
///   I --local--> A(relay, 1-hop path to R) ...rf... G(relay) ...rf... R(responder)
///                 ^                                                      |
///                 |            proof returns A <- G <- R (2 hops)        |
///                 +------------------------------------------------------+
/// ```
///
/// `A` freezes `remaining_hops = 1` when forwarding the request (its stored
/// path says 1 hop). The proof returns through `G`, arriving at `A` with
/// `hops = 2`. The destination-side check `packet.hops != remaining_hops`
/// (2 != 1) drops it, so the initiator never sees the proof.
fn run_asymmetric_return_path_scenario() -> ScenarioOutcome {
    let mut a_drop_delta = 0;
    let mut a_forwarded_proof = false;
    let mut initiator_established = false;
    let mut initiator_active_links = 0;

    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key, announce_raw) = make_responder();
        let mut relay_a = make_transport_node();
        let mut relay_g = make_transport_node();
        let mut initiator = make_initiator();

        // Interfaces.
        let a_local = add_iface(&mut relay_a, "A_local_initiator", true); // I -> A
        let a_mesh = add_iface(&mut relay_a, "A_mesh", false); // A <-> (G/R) RF
        let g_from_a = add_iface(&mut relay_g, "G_from_A", false);
        let g_to_r = add_iface(&mut relay_g, "G_to_R", false);
        let r_iface = add_iface(&mut responder, "R_mesh", false);
        let i_iface = add_iface(&mut initiator, "I_to_A", false);

        // Path install: A and G both learn R at 1 hop DIRECT on their RF iface.
        let _ = relay_a.handle_packet(InterfaceId(a_mesh), &announce_raw);
        let _ = relay_g.handle_packet(InterfaceId(g_to_r), &announce_raw);
        assert!(relay_a.has_path(&dest_hash), "A must hold a path to R");
        assert!(relay_g.has_path(&dest_hash), "G must hold a path to R");
        assert_eq!(
            relay_a.hops_to(&dest_hash),
            Some(1),
            "A's stored path to R must be 1 hop (the value frozen into remaining_hops)"
        );

        // 1. Initiator connects -> broadcasts the link request.
        let (_init_link, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);

        // 2. A receives the request from its local client and forwards it.
        //    LINK_ENTRY_SET fires here: remaining_hops frozen to path.hops = 1.
        let out = relay_a.handle_packet(InterfaceId(a_local), &request);
        let a_forwarded = one_packet(&out);

        // 3. Live topology: R is not directly reachable; A's forward is picked
        //    up by relay G on the shared RF medium (the re-transmitter).
        let out = relay_g.handle_packet(InterfaceId(g_from_a), &a_forwarded);
        let g_forwarded = one_packet(&out);

        // 4. R receives the (now 2-hops-taken) request, accepts, sends proof.
        let out = responder.handle_packet(InterfaceId(r_iface), &g_forwarded);
        let resp_link = link_request_link_id(&out);
        let out = responder.accept_link(&resp_link).unwrap();
        let proof = one_packet(&out);

        // 5. Proof returns through G (hop match at G: 1 == 1) -> forwarded.
        let out = relay_g.handle_packet(InterfaceId(g_to_r), &proof);
        let g_proof = one_packet(&out);

        // 6. Proof reaches A with hops=2 while remaining_hops=1.
        let dropped_before = relay_a.transport().stats().packets_dropped;
        let out = relay_a.handle_packet(InterfaceId(a_mesh), &g_proof);
        a_drop_delta = relay_a.transport().stats().packets_dropped - dropped_before;
        a_forwarded_proof = !action_data(&out).is_empty();

        // 7. Deliver whatever (if anything) A forwarded to the initiator.
        for pkt in action_data(&out) {
            let iout = initiator.handle_packet(InterfaceId(i_iface), &pkt);
            if has_link_established(&iout) {
                initiator_established = true;
            }
        }
        initiator_active_links = initiator.active_link_count();
    });

    ScenarioOutcome {
        a_drop_delta,
        a_forwarded_proof,
        initiator_established,
        initiator_active_links,
        logs,
    }
}

// ----------------------------------------------------------------------------
// GREEN characterization: the reproduction artifact (stays green).
// ----------------------------------------------------------------------------

/// Deterministically reproduce the field drop and prove the exact cause.
/// This characterizes current (buggy) behaviour and stays green, so the
/// default suite is unaffected while the reproduction is fully recorded.
#[test]
fn lrproof_hop_mismatch_drops_proof_characterization() {
    let o = run_asymmetric_return_path_scenario();

    assert_eq!(
        o.a_drop_delta, 1,
        "A must drop exactly the returning LRPROOF.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.a_forwarded_proof,
        "A must NOT forward the dropped proof onward.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.initiator_established,
        "link must NOT establish (proof never reached initiator).\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(o.initiator_active_links, 0, "initiator has zero links");

    // Prove the EXACT cause from the structured logs.
    assert!(
        o.logs
            .contains("froze remaining_hops=path_hops for forwarded link request"),
        "LINK_ENTRY_SET instrumentation must show the freeze.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.logs
            .contains("Dropped LRPROOF, hop count mismatch (remaining_hops)"),
        "the drop must be the remaining_hops mismatch.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.logs.contains("packet_hops=2") && o.logs.contains("remaining_hops=1"),
        "proof must arrive at hops=2 against the frozen remaining_hops=1.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// RED regression guard: desired post-fix behaviour (currently fails).
// ----------------------------------------------------------------------------

/// The link SHOULD establish despite the hop asymmetry. Red on master: A drops
/// the proof at the "hop count mismatch (remaining_hops)" check, so the
/// initiator never establishes. Un-ignore in the same commit that lands the fix
/// so this becomes a regression guard.
#[test]
#[ignore = "LRPROOF hop-mismatch reproduction — red on master until the fix lands"]
fn lrproof_link_should_establish_despite_hop_asymmetry() {
    let o = run_asymmetric_return_path_scenario();
    assert!(
        o.initiator_established,
        "link did NOT establish: A dropped the returning LRPROOF at the \
         hop-count-mismatch check (proof hops=2 vs frozen remaining_hops=1). \
         a_drop_delta={}, a_forwarded_proof={}.\n--- logs ---\n{}",
        o.a_drop_delta, o.a_forwarded_proof, o.logs
    );
}

// ----------------------------------------------------------------------------
// GREEN control: symmetric single-hop relay establishes (no drop).
// ----------------------------------------------------------------------------

/// Same relay code, but the request and the proof traverse the SAME single hop
/// `A -> R` and `R -> A`. `remaining_hops = 1` matches the returning proof's
/// `hops = 1`, the proof is forwarded, and the link establishes. This proves
/// the drop is the hop ASYMMETRY, not the relay forwarding path, and that a
/// clean topology does not trip the check.
#[test]
fn lrproof_symmetric_single_hop_relay_establishes() {
    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key, announce_raw) = make_responder();
        let mut relay_a = make_transport_node();
        let mut initiator = make_initiator();

        let a_local = add_iface(&mut relay_a, "A_local_initiator", true);
        let a_mesh = add_iface(&mut relay_a, "A_mesh", false);
        let r_iface = add_iface(&mut responder, "R_mesh", false);
        let _i_iface = add_iface(&mut initiator, "I_to_A", false);

        let _ = relay_a.handle_packet(InterfaceId(a_mesh), &announce_raw);
        assert_eq!(relay_a.hops_to(&dest_hash), Some(1));

        // 1. connect -> request.
        let (_init_link, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);

        // 2. A forwards (remaining_hops frozen to 1).
        let out = relay_a.handle_packet(InterfaceId(a_local), &request);
        let a_forwarded = one_packet(&out);

        // 3. Delivered DIRECTLY to R (single hop, no second relay).
        let out = responder.handle_packet(InterfaceId(r_iface), &a_forwarded);
        let resp_link = link_request_link_id(&out);
        let out = responder.accept_link(&resp_link).unwrap();
        let proof = one_packet(&out);

        // 4. Proof returns A <- R single hop: hops=1 == remaining_hops=1 -> forward.
        let out = relay_a.handle_packet(InterfaceId(a_mesh), &proof);
        let to_initiator = one_packet(&out);

        // 5. Initiator establishes the link.
        let out = initiator.handle_packet(InterfaceId(_i_iface), &to_initiator);
        assert!(
            has_link_established(&out),
            "symmetric single-hop relay must establish the link"
        );
        assert_eq!(initiator.active_link_count(), 1);
    });

    assert!(
        logs.contains("froze remaining_hops=path_hops for forwarded link request"),
        "LINK_ENTRY_SET must still fire in the green path.\n--- logs ---\n{logs}"
    );
    assert!(
        !logs.contains("Dropped LRPROOF, hop count mismatch"),
        "the symmetric path must NOT drop the proof.\n--- logs ---\n{logs}"
    );
}
