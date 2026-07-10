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
//! Three tests (all GREEN after the fix):
//!   * `lrproof_hop_mismatch_relay_forwards_despite_asymmetry` — relay level:
//!     deterministically drives the asymmetric return path and proves A now
//!     FORWARDS the proof (zero drops) and logs the asymmetry as a warning
//!     instead of dropping. Repurposed from the pre-fix characterization.
//!   * `lrproof_link_should_establish_despite_hop_asymmetry` — regression guard:
//!     the initiator establishes the link despite the hop asymmetry. Before the
//!     fix A dropped the proof at "hop count mismatch (remaining_hops)".
//!   * `lrproof_symmetric_single_hop_relay_establishes` — control: symmetric
//!     1-hop path, link establishes. Unaffected by the fix (no drops added).
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
use crate::traits::{NoStorage, Storage};
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

/// Run `body` with all `leviculum_core` tracing captured into the returned
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
    /// `hops` field on the LRPROOF that `A` actually forwarded toward the
    /// initiator (unpacked off the wire). `None` if `A` forwarded nothing.
    forwarded_proof_hops: Option<u8>,
    /// The `remaining_hops` `A` froze into its link-table entry when it forwarded
    /// the request. This is the `link_entry[IDX_LT_REM_HOPS]` value that Python
    /// `Transport.py:2176` compares `packet.hops` against before forwarding.
    frozen_remaining_hops: Option<u8>,
    /// Did the initiator establish the link?
    initiator_established: bool,
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
    let mut forwarded_proof_hops = None;
    let mut frozen_remaining_hops = None;
    let mut initiator_established = false;

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
        let (init_link, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);

        // 2. A receives the request from its local client and forwards it.
        //    LINK_ENTRY_SET fires here: remaining_hops frozen to path.hops = 1.
        let out = relay_a.handle_packet(InterfaceId(a_local), &request);
        let a_forwarded = one_packet(&out);

        // Read the value A froze into its link table. This is the exact
        // `link_entry[IDX_LT_REM_HOPS]` that Python's Transport.py:2176 gates
        // the LRPROOF forward on. It is 1 (A's optimistic stored path to R).
        frozen_remaining_hops = relay_a
            .transport()
            .storage()
            .get_link_entry(init_link.as_bytes())
            .map(|e| e.remaining_hops);

        // 3. Live topology: R is not directly reachable; A's forward is picked
        //    up by relay G on the shared RF medium (the re-transmitter).
        let out = relay_g.handle_packet(InterfaceId(g_from_a), &a_forwarded);
        let g_forwarded = one_packet(&out);

        // 4. R receives the (now 2-hops-taken) request, accepts, sends proof.
        let out = responder.handle_packet(InterfaceId(r_iface), &g_forwarded);
        // Responder auto-accepts (Stage 1): the proof is in the same output.
        let proof = one_packet(&out);

        // 5. Proof returns through G (hop match at G: 1 == 1) -> forwarded.
        let out = relay_g.handle_packet(InterfaceId(g_to_r), &proof);
        let g_proof = one_packet(&out);

        // 6. Proof reaches A with hops=2 while remaining_hops=1.
        let dropped_before = relay_a.transport().stats().packets_dropped;
        let out = relay_a.handle_packet(InterfaceId(a_mesh), &g_proof);
        a_drop_delta = relay_a.transport().stats().packets_dropped - dropped_before;
        let forwarded = action_data(&out);
        a_forwarded_proof = !forwarded.is_empty();
        // Unpack the actual proof A put back on the wire toward the initiator and
        // read its hop count. This is the `packet.hops` a strict Python client
        // (Transport.py:2176) compares against its frozen remaining_hops.
        forwarded_proof_hops = forwarded
            .first()
            .and_then(|raw| crate::packet::Packet::unpack(raw).ok())
            .map(|p| p.hops);

        // 7. Deliver whatever (if anything) A forwarded to the initiator.
        for pkt in action_data(&out) {
            let iout = initiator.handle_packet(InterfaceId(i_iface), &pkt);
            if has_link_established(&iout) {
                initiator_established = true;
            }
        }
    });

    ScenarioOutcome {
        a_drop_delta,
        a_forwarded_proof,
        forwarded_proof_hops,
        frozen_remaining_hops,
        initiator_established,
        logs,
    }
}

