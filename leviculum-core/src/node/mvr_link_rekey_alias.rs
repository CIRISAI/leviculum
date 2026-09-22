//! mvr: deterministic reproduction of the #66 re-key alias not being resolved
//! in link action paths (identify / request).
//!
//! Field symptom (hardware, tier3 `lora_lncp_proof_retry` /
//! `lora_lncp_link_retry`): the link-establishment retry (Codeberg #66)
//! re-keys the link under a fresh id when the first request/proof is lost.
//! The link finally establishes, but the client's follow-up call fails with
//! "identify failed: link error: link not found".
//!
//! Mechanism this mvr nails down (sans-I/O, no LoRa/Docker/Python, <1 s):
//! the #66 retry re-keys the link under a NEW id and records the
//! caller-visible original id in `link_id_aliases` (`resolve_link_id`,
//! link_management.rs). The accessor methods (`link`/`link_mut`/`close_link`)
//! resolve through the alias, but several ACTION methods in `node/mod.rs`
//! (`identify_link`, `send_request`, `send_response`, `send_resource`,
//! `get_remote_identity`) did a RAW `self.links.get(link_id)` without
//! `resolve_link_id`. A caller holding the original id therefore got
//! "link not found" after a re-key.
//!
//! The same class re-appeared in the RESOURCE action methods
//! (`set_resource_strategy`, `accept_resource`, `reject_resource`): they did
//! the same raw `self.links.get_mut(link_id)` and answered a caller holding
//! the pre-re-key id with `ResourceError::InvalidRequest`. Field symptom
//! (tier3 `lora_lncp_fetch`): `lncp fetch` establishes the link, then
//! `leviculum-cli/src/cp.rs:704` calls `set_resource_strategy` with the id
//! the `LinkEstablished` event handed it and dies with
//! "resource error: invalid resource request" -- the client rejecting its
//! own link.
//!
//! The test establishes a link THROUGH a forced establishment-timeout retry
//! (so the link is re-keyed and the original id is only reachable via the
//! alias), then drives the action methods with the ORIGINAL caller-visible
//! id. Before the fix the action methods return NotFound / LinkNotFound /
//! InvalidRequest; after the fix they succeed.

extern crate std;

