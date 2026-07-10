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
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage, Storage};
use crate::transport::{Action, InterfaceId, TickOutput};

// ----------------------------------------------------------------------------
// Tracing capture: prove the EXACT warning (both frozen operands) rather than
// only observing "the link validated".
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

fn make_initiator() -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new().build(OsRng, clock, NoStorage)
}

/// Build a responder owning a link-accepting destination. Returns the node, its
/// hash, its Ed25519 verifying key, and TWO distinct direct (hops=0) announce
/// packets: one for the long arm, one for the short arm. Two separate announces
/// are packed so relays do not deduplicate them.
fn make_responder() -> (
    EndpointNode,
    crate::DestinationHash,
    [u8; 32],
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
        let ann = dest.announce(None, &mut OsRng, ts).unwrap();
        let mut buf = [0u8; crate::constants::MTU];
        let len = ann.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    };
    let announce_long = pack(TEST_TIME_MS);
    let announce_short = pack(TEST_TIME_MS + 1_000);

    node.register_destination(dest);
    (node, dest_hash, signing_key, announce_long, announce_short)
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
    logs: String,
}

fn run_scenario() -> Outcome {
    let mut warn_packet_hops = None;
    let mut warn_remaining_hops = None;
    let mut warn_taken_hops = None;
    let mut a_hops_to_r = None;
    let mut z_hops_to_r = None;
    let mut link_validated = false;
    let mut a_drop_delta = 0;
    let mut path_request_time_after_sweep = None;
    let mut initiator_established = false;

    let ((), logs) = with_captured_logs(|| {
        let (mut responder, dest_r, signing_key, announce_long, announce_short) = make_responder();
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
        let (init_link, _routed, out) = initiator.connect(dest_r, &signing_key);
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

        // Proof reaches A with packet.hops = 2 while remaining_hops = 3: the
        // honest mismatch. A warns, rewrites, validates, forwards to I.
        let dropped_before = relay_a.transport().stats().packets_dropped;
        let out = relay_a.handle_packet(InterfaceId(a_to_z), &z_proof);
        a_drop_delta = relay_a.transport().stats().packets_dropped - dropped_before;

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
// Assertion 3: healing is suppressed — clean_link_table issues NO path request.
// ----------------------------------------------------------------------------

/// THE bug: a validated entry is skipped by `clean_link_table`
/// (`if entry.validated { continue; }`), so after the link times out and the
/// table is swept, A issues NO fresh path request for R. The wrong path (the
/// stale long arm) is never corrected and the mismatch would recur forever.
///
/// Mirror case (assertion 4): the SAME link entry, had it NOT validated, WOULD
/// request a path — this is exactly the unit test
/// `transport::tests::...::clean_link_table_local_client_link_requests_path`
/// (transport.rs), which sets an UNVALIDATED local-client entry (hops == 0) with
/// a known non-direct path and asserts `get_path_request_time(&dest).is_some()`
/// after the sweep. The only difference from this scenario is the `validated`
/// flag flipped by the rewrite; that flag is what suppresses the heal here.
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
        "healing suppressed: no path request must be issued for R after the \
         validated link times out (clean_link_table skips validated entries). \
         got {:?}.\n--- logs ---\n{}",
        o.path_request_time_after_sweep,
        o.logs
    );
}
