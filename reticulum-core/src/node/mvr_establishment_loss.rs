//! mvr: deterministic characterization of what the link state machine does when
//! the establishment handshake loses exactly one packet (the LinkRequest or the
//! proof).
//!
//! Motivation (reviewer-established, instrumented on `dbg-init-announce-jitter`):
//! the foundational cold-LoRa-link bug fails as
//! `LINK_DIED reason=other detail=handshake_timeout` — path discovery succeeds
//! but the LinkRequest -> proof handshake does not complete over the cold lossy
//! multi-hop path. An announce-jitter A/B over RF was inconclusive (1/8 vs 0/8,
//! p~=1.0): the end-to-end PDR metric is too noisy to resolve the mechanism. So
//! we attack the mechanism DETERMINISTICALLY here, with no RF, no Docker, no
//! Python, sub-second wall clock.
//!
//! The question this module answers: *when the handshake loses exactly one
//! packet, does the initiator retransmit the LinkRequest, does the responder
//! retransmit the proof, or is there exactly ONE attempt that then times out?*
//!
//! Loss injection is trivial in this sans-I/O harness: per-packet delivery is
//! scripted (the same pattern as `mvr_lrproof`), so "dropping" a packet is
//! simply declining to route it to the peer. A `MockClock` is advanced past
//! `Link::establishment_timeout_ms()` to drive `NodeCore::handle_timeout()` ->
//! `check_timeouts()` without sleeping the real ~12 s timeout.
//!
//! Finding (asserted below): the state machine is NOT one-shot. On
//! establishment timeout the initiator REGENERATES its ephemeral keys and
//! RETRANSMITS the LinkRequest (Codeberg #66), up to
//! `max(LINK_REQUEST_MAX_RETRIES, hops)` times before emitting
//! `LINK_DIED detail=handshake_timeout`. A SINGLE proof or request loss is
//! therefore RECOVERED by the retransmit, not fatal. Only persistent loss that
//! outlasts every retry reaches the timeout-death path. This characterises that
//! a Python-compatible establishment retransmit already exists; the lever for
//! the cold-link bug is the retry COUNT / TIMING, not the absence of a retry.

extern crate std;

use std::string::String;
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::LINK_REQUEST_MAX_RETRIES;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkCloseReason;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

// ----------------------------------------------------------------------------
// Tracing capture (prove the EXACT death reason, not just "no link").
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
    let guard = subscriber.set_default();
    let out = body();
    drop(guard);
    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    (out, logs)
}

// ----------------------------------------------------------------------------
// Sans-I/O node helpers (direct initiator <-> responder, one mesh hop).
// ----------------------------------------------------------------------------

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Register a named interface on a NodeCore and return its index.
fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
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

/// Build a responder node owning a link-accepting destination.
/// Returns (node, dest_hash, signing_key).
fn make_responder() -> (EndpointNode, crate::DestinationHash, [u8; 32]) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["establoss"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    node.register_destination(dest);
    (node, dest_hash, signing_key)
}

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, NoStorage)
}

/// Advance the initiator's virtual clock just past its current establishment
/// timeout and run one maintenance tick. Returns the tick output (which carries
/// any retransmitted LinkRequest as an action, or the LinkClosed event on the
/// final timeout). Reads the live timeout from the (possibly re-keyed) link so
/// the test never hardcodes the constant.
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

// ----------------------------------------------------------------------------
// (baseline) No loss -> the link establishes.
// ----------------------------------------------------------------------------

/// Control: with every packet delivered, a single establishment round trip
/// (request -> proof) establishes the link. Proves the harness drives a real
/// establishment and anchors the loss cases below. Also asserts the four
/// per-side packet-level instrumentation events fire (initiator TX/RX and
/// responder RX/TX), documenting which side emits each for the hardware run.
#[test]
fn establishment_baseline_no_loss_establishes() {
    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key) = make_responder();
        let mut initiator = make_initiator();
        let r_iface = add_iface(&mut responder, "R_mesh");
        let i_iface = add_iface(&mut initiator, "I_mesh");

        // Initiator connects -> LinkRequest (broadcast, no path known).
        let (_link_id, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);

        // Responder receives the request, accepts, emits the proof.
        let out = responder.handle_packet(InterfaceId(r_iface), &request);
        let resp_link = link_request_link_id(&out);
        let out = responder.accept_link(&resp_link).unwrap();
        let proof = one_packet(&out);

        // Initiator validates the proof -> link established.
        let out = initiator.handle_packet(InterfaceId(i_iface), &proof);
        assert!(
            has_link_established(&out),
            "baseline: link must establish when no packet is lost"
        );
        assert_eq!(initiator.active_link_count(), 1);
    });

    // Per-side establishment instrumentation must trace every handshake packet.
    for marker in [
        "LINK_REQUEST_TX", // initiator
        "LINK_REQUEST_RX", // responder
        "LINK_PROOF_TX",   // responder
        "LINK_PROOF_RX",   // initiator
    ] {
        assert!(
            logs.contains(marker),
            "baseline must emit the {marker} establishment event.\n--- logs ---\n{logs}"
        );
        assert!(
            logs.contains(&std::format!("{marker} link=")) && logs.contains("t_ms="),
            "{marker} must carry a link id and a t_ms timestamp.\n--- logs ---\n{logs}"
        );
    }
}

