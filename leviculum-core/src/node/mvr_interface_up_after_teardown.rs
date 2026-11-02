//! mvr (981, 924 follow-up): a stale interface-up notification applied after
//! the interface's teardown re-announces every local destination to an index
//! the core itself already tore down.
//!
//! ## The field record
//!
//! The three `no-such-interface` drops of the second miauhaus soak on
//! 5e1d965, same millisecond, 32.2 h into the run:
//!
//! ```text
//! PKT_DROP node=miauhaus dst=e2fe2806... hops=0 iface_in=None iface_out=223 len=167 reason=no-such-interface type=Announce t=115878166
//! PKT_DROP node=miauhaus dst=07d7d360... hops=0 iface_in=None iface_out=223 len=167 reason=no-such-interface type=Announce t=115878166
//! PKT_DROP node=miauhaus dst=cafa583f... hops=0 iface_in=None iface_out=223 len=193 reason=no-such-interface type=Announce t=115878166
//! ```
//!
//! Own announces (`hops=0`), one per local destination, all addressed to one
//! interface in one tick: the shape of `handle_interface_up`'s Block-D
//! re-announce (`announce_local_destinations`, one fresh announce per
//! daemon-owned destination as an immediate `SendPacket` on that interface
//! alone) — not of 924's grace-held path response, which answers one
//! destination per request. 924's cull cannot reach these: they are never
//! announce-table entries, they are actions emitted after the teardown.
//!
//! The driver race behind it (lnsd, `driver/mod.rs`): a reconnecting
//! auto-connect TCP client's task sends its reconnect notification
//! (`tcp_client_reconnect_loop`, `interfaces/tcp.rs`) in the same second the
//! auto-connect manager's 1 s poll sees the interface offline past
//! `DETACH_THRESHOLD_SECS` (12 s) and retires it — the reconnect backoff and
//! the detach threshold mature on the same timescale after a connection
//! death, so the collision is structurally favoured, not freak. The retire
//! runs `handle_interface_teardown` and removes the interface from the
//! registry; the buffered reconnect event is then drained and
//! `handle_interface_up` re-announces to the dead index, which
//! `dispatch_actions` drops as `no-such-interface`, one drop per local
//! destination, same millisecond. A retired auto-connect id is never reused
//! (it respawns under a fresh id), so the stale index can never be a
//! legitimate later interface.
//!
//! ## What this pins
//!
//! The core keeps a tombstone for every index it tore down
//! (`Transport::mark_interface_torn_down`), cleared when a driver registers
//! the index again (`set_interface_name`, which every registration path
//! calls before `handle_interface_up`). `handle_interface_up` on a
//! tombstoned index announces nothing and says so (`IFACE_UP_STALE`).
//!
//! Host model: one node, deterministic, sub-second, no driver.

extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use alloc::collections::BTreeMap;
use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::PacketType;
use crate::test_log_capture::with_captured_logs;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{dispatch_actions, Action, DropReason, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The retired auto-connect interface, the one the stale notification names.
const GONE: usize = 6;
/// The interface the driver still has.
const KEPT: usize = 0;

/// A transport node as miauhaus runs it, with three announced local
/// destinations (the soak's three same-millisecond drops, one per
/// destination).
fn host() -> (Node, Vec<DestinationHash>) {
    let mut host = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    host.set_interface_name(KEPT, String::from("kept"));
    host.set_interface_name(GONE, String::from("gone"));
    let mut hashes = Vec::new();
    for aspect in ["one", "two", "three"] {
        let dest = Destination::new(
            Some(Identity::generate(&mut OsRng)),
            Direction::In,
            DestinationType::Single,
            "mvr981",
            &[aspect],
        )
        .unwrap();
        let hash = *dest.hash();
        host.register_destination(dest);
        // Block D re-announces only destinations that have announced before
        // (announce cache present); announce once, drop the output.
        let _ = host.announce_destination(&hash, None).unwrap();
        hashes.push(hash);
    }
    // The setup's announces leave paced on the fresh interfaces (order
    // 1320); let them all go so the interface-up starts with empty queues.
    let _ = host.release_paced_announces();
    host.transport
        .clock()
        .advance(crate::transport::FRESH_ANNOUNCE_SPACING_MS);
    (host, hashes)
}

/// The announces `output` addresses to `iface`.
fn announces_to(output: &TickOutput, iface: usize) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface: i, data, .. } if i.0 == iface => Some(data.clone()),
            _ => None,
        })
        .collect()
}

/// Dispatch `output` into a driver slice that has only `KEPT`, as lnsd's
/// registry does once `GONE` was removed.
fn dispatch_without_gone(output: TickOutput) -> (Vec<(InterfaceId, DropReason)>, String) {
    let mut kept = MockInterface::new("kept", KEPT as u8);
    with_captured_logs(|| {
        let mut slice: Vec<&mut dyn crate::traits::Interface> = vec![&mut kept];
        dispatch_actions(&mut slice, output.actions, &BTreeMap::new()).drops
    })
}

/// THE pin (981): an interface-up for an index the core tore down announces
/// nothing. Before the fix the output carried one fresh own announce per
/// local destination addressed to `GONE`, and the dispatch dropped every one
/// as `no-such-interface` in the same tick — the soak's three drops.
#[test]
fn an_interface_up_after_the_teardown_announces_nothing() {
    let (mut host, _) = host();
    let _ = host.handle_interface_down(InterfaceId(GONE));

    // The stale buffered reconnect notification, applied after the retire.
    let (out, up_logs) = with_captured_logs(|| host.handle_interface_up(GONE));

    let stale = announces_to(&out, GONE);
    let (drops, logs) = dispatch_without_gone(out);
    assert!(
        drops.is_empty(),
        "no dead send: {drops:?} ({} announces to {GONE})\n--- logs ---\n{logs}",
        stale.len()
    );
    assert!(
        up_logs.contains("IFACE_UP_STALE"),
        "the refusal says so\n--- logs ---\n{up_logs}"
    );
}

/// Positive control: without the teardown the same call is the healthy
/// Block-D re-announce — one fresh own announce per destination, all
/// addressed to the recovered interface alone, the field record's shape.
#[test]
fn control_a_live_interface_up_reannounces_every_destination() {
    let (mut host, hashes) = host();
    let mut out = host.handle_interface_up(GONE);
    // The first at once, the others a spacing apart (order 1320).
    out.actions.extend(host.release_paced_announces());
    let announces = announces_to(&out, GONE);
    assert_eq!(
        announces.len(),
        hashes.len(),
        "one announce per announced destination: {:?}",
        out.actions
    );
    for raw in &announces {
        let packet = crate::packet::Packet::unpack(raw).unwrap();
        assert_eq!(packet.flags.packet_type, PacketType::Announce);
        assert_eq!(packet.hops, 0, "the node's own announce");
    }
    assert!(
        announces_to(&out, KEPT).is_empty(),
        "targeted at the recovered interface alone"
    );
}

/// Positive control: a re-registered index (a bootstrap re-attach returns
/// under its original index; every registration path names the interface
/// before `handle_interface_up`) announces again — the tombstone clears.
#[test]
fn control_a_reregistered_index_reannounces() {
    let (mut host, hashes) = host();
    let _ = host.handle_interface_down(InterfaceId(GONE));
    host.set_interface_name(GONE, String::from("gone-again"));
    let mut out = host.handle_interface_up(GONE);
    out.actions.extend(host.release_paced_announces());
    assert_eq!(
        announces_to(&out, GONE).len(),
        hashes.len(),
        "re-registration restores the Block-D re-announce: {:?}",
        out.actions
    );
}
