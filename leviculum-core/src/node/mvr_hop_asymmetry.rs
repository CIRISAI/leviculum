//! mvr: HONEST reproduction of the #38 LRPROOF hop asymmetry and the healing it
//! suppresses. No shim, no hop-byte tampering, no hand-poking of `packet.hops` —
//! the mismatch arises from an honest topology in which the ANNOUNCE and the
//! LINK/proof reach the destination over routes of different length.
//!
//! ## Why this exists next to `mvr_lrproof.rs`
//!
//! `mvr_lrproof.rs` produces the OTHER sign of the asymmetry (proof arrives with
//! MORE hops than the frozen `remaining_hops`) and asserts the forward/rewrite.
//! It does NOT exercise the healing consequence. This mvr reproduces the field
//! sign where the proof arrives with FEWER hops than the frozen count
//! (`packet_hops=4 remaining_hops=5` on hamster, 2026-07-10; see
//! `docs/src/architecture-hop-counting.md` "Field evidence"), with a
//! local-client initiator (`hops == 0`), and then asserts the bug the doc calls
//! "the single most important sentence on this page": a relay that rewrites a
//! mismatching hop count so the proof is accepted makes the link succeed once
//! and guarantees the wrong path is never corrected — `clean_link_table` skips
//! validated entries, so no fresh path is ever requested.
//!
//! ## The honest topology (no shim)
//!
//! ```text
//!                     announce (long arm)                short arm
//!   R ---R_to_Y--> Y ---Y_to_Z--> Z ---Z_to_A--> A       R <--Z_to_R--> Z
//!                                                 |
//!   I --(local client, IPC)--> A(relay under test)
//! ```
//!
//! The announce reaches A over the LONG arm `R -> Y -> Z -> A`, so A's path
//! table records the long length. Z later learns a SHORT direct path to R
//! (`R -> Z`) and does NOT re-announce it to A, so A keeps the stale long entry.
//! The two relays now disagree about the tree (the doc's "the two coincide only
//! while all those relays agree on the same tree"). When the link is opened, A
//! forwards toward its next hop Z, and Z — hop by hop, by its own `next_hop` —
//! routes the request over its short direct arm. The proof returns over that
//! short arm and arrives at A shorter than the frozen count.
//!
//! Nothing about the hop byte is doctored. Every counter is what an honest node
//! computed from an honest delivery. The asymmetry is purely route-length.
//!
//! ## Exact hop numbers (all derived, none injected)
//!
//! Path learning (recorded hops = wire hops + the receipt increment):
//!   * announce, long arm: R(wire 0) -> Y records 1, rebroadcasts wire 1
//!     -> Z records 2, rebroadcasts wire 2 -> A records **hops_to(R) = 3**.
//!   * announce, short arm: R(wire 0) -> Z records **hops_to(R) = 1** (direct);
//!     Z does not forward this to A, so A stays at 3.
//!
//! Link request (I is a LOCAL CLIENT of A, so the IPC hop is free):
//!   * I builds request wire 0 -> A: +1 receipt, -1 local-client => `packet.hops
//!     = 0` at A. A freezes `link_entry.hops = 0` (the taken hops — this is the
//!     `hops == 0` that identifies a local-client link) and `remaining_hops = 3`
//!     (its stale long path). A forwards toward Z carrying wire 0.
//!   * Z: +1 => 1, path to R is direct so forwards Type1 to R carrying wire 1.
//!   * R: +1 => 2, accepts, proves.
//!
//! Proof (returns over the SHORT arm A <- Z <- R):
//!   * R emits proof wire 0 -> Z: +1 => 1; at Z `packet.hops(1) == remaining(1)`,
//!     no asymmetry, forwards toward A carrying wire 1.
//!   * A: +1 => `packet.hops = 2`. A's frozen `remaining_hops = 3`. **2 != 3**,
//!     an HONEST mismatch (`packet_hops=2 hops=0 remaining_hops=3 dir=next_hop`),
//!     the same shape and sign as the field's `packet_hops=4 remaining_hops=5`.
//!
//! A rewrites the forwarded proof to 3 (the frozen count), validates the link,
//! and therefore `clean_link_table` never requests a fresh path for R. The wrong
//! path survives and the mismatch would recur for the life of the path entry.
//!
//! Sans-I/O: no LoRa, no Docker, no Python, sub-second wall clock. Per-packet
//! delivery is scripted so the route-length asymmetry is deterministic.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage, Storage};
use crate::transport::{Action, InterfaceId, TickOutput};

// Tracing capture (prove the EXACT warning, both frozen operands) routes
// through the shared global-subscriber helper so a callsite registered by an
// earlier parallel test cannot hide events.
use crate::test_log_capture::with_captured_logs;

// ----------------------------------------------------------------------------
// Sans-I/O node helpers
// ----------------------------------------------------------------------------

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;
type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

fn add_iface<S>(
    node: &mut NodeCore<OsRng, MockClock, S>,
    name: &'static str,
    local_client: bool,
) -> usize
where
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

/// All bytes an output wants to put on the wire this step.
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

fn make_transport_node() -> TransportNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    )
}

/// A transport relay running the STRICT reference check
/// (`lrproof_rewrite_on_asymmetry = false`): a proof whose hops match neither
/// frozen count is dropped, arming the healing loop.
fn make_transport_node_strict() -> TransportNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new()
        .enable_transport(true)
        .lrproof_rewrite_on_asymmetry(false)
        .build(OsRng, clock, MemoryStorage::with_defaults())
}

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, NoStorage)
}

/// Build a responder owning a link-accepting destination. Returns the node, its
/// hash, its Ed25519 verifying key, and THREE distinct direct (hops=0) announce
/// packets: one for the long arm, one for the short arm, and one used by the
/// heal scenario to re-teach the short arm after the strict drop. Separate
/// announces are packed so relays do not deduplicate them.
fn make_responder() -> (
    EndpointNode,
    crate::DestinationHash,
    [u8; 32],
    Vec<u8>,
    Vec<u8>,
    Vec<u8>,
) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["hopasym"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    let mut pack = |ts: u64| {
        let ann = dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = ann.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    };
    let announce_long = pack(TEST_TIME_MS);
    let announce_short = pack(TEST_TIME_MS + 1_000);
    let announce_heal = pack(TEST_TIME_MS + 2_000);

    node.register_destination(dest);
    (
        node,
        dest_hash,
        signing_key,
        announce_long,
        announce_short,
        announce_heal,
    )
}