use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::LINK_PENDING_TIMEOUT_MS;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::resource::ResourceStrategy;
use crate::test_utils::{MockClock, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// All bytes a node wants to put on the wire this step (SendPacket + Broadcast).
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

/// Build a responder owning a link-accepting destination.
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
        &["rekey"],
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

/// As [`establish_link_via_rekey`], but also hands back the initiator's
/// `TickOutput` for the establishing proof. That output carries the RTT
/// packet the responder needs to reach Active, so only this variant can
/// build a pair that can actually carry a resource.
fn establish_link_via_rekey_full() -> (
    EndpointNode,
    EndpointNode,
    LinkId,
    LinkId,
    crate::DestinationHash,
    TickOutput,
) {
    let (mut responder, dest_hash, signing_key) = make_responder();
    let mut initiator = make_initiator();

    // 1. Connect: broadcasts the first link request. DROP it (never delivered),
    //    so the establishment times out and the #66 retry re-keys the link.
    let (caller_link_id, _routed, _out) =
        initiator.connect(dest_hash, &signing_key).expect("connect");

    // 2. Force the establishment timeout -> retry with fresh keys (re-key).
    let now = initiator.transport().clock().now_ms();
    initiator
        .transport()
        .clock()
        .set(now + LINK_PENDING_TIMEOUT_MS + 1);
    let out = initiator.handle_timeout();
    let retry_request = one_packet(&out);

    // 3. Deliver the retry to the responder; it auto-accepts (Stage 1) and the
    //    proof walks back.
    let out = responder.handle_packet(InterfaceId(0), &retry_request);
    let proof = one_packet(&out);
    let out = initiator.handle_packet(InterfaceId(0), &proof);
    assert!(
        has_link_established(&out),
        "link must establish via the re-keyed retry"
    );
    assert_eq!(initiator.active_link_count(), 1);

    // The link must have actually been re-keyed: the live wire id differs from
    // the caller-visible id, so the action methods can only find it by
    // resolving the alias. Otherwise this mvr would be vacuous.
    let wire_link_id = *initiator
        .link(&caller_link_id)
        .expect("original id must resolve via the alias accessor")
        .id();
    assert_ne!(
        wire_link_id, caller_link_id,
        "link must be re-keyed (wire id != caller id) for this mvr to exercise the alias path"
    );

    (
        initiator,
        responder,
        caller_link_id,
        wire_link_id,
        dest_hash,
        out,
    )
}

/// Drive an initiator-side link to Active THROUGH a forced establishment-timeout
/// retry, so the link is re-keyed (Codeberg #66) and is reachable from the
/// original caller-visible id only via `link_id_aliases`. Returns
/// `(initiator, responder, caller_link_id, wire_link_id, dest_hash)`.
fn establish_link_via_rekey() -> (
    EndpointNode,
    EndpointNode,
    LinkId,
    LinkId,
    crate::DestinationHash,
) {
    let (initiator, responder, caller_link_id, wire_link_id, dest_hash, _proof_out) =
        establish_link_via_rekey_full();
    (
        initiator,
        responder,
        caller_link_id,
        wire_link_id,
        dest_hash,
    )
}

/// A re-keyed link with BOTH sides Active: the initiator's post-proof RTT
/// packet is delivered so the responder leaves `PendingIncoming` and can
/// advertise a resource into the link. The initiator is the re-keyed side, so
/// it is the one holding a pre-re-key id; the responder addresses the link by
/// its wire id (responder links are never re-keyed, the #66 retry is
/// initiator-only). Returns `(initiator, responder, caller_link_id,
/// wire_link_id)`.
fn active_pair_via_rekey() -> (EndpointNode, EndpointNode, LinkId, LinkId) {
    let (mut initiator, mut responder, caller_link_id, wire_link_id, _dest, proof_out) =
        establish_link_via_rekey_full();

    let mut to_responder = action_data(&proof_out);
    for _ in 0..4 {
        if to_responder.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for pkt in to_responder {
            back.extend(action_data(&responder.handle_packet(InterfaceId(0), &pkt)));
        }
        let mut forward = Vec::new();
        for pkt in back {
            forward.extend(action_data(&initiator.handle_packet(InterfaceId(0), &pkt)));
        }
        to_responder = forward;
    }

    assert!(
        responder
            .link(&wire_link_id)
            .expect("responder knows the re-keyed link by its wire id")
            .is_active(),
        "responder side must reach Active before it can advertise a resource"
    );
    (initiator, responder, caller_link_id, wire_link_id)
}

/// Park an application resource ADV on the RE-KEYED initiator side and return
/// the initiator's `TickOutput` for the ADV packet.
///
/// The `AcceptApp` strategy is applied through `link_mut` (an accessor that
/// has always resolved the alias) rather than through `set_resource_strategy`,
/// so each of the three resource action methods is tested in isolation and a
/// broken `set_resource_strategy` cannot mask a broken `accept_resource`.
fn park_adv_on_rekeyed_initiator() -> (EndpointNode, LinkId, TickOutput) {
    let (mut initiator, mut responder, caller_link_id, wire_link_id) = active_pair_via_rekey();

    initiator
        .link_mut(&caller_link_id)
        .expect("original id must resolve via the alias accessor")
        .set_resource_strategy(ResourceStrategy::AcceptApp);

    let payload: Vec<u8> = (0..3000usize).map(|i| (i % 251) as u8).collect();
    let (_hash, out) = responder
        .send_resource(&wire_link_id, &payload, None, false)
        .expect("responder advertises a resource into the re-keyed link");
    let adv = one_packet(&out);

    let adv_out = initiator.handle_packet(InterfaceId(0), &adv);
    assert!(
        adv_out
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. })),
        "AcceptApp must park the ADV for the application.\nevents: {:?}",
        adv_out.events
    );
    (initiator, caller_link_id, adv_out)
}

/// `identify_link` with the original caller-visible id must succeed after a
/// re-key. Before the fix it returned `LinkError::NotFound` (the observed
/// field failure "identify failed: link error: link not found").
#[test]
fn rekey_alias_resolved_for_identify_link() {
    let (mut initiator, _responder, caller_link_id, _wire, _dest) = establish_link_via_rekey();
    let identity = Identity::generate(&mut OsRng);

    let result = initiator.identify_link(&caller_link_id, &identity);
    assert!(
        result.is_ok(),
        "identify_link with the original caller-visible id must resolve the \
         re-key alias, got {:?}",
        result.err()
    );
}

/// `send_request` with the original caller-visible id must succeed after a
/// re-key. Before the fix it returned `RequestError::LinkNotFound`.
#[test]
fn rekey_alias_resolved_for_send_request() {
    let (mut initiator, _responder, caller_link_id, _wire, _dest) = establish_link_via_rekey();

    let result = initiator.send_request(&caller_link_id, "time", None, None);
    assert!(
        result.is_ok(),
        "send_request with the original caller-visible id must resolve the \
         re-key alias, got {:?}",
        result.err()
    );
}

