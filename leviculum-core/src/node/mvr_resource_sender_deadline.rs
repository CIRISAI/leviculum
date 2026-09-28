//! mvr: deterministic reproduction of Codeberg #388 — a sender whose peer goes
//! silent mid-transfer keeps the resource in flight far past the deadline its
//! own watchdogs advertise, so the application that started it hangs.
//!
//! Observed (`emulated/pathchoice_loss50_lnsd`, lnsd 4b016484, 2026-09-28,
//! `/home/lew/ci/p375/pathchoice_loss50_lnsd.log`): a 2 KiB `lncp` push over a
//! 50 % lossy pair, link RTT 701 ms. The receiver took every part by
//! 04:52:22 and sent its proof; the proof was lost, and so were the sender's
//! cache requests but one. The wrapper killed the sender at its 189 s budget
//! (exit 124) while the sender's `AwaitingProof` watchdog was 93 s into a
//! 253 s wait. No timer was going to speak first.
//!
//! Reference-first (RNS 1.3.5, `reference/Reticulum/RNS/Resource.py`):
//!
//! - `AWAITING_PROOF` — `Resource.py:1067-1068` sets `retries_left = 3` at the
//!   instant the last part goes out, and `:644` waits
//!   `rtt*PROOF_TIMEOUT_FACTOR + SENDER_GRACE_TIME` per round with no
//!   per-retry extra. Four rounds of 12.1 s = 48 s at a 701 ms RTT. Ours
//!   charged `RESOURCE_MAX_RETRIES` (16) rounds AND a growing
//!   `PER_RETRY_DELAY` on top: 253 s.
//! - `TRANSFERRING`, initiator side — `Resource.py:629-637` computes ONE
//!   budget, `rtt * timeout_factor * max_retries + sender_grace_time +
//!   max_extra_wait`, and cancels when it is spent. `timeout_factor` is the
//!   link's `TRAFFIC_TIMEOUT_FACTOR` (`Resource.py:344`); the smaller
//!   `PART_TIMEOUT_FACTOR` belongs to the receiver. 145 s at a 701 ms RTT.
//!   Ours looped sixteen slots that each re-charged `SENDER_GRACE_TIME`: 242 s.
//!
//! What the tests here pin is the bound, not the schedule: each one steps a
//! `MockClock` and asserts the transfer has failed — with the watchdog named —
//! by the budget the reference would have spent, and that it has not failed
//! before the first round could possibly expire.
//!
//! Sans-I/O: direct initiator <-> responder over one mesh hop, RTT forced so
//! the arithmetic in the assertions is exact rather than whatever the
//! handshake happened to measure.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::outgoing::{proof_round_ms, sender_part_budget_ms, sender_proof_budget_ms};
use crate::resource::{ResourceError, ResourceStatus};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The RTT both sides are pinned to. One second keeps every budget in the
/// assertions a round number and stays far below the link's own stale horizon
/// (keepalive ~205 s, stale ~410 s at this RTT), so a stale close can never be
/// the thing that ends a transfer here.
const RTT_MS: u64 = 1_000;

/// How finely the clock is stepped while waiting for a watchdog. Fine enough
/// that "failed by the budget" is a tight claim, coarse enough that the
/// longest test is a few hundred ticks.
const STEP_MS: u64 = 250;

// ----------------------------------------------------------------------------
// Sans-I/O helpers (same pattern as the other mvr modules).
// ----------------------------------------------------------------------------

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
        &["deadline"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    (node, dest_hash, signing_key)
}

/// Drive a clean initiator <-> responder link to Active on both sides, with the
/// initiator's RTT pinned to [`RTT_MS`].
fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let (mut responder, dest_hash, signing_key) = make_responder();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut initiator = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");

    let mut for_responder = action_data(&out);
    for _ in 0..8 {
        if for_responder.is_empty() {
            break;
        }
        let back = deliver_all(&mut responder, r_iface, for_responder);
        for_responder = deliver_all(&mut initiator, i_iface, back);
    }
    assert_eq!(
        initiator.active_link_count(),
        1,
        "precondition: initiator link must be active"
    );
    assert_eq!(
        responder.active_link_count(),
        1,
        "precondition: responder link must be active"
    );

    // Pin the RTT on the sender (the side under test) and derive its keepalive
    // from it, so the stale watchdog sits where a real 1 s-RTT link puts it.
    {
        let link = initiator.link_mut(&link_id).expect("initiator link");
        link.set_rtt_ms(RTT_MS);
        link.update_keepalive_from_rtt(RTT_MS as f64 / 1000.0);
    }

    (initiator, responder, i_iface, r_iface, link_id)
}