/// Feed an announce into a relay, advance its clock past the rebroadcast delay,
/// and collect the forwarded announce bytes (a relay schedules the rebroadcast;
/// it surfaces on the next timeout poll).
fn forward_announce(relay: &mut TransportNode, in_iface: usize, raw: &[u8]) -> Vec<Vec<u8>> {
    let _ = relay.handle_packet(InterfaceId(in_iface), raw);
    let now = relay.transport().clock().now_ms();
    relay.transport().clock().set(now + 100_000);
    let out = relay.handle_timeout();
    action_data(&out)
}

// ----------------------------------------------------------------------------
// The honest asymmetric-route scenario
// ----------------------------------------------------------------------------

struct Outcome {
    /// Was the honest hop-asymmetry warning emitted at relay A?
    warning_fired: bool,
    /// The exact `packet_hops` operand reported on the warning line.
    warn_packet_hops: Option<u8>,
    /// The exact `remaining_hops` operand reported on the warning line.
    warn_remaining_hops: Option<u8>,
    /// The `hops` (taken hops) operand reported on the warning line.
    warn_taken_hops: Option<u8>,
    /// `A`'s stored path length to R (the value frozen into remaining_hops).
    a_hops_to_r: Option<u8>,
    /// `Z`'s stored path length to R (the short arm it routes over).
    z_hops_to_r: Option<u8>,
    /// A's link entry after the proof: is it validated?
    link_validated: bool,
    /// Number of packets A dropped when the proof arrived.
    a_drop_delta: u64,
    /// After the clock is advanced past the link timeout and the table swept:
    /// did A issue a path request for R? (`Some(t)` = yes.)
    path_request_time_after_sweep: Option<u64>,
    /// Sanity: did the initiator establish the link?
    initiator_established: bool,
    /// #330: A's stored path length to R AFTER the validated proof was handled.
    /// 1.5.0 rebalances it to the length the proof travelled; 1.3.5 leaves the
    /// stale value standing.
    a_hops_to_r_after_proof: Option<u8>,
    /// #330: A's frozen `remaining_hops` for the link AFTER the proof.
    a_remaining_hops_after_proof: Option<u8>,
    /// The hop byte A put on the wire when it forwarded the proof to I. Pins the
    /// #38 rewrite: the downstream peer still reads the count IT expects, even
    /// though A adopted the other one for itself.
    forwarded_proof_wire_hops: Option<u8>,
    logs: String,
}

fn run_scenario() -> Outcome {
    run_scenario_forge(false)
}

