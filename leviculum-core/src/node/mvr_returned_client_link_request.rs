//! mvr: a daemon routes to its own returned client through the mesh (#374).
//!
//! ## The measured failure (periculum 370, orphan_link_initiator cells)
//!
//! charlie = lnsd (transport) + an lxmf helper over the shared-instance
//! IPC. The helper restarts (same identity, persisted) and announces
//! nothing. From charlie's daemon logs
//! (`orphan_link_initiator_rnsd_charlie_daemon_2026-09-27T20-00-18Z.log`):
//!
//! 1. The IPC drop culls the helper's hops=0 path — and with it the
//!    random-blob replay memory (`handle_interface_down` →
//!    `remove_paths_for_interface`).
//! 2. Two seconds later charlie's OWN rebroadcast of the helper's announce
//!    comes back off the radio via bravo and re-enters as
//!    `PATH_ADD … hops=2 iface=serial_0 reason="new_destination"`: the
//!    daemon now believes its own client's destination is two hops away.
//! 3. Every following link request arrives from bravo in final-hop Type1
//!    form (no transport id); with the path pointing at the radio,
//!    `for_local` (path hops == 0, Transport.py:1513) is false and all 14
//!    die as `overheard-transport-id`. 0 of 100 messages arrived.
//! 4. Path requests for the helper die in the requestor guard ("next hop
//!    is the requestor") — until #374 with no counter.
//!
//! ## Reference
//!
//! Python 1.3.5 has the same hole: the culled entry takes the random
//! blobs with it (Transport.py:784-787), the echo re-enters as an unknown
//! destination (Transport.py:1830-1832 `should_add = True`), and a Type1
//! link request without a table entry falls through `Transport.inbound`
//! unhandled. Only the client's next own announce heals it (hops
//! decrement at :1481-1482, then the ≤-hops displacement at :1768-1778) —
//! and the restarted helper never announces. Both fixes here are
//! wire-invisible deviations under the deviation rule (P1 delivery):
//! the daemon refuses its own stale emission as a path source, and hands
//! a link request for a client-registered destination to the connected
//! clients instead of the mesh.
//!
//! Sans-I/O: 1 node, 2-3 mock interfaces, deterministic, sub-second.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Storage;
use crate::transport::{Action, InterfaceId, PathEntry, TickOutput};

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The daemon: a transport node with one radio interface and one
/// local-client IPC interface (charlie's shape in the 370 cells).
fn make_daemon() -> (Node, usize, usize) {
    let mut node: Node = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let serial = node
        .transport
        .register_interface(Box::new(MockInterface::new("serial", 0)));
    node.set_interface_name(serial, String::from("serial_0"));
    let client = node
        .transport
        .register_interface(Box::new(MockInterface::new("client", 1)));
    node.set_interface_name(client, String::from("Local[rns/default]/0"));
    node.transport.set_local_client(client, true);
    (node, serial, client)
}

/// The helper's identity and destination, with announce packers.
struct Helper {
    dest_hash: crate::DestinationHash,
    dest: Destination,
}

fn make_helper() -> Helper {
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    Helper { dest_hash, dest }
}

impl Helper {
    /// A direct (wire hops 0) announce emitted at `ts` — what the helper
    /// sends over the IPC to register.
    fn direct_announce(&mut self, ts: u64) -> Vec<u8> {
        let ann = self.dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap();
        let mut buf = [0u8; MTU];
        let len = ann.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }
}

/// The SAME announce bytes one relay later: HEADER_2, wire hops 1, the
/// relay's identity as transport_id — bravo echoing charlie's own
/// rebroadcast back at it. Same random blob, same emission.
fn echoed_via(raw: &[u8], via: [u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    let mut p = Packet::unpack(raw).unwrap();
    p.flags.header_type = HeaderType::Type2;
    p.flags.transport_type = TransportType::Transport;
    p.hops = 1;
    p.transport_id = Some(via);
    let mut buf = [0u8; MTU];
    let len = p.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// A final-hop (Type1, no transport id) link request for `dest`, the form
/// bravo puts on the air for a destination it believes is one hop away.
fn final_hop_link_request(dest: &crate::DestinationHash) -> Vec<u8> {
    let lr = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::LinkRequest,
        },
        hops: 1,
        transport_id: None,
        destination_hash: *dest.as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![0xAB; 64]),
    };
    let mut buf = [0u8; MTU];
    let len = lr.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// Link requests for `dest` sent to `iface` in this output.
