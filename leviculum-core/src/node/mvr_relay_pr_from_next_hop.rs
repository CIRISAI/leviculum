//! mvr: a relay whose next hop toward D IS the requestor swallows the
//! path request — no answer, no forward, no recovery (Codeberg #365,
//! walk 2 and walk 5).
//!
//! ## The field failure this reproduces
//!
//! Walk 2 (2026-09-04, ledger `365.md`): the Pocket, rebooted out of
//! BLE range, issued 20 path requests for the phone over LoRa, one per
//! minute, all received by the T114 (rx +24) — and the T114 forwarded
//! none (fwd 0) and answered none. The T114 held the phone's
//! destinations via the POCKET (learned from the Pocket's own LoRa
//! rebroadcast, next_hop = Pocket), so the requestor-is-next-hop guard
//! refused the answer — correctly, that route leads straight back
//! through the requestor — and then nothing else happened: the request
//! died at the relay. The cell stayed dark until the phone happened to
//! announce again (16:13). Walk 5 (2026-09-06/07, boards 28de836, no
//! reboot) matches the same signature: after the Pocket's BLE loss,
//! nothing over LoRa.
//!
//! The state is reachable without any reboot: the #365 cull itself
//! removes the relay's DIRECT entry when the phone's BLE link to the
//! relay flaps, Columba does not re-announce on reconnect (walk-2
//! finding, #255 comment), and the phone's next announce can reach the
//! relay only as the Pocket's LoRa rebroadcast — installed into the
//! void as next_hop = Pocket (`handle_announce`, new-destination arm).
//!
//! ## Reference semantics
//!
//! Python has the identical dead end, and knows it: the
//! requestor-is-next-hop branch (Transport.py:2958-2966) drops the
//! request with a TODO — "Doing path invalidation here would decrease
//! the network convergence time. Maybe just drop it?" — and the elif
//! chain means the discovery/forward branches are unreachable while any
//! path entry exists. Our `handle_path_request` mirrors that shape
//! (transport.rs, `next_hop_is_requestor` early return before cases
//! 2b/3). So this is a semantic gap shared with the reference; closing
//! it (e.g. treating the request as the invalidation signal upstream's
//! TODO asks for, then re-originating discovery) is a deviation-rule
//! decision — which is why this mvr pins the failure and no fix ships
//! with it.
//!
//! ## Shape
//!
//! 2 nodes (both the boards' exact storage type), deterministic,
//! sub-second. A (the Pocket) learns D direct on BLE, loses the peer,
//! culls, and emits the real 48-byte path request. R (the T114) holds D
//! via A. The request is piped into R; R must react on SOME carrier —
//! an answer or a re-originated request — and today it does neither.
//! The control pins the same harness green with the healthy relay state
//! (D held direct), so a red here is the mechanism, not the rig.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, PATH_REQUEST_GRACE_MS, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketType, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::{Action, InterfaceId, TickOutput};

/// Both boards' exact shape: `EmbeddedStorage`, transport enabled.
type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

fn make_node() -> Box<EmbeddedNode> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    )
}

fn add_iface(node: &mut EmbeddedNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// The phone: identity + destination + announce packers (same helper
/// shape as `mvr_ble_peer_loss_reroute`).
struct Peer {
    identity_hash: [u8; TRUNCATED_HASHBYTES],
    dest_hash: crate::DestinationHash,
    dest: Destination,
}

fn make_peer(app: &'static str) -> Peer {
    let identity = Identity::generate(&mut OsRng);
    let identity_hash = *identity.hash();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &[app],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    Peer {
        identity_hash,
        dest_hash,
        dest,
    }
}

impl Peer {
    /// A direct (wire hops 0) announce emitted at `ts`.
    fn direct_announce(&mut self, ts: u64) -> Vec<u8> {
        let ann = self.dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap();
        let mut buf = [0u8; MTU];
        let len = ann.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }

    /// The same announce one relay later: HEADER_2, wire hops 1, the
    /// relay's identity hash as `transport_id`.
    fn relayed_announce(&mut self, ts: u64, via: [u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
        let raw = self.direct_announce(ts);
        let mut p = Packet::unpack(&raw).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = 1;
        p.transport_id = Some(via);
        let mut buf = [0u8; MTU];
        let len = p.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }
}

/// Raw bytes of every action in this output, tagged with the send
/// target (`None` = broadcast).
fn wire_out(out: &TickOutput) -> Vec<(Option<InterfaceId>, Vec<u8>)> {
    out.actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { iface, data } => (Some(*iface), data.clone()),
            Action::Broadcast { data, .. } => (None, data.clone()),
        })
        .collect()
}