/// The read pair the driver's #126 accessors (`link_is_established` /
/// `link_destination`) delegate to — `link()` → `is_active` /
/// `destination_hash` — must resolve the re-key alias: queried with the
/// ORIGINAL caller-visible id, the re-keyed link reads as established and
/// names the dialed destination, in parity with the live wire id. The
/// driver-side test (`link_accessors_gate_on_established_and_expose_destination`,
/// leviculum-std) covers the unknown/pending/active gating; the re-key leg
/// lives here because only the core test rig owns a warpable clock.
#[test]
fn rekey_alias_resolved_for_establishment_and_destination_reads() {
    let (initiator, _responder, caller_link_id, wire, dest_hash) = establish_link_via_rekey();

    let via_original = initiator
        .link(&caller_link_id)
        .expect("original id must resolve via the alias");
    assert!(
        via_original.is_active(),
        "re-keyed link must read as established via the original id"
    );
    assert_eq!(
        *via_original.destination_hash(),
        dest_hash,
        "original id must resolve to the dialed destination"
    );

    let via_wire = initiator.link(&wire).expect("wire id must resolve");
    assert!(via_wire.is_active());
    assert_eq!(*via_wire.destination_hash(), dest_hash);
}

/// `get_remote_identity` must also resolve the alias (returns `None` here only
/// because the peer has not identified, never panics / mis-resolves). Guards
/// the accessor parity for the read path.
#[test]
fn rekey_alias_resolved_for_remote_identity() {
    let (initiator, _responder, caller_link_id, wire, _dest) = establish_link_via_rekey();
    // Both ids must agree (neither peer identified yet -> both None).
    assert_eq!(
        initiator.get_remote_identity(&caller_link_id).is_some(),
        initiator.get_remote_identity(&wire).is_some(),
        "remote_identity must resolve the original id the same as the wire id"
    );
}

/// THE reported red (tier3 `lora_lncp_fetch`): `set_resource_strategy` with
/// the original caller-visible id must succeed after a re-key and must land
/// on the LIVE link, not on a phantom. Before the fix it returned
/// `ResourceError::InvalidRequest`, surfaced to the user as
/// "resource error: invalid resource request" right after `LINK_ESTAB`.
#[test]
fn rekey_alias_resolved_for_set_resource_strategy() {
    let (mut initiator, _responder, caller_link_id, _wire, _dest) = establish_link_via_rekey();

    let result = initiator.set_resource_strategy(&caller_link_id, ResourceStrategy::AcceptAll);
    assert!(
        result.is_ok(),
        "set_resource_strategy with the original caller-visible id must \
         resolve the re-key alias, got {:?}",
        result.err()
    );
    assert_eq!(
        initiator
            .link(&caller_link_id)
            .expect("link still reachable")
            .resource_strategy(),
        ResourceStrategy::AcceptAll,
        "the strategy must be applied to the re-keyed link itself"
    );
}

/// `accept_resource` with the original caller-visible id must start the parked
/// transfer after a re-key. The caller can only ever hold the ORIGINAL id
/// here: `ResourceAdvertised` is emitted with it (the event boundary rewrites
/// wire ids back through `link_origin_ids`), so the documented
/// "call this after receiving ResourceAdvertised" usage feeds the stale id
/// straight back in.
#[test]
fn rekey_alias_resolved_for_accept_resource() {
    let (mut initiator, caller_link_id, adv_out) = park_adv_on_rekeyed_initiator();

    let advertised_id = adv_out
        .events
        .iter()
        .find_map(|e| match e {
            NodeEvent::ResourceAdvertised { link_id, .. } => Some(*link_id),
            _ => None,
        })
        .expect("ResourceAdvertised present");
    assert_eq!(
        advertised_id, caller_link_id,
        "the event hands the application the ORIGINAL id, so that id must work"
    );

    let out = initiator
        .accept_resource(&advertised_id)
        .expect("accept_resource with the original caller-visible id must resolve the alias");
    assert!(
        out.events.iter().any(|e| matches!(
            e,
            NodeEvent::ResourceTransferStarted {
                is_sender: false,
                ..
            }
        )),
        "accept_resource must start the parked transfer.\nevents: {:?}",
        out.events
    );
    assert!(
        !out.actions.is_empty(),
        "accept_resource must answer the ADV with a resource REQ packet"
    );
}

/// `reject_resource` with the original caller-visible id must consume the
/// parked ADV after a re-key and emit the receiver cancel. Same reachability
/// as `accept_resource`: the id comes from `ResourceAdvertised`.
#[test]
fn rekey_alias_resolved_for_reject_resource() {
    let (mut initiator, caller_link_id, _adv_out) = park_adv_on_rekeyed_initiator();

    let out = initiator
        .reject_resource(&caller_link_id)
        .expect("reject_resource with the original caller-visible id must resolve the alias");
    assert!(
        !out.actions.is_empty(),
        "reject_resource must send the receiver cancel (RCL) on the re-keyed link"
    );
    // The ADV is consumed: a second reject has nothing left to decline.
    assert!(
        matches!(
            initiator.reject_resource(&caller_link_id),
            Err(crate::resource::ResourceError::NoPendingResource)
        ),
        "the parked ADV must have been taken off the LIVE link"
    );
}