fn link_requests_to(out: &TickOutput, iface: usize, dest: &[u8; TRUNCATED_HASHBYTES]) -> usize {
    out.actions
        .iter()
        .filter(|a| {
            let (target, data) = match a {
                Action::SendPacket { iface, data, .. } => (Some(*iface), data),
                Action::Broadcast { data, .. } => (None, data),
            };
            target == Some(InterfaceId(iface))
                && Packet::unpack(data)
                    .map(|p| {
                        p.flags.packet_type == PacketType::LinkRequest
                            && &p.destination_hash == dest
                    })
                    .unwrap_or(false)
        })
        .count()
}

/// Walk the daemon into the 370 trap state up to the helper's restart:
/// registered, disconnected. Returns the registration announce bytes.
fn register_and_disconnect(node: &mut Node, client: usize, helper: &mut Helper) -> Vec<u8> {
    let registration = helper.direct_announce(TEST_TIME_MS);
    let _ = node.handle_packet(InterfaceId(client), &registration);
    let entry = node
        .transport
        .get_path_clone(helper.dest_hash.as_bytes())
        .expect("registration installs the local path");
    assert_eq!(entry.hops, 0, "a local client's destination is 0 hops away");

    let _ = node.handle_interface_down(InterfaceId(client));
    assert!(
        node.transport
            .get_path_clone(helper.dest_hash.as_bytes())
            .is_none(),
        "the IPC drop culls the client's path (the replay memory dies here)"
    );
    registration
}

/// THE pin, first half: the daemon's own announce echoed back off the
/// radio must not become a path for its client's destination. RED on
/// master: `PATH_ADD hops=2 iface=serial_0 reason="new_destination"`
/// (the 370 logs, 19:55:09.635).
#[test]
fn a_daemons_own_echo_installs_no_path_for_its_clients_destination() {
    let (mut node, serial, client) = make_daemon();
    let mut helper = make_helper();
    let bravo = *Identity::generate(&mut OsRng).hash();

    let registration = register_and_disconnect(&mut node, client, &mut helper);

    let _ = node.handle_packet(InterfaceId(serial), &echoed_via(&registration, bravo));
    assert!(
        node.transport
            .get_path_clone(helper.dest_hash.as_bytes())
            .is_none(),
        "the daemon's own rebroadcast, echoed back at hops=2, is stale news \
         about its own client and must not enter the path table"
    );
}

/// THE pin, second half: with the table empty and the client back (same
/// identity, no announce — the 370 cell's design), a link request from
/// the radio reaches the client. RED on master: it dies as
/// `overheard-transport-id` (14 of 15 in the 0/100 run), nothing reaches
/// the IPC.
#[test]
fn a_link_request_reaches_the_returned_client() {
    let (mut node, serial, client) = make_daemon();
    let mut helper = make_helper();
    let bravo = *Identity::generate(&mut OsRng).hash();

    let registration = register_and_disconnect(&mut node, client, &mut helper);
    let _ = node.handle_packet(InterfaceId(serial), &echoed_via(&registration, bravo));

    // The helper reconnects: a NEW interface index (the daemon-side
    // connection object died with the socket), marked local, silent.
    let client2 = node
        .transport
        .register_interface(Box::new(MockInterface::new("client2", 2)));
    node.set_interface_name(client2, String::from("Local[rns/default]/0"));
    node.transport.set_local_client(client2, true);

    let lr_raw = final_hop_link_request(&helper.dest_hash);
    let out = node.handle_packet(InterfaceId(serial), &lr_raw);

    assert_eq!(
        link_requests_to(&out, client2, helper.dest_hash.as_bytes()),
        1,
        "a link request for a destination a local client registered must be \
         handed to the returned client, not dropped as overheard"
    );
    assert_eq!(
        node.transport().stats().lr_local_client_redirects(),
        1,
        "the redirect is counted (PKT_DROP_SUMMARY lr_local_client_redirect)"
    );

    // The proof anchor: the link entry points at the client, 0 hops
    // remaining, received from the radio — the healthy for_local shape.
    let link_id = crate::link::Link::calculate_link_id(&lr_raw);
    let entry = node
        .transport()
        .storage()
        .get_link_entry(link_id.as_bytes())
        .expect("the redirected request anchors a link entry");
    assert_eq!(entry.next_hop_interface_index, client2);
    assert_eq!(entry.remaining_hops, 0);
}

