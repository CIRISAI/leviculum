//! mvr: a shared-instance client forgets a peer it used five minutes ago, the
//! instant that peer's path goes away (Codeberg #389, reference finding from
//! periculum 387).
//!
//! ## The defect
//!
//! Our recall source is the announce cache: `Transport::recall_identity_hash`,
//! `NodeCore::recall_app_data` and all five `destination_data` /
//! `identity_data` RPC ops read it and report "unknown" for anything not in
//! it. `Storage::clean_announce_cache` kept an entry on exactly three grounds
//! — it has a path, it is one of ours, or an application pinned it with the
//! retain sentinel. A recency stamp bought it nothing, even though we record
//! one: `Transport::used_destination_data` writes it for the `destination_data
//! used` RPC, and the LXMF router writes it for every propagation node it
//! selects or fetches from (`leviculum-lxmf/src/router/propagation_runtime.rs`).
//!
//! So a peer this node had used minutes before lost its cached announce on the
//! first sweep after its path expired. A shared-instance client — `lxmf-node`,
//! `lnmsg`, `lnomad` — feels that first, for two reasons: it holds no path of
//! its own beyond what the daemon installs, so a daemon reconnect or a peer
//! loss drops the lot; and our sweep runs from `Transport::poll` on every
//! tick, not on a 300 s jobs cadence, so "the path is momentarily gone" and
//! "the sweep instant" are the same instant here.
//!
//! ## The reference contract
//!
//! `Identity.clean_known_destinations`
//! (`reference/Reticulum/RNS/Identity.py:310-354`) spares a pathless entry on
//! three grounds, not one: retained (`[4] == -1`, Identity.py:344-346), used
//! within `DESTINATION_TIMEOUT * 1.25` — 8.75 days, Identity.py:350-352 — and
//! never used but announced within `UNUSED_DESTINATION_LINGER`, 6 minutes,
//! Identity.py:349. Only the middle arm is what a *used* peer needs, and it is
//! the one we did not have.
//!
//! What periculum 387 measured on the Python side is a different failure with
//! the same shape: an RNS 1.5.2 client RPCs its `used` mark to the DAEMON's
//! table (`Reticulum.py:1338-1350`) and never writes its own, so its own slot
//! stays `0` and its own 300 s sweep judges the entry never-used and drops it
//! six minutes after the last announce. Its helper lost charlie 299 s in,
//! having used charlie 82 times. We do not have that half of the bug — our
//! client marks its own table — which is precisely why the missing middle arm
//! was the whole cost here.
//!
//! ## What this file pins
//!
//! A client-shaped node (`enable_transport(false)`, the shape
//! `ReticulumNodeBuilder::connect_to_shared_instance` is documented to be used
//! with) learns a peer from a real announce, uses it, then loses the path.
//!
//! 1. it must still recall the peer five minutes later (red before #389);
//! 2. a peer it never used and has no path to is still swept (negative
//!    control — the fix must not turn the sweep off);
//! 3. past the used linger the used peer goes too (the arm is a linger, not a
//!    second retain pin).
//!
//! Sans-I/O: no daemon, no Docker, no Python, sub-second wall clock.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{KNOWN_DEST_USED_LINGER_MS, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationHash, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::{Action, InterfaceId, TickOutput};

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The one interface a shared-instance client has: the socket to its daemon.
const DAEMON: usize = 0;

/// "Five minutes ago", the span periculum 387 named.
const FIVE_MINUTES_MS: u64 = 5 * 60 * 1_000;

/// A client: no transport, one interface, and nothing of its own announced.
fn make_client() -> Node {
    let mut node = NodeCoreBuilder::new().enable_transport(false).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    node.transport
        .register_interface(std::boxed::Box::new(MockInterface::new("daemon", 0)));
    node.set_interface_name(DAEMON, String::from("daemon"));
    node
}

/// A peer that owns one announce-able destination.
fn make_peer(aspect: &'static str) -> (Node, DestinationHash) {
    let mut node = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &[aspect],
    )
    .unwrap();
    let hash = *dest.hash();
    node.register_destination(dest);
    (node, hash)
}

/// The raw announce a peer emits for its own destination, `app_data` and all.
fn own_announce(node: &mut Node, dest: &DestinationHash, app_data: &[u8]) -> Vec<u8> {
    let out = node.announce_destination(dest, Some(app_data)).unwrap();
    first_packet(&out)
}

fn first_packet(output: &TickOutput) -> Vec<u8> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .next()
        .expect("expected an outbound packet")
}

/// Everything the client is asked about a peer by the code paths that read the
/// announce cache: the recall, the app data, and whether the known-destination
/// ops still find it. All three answer from the same map, so all three flip
/// together — which is why one helper reports them as one answer.
fn still_known(client: &mut Node, dest: &DestinationHash) -> bool {
    let cached = client
        .transport
        .storage()
        .get_announce_cache(dest.as_bytes())
        .is_some();
    let app_data = client.recall_app_data(dest).is_some();
    assert_eq!(
        cached, app_data,
        "recall_app_data and the announce cache must agree; they are one map"
    );
    cached
}

/// Advance the client's clock and let its periodic work run, which is what
/// puts `clean_path_states` -> `clean_announce_cache` over the cache.
fn tick_at(client: &mut Node, at_ms: u64) {
    client.transport().clock().set(at_ms);
    let _ = client.handle_timeout();
}