/// Announces naming `dest` in this output — the shape of a path answer.
fn announces_for(out: &TickOutput, dest: &[u8; TRUNCATED_HASHBYTES]) -> usize {
    wire_out(out)
        .iter()
        .filter(|(_, data)| {
            Packet::unpack(data)
                .map(|p| p.flags.packet_type == PacketType::Announce && &p.destination_hash == dest)
                .unwrap_or(false)
        })
        .count()
}

/// Path requests naming `dest` in this output — the shape of a
/// re-originated / forwarded discovery.
fn path_requests_for(
    out: &TickOutput,
    pr_hash: &[u8; TRUNCATED_HASHBYTES],
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> usize {
    wire_out(out)
        .iter()
        .filter(|(_, data)| {
            Packet::unpack(data)
                .map(|p| {
                    p.flags.packet_type == PacketType::Data
                        && &p.destination_hash == pr_hash
                        && p.data.as_slice().len() >= TRUNCATED_HASHBYTES
                        && &p.data.as_slice()[..TRUNCATED_HASHBYTES] == dest
                })
                .unwrap_or(false)
        })
        .count()
}

/// Walk the Pocket half of the chain for real: D learned direct on BLE,
/// the peer-lost cull, then the path request the board actually puts on
/// the wire. Returns the raw 48-byte-payload request packet.
fn pocket_emits_path_request(phone: &mut Peer) -> Vec<u8> {
    let mut pocket = make_node();
    let ble = add_iface(&mut pocket, "ble_nrf", 0);
    let _lora = add_iface(&mut pocket, "lora_sx1262", 1);
    let pocket_id = *pocket.identity().hash();
    let pr_hash = *pocket.transport().path_request_hash();

    let _ = pocket.handle_packet(InterfaceId(ble), &phone.direct_announce(TEST_TIME_MS));
    assert!(pocket.has_path(&phone.dest_hash), "direct path installed");
    let _ = pocket.handle_interface_peer_lost(InterfaceId(ble), phone.identity_hash);
    assert!(
        !pocket.has_path(&phone.dest_hash),
        "the cull must fire first — otherwise this is mechanism 1/2, not 3"
    );

    let out = pocket.request_path(&phone.dest_hash);
    let request = wire_out(&out)
        .into_iter()
        .map(|(_, data)| data)
        .find(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .expect("the culled Pocket must solicit the path");

    // The trap only arms on the 48-byte transport form naming the
    // Pocket as requestor (handle_path_request, requestor guard).
    let payload = Packet::unpack(&request).unwrap().data.as_slice().to_vec();
    assert_eq!(payload.len(), 3 * TRUNCATED_HASHBYTES, "transport form");
    assert_eq!(
        &payload[TRUNCATED_HASHBYTES..2 * TRUNCATED_HASHBYTES],
        &pocket_id,
        "requestor field is the Pocket's transport id"
    );
    request
}

/// Run the relay's scheduler dry BEFORE the request goes in. The
/// relayed announce received during setup arms an ordinary rebroadcast
/// in the announce_table; in compressed mvr time that rebroadcast would
/// fire inside the observation window and read as an "answer". In the
/// field it fired seconds after the announce, ~40 minutes before the
/// walk's BLE loss — setup traffic, not a reaction to the request.
fn drain_scheduler(relay: &mut EmbeddedNode) {
    for _ in 0..12 {
        let next = relay.transport().clock().now_ms() + 10_000;
        relay.transport().clock().set(next);
        if relay.handle_timeout().actions.is_empty() {
            break;
        }
    }
}

/// Everything the relay puts on the wire in response to the request:
/// the immediate actions plus the deferred grace-window answer
/// (announce_table entry fired by the scheduler).
fn relay_reaction(relay: &mut EmbeddedNode, lora: usize, request: &[u8]) -> Vec<TickOutput> {
    let immediate = relay.handle_packet(InterfaceId(lora), request);
    let fire_at = relay.transport().clock().now_ms() + PATH_REQUEST_GRACE_MS + 5_000;
    relay.transport().clock().set(fire_at);
    let deferred = relay.handle_timeout();
    std::vec![immediate, deferred]
}

/// Positive control: the healthy walk-5 start state. The relay holds D
/// DIRECT (its own BLE link to the phone), the request arrives over
/// LoRa from a third party — the relay answers with its cached announce
/// after the grace. Green today; pins that the harness would see an
/// answer if one were given.
#[test]
fn control_relay_with_direct_path_answers_the_request() {
    let mut phone = make_peer("prswallow");
    let request = pocket_emits_path_request(&mut phone);

    let mut relay = make_node();
    let ble = add_iface(&mut relay, "ble_nrf", 0);
    let lora = add_iface(&mut relay, "lora_sx1262", 1);
    let _ = relay.handle_packet(
        InterfaceId(ble),
        &phone.direct_announce(TEST_TIME_MS + 1_000),
    );
    let entry = relay
        .transport
        .get_path_clone(phone.dest_hash.as_bytes())
        .expect("relay holds D direct");
    assert_eq!(entry.next_hop, None, "direct: no next hop to match");

    drain_scheduler(&mut relay);
    let answered: usize = relay_reaction(&mut relay, lora, &request)
        .iter()
        .map(|out| announces_for(out, phone.dest_hash.as_bytes()))
        .sum();
    assert!(
        answered > 0,
        "a relay holding D direct must answer the path request"
    );
}

/// THE walk-2/walk-5 mechanism: the relay holds D via the requestor.
/// Refusing to ANSWER is correct — that route leads back through the
/// requestor — but the request, which is itself the signal that the
/// route via the requestor is dead, must not die silently at the only
/// node that could recover the cell: the relay must react on some
/// carrier (re-originate/forward the discovery toward its other
/// interfaces, or invalidate and answer once a real path exists).
/// Today it does neither, here and in the reference (Transport.py:2958
/// TODO), and the cell stays dark until the phone happens to announce.
#[test]
#[ignore = "Codeberg #365: relay drops a path request from its own next hop \
            (reference-identical, Transport.py:2958 TODO); fix direction \
            awaits the phone measurement — deliberately red until decided"]
fn relay_holding_the_requestor_as_next_hop_swallows_the_request() {
    let mut phone = make_peer("prswallow");

    // The Pocket half, walked for real; its identity is the transport
    // id inside the request AND the next hop the relay holds.
    let mut pocket = make_node();
    let p_ble = add_iface(&mut pocket, "ble_nrf", 0);
    let _ = add_iface(&mut pocket, "lora_sx1262", 1);
    let pocket_id = *pocket.identity().hash();
    let pr_hash = *pocket.transport().path_request_hash();
    let _ = pocket.handle_packet(InterfaceId(p_ble), &phone.direct_announce(TEST_TIME_MS));
    let _ = pocket.handle_interface_peer_lost(InterfaceId(p_ble), phone.identity_hash);
    let pocket_out = pocket.request_path(&phone.dest_hash);
    let request = wire_out(&pocket_out)
        .into_iter()
        .map(|(_, data)| data)
        .find(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .expect("the culled Pocket must solicit the path");

    // The relay's trap state: D via the Pocket (walk 2: rebooted relay
    // + Columba silent on reconnect; post-28de836 also reachable via
    // the relay's own cull on a BLE flap).
    let mut relay = make_node();
    let _ble = add_iface(&mut relay, "ble_nrf", 0);
    let lora = add_iface(&mut relay, "lora_sx1262", 1);
    let _ = relay.handle_packet(
        InterfaceId(lora),
        &phone.relayed_announce(TEST_TIME_MS, pocket_id),
    );
    let entry = relay
        .transport
        .get_path_clone(phone.dest_hash.as_bytes())
        .expect("relay holds D via the Pocket");
    assert_eq!(entry.next_hop, Some(pocket_id), "next hop IS the requestor");
    // Pin WHICH branch the silence comes from: with the cache missing,
    // case 2b would fall through toward discovery (#169 deviation) and
    // silence would mean something else. Cache present + path present +
    // requestor == next_hop leaves exactly the requestor guard.
    assert!(
        relay
            .transport
            .storage()
            .get_announce_cache(phone.dest_hash.as_bytes())
            .is_some(),
        "the relay holds the cached announce it could answer with"
    );

    drain_scheduler(&mut relay);
    let reactions = relay_reaction(&mut relay, lora, &request);
    let answers: usize = reactions
        .iter()
        .map(|out| announces_for(out, phone.dest_hash.as_bytes()))
        .sum();
    let forwards: usize = reactions
        .iter()
        .map(|out| path_requests_for(out, &pr_hash, phone.dest_hash.as_bytes()))
        .sum();
    assert!(
        answers + forwards > 0,
        "the relay swallowed the path request: no answer (correct — the \
         route leads back through the requestor) but also no forwarded or \
         re-originated discovery on any other carrier; the requestor can \
         never recover and the cell stays dark until the destination \
         happens to announce (walk 2: rx +24, fwd 0; walk 5: nothing \
         over LoRa after the BLE loss)"
    );
}