/// CONTROL: a client that genuinely moved announces a NEWER emission from
/// the mesh; that path must install, and link requests must NOT be
/// redirected to the local clients — the moved client wins.
#[test]
fn a_moved_clients_newer_announce_still_wins() {
    let (mut node, serial, client) = make_daemon();
    let mut helper = make_helper();
    let bravo = *Identity::generate(&mut OsRng).hash();

    let _ = register_and_disconnect(&mut node, client, &mut helper);

    // 30 s later, elsewhere: a FRESH announce (new random blob, newer
    // emission) arrives relayed off the radio.
    let fresh = helper.direct_announce(TEST_TIME_MS + 30_000);
    let _ = node.handle_packet(InterfaceId(serial), &echoed_via(&fresh, bravo));
    let entry = node
        .transport
        .get_path_clone(helper.dest_hash.as_bytes())
        .expect("a genuinely newer emission installs the remote path");
    assert_eq!(entry.hops, 2, "the moved client is now 2 hops away");

    let client2 = node
        .transport
        .register_interface(Box::new(MockInterface::new("client2", 2)));
    node.set_interface_name(client2, String::from("Local[rns/default]/0"));
    node.transport.set_local_client(client2, true);

    let lr_raw = final_hop_link_request(&helper.dest_hash);
    let out = node.handle_packet(InterfaceId(serial), &lr_raw);
    assert_eq!(
        link_requests_to(&out, client2, helper.dest_hash.as_bytes()),
        0,
        "with a live remote path the redirect must stay out of the way"
    );
    assert_eq!(node.transport().stats().lr_local_client_redirects(), 0);
}

/// The other #374 counter: a path request refused because the next hop
/// toward the destination IS the requestor is a counted drop
/// (`next_hop_is_requestor`), not a silent debug line.
#[test]
fn a_refused_path_request_moves_the_requestor_counter() {
    let (mut node, serial, _client) = make_daemon();
    let requestor = *Identity::generate(&mut OsRng).hash();
    let dest = [0x37u8; TRUNCATED_HASHBYTES];

    node.transport.insert_path(
        dest,
        PathEntry {
            hops: 2,
            expires_ms: u64::MAX,
            interface_index: serial,
            random_blobs: Vec::new(),
            next_hop: Some(requestor),
            via_peer: None,
        },
    );

    // The 48-byte transport form: dest + requestor transport id + tag.
    let mut payload = Vec::with_capacity(3 * TRUNCATED_HASHBYTES);
    payload.extend_from_slice(&dest);
    payload.extend_from_slice(&requestor);
    payload.extend_from_slice(&[0x11u8; TRUNCATED_HASHBYTES]);
    let pr = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Plain,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *node.transport().path_request_hash(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; MTU];
    let len = pr.pack(&mut buf).unwrap();

    assert_eq!(node.transport().stats().drops_next_hop_is_requestor(), 0);
    let _ = node.handle_packet(InterfaceId(serial), &buf[..len]);
    assert_eq!(
        node.transport().stats().drops_next_hop_is_requestor(),
        1,
        "the requestor-guard refusal must be visible as a counter"
    );
}
