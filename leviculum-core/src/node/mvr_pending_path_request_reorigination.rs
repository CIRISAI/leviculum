//! mvr: a pending discovery does not re-originate the requester's retries
//! (Codeberg #433, the amplifier behind the 2026-09-27 field day).
//!
//! ## The field failure this reproduces
//!
//! The Columba phone retried a path request for one destination at about
//! 1.2 per second for three hours. Every retry carries a FRESH random tag
//! (`Transport.request_path` draws one per call), so the tag dedup at
//! `transport.rs::handle_path_request` never bites, and every relay in
//! range re-keyed every retry onto every other interface it had. The
//! result was 96.8 % of the air occupied by path requests (order 364 §4,
//! the Pocket's duty lock) and 4 359 frames off the base T114 in three
//! hours (order 368 §1). The refusals the base logged were correct
//! (`next hop is the requestor`, 130 of them); the RE-ORIGINATION was
//! the waste.
//!
//! Note what did NOT contain it: the #87 path-request ingress burst
//! limiter. Its threshold on an interface younger than `IC_NEW_TIME_MS`
//! is `IC_PR_BURST_FREQ_NEW_HZ` = 3 Hz (`transport.rs:124`), and 1.2 Hz
//! sits under it by design. This mvr runs at 1 Hz for the same reason:
//! the mechanism under test has to be the only thing that can fire.
//!
//! ## The reference
//!
//! Both reference lines suppress the rebroadcast outright while a
//! discovery for the destination is pending:
//!
//! - 1.3.5 (`reference/Reticulum/RNS/Transport.py:3015-3017`): the
//!   presence of a `discovery_path_requests` entry alone is the gate —
//!   "There is already a waiting path request …" is logged and the
//!   rebroadcast loop at `:3033-3040` sits in the `else` arm. The entry
//!   carries one `requesting_interface` and a `timeout` of
//!   `PATH_REQUEST_TIMEOUT` (15 s, `Transport.py:134`).
//! - 1.5.2 (`Transport.py:3532-3583`, wheel under
//!   `~/.local/state/leviculum-ci/rns-1.5.2`): same suppression, gated
//!   additionally on an `engaged` flag, and the entry carries a LIST of
//!   `requesting_interfaces` which the eventual answer is sent to, one
//!   targeted `PATH_RESPONSE` each (`:2432-2456`). Later requesters
//!   during the pending window are appended to that list and nothing is
//!   rebroadcast for them (the batching arm at `:1871-1882`). The
//!   window is `max(PATH_REQUEST_TIMEOUT, medium_path_timeout())`
//!   (`:3556`), which for a LoRa-bitrate mesh is the 15 s floor.
//!
//! Ours logged the same sentence and rebroadcast anyway
//! (`transport.rs::handle_path_request`, the `active_discovery` arm).
//! Our pending window is `DISCOVERY_TIMEOUT_MS` = 30 s
//! (`constants.rs:232`), deliberately twice the reference's 15 s so the
//! retry cadence has room on slow links; the suppression therefore
//! covers twice as long a window, which is strictly more airtime saved
//! for the same semantics.
//!
//! ## Shape
//!
//! Two relays, deterministic, sub-second of simulated time, one named
//! failure mode: the count of re-originated path requests per pending
//! window. The retry cadence of our OWN pending discovery
//! (`retry_pending_discoveries`, one emission per
//! `DISCOVERY_RETRY_INTERVAL_MS` = 5 s) is a separate, separately
//! bounded mechanism and is deliberately not driven inside the counted
//! window — `mvr_path_response_retries` owns it. What is counted here is
//! exactly what an incoming retry causes.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{DISCOVERY_TIMEOUT_MS, MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, InterfaceMode};
use crate::transport::{Action, InterfaceId, TickOutput};

/// Both boards' exact shape: `EmbeddedStorage`, transport enabled.
type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

/// The requester's interface on the relay, and the one the discovery
/// re-origination has to reach.
const LORA: usize = 0;
const UPLINK: usize = 1;

fn make_relay() -> Box<EmbeddedNode> {
    let mut node = NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    );
    add_iface(&mut node, "lora_sx1262", 0);
    add_iface(&mut node, "uplink_tcp", 1);
    // Gateway is a `DISCOVER_PATHS_FOR` mode (Transport.py:2917-2918
    // equivalent, `traits.rs::discovers_paths`): without it the relay
    // never re-originates and there is nothing to suppress.
    node.set_interface_mode(LORA, InterfaceMode::Gateway);
    node.set_interface_mode(UPLINK, InterfaceMode::Gateway);
    node
}

fn add_iface(node: &mut EmbeddedNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// The destination nobody in this scenario has a path to.
fn make_dest() -> Destination {
    Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap()
}

/// Raw bytes of every action in this output, tagged with the send
/// target (`None` = broadcast).
fn wire_out(out: &TickOutput) -> Vec<(Option<InterfaceId>, Vec<u8>)> {
    out.actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { iface, data, .. } => (Some(*iface), data.clone()),
            Action::Broadcast { data, .. } => (None, data.clone()),
        })
        .collect()
}