// ----------------------------------------------------------------------------
// (proof dropped once) The responder's proof is lost a single time.
// ----------------------------------------------------------------------------

/// KEY CHARACTERIZATION. The responder's proof is dropped exactly once. The
/// hypothesis under test was "one attempt, no establishment retransmit, then
/// ESTABLISHMENT_TIMEOUT". The ASSERTED observed behaviour REFUTES it:
///   * the initiator emits EXACTLY ONE retransmitted LinkRequest on the
///     establishment timeout (proving an establishment-level retransmit exists),
///   * that retransmit carries FRESH ephemeral keys / a new link id (Codeberg
///     #66) and is logged as "retrying with fresh keys",
///   * the responder accepts the retransmitted request and its second proof IS
///     delivered, so the link ESTABLISHES.
///
/// Conclusion: a single proof loss is RECOVERED by the retransmit, not fatal.
#[test]
fn establishment_proof_dropped_once_recovers_via_retransmit() {
    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key) = make_responder();
        let mut initiator = make_initiator();
        let r_iface = add_iface(&mut responder, "R_mesh");
        let i_iface = add_iface(&mut initiator, "I_mesh");

        let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);

        // Responder accepts and builds the proof...
        let out = responder.handle_packet(InterfaceId(r_iface), &request);
        let resp_link = link_request_link_id(&out);
        let out = responder.accept_link(&resp_link).unwrap();
        let _dropped_proof = one_packet(&out); // DROP: never delivered to initiator.

        // Initiator's establishment timer fires -> it must retransmit.
        let out = tick_past_establishment_timeout(&mut initiator, &link_id);
        let retried = action_data(&out);
        assert_eq!(
            retried.len(),
            1,
            "initiator must retransmit EXACTLY ONE LinkRequest on establishment timeout \
             (proves an establishment-level retransmit exists)"
        );
        assert!(
            !has_timeout_close(&out),
            "the first establishment timeout must retransmit, NOT close the link \
             (retries remain)"
        );
        let retried_request = retried.into_iter().next().unwrap();
        assert_ne!(
            retried_request, request,
            "the retransmit must carry fresh bytes (re-keyed), not an identical resend"
        );

        // Responder treats the fresh-keys request as a new link and proves it;
        // this second proof IS delivered -> the link recovers and establishes.
        let out = responder.handle_packet(InterfaceId(r_iface), &retried_request);
        let resp_link2 = link_request_link_id(&out);
        let out = responder.accept_link(&resp_link2).unwrap();
        let proof2 = one_packet(&out);

        let out = initiator.handle_packet(InterfaceId(i_iface), &proof2);
        assert!(
            has_link_established(&out),
            "a SINGLE proof loss must be RECOVERED by the establishment retransmit"
        );
        assert_eq!(initiator.active_link_count(), 1);
    });

    assert!(
        logs.contains("retrying with fresh keys"),
        "the retransmit must be the Codeberg #66 fresh-keys retry.\n--- logs ---\n{logs}"
    );
}

// ----------------------------------------------------------------------------
// (LinkRequest dropped once) The initiator's LinkRequest is lost a single time.
// ----------------------------------------------------------------------------