/// The scenario above, with one switch: `forge_proof_signature` flips a byte of
/// the returning proof's Ed25519 signature just before it reaches A. Everything
/// else — the topology, the hop counts, the honest mismatch — is identical, so
/// the only difference in the outcome is what an INVALID signature buys: the
/// negative control for #330's adoption (a convenient hop count on an unsigned
/// proof must move nothing).
fn run_scenario_forge(forge_proof_signature: bool) -> Outcome {
    let mut warn_packet_hops = None;
    let mut warn_remaining_hops = None;
    let mut warn_taken_hops = None;
    let mut a_hops_to_r = None;
    let mut z_hops_to_r = None;
    let mut link_validated = false;
    let mut a_drop_delta = 0;
    let mut path_request_time_after_sweep = None;
    let mut initiator_established = false;
    let mut a_hops_to_r_after_proof = None;
    let mut a_remaining_hops_after_proof = None;
    let mut forwarded_proof_wire_hops = None;

    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_r, signing_key, announce_long, announce_short, _announce_heal) =
            make_responder();
        let mut relay_a = make_transport_node();
        let mut relay_z = make_transport_node();
        let mut relay_y = make_transport_node();
        let mut initiator = make_initiator();

        // Interfaces (index order fixed by call order).
        let a_local = add_iface(&mut relay_a, "A_local_initiator", true); // I -> A (IPC)
        let a_to_z = add_iface(&mut relay_a, "A_to_Z", false); // A <-> Z
        let z_to_a = add_iface(&mut relay_z, "Z_to_A", false);
        let z_to_r = add_iface(&mut relay_z, "Z_to_R", false); // short direct arm to R
        let z_from_y = add_iface(&mut relay_z, "Z_from_Y", false); // long arm
        let y_from_r = add_iface(&mut relay_y, "Y_from_R", false);
        // Y needs an outbound interface toward Z to rebroadcast the announce on.
        let _y_to_z = add_iface(&mut relay_y, "Y_to_Z", false);
        // R only handles the request/proof over the short arm; the long announce
        // is pre-packed, so R has no interface toward Y.
        let r_to_z = add_iface(&mut responder, "R_to_Z", false);
        let i_to_a = add_iface(&mut initiator, "I_to_A", false);

        // --- Path learning over the LONG arm: R -> Y -> Z -> A ---------------
        // Y forwards the direct announce (wire 0 -> wire 1).
        let y_fwds = forward_announce(&mut relay_y, y_from_r, &announce_long);
        assert_eq!(y_fwds.len(), 1, "Y forwards exactly one announce");
        // Z forwards Y's announce (wire 1 -> wire 2); Z records R at 2 hops via Y.
        let mut z_fwds = Vec::new();
        for f in &y_fwds {
            z_fwds.extend(forward_announce(&mut relay_z, z_from_y, f));
        }
        assert_eq!(z_fwds.len(), 1, "Z forwards exactly one announce");
        // A receives Z's announce and records R at 3 hops via Z (next_hop = Z).
        for f in &z_fwds {
            let _ = relay_a.handle_packet(InterfaceId(a_to_z), f);
        }
        a_hops_to_r = relay_a.hops_to(&dest_r);
        assert_eq!(
            a_hops_to_r,
            Some(3),
            "A must record the LONG arm length (3 hops) as its path to R"
        );

        // --- Z acquires the SHORT direct arm and does NOT re-announce to A ---
        // Advance Z's clock so the second announce is fresh, then deliver R's
        // direct announce straight to Z. 1 < 2 so Z replaces its via-Y entry.
        let znow = relay_z.transport().clock().now_ms();
        relay_z.transport().clock().set(znow + 30_000);
        let _ = relay_z.handle_packet(InterfaceId(z_to_r), &announce_short);
        z_hops_to_r = relay_z.hops_to(&dest_r);
        assert_eq!(
            z_hops_to_r,
            Some(1),
            "Z must now hold the SHORT direct arm (1 hop) to R, unknown to A"
        );

        // --- The local client opens a link to R through A -------------------
        let (init_link, _routed, out) = initiator.connect(dest_r, &signing_key).expect("connect");
        let request = one_packet(&out);

        // A forwards the request; freezes remaining_hops = 3, hops = 0.
        let out = relay_a.handle_packet(InterfaceId(a_local), &request);
        let a_forwarded = one_packet(&out);

        // Z forwards over its short direct arm to R.
        let out = relay_z.handle_packet(InterfaceId(z_to_a), &a_forwarded);
        let z_forwarded = one_packet(&out);

        // R accepts and proves (auto-accept, ProofStrategy::All).
        let out = responder.handle_packet(InterfaceId(r_to_z), &z_forwarded);
        let proof = one_packet(&out);

        // Proof returns Z (short arm): packet.hops(1) == remaining(1), no warn.
        let out = relay_z.handle_packet(InterfaceId(z_to_r), &proof);
        let z_proof = one_packet(&out);

        // Negative control: corrupt the first signature byte (proof data starts
        // at wire offset 19 on a Type1 packet: flags(1) hops(1) dest(16)
        // context(1)). The hop byte is untouched, so the mismatch A sees is
        // exactly the same one.
        let z_proof = if forge_proof_signature {
            let mut forged = z_proof;
            assert!(forged.len() > 19, "proof packet must carry proof data");
            forged[19] ^= 0xff;
            forged
        } else {
            z_proof
        };

        // Proof reaches A with packet.hops = 2 while remaining_hops = 3: the
        // honest mismatch. A warns, rewrites, validates, forwards to I.
        let dropped_before = relay_a.transport().stats().packets_dropped;
        let out = relay_a.handle_packet(InterfaceId(a_to_z), &z_proof);
        a_drop_delta = relay_a.transport().stats().packets_dropped - dropped_before;

        // #330 observables, read at the moment the proof was handled: what A now
        // believes the route to R costs, what it froze for the link, and what it
        // actually put on the wire toward I.
        a_hops_to_r_after_proof = relay_a.hops_to(&dest_r);
        a_remaining_hops_after_proof = relay_a
            .transport()
            .storage()
            .get_link_entry(init_link.as_bytes())
            .map(|e| e.remaining_hops);
        forwarded_proof_wire_hops = action_data(&out).first().map(|p| p[1]);

        for pkt in action_data(&out) {
            let iout = initiator.handle_packet(InterfaceId(i_to_a), &pkt);
            if has_link_established(&iout) {
                initiator_established = true;
            }
        }

        // The link entry at A is now validated by the rewritten proof.
        link_validated = relay_a
            .transport()
            .storage()
            .get_link_entry(init_link.as_bytes())
            .map(|e| e.validated)
            .unwrap_or(false);

        // --- The suppressed healing: sweep the link table past the timeout ---
        // The validated entry is skipped by clean_link_table, so NO fresh path
        // is requested for R. Advance A's clock past LINK_TIMEOUT_MS and poll.
        let anow = relay_a.transport().clock().now_ms();
        relay_a
            .transport()
            .clock()
            .set(anow + crate::constants::LINK_TIMEOUT_MS + 2_000);
        let _ = relay_a.handle_timeout();
        path_request_time_after_sweep = relay_a
            .transport()
            .storage()
            .get_path_request_time(dest_r.as_bytes());
    });

    // Parse the honest warning line for its exact operands.
    let mut warning_fired = false;
    for line in logs.lines() {
        if line.contains(
            "LRPROOF hop asymmetry: rewriting forwarded hops to the frozen count (remaining_hops)",
        ) {
            warning_fired = true;
            warn_packet_hops = field_u8(line, "packet_hops=");
            warn_remaining_hops = field_u8(line, "remaining_hops=");
            warn_taken_hops = field_u8(line, "hops=");
        }
    }

    Outcome {
        warning_fired,
        warn_packet_hops,
        warn_remaining_hops,
        warn_taken_hops,
        a_hops_to_r,
        z_hops_to_r,
        link_validated,
        a_drop_delta,
        path_request_time_after_sweep,
        initiator_established,
        a_hops_to_r_after_proof,
        a_remaining_hops_after_proof,
        forwarded_proof_wire_hops,
        logs,
    }
}