/// Path requests naming `dest` in this output, with their send target.
fn path_requests_for(
    node: &EmbeddedNode,
    out: &TickOutput,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<Option<InterfaceId>> {
    let pr_hash = *node.transport().path_request_hash();
    wire_out(out)
        .into_iter()
        .filter_map(|(target, data)| {
            let p = Packet::unpack(&data).ok()?;
            (p.flags.packet_type == PacketType::Data
                && p.destination_hash == pr_hash
                && p.data.as_slice().len() >= TRUNCATED_HASHBYTES
                && &p.data.as_slice()[..TRUNCATED_HASHBYTES] == dest)
                .then_some(target)
        })
        .collect()
}

/// Announces naming `dest` in this output, with their send target.
fn announces_for(out: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> Vec<Option<InterfaceId>> {
    wire_out(out)
        .into_iter()
        .filter_map(|(target, data)| {
            let p = Packet::unpack(&data).ok()?;
            (p.flags.packet_type == PacketType::Announce && &p.destination_hash == dest)
                .then_some(target)
        })
        .collect()
}

/// One retry as the phone puts it on the wire: a 48-byte transport-form
/// path request with a FRESH random tag. Built from a throwaway node so
/// the tag is new every call, exactly as `Transport.request_path` draws
/// one per call — which is why our tag dedup never saw a duplicate.
fn fresh_retry(dest: &[u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    let mut requester = make_relay();
    let pr_hash = *requester.transport().path_request_hash();
    let out = requester.request_path(&crate::DestinationHash::new(*dest));
    wire_out(&out)
        .into_iter()
        .map(|(_, data)| data)
        .find(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .expect("the requester emits a path request")
}

/// Feed `n` fresh retries one simulated second apart and return how many
/// path requests for `dest` the relay re-originated in total.
fn reoriginations_over_retries(
    relay: &mut EmbeddedNode,
    dest: &[u8; TRUNCATED_HASHBYTES],
    n: usize,
) -> usize {
    let mut total = 0;
    for _ in 0..n {
        let out = relay.handle_packet(InterfaceId(LORA), &fresh_retry(dest));
        total += path_requests_for(relay, &out, dest).len();
        relay.transport().clock().advance(1_000);
    }
    total
}

/// THE pin: while one discovery is pending, the requester's retries cost
/// one re-origination per relay, not one per retry.
#[test]
fn a_pending_discovery_re_originates_once_per_relay() {
    let dest = make_dest();
    let d = *dest.hash().as_bytes();

    for relay_name in ["base_t114", "pocket_v2"] {
        let mut relay = make_relay();
        assert!(
            !relay.has_path(&crate::DestinationHash::new(d)),
            "premise ({relay_name}): the relay holds no path to D"
        );

        let reoriginated = reoriginations_over_retries(&mut relay, &d, 6);
        assert_eq!(
            reoriginated, 1,
            "{relay_name}: six retries inside one pending window must cost \
             ONE re-origination (1.3.5 Transport.py:3015-3017, 1.5.2 :3541); \
             today every retry is re-keyed onto the uplink"
        );

        // Positive control for the counter: the five suppressed retries
        // are counted, and named, not silently dropped.
        assert_eq!(
            relay
                .transport()
                .stats()
                .path_request_pending_suppressions(),
            5,
            "{relay_name}: five of the six retries were suppressed"
        );
    }
}

/// The pending window is a window, not a mute: once it expires the next
/// retry re-originates once more, which is the reference's behaviour
/// (the entry is culled by timeout at 1.3.5 Transport.py:2967-2972 /
/// 1.5.2 :1005-1011, and the next request finds the table empty).
#[test]
fn the_next_retry_after_the_window_re_originates_again() {
    let dest = make_dest();
    let d = *dest.hash().as_bytes();
    let mut relay = make_relay();

    assert_eq!(
        reoriginations_over_retries(&mut relay, &d, 3),
        1,
        "the window opens with one re-origination"
    );

    // Past DISCOVERY_TIMEOUT_MS the maintenance pass culls the entry.
    relay.transport().clock().advance(DISCOVERY_TIMEOUT_MS + 1);
    let _ = relay.handle_timeout();

    assert_eq!(
        reoriginations_over_retries(&mut relay, &d, 3),
        1,
        "the window that expired opens a new one: one more re-origination, \
         not none and not three"
    );
}

/// A later requester on a DIFFERENT interface is batched onto the
/// pending discovery instead of re-originating one, and the answer
/// reaches it: 1.5.2 appends the interface to `requesting_interfaces`
/// (Transport.py:1871-1876) and answers every entry in the list
/// (`:2432-2456`). Our 1.3.5-shaped storage keeps one interface, so the
/// extra requesters ride a side table with the same semantics.
#[test]
fn every_requester_in_the_window_gets_the_answer() {
    let mut dest = make_dest();
    let d = *dest.hash().as_bytes();
    let mut relay = make_relay();
    let second_lora = add_iface(&mut relay, "lora_second", 2);
    relay.set_interface_mode(second_lora, InterfaceMode::Gateway);

    // Requester A on LORA opens the window.
    let out = relay.handle_packet(InterfaceId(LORA), &fresh_retry(&d));
    assert!(
        !path_requests_for(&relay, &out, &d).is_empty(),
        "A's request opens the discovery"
    );

    // Requester B on the second LoRa interface, inside the window.
    relay.transport().clock().advance(1_000);
    let out = relay.handle_packet(InterfaceId(second_lora), &fresh_retry(&d));
    assert!(
        path_requests_for(&relay, &out, &d).is_empty(),
        "B's request is batched onto A's pending discovery, not re-originated"
    );

    // The answer arrives on the uplink.
    relay.transport().clock().advance(1_000);
    let ts = relay.transport().clock().now_ms();
    let announce = dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap();
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).unwrap();
    let out = relay.handle_packet(InterfaceId(UPLINK), &buf[..len]);

    let answered = announces_for(&out, &d);
    assert!(
        answered.contains(&Some(InterfaceId(LORA))),
        "the first requester's interface is answered; answered={answered:?}"
    );
    assert!(
        answered.contains(&Some(InterfaceId(second_lora))),
        "the requester batched into the window is answered too — otherwise \
         the suppression costs B its answer; answered={answered:?}"
    );
}