/// Mirror of the proof-loss case for the other establishment packet: the
/// initiator's LinkRequest never reaches the responder. Same observed
/// behaviour: the initiator retransmits exactly one fresh-keys LinkRequest on
/// the establishment timeout, the retransmit reaches the responder, and the
/// link establishes. A single request loss is RECOVERED, not fatal.
#[test]
fn establishment_link_request_dropped_once_recovers_via_retransmit() {
    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key) = make_responder();
        let mut initiator = make_initiator();
        let r_iface = add_iface(&mut responder, "R_mesh");
        let i_iface = add_iface(&mut initiator, "I_mesh");

        let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let _dropped_request = one_packet(&out); // DROP: never delivered to responder.

        // The responder never saw the request, so it has no pending link.
        assert_eq!(
            responder.pending_link_count(),
            0,
            "responder must not have seen the dropped request"
        );

        // Initiator's establishment timer fires -> retransmit.
        let out = tick_past_establishment_timeout(&mut initiator, &link_id);
        let retried = action_data(&out);
        assert_eq!(
            retried.len(),
            1,
            "initiator must retransmit EXACTLY ONE LinkRequest after the request was lost"
        );
        assert!(
            !has_timeout_close(&out),
            "first timeout retransmits, not closes"
        );
        let retried_request = retried.into_iter().next().unwrap();

        // This retransmit IS delivered -> the responder proves it -> establishes.
        let out = responder.handle_packet(InterfaceId(r_iface), &retried_request);
        let resp_link = link_request_link_id(&out);
        let out = responder.accept_link(&resp_link).unwrap();
        let proof = one_packet(&out);

        let out = initiator.handle_packet(InterfaceId(i_iface), &proof);
        assert!(
            has_link_established(&out),
            "a SINGLE LinkRequest loss must be RECOVERED by the establishment retransmit"
        );
        assert_eq!(initiator.active_link_count(), 1);
    });

    assert!(
        logs.contains("retrying with fresh keys"),
        "the retransmit must be the Codeberg #66 fresh-keys retry.\n--- logs ---\n{logs}"
    );
}

// ----------------------------------------------------------------------------
// (persistent loss) Every proof is lost -> the timeout-death path is reached.
// ----------------------------------------------------------------------------

/// The timeout-death path that the field bug actually hits. EVERY proof is
/// dropped (loss outlasts every retry). The initiator retransmits the
/// LinkRequest a bounded number of times and then, with no retries left, emits
/// `LINK_DIED reason=other detail=handshake_timeout` and a `LinkClosed(Timeout)`
/// event. Asserts the exact attempt budget: `1 + max(LINK_REQUEST_MAX_RETRIES,
/// hops)` total LinkRequests (initial + retries). For this direct 1-hop link
/// that is `1 + 2 = 3`. This is the deterministic stand-in for the cold-link
/// `handshake_timeout` failure: the lever is the retry budget vs. the path's
/// loss rate, not a missing retransmit.
#[test]
fn establishment_persistent_proof_loss_dies_after_bounded_retries() {
    let (died, logs) = with_captured_logs(|| {
        let (mut responder, dest_hash, signing_key) = make_responder();
        let mut initiator = make_initiator();
        let r_iface = add_iface(&mut responder, "R_mesh");

        let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key);
        let request = one_packet(&out);

        // Responder accepts and proves once; that proof is dropped.
        let out = responder.handle_packet(InterfaceId(r_iface), &request);
        let resp_link = link_request_link_id(&out);
        let _ = responder.accept_link(&resp_link).unwrap(); // proof dropped.

        let mut died = false;

        // Drive timeouts until the link dies. Each retransmit is also dropped
        // (we never route it onward), so loss is persistent. Bounded loop with a
        // generous cap so a regression that loops forever fails loudly.
        for _ in 0..16 {
            if initiator.link(&link_id).is_none() {
                break;
            }
            let out = tick_past_establishment_timeout(&mut initiator, &link_id);
            if has_timeout_close(&out) {
                died = true;
                break;
            }
        }
        died
    });

    assert!(
        died,
        "persistent proof loss must end in the LinkClosed(Timeout) death path.\n--- logs ---\n{logs}"
    );

    // Count the establishment retransmits unambiguously from the retry log line
    // (the raw outbound packet count is contaminated by unrelated path-request
    // rebroadcasts that `handle_timeout` also emits over the elapsed virtual
    // time). hops == 1 for this direct link, so the retry budget is
    // max(LINK_REQUEST_MAX_RETRIES, 1) == LINK_REQUEST_MAX_RETRIES.
    let retransmits = logs.matches("retrying with fresh keys").count() as u64;
    let expected_retransmits = core::cmp::max(LINK_REQUEST_MAX_RETRIES as u64, 1);
    assert_eq!(
        retransmits, expected_retransmits,
        "initiator must retransmit the LinkRequest exactly {expected_retransmits} times \
         (1 initial + {expected_retransmits} retries = {} total attempts) before death.\n--- logs ---\n{logs}",
        expected_retransmits + 1
    );

    assert!(
        logs.contains("detail=handshake_timeout"),
        "death must be reported as LINK_DIED detail=handshake_timeout (the field symptom).\n--- logs ---\n{logs}"
    );
}