/// Read the `u8` value of `key` (e.g. `"packet_hops="`) from a log line. Matches
/// the FIRST occurrence of the exact key token, guarding against substring
/// collisions by requiring the char before `key` to be a boundary (space or `=`
/// start of line). `hops=` is looked up so that it does not match inside
/// `packet_hops=`/`remaining_hops=`/`path_hops_now=` by requiring a leading
/// space.
fn field_u8(line: &str, key: &str) -> Option<u8> {
    let mut search = line;
    loop {
        let pos = search.find(key)?;
        let before_ok = pos == 0 || search.as_bytes()[pos - 1] == b' ';
        if before_ok {
            let rest = &search[pos + key.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            return rest[..end].parse().ok();
        }
        search = &search[pos + key.len()..];
    }
}

// ----------------------------------------------------------------------------
// Assertion 1: the HONEST warning fired with hops=0 and packet_hops != remaining.
// ----------------------------------------------------------------------------

/// The relay forwards the LRPROOF for a local-client link whose taken hops (0)
/// disagree with the frozen remaining_hops (3), produced by a two-arm topology
/// of unequal length WITHOUT any shim. The warning reports both honest operands.
#[test]
fn hop_asymmetry_warning_reports_honest_local_client_mismatch() {
    let o = run_scenario();

    // The topology is honest: A learned the long arm, Z routes the short arm.
    assert_eq!(o.a_hops_to_r, Some(3), "A's frozen path length (long arm)");
    assert_eq!(o.z_hops_to_r, Some(1), "Z's live path length (short arm)");

    assert!(
        o.warning_fired,
        "the honest hop-asymmetry warning must fire.\n--- logs ---\n{}",
        o.logs
    );
    // hops == 0 identifies a local-client-initiated link (the field's `hops=0`).
    assert_eq!(
        o.warn_taken_hops,
        Some(0),
        "the warning must report the local-client taken hops (hops=0).\n--- logs ---\n{}",
        o.logs
    );
    // Both operands of the honest mismatch are reported, and they disagree.
    let ph = o.warn_packet_hops.expect("packet_hops on warning line");
    let rh = o
        .warn_remaining_hops
        .expect("remaining_hops on warning line");
    assert_eq!(
        (ph, rh),
        (2, 3),
        "honest mismatch: proof arrived over the short arm (packet_hops=2) \
         against the frozen long-arm count (remaining_hops=3).\n--- logs ---\n{}",
        o.logs
    );
    assert_ne!(
        ph, rh,
        "the reported operands must genuinely disagree (no shim aligned them).\n--- logs ---\n{}",
        o.logs
    );
    // And it was a forward, not a drop.
    assert_eq!(o.a_drop_delta, 0, "A must forward, not drop, the proof");
    assert!(
        !o.logs.contains("Dropped LRPROOF, hop count mismatch"),
        "the proof must not be dropped for the hop mismatch.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// Assertion 2: the rewrite validates the link at the relay.
// ----------------------------------------------------------------------------

/// Because the relay rewrites the mismatching proof to the frozen count, the
/// link entry at A becomes validated. (Sanity: the initiator also establishes.)
#[test]
fn hop_asymmetry_rewrite_validates_link_at_relay() {
    let o = run_scenario();
    assert!(
        o.link_validated,
        "the rewrite must mark A's link entry validated.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.initiator_established,
        "sanity: the rewritten proof establishes the link at the initiator.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// Assertion 3: no path request after the sweep — and since #330, none is owed.
// ----------------------------------------------------------------------------

/// A validated entry is skipped by `clean_link_table`
/// (`if entry.validated { continue; }`), so after the link times out and the
/// table is swept, A issues NO fresh path request for R.
///
/// Until #330 that was the bug: the rewrite validated the link, the sweep asked
/// for nothing, and the stale long-arm path survived to mis-freeze the next
/// link. Since #330 the sweep has nothing to ask FOR — the validated proof
/// rebalanced the path entry in place (see
/// `relay_adopts_validated_proof_hop_count_into_link_and_path`), so the silence
/// here is correctness, not suppression. The companion assertion below pins
/// exactly that, so this test cannot go green on a stale path again.
///
/// Mirror case: the SAME link entry, had it NOT validated, WOULD request a path
/// — this is exactly the unit test
/// `transport::tests::...::clean_link_table_local_client_link_requests_path`
/// (transport.rs), which sets an UNVALIDATED local-client entry (hops == 0) with
/// a known non-direct path and asserts `get_path_request_time(&dest).is_some()`
/// after the sweep.
#[test]
fn hop_asymmetry_validation_suppresses_path_rediscovery() {
    let o = run_scenario();

    // Precondition for the assertion to be meaningful: the entry was validated.
    assert!(
        o.link_validated,
        "precondition: the link must have validated for this to test the \
         suppression.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.path_request_time_after_sweep.is_none(),
        "no path request must be issued for R after the validated link times \
         out (clean_link_table skips validated entries). got {:?}.\n--- logs ---\n{}",
        o.path_request_time_after_sweep,
        o.logs
    );
    // And the reason the silence is harmless: the path is already right.
    assert_eq!(
        o.a_hops_to_r_after_proof,
        Some(2),
        "#330: nothing is owed to the sweep because the validated proof already \
         corrected the path to R.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// #330 (relay shape): the validated proof's hop count is ADOPTED, not dropped.
//
// Python 1.5.0 `Transport.py:2540` + `ALLOW_LINK_PATH_REBALANCE = True`:
//   if packet.hops != link_entry[IDX_LT_REM_HOPS] and ALLOW_LINK_PATH_REBALANCE:
//       ... validate signature ...
//       if peer_identity.validate(signature, signed_data) and not link_entry[IDX_LT_VALIDATED]:
//           link_entry[IDX_LT_REM_HOPS] = packet.hops
//           path_entry[IDX_PT_HOPS]     = packet.hops
// 1.3.5 (`Transport.py:2176`) had only the `==` check and dropped everything
// else, which is what our #38 work reproduced with a rewrite bolted on.
//
// This is the same honest topology as the tests above; the proof travelled
// A <- Z <- R, two hops, while A's announce-learned entry says three. The proof
// is the better witness: it went the route.
// ----------------------------------------------------------------------------

/// The relay adopts the hop count of a signature-validated LRPROOF into both the
/// link entry and the path table, and keeps forwarding the count the downstream
/// peer expects (#38, so a 1.3.5 initiator behind us still accepts the proof).
#[test]
fn relay_adopts_validated_proof_hop_count_into_link_and_path() {
    let o = run_scenario();

    // The mismatch really happened (topology, not shim).
    assert_eq!(o.a_hops_to_r, Some(3), "A's frozen path length (long arm)");
    assert_eq!(
        (o.warn_packet_hops, o.warn_remaining_hops),
        (Some(2), Some(3)),
        "precondition: the honest mismatch (proof 2 hops, frozen 3).\n--- logs ---\n{}",
        o.logs
    );

    // Adoption, both halves, exactly what 1.5.0 writes.
    assert_eq!(
        o.a_remaining_hops_after_proof,
        Some(2),
        "#330: the link entry's remaining_hops must become the proof's count \
         (Transport.py:2556).\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.a_hops_to_r_after_proof,
        Some(2),
        "#330: the path entry for R must become the proof's count \
         (Transport.py:2560).\n--- logs ---\n{}",
        o.logs
    );

    // The adoption is logged as such, with both counts on the line.
    assert!(
        o.logs.contains(
            "LRPROOF hop asymmetry: rebalanced link and path to the validated \
proof's hop count (#330)"
        ),
        "the rebalance must be visible in the journal.\n--- logs ---\n{}",
        o.logs
    );

    // Our one deviation from 1.5.0 (which forwards packet.hops unchanged): the
    // copy leaving toward the initiator still carries the PRE-adoption count, so
    // a strict 1.3.5 initiator that froze its expectation from the stale
    // announce still matches its pending link (#38, Transport.py:2228).
    assert_eq!(
        o.forwarded_proof_wire_hops,
        Some(3),
        "the forwarded proof must still carry the frozen count for the \
         downstream peer.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.initiator_established,
        "and the link must still establish at the initiator.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// The heal, with the strict flag OFF (lrproof_rewrite_on_asymmetry = false).
//
// The SAME honest asymmetry, but the relay now drops the mismatching proof
// instead of rewriting it. The drop is the sensor of the healing loop:
//   1. proof DROPPED, link entry stays validated == false;
//   2. after the link times out, clean_link_table (local-client sub-case,
//      taken hops == 0) issues a path request for the ORIGINAL destination R;
//   3. a fresh path for R reflecting the SHORT arm is learned, a second link
//      attempt agrees (packet_hops == remaining_hops), no warning fires, and the
//      link establishes. Fail once, heal, succeed.
// ----------------------------------------------------------------------------

struct HealOutcome {
    /// A's stored path length to R at first-attempt time (the frozen long arm).
    a_hops_to_r_before: Option<u8>,
    /// Z's stored path length to R (the short arm it routes over).
    z_hops_to_r: Option<u8>,
    /// Number of packets A dropped when the first proof arrived (must be 1).
    first_drop_delta: u64,
    /// A's first link entry after the dropped proof: validated? (must be false.)
    first_link_validated: bool,
    /// Did the old "rewriting" warning fire? (Must NOT, under the strict flag.)
    rewrite_warning_fired: bool,
    /// Did the new "dropping proof" warning fire? (Must, on the honest mismatch.)
    drop_warning_fired: bool,
    /// After the sweep: did A issue a path request for R? (`Some(t)` = yes.)
    path_request_time_after_sweep: Option<u64>,
    /// A's stored path length to R after the fresh short-arm announce (must be 2).
    a_hops_to_r_after_heal: Option<u8>,
    /// The second proof's hop count AS A COUNTS IT (wire byte + receipt).
    second_proof_packet_hops: Option<u8>,
    /// A's frozen remaining_hops for the second link (must equal the above).
    second_remaining_hops: Option<u8>,
    /// Did ANY hop-asymmetry warning fire on the second attempt? (Must NOT.)
    second_attempt_warning_fired: bool,
    /// A's second link entry after the matching proof: validated? (must be true.)
    second_link_validated: bool,
    /// Did the initiator establish the link on the second attempt?
    second_initiator_established: bool,
    logs: String,
}

fn run_heal_scenario() -> HealOutcome {
    let mut a_hops_to_r_before = None;
    let mut z_hops_to_r = None;
    let mut first_drop_delta = 0;
    let mut first_link_validated = true;
    let mut path_request_time_after_sweep = None;
    let mut a_hops_to_r_after_heal = None;
    let mut second_proof_packet_hops = None;
    let mut second_remaining_hops = None;
    let mut second_link_validated = false;
    let mut second_initiator_established = false;

    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_r, signing_key, announce_long, announce_short, announce_heal) =
            make_responder();
        // The relay under test runs the STRICT reference check.
        let mut relay_a = make_transport_node_strict();
        let mut relay_z = make_transport_node();
        let mut relay_y = make_transport_node();
        let mut initiator = make_initiator();

        // Interfaces (index order fixed by call order).
        let a_local = add_iface(&mut relay_a, "A_local_initiator", true); // I -> A (IPC)
        let a_to_z = add_iface(&mut relay_a, "A_to_Z", false); // A <-> Z
        let z_to_a = add_iface(&mut relay_z, "Z_to_A", false);
        let z_to_r = add_iface(&mut relay_z, "Z_to_R", false); // short direct arm to R
        let z_from_y = add_iface(&mut relay_z, "Z_from_Y", false); // long arm
        let y_from_r = add_iface(&mut relay_y, "Y_from_R", false);
        let _y_to_z = add_iface(&mut relay_y, "Y_to_Z", false);
        let r_to_z = add_iface(&mut responder, "R_to_Z", false);
        let i_to_a = add_iface(&mut initiator, "I_to_A", false);

        // --- Path learning over the LONG arm: R -> Y -> Z -> A ---------------
        let y_fwds = forward_announce(&mut relay_y, y_from_r, &announce_long);
        assert_eq!(y_fwds.len(), 1, "Y forwards exactly one announce");
        let mut z_fwds = Vec::new();
        for f in &y_fwds {
            z_fwds.extend(forward_announce(&mut relay_z, z_from_y, f));
        }
        assert_eq!(z_fwds.len(), 1, "Z forwards exactly one announce");
        for f in &z_fwds {
            let _ = relay_a.handle_packet(InterfaceId(a_to_z), f);
        }
        a_hops_to_r_before = relay_a.hops_to(&dest_r);

        // --- Z acquires the SHORT direct arm and does NOT re-announce to A ---
        let znow = relay_z.transport().clock().now_ms();
        relay_z.transport().clock().set(znow + 30_000);
        let _ = relay_z.handle_packet(InterfaceId(z_to_r), &announce_short);
        z_hops_to_r = relay_z.hops_to(&dest_r);

        // --- First link attempt: proof arrives short, is DROPPED -------------
        let (link1, _routed, out) = initiator.connect(dest_r, &signing_key).expect("connect");
        let request = one_packet(&out);
        let out = relay_a.handle_packet(InterfaceId(a_local), &request);
        let a_forwarded = one_packet(&out);
        let out = relay_z.handle_packet(InterfaceId(z_to_a), &a_forwarded);
        let z_forwarded = one_packet(&out);
        let out = responder.handle_packet(InterfaceId(r_to_z), &z_forwarded);
        let proof = one_packet(&out);
        let out = relay_z.handle_packet(InterfaceId(z_to_r), &proof);
        let z_proof = one_packet(&out);

        // Proof reaches A with packet.hops = 2, frozen remaining_hops = 3. Under
        // the strict flag A DROPS it (no rewrite, no forward).
        let dropped_before = relay_a.transport().stats().packets_dropped;
        let out = relay_a.handle_packet(InterfaceId(a_to_z), &z_proof);
        first_drop_delta = relay_a.transport().stats().packets_dropped - dropped_before;
        assert!(
            action_data(&out).is_empty(),
            "strict drop must not forward the proof to the initiator"
        );
        first_link_validated = relay_a
            .transport()
            .storage()
            .get_link_entry(link1.as_bytes())
            .map(|e| e.validated)
            .unwrap_or(false);

        // --- Sweep past the link timeout: the heal sensor fires --------------
        // clean_link_table sees an UNVALIDATED local-client entry (taken hops
        // 0) with a known non-direct path and requests a fresh path for R.
        let anow = relay_a.transport().clock().now_ms();
        relay_a
            .transport()
            .clock()
            .set(anow + crate::constants::LINK_TIMEOUT_MS + 2_000);
        let _ = relay_a.handle_timeout();
        path_request_time_after_sweep = relay_a
            .transport()
            .storage()
            .get_path_request_time(dest_r.as_bytes());

        // --- The path is relearned over the SHORT arm ------------------------
        // R re-announces; Z (path to R still direct) rebroadcasts it to A, which
        // now records R at 2 hops via Z — matching the route the proof travels.
        // 2 <= 3, so the fresher shorter path replaces A's stale long-arm entry.
        let z_heal_fwds = forward_announce(&mut relay_z, z_to_r, &announce_heal);
        assert!(
            !z_heal_fwds.is_empty(),
            "Z must rebroadcast the heal announce toward A"
        );
        let _ = relay_a.handle_packet(InterfaceId(a_to_z), &z_heal_fwds[0]);
        a_hops_to_r_after_heal = relay_a.hops_to(&dest_r);

        // --- Second link attempt: it now agrees and establishes --------------
        let (link2, _routed2, out) = initiator.connect(dest_r, &signing_key).expect("connect");
        let request2 = one_packet(&out);
        let out = relay_a.handle_packet(InterfaceId(a_local), &request2);
        let a_forwarded2 = one_packet(&out);
        let out = relay_z.handle_packet(InterfaceId(z_to_a), &a_forwarded2);
        let z_forwarded2 = one_packet(&out);
        let out = responder.handle_packet(InterfaceId(r_to_z), &z_forwarded2);
        let proof2 = one_packet(&out);
        let out = relay_z.handle_packet(InterfaceId(z_to_r), &proof2);
        let z_proof2 = one_packet(&out);

        // A counts the returning proof as wire-byte + one receipt increment.
        second_proof_packet_hops = Some(z_proof2[1].saturating_add(1));
        second_remaining_hops = relay_a
            .transport()
            .storage()
            .get_link_entry(link2.as_bytes())
            .map(|e| e.remaining_hops);

        let out = relay_a.handle_packet(InterfaceId(a_to_z), &z_proof2);
        second_link_validated = relay_a
            .transport()
            .storage()
            .get_link_entry(link2.as_bytes())
            .map(|e| e.validated)
            .unwrap_or(false);
        for pkt in action_data(&out) {
            let iout = initiator.handle_packet(InterfaceId(i_to_a), &pkt);
            if has_link_established(&iout) {
                second_initiator_established = true;
            }
        }
    });

    // Partition the warnings: any on the first attempt (the drop) vs the second
    // attempt. Because the drop-warning and the rewrite-warning carry different
    // final text, we can test each independently over the whole log.
    let rewrite_warning_fired = logs.contains(
        "LRPROOF hop asymmetry: rewriting forwarded hops to the frozen count (remaining_hops)",
    );
    let drop_warning_fired = logs.contains(
        "LRPROOF hop asymmetry: dropping proof, hops match neither frozen count (remaining_hops)",
    );
    // The second attempt must be silent. Since packet_hops == remaining_hops on
    // that attempt, NEITHER the drop nor the rewrite branch is entered, so the
    // TOTAL count of asymmetry warnings across the whole run must be exactly one
    // (the first-attempt drop). Any second warning would appear here.
    let total_asymmetry_warnings = logs
        .lines()
        .filter(|l| l.contains("LRPROOF hop asymmetry:"))
        .count();
    let second_attempt_warning_fired = total_asymmetry_warnings > 1;

    HealOutcome {
        a_hops_to_r_before,
        z_hops_to_r,
        first_drop_delta,
        first_link_validated,
        rewrite_warning_fired,
        drop_warning_fired,
        path_request_time_after_sweep,
        a_hops_to_r_after_heal,
        second_proof_packet_hops,
        second_remaining_hops,
        second_attempt_warning_fired,
        second_link_validated,
        second_initiator_established,
        logs,
    }
}

/// Heal step 1: with the strict flag OFF, the honest asymmetry proof is DROPPED
/// (not rewritten, not forwarded), and A's link entry stays unvalidated.
#[test]
fn strict_flag_drops_honest_asymmetry_proof_and_leaves_link_unvalidated() {
    let o = run_heal_scenario();

    // The topology is the same honest one as the rewrite scenario.
    assert_eq!(
        o.a_hops_to_r_before,
        Some(3),
        "A's frozen path length (long arm)"
    );
    assert_eq!(o.z_hops_to_r, Some(1), "Z's live path length (short arm)");

    assert_eq!(
        o.first_drop_delta, 1,
        "the strict relay must DROP exactly one packet (the mismatching proof).\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.drop_warning_fired,
        "the strict drop warning must fire on the honest mismatch.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.rewrite_warning_fired,
        "the old rewrite warning must NOT fire under the strict flag.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.first_link_validated,
        "the dropped proof must leave A's link entry unvalidated (heal sensor armed).\n--- logs ---\n{}",
        o.logs
    );
}

/// Heal step 2: the unvalidated local-client link times out and `clean_link_table`
/// issues a fresh path request for the ORIGINAL destination R.
#[test]
fn strict_flag_drop_triggers_path_rediscovery() {
    let o = run_heal_scenario();

    assert!(
        !o.first_link_validated,
        "precondition: the link must be unvalidated for the sweep to heal it.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.path_request_time_after_sweep.is_some(),
        "healing armed: clean_link_table must request a fresh path for R after \
         the unvalidated local-client link times out. got {:?}.\n--- logs ---\n{}",
        o.path_request_time_after_sweep,
        o.logs
    );
}

/// Heal step 3 (the decisive one): after the short arm is relearned, a second
/// link attempt AGREES — the returning proof's hop count matches the frozen
/// remaining_hops, no warning fires, and the link establishes. Convergence:
/// fail once, heal, succeed.
#[test]
fn strict_flag_second_attempt_converges_after_heal() {
    let o = run_heal_scenario();

    // The relearned path reflects the SHORT arm: A now reaches R in 2 hops
    // (A -> Z -> R), the route the proof actually travels.
    assert_eq!(
        o.a_hops_to_r_after_heal,
        Some(2),
        "after the heal, A must record the SHORT arm length (2 hops) to R.\n--- logs ---\n{}",
        o.logs
    );

    // The second proof MATCHES: packet_hops == remaining_hops, both 2.
    assert_eq!(
        o.second_proof_packet_hops,
        Some(2),
        "the second proof must arrive at A with packet_hops = 2.\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.second_remaining_hops,
        Some(2),
        "A's frozen remaining_hops for the second link must be 2 (the short arm).\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.second_proof_packet_hops, o.second_remaining_hops,
        "convergence: the returning proof now matches the frozen count.\n--- logs ---\n{}",
        o.logs
    );

    // No asymmetry warning fires on the matching second attempt.
    assert!(
        !o.second_attempt_warning_fired,
        "the second attempt must be silent (no drop, no rewrite): the counts \
         agree.\n--- logs ---\n{}",
        o.logs
    );

    // And the link establishes end to end.
    assert!(
        o.second_link_validated,
        "the matching proof must validate A's second link entry.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.second_initiator_established,
        "the matching proof must establish the link at the initiator. \
         Convergence: fail once, heal, succeed.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// #330 negative control (relay shape): a forged proof adopts NOTHING.
// ----------------------------------------------------------------------------

/// The same honest mismatch, but the proof's signature does not hold. Python
/// 1.5.0 rebalances only inside `if peer_identity.validate(signature,
/// signed_data) and not link_entry[IDX_LT_VALIDATED]` (`Transport.py:2555`), and
/// so do we: the proof is dropped, the link entry keeps its frozen count, and the
/// path table keeps the length the announce installed. A convenient hop count is
/// not an argument; a signature is.
#[test]
fn relay_refuses_to_adopt_the_hop_count_of_a_forged_proof() {
    let o = run_scenario_forge(true);

    // Same honest mismatch as the valid run (the hop byte was not touched).
    assert_eq!(o.a_hops_to_r, Some(3), "A's frozen path length (long arm)");
    assert_eq!(
        (o.warn_packet_hops, o.warn_remaining_hops),
        (Some(2), Some(3)),
        "precondition: the forged proof presents the same mismatch.\n--- logs ---\n{}",
        o.logs
    );

    // It was dropped for the signature, not forwarded.
    assert_eq!(
        o.a_drop_delta, 1,
        "the forged proof must be dropped.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.logs
            .contains("Dropped LRPROOF, signature verification failed"),
        "the drop must be attributed to the signature.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.initiator_established,
        "no link may establish on a forged proof.\n--- logs ---\n{}",
        o.logs
    );

    // And nothing was adopted.
    assert_eq!(
        o.a_remaining_hops_after_proof,
        Some(3),
        "#330 negative control: the link entry must keep its frozen count.\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.a_hops_to_r_after_proof,
        Some(3),
        "#330 negative control: the path to R must keep the announce's length.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.logs.contains("rebalanced link and path"),
        "no rebalance may be logged for a forged proof.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.logs.contains("PATH_REBALANCE"),
        "no path rebalance may happen for a forged proof.\n--- logs ---\n{}",
        o.logs
    );
}

// ----------------------------------------------------------------------------
// #330, second shape: WE are the INITIATOR, the proof returns over a shorter
// route than our path table promised.
//
// Python 1.5.0 `Transport.py:2608` + `ALLOW_LINK_PATH_REBALANCE = True`:
//   if packet.hops != link.expected_hops and link.status == PENDING and ALLOW...:
//       ... validate signature against link.destination.identity ...
//       if valid and not link.rebalanced:
//           link.expected_hops = packet.hops
//           path_entry[IDX_PT_HOPS] = packet.hops
// 1.3.5 (`Transport.py:2228`) had only `packet.hops == link.expected_hops` (or
// the PATHFINDER_M escape) and matched no pending link otherwise, so the proof
// was never validated and `create_link` timed out.
//
// Our endpoint never had that gate: it validates whatever proof carries the
// link id, so the LINK has always formed here (see the
// `lrproof_hop_undercount` interop tests). What it did NOT do is learn
// anything from the disagreement — the path entry kept the announce's length.
// That is what this shape pins.
//
// Topology (the announce takes the long arm, the link takes the short one):
//
// ```text
//   R --R_to_Y--> Y --Y_to_Z--> Z --Z_to_I--> I     (announce, 3 hops at I)
//   R <--Z_to_R--> Z <--Z_to_I--> I                 (link + proof, 2 hops at I)
// ```
// ----------------------------------------------------------------------------

/// An endpoint that keeps a path table (`MemoryStorage`) but forwards nothing
/// for anybody (`enable_transport` stays off, the default). `make_initiator`
/// above uses `NoStorage`, which cannot hold the stale path this shape needs.
fn make_initiator_with_paths() -> TransportNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults())
}

struct TerminusOutcome {
    /// I's stored path length to R before the link (the long arm).
    i_hops_to_r_before: Option<u8>,
    /// Z's stored path length to R (the short arm it routes over).
    z_hops_to_r: Option<u8>,
    /// The hop count the proof carried when I counted it (wire byte + receipt).
    proof_packet_hops: Option<u8>,
    /// What `connect` froze on the link from the path table.
    link_hops_before: Option<u8>,
    /// I's stored path length to R after the proof was validated.
    i_hops_to_r_after: Option<u8>,
    /// The link's hop count after the proof was validated.
    link_hops_after: Option<u8>,
    /// Did the link establish at I?
    established: bool,
    logs: String,
}

fn run_terminus_scenario(forge_proof_signature: bool) -> TerminusOutcome {
    let mut i_hops_to_r_before = None;
    let mut z_hops_to_r = None;
    let mut proof_packet_hops = None;
    let mut link_hops_before = None;
    let mut i_hops_to_r_after = None;
    let mut link_hops_after = None;
    let mut established = false;

    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_r, signing_key, announce_long, announce_short, _announce_heal) =
            make_responder();
        let mut initiator = make_initiator_with_paths();
        let mut relay_z = make_transport_node();
        let mut relay_y = make_transport_node();

        let i_to_z = add_iface(&mut initiator, "I_to_Z", false);
        let z_to_i = add_iface(&mut relay_z, "Z_to_I", false);
        let z_to_r = add_iface(&mut relay_z, "Z_to_R", false); // short direct arm
        let z_from_y = add_iface(&mut relay_z, "Z_from_Y", false); // long arm
        let y_from_r = add_iface(&mut relay_y, "Y_from_R", false);
        let _y_to_z = add_iface(&mut relay_y, "Y_to_Z", false);
        let r_to_z = add_iface(&mut responder, "R_to_Z", false);

        // --- Path learning over the LONG arm: R -> Y -> Z -> I ---------------
        let y_fwds = forward_announce(&mut relay_y, y_from_r, &announce_long);
        assert_eq!(y_fwds.len(), 1, "Y forwards exactly one announce");
        let mut z_fwds = Vec::new();
        for f in &y_fwds {
            z_fwds.extend(forward_announce(&mut relay_z, z_from_y, f));
        }
        assert_eq!(z_fwds.len(), 1, "Z forwards exactly one announce");
        for f in &z_fwds {
            let _ = initiator.handle_packet(InterfaceId(i_to_z), f);
        }
        i_hops_to_r_before = initiator.hops_to(&dest_r);
        assert_eq!(
            i_hops_to_r_before,
            Some(3),
            "I must record the LONG arm length (3 hops) as its path to R"
        );

        // --- Z acquires the SHORT direct arm and does NOT re-announce --------
        let znow = relay_z.transport().clock().now_ms();
        relay_z.transport().clock().set(znow + 30_000);
        let _ = relay_z.handle_packet(InterfaceId(z_to_r), &announce_short);
        z_hops_to_r = relay_z.hops_to(&dest_r);
        assert_eq!(
            z_hops_to_r,
            Some(1),
            "Z must now hold the SHORT direct arm (1 hop) to R, unknown to I"
        );

        // --- I opens the link; it freezes hops = 3 from its stale path -------
        let (link, _routed, out) = initiator.connect(dest_r, &signing_key).expect("connect");
        link_hops_before = initiator.link(&link).map(|l| l.hops());
        let request = one_packet(&out);

        // Z routes it over the short direct arm; R accepts and proves.
        let out = relay_z.handle_packet(InterfaceId(z_to_i), &request);
        let z_forwarded = one_packet(&out);
        let out = responder.handle_packet(InterfaceId(r_to_z), &z_forwarded);
        let proof = one_packet(&out);

        // Proof back through Z: packet.hops(1) == its remaining(1), no asymmetry
        // at Z — the disagreement belongs to the terminus.
        let out = relay_z.handle_packet(InterfaceId(z_to_r), &proof);
        let z_proof = one_packet(&out);
        let z_proof = if forge_proof_signature {
            let mut forged = z_proof;
            assert!(forged.len() > 19, "proof packet must carry proof data");
            forged[19] ^= 0xff;
            forged
        } else {
            z_proof
        };

        // I counts the returning proof as wire byte + one receipt increment.
        proof_packet_hops = Some(z_proof[1].saturating_add(1));

        let out = initiator.handle_packet(InterfaceId(i_to_z), &z_proof);
        established = has_link_established(&out);
        i_hops_to_r_after = initiator.hops_to(&dest_r);
        link_hops_after = initiator.link(&link).map(|l| l.hops());
    });

    TerminusOutcome {
        i_hops_to_r_before,
        z_hops_to_r,
        proof_packet_hops,
        link_hops_before,
        i_hops_to_r_after,
        link_hops_after,
        established,
        logs,
    }
}

/// #330, terminus shape: the proof came back two hops where the path table said
/// three, its signature holds, so both the link and the path entry adopt the two.
#[test]
fn initiator_adopts_validated_proof_hop_count_into_link_and_path() {
    let o = run_terminus_scenario(false);

    // The honest disagreement: frozen 3, proof 2.
    assert_eq!(o.i_hops_to_r_before, Some(3), "I's path length (long arm)");
    assert_eq!(o.z_hops_to_r, Some(1), "Z's path length (short arm)");
    assert_eq!(
        o.link_hops_before,
        Some(3),
        "connect must freeze the stale path length on the link.\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.proof_packet_hops,
        Some(2),
        "the proof must arrive at I two hops long.\n--- logs ---\n{}",
        o.logs
    );

    // The link forms — it always did here, we never had 1.3.5's terminus gate.
    assert!(
        o.established,
        "the link must establish at the terminus.\n--- logs ---\n{}",
        o.logs
    );

    // Adoption, both halves (Python 1.5.0 Transport.py:2632-2637).
    assert_eq!(
        o.link_hops_after,
        Some(2),
        "#330: the link must adopt the proof's hop count.\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.i_hops_to_r_after,
        Some(2),
        "#330: the path entry for R must adopt the proof's hop count.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        o.logs.contains("LRPROOF hop asymmetry at link terminus"),
        "the terminus rebalance must be visible in the journal.\n--- logs ---\n{}",
        o.logs
    );
    // And it is the TERMINUS arm that did it, not the relay one: Z saw its own
    // frozen count agree with the proof (1 == 1) and logged no asymmetry at all.
    assert!(
        !o.logs.contains("LRPROOF hop asymmetry: rebalanced"),
        "the relay arm must not be involved in this shape.\n--- logs ---\n{}",
        o.logs
    );
}

/// #330 negative control at the terminus: a proof whose signature does not hold
/// establishes nothing and teaches nothing. `process_proof` refuses it, the link
/// is closed, and the path keeps the announce's length.
#[test]
fn initiator_refuses_to_adopt_the_hop_count_of_a_forged_proof() {
    let o = run_terminus_scenario(true);

    assert_eq!(o.i_hops_to_r_before, Some(3), "I's path length (long arm)");
    assert_eq!(
        o.proof_packet_hops,
        Some(2),
        "precondition: the forged proof presents the same short count.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.established,
        "no link may establish on a forged proof.\n--- logs ---\n{}",
        o.logs
    );
    assert_eq!(
        o.i_hops_to_r_after,
        Some(3),
        "#330 negative control: the path to R must keep the announce's length.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.logs.contains("LRPROOF hop asymmetry at link terminus"),
        "no terminus rebalance may be logged for a forged proof.\n--- logs ---\n{}",
        o.logs
    );
    assert!(
        !o.logs.contains("PATH_REBALANCE"),
        "no path rebalance may happen for a forged proof.\n--- logs ---\n{}",
        o.logs
    );
}