/// Advertise `data` from the initiator, have the responder accept it, and hand
/// the responder's first part request back to the initiator.
///
/// The parts the initiator emits in reply are RETURNED rather than delivered:
/// from this point the peer hears nothing, which is the failure mode under
/// test, and a caller that wants to let one part through late can.
fn advertise_and_take_one_request(
    initiator: &mut EndpointNode,
    responder: &mut EndpointNode,
    r_iface: usize,
    i_iface: usize,
    link_id: &LinkId,
    data: &[u8],
) -> ([u8; 32], Vec<Vec<u8>>) {
    responder
        .set_resource_strategy(link_id, crate::resource::ResourceStrategy::AcceptApp)
        .expect("responder link must exist to set resource strategy");

    let (resource_hash, out) = initiator
        .send_resource(link_id, data, None, false)
        .expect("send_resource must advertise");

    let mut advertised = false;
    for pkt in action_data(&out) {
        let resp_out = responder.handle_packet(InterfaceId(r_iface), &pkt);
        advertised |= resp_out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. }));
    }
    assert!(
        advertised,
        "responder must surface a ResourceAdvertised event"
    );

    let accepted = responder
        .accept_resource(link_id)
        .expect("accept_resource must start the incoming transfer");
    let requests = action_data(&accepted);
    assert!(
        !requests.is_empty(),
        "accepting the advertisement must produce a part request"
    );
    // The initiator answers the REQ with parts; the caller decides their fate.
    let parts = deliver_all(initiator, i_iface, requests);

    (resource_hash, parts)
}