/// The failure periculum 387 pointed at, on our side of the fence.
///
/// The client learns a peer, uses it, and then loses the path — the peer went
/// quiet, or the daemon link flapped and took its path table with it. Five
/// minutes later the peer must still be a peer.
#[test]
fn a_peer_used_five_minutes_ago_survives_losing_its_path() {
    let mut client = make_client();
    let (mut peer, peer_hash) = make_peer("used");
    let _ = client.handle_packet(
        InterfaceId(DAEMON),
        &own_announce(&mut peer, &peer_hash, b"propagation-node"),
    );

    // Precondition: the announce landed, path and all.
    assert!(
        client.hops_to(&peer_hash).is_some(),
        "precondition: the client must have learned the path from the announce"
    );
    assert!(
        still_known(&mut client, &peer_hash),
        "precondition: the client must know the peer it just heard"
    );

    // Use it, the way the LXMF router marks the propagation node it picked
    // and the way the daemon's `destination_data used` RPC marks a recall.
    let used_at = client.transport().clock().now_ms();
    assert!(
        client.used_destination_data(peer_hash.as_bytes()),
        "a known, unpinned destination takes the use mark"
    );

    // The path goes away — the peer stopped answering, or the client's link
    // to its daemon flapped and every path it had installed went with it.
    assert!(
        client.remove_path(peer_hash.as_bytes()),
        "the path was there"
    );

    tick_at(&mut client, used_at + FIVE_MINUTES_MS);

    assert!(
        still_known(&mut client, &peer_hash),
        "a peer used {FIVE_MINUTES_MS} ms ago must survive its path going away: \
         the reference keeps a used entry for DESTINATION_TIMEOUT * 1.25 \
         (Identity.py:350-352), and the recall, the app data and every \
         destination_data op read this one map"
    );
    assert!(
        client.used_destination_data(peer_hash.as_bytes()),
        "and it must still take a use mark — an op on a forgotten destination \
         reports false and silently does nothing"
    );
}

/// Negative control. The fix must not stop the sweep: a destination the client
/// heard once, never used and has no path to is still swept, so the cache is
/// still bounded by more than its capacity.
#[test]
fn a_peer_never_used_is_still_swept_when_its_path_goes() {
    let mut client = make_client();
    let (mut peer, peer_hash) = make_peer("never");
    let _ = client.handle_packet(
        InterfaceId(DAEMON),
        &own_announce(&mut peer, &peer_hash, b"stranger"),
    );
    assert!(
        still_known(&mut client, &peer_hash),
        "precondition: the client must know the peer it just heard"
    );

    let now = client.transport().clock().now_ms();
    assert!(
        client.remove_path(peer_hash.as_bytes()),
        "the path was there"
    );
    tick_at(&mut client, now + FIVE_MINUTES_MS);

    assert!(
        !still_known(&mut client, &peer_hash),
        "never used and no path: nothing holds this entry"
    );
}

/// The used arm is a linger, not a second retain pin: past
/// `KNOWN_DEST_USED_LINGER_MS` the entry goes, exactly as Python's
/// `unused_for > DESTINATION_TIMEOUT * 1.25` decides (Identity.py:352).
#[test]
fn a_used_peer_is_swept_once_the_linger_runs_out() {
    let mut client = make_client();
    let (mut peer, peer_hash) = make_peer("aged");
    let _ = client.handle_packet(
        InterfaceId(DAEMON),
        &own_announce(&mut peer, &peer_hash, b"propagation-node"),
    );
    let used_at = client.transport().clock().now_ms();
    assert!(client.used_destination_data(peer_hash.as_bytes()));
    assert!(
        client.remove_path(peer_hash.as_bytes()),
        "the path was there"
    );

    // One tick inside the linger, one past it. The first is what makes the
    // second readable: without it a green here could mean the entry had
    // already gone for some other reason.
    tick_at(&mut client, used_at + KNOWN_DEST_USED_LINGER_MS);
    assert!(
        still_known(&mut client, &peer_hash),
        "at the linger boundary the entry is still kept"
    );
    tick_at(&mut client, used_at + KNOWN_DEST_USED_LINGER_MS + 1);
    assert!(
        !still_known(&mut client, &peer_hash),
        "past the linger a used, pathless, unpinned entry is swept"
    );
}

/// The identity itself was never the thing at risk, and saying so is what
/// keeps the report honest: `known_identities` is a separate table
/// (`MemoryStorage::known_identities`) that no sweep touches, so encrypting to
/// a swept peer still works. What the sweep took was the *recall* — the app
/// data, the identity-hash lookup, and every known-destination op — which is
/// what a propagation node reads to decide whether a peer syncing into it is a
/// node at all (Codeberg #417).
#[test]
fn a_swept_peer_keeps_its_public_key_but_loses_its_recall() {
    let mut client = make_client();
    let (mut peer, peer_hash) = make_peer("keys");
    let _ = client.handle_packet(
        InterfaceId(DAEMON),
        &own_announce(&mut peer, &peer_hash, b"stranger"),
    );

    let now = client.transport().clock().now_ms();
    assert!(
        client.remove_path(peer_hash.as_bytes()),
        "the path was there"
    );
    tick_at(&mut client, now + FIVE_MINUTES_MS);

    assert!(
        !still_known(&mut client, &peer_hash),
        "never used and no path: the recall goes"
    );
    let key: Option<[u8; TRUNCATED_HASHBYTES]> = client
        .transport
        .storage()
        .get_identity(peer_hash.as_bytes())
        .map(|_| *peer_hash.as_bytes());
    assert_eq!(
        key,
        Some(*peer_hash.as_bytes()),
        "the public key survives the sweep — the loss is the recall, not the crypto"
    );
}