// ----------------------------------------------------------------------------
// Relay-level: proof is forwarded despite the hop asymmetry (post-fix).
// ----------------------------------------------------------------------------

/// Post-fix relay behaviour: the asymmetric returning LRPROOF is forwarded, not
/// dropped. Repurposed from the pre-fix characterization (which asserted the
/// drop). Asserts at the RELAY level (A forwards, zero drops, hop asymmetry is
/// logged as a warning), complementing the guard test below which asserts the
/// initiator establishes.
#[test]
fn lrproof_hop_mismatch_relay_forwards_despite_asymmetry() {
    let o = run_asymmetric_return_path_scenario();

    assert_eq!(
        o.a_drop_delta, 0,
        "A must NOT drop the asymmetric LRPROOF.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.a_forwarded_proof,
        "A must forward the proof onward despite the hop asymmetry.\n--- logs ---\n{}",
        o.logs
    );

    // Prove the freeze still happens and the asymmetry is now a warning, not a drop.
    // BUG-4: the freeze is shown by the structured LINK_ENTRY_SET event fields,
    // not the old free-text message (dropped in BUG-1 because its spaces and
    // embedded `=` corrupted the canonical event-log line).
    assert!(
        o.logs.contains("event=\"LINK_ENTRY_SET\""),
        "LINK_ENTRY_SET instrumentation must fire.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.logs.contains("froze remaining_hops=path_hops"),
        "the redundant free-text message must be gone (it corrupted the event-log line).\n\
         --- logs ---\n{}",
        o.logs
    );
    assert!(
        o.logs.contains(
            "LRPROOF hop asymmetry: rewriting forwarded hops to the frozen count (remaining_hops)"
        ),
        "the asymmetry must be logged as a forward, not a drop.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.logs.contains("Dropped LRPROOF, hop count mismatch"),
        "the proof must NOT be dropped for a hop mismatch anymore.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.logs.contains("packet_hops=2") && o.logs.contains("remaining_hops=1"),
        "proof must still arrive at hops=2 against the frozen remaining_hops=1.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// Codeberg #38: Python-compat FIX proof — the forwarded proof's hops now EQUALS
// the downstream's frozen remaining_hops, the exact invariant Python enforces
// before forwarding an LRPROOF (Transport.py:2176). A strict Python client on
// the receiving side therefore ACCEPTS this proof and establishes the link.
// ----------------------------------------------------------------------------

/// Deterministic proof of the #38 fix: our relay rewrites the forwarded LRPROOF
/// hop count so a strict Python client accepts it.
///
/// Python `RNS/Transport.py:2176` forwards an LRPROOF to the next / local-client
/// interface ONLY when `packet.hops == link_entry[IDX_LT_REM_HOPS]` (STRICT
/// equality, guarded by transport_enabled/for_local_client_link/from_local_client).
/// A Python node in the relay position DROPS a proof whose arriving hop count is
/// not exactly the remaining_hops it froze when it forwarded the request.
///
/// Our relay `transport.rs` logs
/// `LRPROOF hop asymmetry: rewriting forwarded hops to the frozen count (remaining_hops)`
/// on the mismatch, and REWRITES the forwarded packet's `hops` down to the frozen
/// `remaining_hops` before it goes on the wire (`forward_on_interface_from`
/// does not re-increment). So the proof leaves our relay carrying hops=1,
/// matching the frozen remaining_hops=1.
///
/// This test asserts the FIXED invariant holds: the forwarded proof's `hops` (1)
/// equals the frozen `remaining_hops` (1). That equality is precisely what a
/// strict Python peer requires to accept the proof and establish the link.
#[test]
fn lrproof_forwarded_proof_hops_match_downstream_remaining_python_would_accept() {
    let o = run_asymmetric_return_path_scenario();

    let fwd_hops = o.forwarded_proof_hops.unwrap_or_else(|| {
        panic!(
            "A must forward a proof so we can inspect its hops.\n--- logs ---\n{}",
            o.logs
        )
    });
    let remaining = o.frozen_remaining_hops.unwrap_or_else(|| {
        panic!(
            "A must have frozen a remaining_hops in its link table.\n--- logs ---\n{}",
            o.logs
        )
    });

    // Exact values pin the scenario (optimistic 1-hop path, 2-hop live return).
    assert_eq!(
        remaining, 1,
        "A must freeze remaining_hops=1 from its optimistic stored path.\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        fwd_hops, 1,
        "the proof A forwards toward the initiator must be rewritten to hops=1 \
         (the frozen remaining_hops), not the asymmetric hops=2.\n--- logs ---\n{}",
        o.logs
    );

    // THE fix: our relay forwards a proof whose hops == the frozen remaining_hops.
    // Python's Transport.py:2176 requires this equality; it now holds, so a strict
    // Python client accepts the proof and establishes instead of timing out.
    assert_eq!(
        fwd_hops, remaining,
        "FIX of #38: our relay rewrites the forwarded LRPROOF to hops={fwd_hops} \
         == frozen remaining_hops={remaining}. A strict Python peer (Transport.py:2176 \
         requires packet.hops == remaining_hops) now accepts this and establishes; \
         both our stack and a Python peer establish.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// BUG-1: the LINK_ENTRY_SET emission carries no corrupting free-text message.
// ----------------------------------------------------------------------------

/// The LINK_ENTRY_SET instrumentation must emit structured fields only. The
/// old trailing message `"froze remaining_hops=path_hops for forwarded link
/// request"` rendered under tracing's `message` field; its spaces split the
/// line and its embedded `=` produced a SECOND `remaining_hops` token that
/// shadowed the real one. After the fix the freeze is shown by the structured
/// fields alone, so `remaining_hops` appears exactly once on the line.
#[test]
fn link_entry_set_log_line_has_no_corrupting_message() {
    let o = run_asymmetric_return_path_scenario();

    // Isolate the LINK_ENTRY_SET tracing line(s).
    let link_lines: Vec<&str> = o
        .logs
        .lines()
        .filter(|l| l.contains("event=\"LINK_ENTRY_SET\""))
        .collect();
    assert!(
        !link_lines.is_empty(),
        "expected at least one LINK_ENTRY_SET line.\n--- logs ---\n{}",
        o.logs
    );

    for line in &link_lines {
        assert!(
            !line.contains("froze remaining_hops=path_hops"),
            "LINK_ENTRY_SET line still carries the free-text message: {line}"
        );
        // The embedded `=` in the old message produced a second
        // `remaining_hops=` token; with the message gone it appears once.
        let occurrences = line.matches("remaining_hops=").count();
        assert_eq!(
            occurrences, 1,
            "remaining_hops= must appear exactly once (key collision otherwise): {line}"
        );
        // The structured freeze fields survive.
        assert!(
            line.contains("packet_hops="),
            "structured packet_hops field missing: {line}"
        );
    }
}

// ----------------------------------------------------------------------------
// RED regression guard: desired post-fix behaviour (currently fails).
// ----------------------------------------------------------------------------

/// The link establishes despite the hop asymmetry. Regression guard for the fix:
/// before it, A dropped the proof at the "hop count mismatch (remaining_hops)"
/// check, so the initiator never established.
#[test]
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
        // Responder auto-accepts (Stage 1): the proof is in the same output.
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

    // BUG-4: LINK_ENTRY_SET still fires, shown by the structured event name
    // rather than the removed free-text message (BUG-1).
    assert!(
        logs.contains("event=\"LINK_ENTRY_SET\""),
        "LINK_ENTRY_SET must still fire in the green path.\n--- logs ---\n{logs}"
    );
    assert!(
        !logs.contains("froze remaining_hops=path_hops"),
        "the redundant free-text message must be gone.\n--- logs ---\n{logs}"
    );
    assert!(
        !logs.contains("Dropped LRPROOF, hop count mismatch"),
        "the symmetric path must NOT drop the proof.\n--- logs ---\n{logs}"
    );
}

// ----------------------------------------------------------------------------
// Data path: an established asymmetric link must also CARRY data.
// ----------------------------------------------------------------------------

/// Pack a minimal LINK data packet addressed to `link_id` with the given
/// pre-receipt hop count (the relay increments hops by 1 on receipt).
fn build_link_data_packet(link_id: &LinkId, hops: u8) -> Vec<u8> {
    use crate::packet::{
        HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
    };

    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Link,
            packet_type: PacketType::Data,
        },
        hops,
        transport_id: None,
        destination_hash: *link_id.as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![0xAA; 16]),
    };
    let mut buf = [0u8; crate::constants::MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// After the link establishes over the asymmetric path, a data packet returning
/// from the destination side reaches relay `A` with `hops=2` while the frozen
/// `remaining_hops=1`. Before the data-path fix the relay dropped it at the
/// "Dropped data packet, hop count mismatch (remaining_hops)" check, so the
/// established link could not carry any traffic. The relay must now forward it.
#[test]
fn lrproof_link_carries_data_despite_hop_asymmetry() {
    let ((a_data_drop_delta, a_forwarded_data), logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key, announce_raw) = make_responder();
        let mut relay_a = make_transport_node();
        let mut relay_g = make_transport_node();
        let mut initiator = make_initiator();

        let a_local = add_iface(&mut relay_a, "A_local_initiator", true);
        let a_mesh = add_iface(&mut relay_a, "A_mesh", false);
        let g_from_a = add_iface(&mut relay_g, "G_from_A", false);
        let g_to_r = add_iface(&mut relay_g, "G_to_R", false);
        let r_iface = add_iface(&mut responder, "R_mesh", false);
        let i_iface = add_iface(&mut initiator, "I_to_A", false);

        let _ = relay_a.handle_packet(InterfaceId(a_mesh), &announce_raw);
        let _ = relay_g.handle_packet(InterfaceId(g_to_r), &announce_raw);

        // Establish the link over the asymmetric path (works after the LRPROOF fix).
        let (init_link, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);
        let out = relay_a.handle_packet(InterfaceId(a_local), &request);
        let a_forwarded = one_packet(&out);
        let out = relay_g.handle_packet(InterfaceId(g_from_a), &a_forwarded);
        let g_forwarded = one_packet(&out);
        let out = responder.handle_packet(InterfaceId(r_iface), &g_forwarded);
        // Responder auto-accepts (Stage 1): the proof is in the same output.
        let proof = one_packet(&out);
        let out = relay_g.handle_packet(InterfaceId(g_to_r), &proof);
        let g_proof = one_packet(&out);
        let out = relay_a.handle_packet(InterfaceId(a_mesh), &g_proof);
        for pkt in action_data(&out) {
            let _ = initiator.handle_packet(InterfaceId(i_iface), &pkt);
        }
        assert!(
            initiator.active_link_count() == 1,
            "precondition: the link must be established before sending data"
        );

        // Data returns from the destination side: arrives at A on a_mesh
        // (next_hop side). Crafted hops=1 becomes hops=2 after the receipt
        // increment, mismatching the frozen remaining_hops=1.
        let data_pkt = build_link_data_packet(&init_link, 1);
        let dropped_before = relay_a.transport().stats().packets_dropped;
        let out = relay_a.handle_packet(InterfaceId(a_mesh), &data_pkt);
        let drop_delta = relay_a.transport().stats().packets_dropped - dropped_before;
        let forwarded = !action_data(&out).is_empty();
        (drop_delta, forwarded)
    });

    assert_eq!(
        a_data_drop_delta, 0,
        "A must NOT drop the asymmetric link data packet.\n--- logs ---\n{logs}"
    );
    assert!(
        a_forwarded_data,
        "A must forward the link data packet toward the initiator.\n--- logs ---\n{logs}"
    );
    assert!(
        !logs.contains("Dropped data packet, hop count mismatch"),
        "link data must NOT be dropped for a hop mismatch anymore.\n--- logs ---\n{logs}"
    );
}