/// Step the initiator's clock in [`STEP_MS`] ticks until its outgoing resource
/// fails, or until `limit_ms` have passed since `from_ms`.
///
/// Returns `(elapsed_ms, error)` on failure, `None` if the transfer was still
/// in flight when the limit ran out.
fn wait_for_failure(
    initiator: &mut EndpointNode,
    link_id: &LinkId,
    resource_hash: &[u8; 32],
    from_ms: u64,
    limit_ms: u64,
) -> Option<(u64, ResourceError)> {
    let mut elapsed = 0;
    while elapsed <= limit_ms {
        elapsed += STEP_MS;
        initiator.transport().clock().set(from_ms + elapsed);
        let out = initiator.handle_timeout();
        for event in &out.events {
            if let NodeEvent::ResourceFailed {
                resource_hash: rh,
                error,
                is_sender: true,
                ..
            } = event
            {
                if rh == resource_hash {
                    return Some((elapsed, *error));
                }
            }
        }
        assert!(
            initiator.link(link_id).is_some(),
            "the link must outlive the resource watchdog at {elapsed} ms — a \
             stale close would make this test measure the wrong timer"
        );
    }
    None
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

/// The observed #388 hang: every part has been sent, the peer's proof never
/// arrives, and the sender must give up inside its own proof budget.
///
/// RED before the fix: the sender spends `RESOURCE_MAX_RETRIES` rounds with a
/// growing `PER_RETRY_DELAY` — 268 s at this RTT — so nothing has failed by the
/// 52 s the reference would have spent.
#[test]
fn a_sender_whose_proof_never_arrives_fails_inside_its_proof_budget() {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();

    // Small enough that the receiver's first window covers every part, so the
    // sender reaches AwaitingProof on that one request.
    let data = std::vec![0x5Au8; 512];
    let (resource_hash, _parts) = advertise_and_take_one_request(
        &mut initiator,
        &mut responder,
        r_iface,
        i_iface,
        &link_id,
        &data,
    );

    assert_eq!(
        initiator
            .link(&link_id)
            .expect("link")
            .outgoing_resource_status(),
        Some(ResourceStatus::AwaitingProof),
        "precondition: one request must have covered every part"
    );

    // The reference's budget at this RTT, spelled out rather than computed, so
    // this assertion is about behaviour and not about our own arithmetic:
    // RESOURCE_MAX_PROOF_RETRIES + 1 rounds of (rtt*PROOF_TIMEOUT_FACTOR +
    // SENDER_GRACE_TIME) = 4 * 13_000. `budgets_match_the_reference_formulas`
    // below is what ties the code's formula to it.
    let budget = 52_000;

    let from_ms = initiator.transport().clock().now_ms();
    let outcome = wait_for_failure(
        &mut initiator,
        &link_id,
        &resource_hash,
        from_ms,
        budget + STEP_MS,
    );

    let (elapsed, error) = outcome.unwrap_or_else(|| {
        panic!(
            "a sender in AwaitingProof must fail the transfer within its own \
             {budget} ms proof budget; after that budget it was still in flight \
             ({:?})",
            initiator
                .link(&link_id)
                .and_then(|l| l.outgoing_resource_status())
        )
    });
    assert_eq!(
        error,
        ResourceError::ProofTimeout,
        "the failure must name the proof watchdog, not just 'timed out'"
    );
    assert!(
        elapsed > proof_round_ms(RTT_MS),
        "giving up inside a single proof round ({elapsed} ms) would skip the \
         cache requests the reference sends first"
    );
}

/// The same defect one phase earlier: the peer asked for parts once and then
/// stopped, leaving parts outstanding. The sender waits on a REQ that will
/// never come and must stop within the reference's global budget.
///
/// RED before the fix: sixteen slots that each re-charge `SENDER_GRACE_TIME`
/// come to 252 s at this RTT, against the reference's 174 s.
#[test]
fn a_sender_whose_peer_stops_requesting_parts_fails_inside_the_global_budget() {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();

    // Large enough that the receiver's first window leaves parts outstanding,
    // so the sender stays in Transferring.
    let data = std::vec![0xA5u8; 64 * 1024];
    let (resource_hash, _parts) = advertise_and_take_one_request(
        &mut initiator,
        &mut responder,
        r_iface,
        i_iface,
        &link_id,
        &data,
    );

    assert_eq!(
        initiator
            .link(&link_id)
            .expect("link")
            .outgoing_resource_status(),
        Some(ResourceStatus::Transferring),
        "precondition: one window must leave parts outstanding"
    );

    // Spelled out for the same reason as in the proof test above:
    // rtt*TRAFFIC_TIMEOUT_FACTOR*RESOURCE_MAX_RETRIES + SENDER_GRACE_TIME +
    // max_extra_wait = 96_000 + 10_000 + 68_000.
    let budget = 174_000;

    let from_ms = initiator.transport().clock().now_ms();
    let outcome = wait_for_failure(
        &mut initiator,
        &link_id,
        &resource_hash,
        from_ms,
        budget + STEP_MS,
    );

    let (elapsed, error) = outcome.unwrap_or_else(|| {
        panic!(
            "a sender in Transferring must fail the transfer within its own \
             {budget} ms part-request budget; after that budget it was still in \
             flight ({:?})",
            initiator
                .link(&link_id)
                .and_then(|l| l.outgoing_resource_status())
        )
    });
    assert_eq!(
        error,
        ResourceError::PartRequestTimeout,
        "the failure must name the part-request watchdog"
    );
    assert!(
        elapsed + STEP_MS >= budget,
        "the sender gave up after {elapsed} ms, well inside the {budget} ms the \
         reference spends — a receiver that is merely slow would be abandoned"
    );
}

/// Guard: the budget is measured from the LAST part request, so a peer that is
/// slow but still answering restarts it and is never abandoned.
///
/// Without this, shortening the budget could be "fixed" by shortening it to
/// nothing: the test spends more than one whole budget in total, with one fresh
/// request in the middle, and the transfer must survive all of it.
#[test]
fn a_fresh_part_request_restarts_the_senders_budget() {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();

    let data = std::vec![0xA5u8; 64 * 1024];
    let (resource_hash, parts) = advertise_and_take_one_request(
        &mut initiator,
        &mut responder,
        r_iface,
        i_iface,
        &link_id,
        &data,
    );
    assert!(
        !parts.is_empty(),
        "precondition: the first request must have produced parts to hold back"
    );

    let budget = 174_000;
    let first_leg = 20_000;

    // Leg 1: a short silence, well inside the budget.
    let from_ms = initiator.transport().clock().now_ms();
    assert!(
        wait_for_failure(&mut initiator, &link_id, &resource_hash, from_ms, first_leg).is_none(),
        "the sender must not give up {first_leg} ms into a {budget} ms budget"
    );

    // One held part finally reaches the peer. That resets the receiver's own
    // retry count, and the request it answers with is the sender's proof that
    // the peer is alive.
    responder
        .transport()
        .clock()
        .set(initiator.transport().clock().now_ms());
    let mut fresh_request = Vec::new();
    for pkt in parts {
        fresh_request.extend(action_data(
            &responder.handle_packet(InterfaceId(r_iface), &pkt),
        ));
        if !fresh_request.is_empty() {
            break;
        }
    }
    assert!(
        !fresh_request.is_empty(),
        "a part arriving late must make the receiver request the rest"
    );
    let _ = deliver_all(&mut initiator, i_iface, fresh_request);
    assert_eq!(
        initiator
            .link(&link_id)
            .expect("link")
            .outgoing_resource_status(),
        Some(ResourceStatus::Transferring),
        "a fresh request must leave the transfer running"
    );

    // Leg 2: nearly a whole budget of silence AFTER the fresh request. Total
    // elapsed since the first request is now first_leg + budget - one step,
    // i.e. more than a budget — so a sender that had NOT restarted its clock
    // would have given up by here.
    let from_ms = initiator.transport().clock().now_ms();
    let outcome = wait_for_failure(
        &mut initiator,
        &link_id,
        &resource_hash,
        from_ms,
        budget - 2 * STEP_MS,
    );
    assert!(
        outcome.is_none(),
        "the fresh request must restart the budget; the sender gave up after \
         {budget} ms measured from the FIRST request instead: {outcome:?}"
    );
}

/// The formulas themselves, against the reference's arithmetic at three RTTs.
///
/// The behavioural tests above quote literal budgets so that a red there is
/// always a red about the sender's behaviour. This is the other half: it pins
/// the formulas those literals came from to `Resource.py:631-632` and
/// `Resource.py:644` + `:1067-1068`.
#[test]
fn budgets_match_the_reference_formulas() {
    use crate::resource::{
        PROOF_TIMEOUT_FACTOR, RESOURCE_MAX_PROOF_RETRIES, RESOURCE_MAX_RETRIES,
        SENDER_GRACE_TIME_MS,
    };

    // sum((r+1) * PER_RETRY_DELAY for r in range(MAX_RETRIES)) — Resource.py:631
    let max_extra_wait = 68_000;

    for rtt_ms in [1_u64, 701, 4_000] {
        let expected_part =
            rtt_ms * crate::constants::TRAFFIC_TIMEOUT_FACTOR * RESOURCE_MAX_RETRIES as u64
                + SENDER_GRACE_TIME_MS
                + max_extra_wait;
        assert_eq!(
            sender_part_budget_ms(rtt_ms),
            expected_part,
            "part-request budget at rtt {rtt_ms} ms"
        );

        let expected_round = rtt_ms * PROOF_TIMEOUT_FACTOR + SENDER_GRACE_TIME_MS;
        assert_eq!(
            proof_round_ms(rtt_ms),
            expected_round,
            "proof round at rtt {rtt_ms} ms"
        );
        assert_eq!(
            sender_proof_budget_ms(rtt_ms),
            expected_round * (RESOURCE_MAX_PROOF_RETRIES as u64 + 1),
            "proof budget at rtt {rtt_ms} ms"
        );
    }

    // The two numbers the behavioural tests spell out.
    assert_eq!(sender_part_budget_ms(1_000), 174_000);
    assert_eq!(sender_proof_budget_ms(1_000), 52_000);
}
