//! mvr: what a board does with a relayed announce it has already demodulated
//! — and whether the daemon behind its serial link ever sees it.
//!
//! ## Field failure
//!
//! `lora_t114_lncp_bidir`, 2026-09-22 corpus, red on a failed path query.
//! Alpha's board demodulated the announce it needed twice and cleanly
//! (`[LORA] RX 183 bytes rssi=-39 snr=13 flags=0x51 dst=bb124c3b`), logged
//! the first copy as a new neighbour (`[ANNOUNCE] trigger
//! reason=new-neighbour`, which only fires from a `NodeEvent::
//! AnnounceReceived`, so the announce reached the core and updated the path
//! table) — and emitted no `T114_SERIAL_TX` for either copy. No drop bucket
//! moved: the daemon never received it and never discarded it. Sixty seconds
//! earlier the same board forwarded an identically shaped frame correctly.
//!
//! ## The board this models
//!
//! `leviculum-nrf/src/bin/t114.rs:248-290` — three interfaces, serial 0,
//! LoRa 1, BLE 2, all three `Gateway`, `enable_transport(true)`,
//! `max_queued_announces(8)`. The board registers NO interface as a
//! shared-instance local client, which is the fact this file is really
//! about: `Transport::handle_announce`'s "forward announce to local client
//! interfaces" block (`transport.rs`, `if self.has_local_clients()`) does
//! not run on a board, so the ONLY thing that can put a received announce
//! on the serial link is the rebroadcast scheduler
//! (`Transport::check_announce_rebroadcasts`) or a targeted discovery path
//! response. Whatever excludes an announce from the announce table also
//! excludes the daemon from ever hearing about it.
//!
//! ## What this file measures
//!
//! Three questions, each an announce fed in on LoRa and a count of what came
//! out on serial afterwards:
//!
//! 1. the positive control — a plain relayed announce must reach the host;
//! 2. the reviewer's first hypothesis — two announces whose first eight wire
//!    bytes are IDENTICAL (`[flags][hops][transport_id[0..6]]`, which is what
//!    the board's `pkt_hash8=` log field prints, and which collapses for
//!    every announce relayed by the same board at the same hop count) must
//!    both reach the host;
//! 3. the same announce carried as a `PATH_RESPONSE`, which is the one
//!    announce shape `handle_announce` deliberately keeps out of the announce
//!    table.
//!
//! Sans-I/O: no LoRa, no Docker, no Python, sub-second wall clock.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketContext, PacketType, TransportType};
use crate::test_utils::{MockClock, TEST_TIME_MS};
use crate::traits::{Clock, InterfaceMode};
use crate::transport::{Action, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The board's interface indices, in the order `bin/t114.rs` registers them.
const SERIAL: usize = 0;
const LORA: usize = 1;
const BLE: usize = 2;

const IFACES: [(usize, &str, u32); 3] = [
    (SERIAL, "serial_usb", 564),
    (LORA, "lora_sx1262", 508),
    (BLE, "ble", 564),
];

/// The announce-cap bitrate a live SF8/BW125/CR5 radio registers
/// (`leviculum-nrf/src/lora.rs`, `AnnounceCap::sync`). Only the LoRa
/// interface ever gets one, which is why the serial link is never throttled
/// and a missing serial TX can never be blamed on the cap.
const LORA_ANNOUNCE_CAP_BPS: u32 = 3_125;

/// A board wired the way `bin/t114.rs` wires one. Deliberately registers no
/// `Box<dyn Interface>` and no local client: the firmware registers neither,
/// and both absences are load-bearing for what this file measures.
fn make_board() -> Node {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new()
        .enable_transport(true)
        .max_queued_announces(8)
        .max_random_blobs(8)
        .build(OsRng, clock, MemoryStorage::with_defaults());
    for (idx, name, hw_mtu) in IFACES {
        node.set_interface_name(idx, String::from(name));
        node.set_interface_hw_mtu(idx, hw_mtu);
        node.set_interface_mode(idx, InterfaceMode::Gateway);
    }
    node.register_interface_bitrate(LORA, LORA_ANNOUNCE_CAP_BPS);
    // No `set_interface_local_client` anywhere: the firmware calls it on no
    // interface, so `handle_announce`'s local-client forward cannot run here
    // either. That absence is the point of this file, not an omission.
    node
}

/// A peer that owns one announce-able destination, standing in for the
/// daemon behind the far board.
fn make_origin(aspect: &'static str) -> (Node, DestinationHash) {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lnode",
        &[aspect],
    )
    .unwrap();
    let hash = *dest.hash();
    node.register_destination(dest);
    (node, hash)
}

/// The raw Type-1 announce a node emits for its own destination.
fn own_announce(node: &mut Node, dest: &DestinationHash) -> Vec<u8> {
    let out = node.announce_destination(dest, Some(b"mvr")).unwrap();
    let raw = out
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .next()
        .expect("announce must produce an outbound packet");
    assert_eq!(
        Packet::unpack(&raw).unwrap().flags.packet_type,
        PacketType::Announce
    );
    raw
}

/// Re-shape an announce the way a relay board puts it back on the air:
/// header Type 2, transport propagation, the relay's own transport id, wire
/// hops 1. This is exactly the frame alpha demodulated (`flags=0x51`).
fn relayed(raw: &[u8], transport_id: [u8; TRUNCATED_HASHBYTES], context: PacketContext) -> Vec<u8> {
    let mut packet = Packet::unpack(raw).unwrap();
    packet.flags.header_type = HeaderType::Type2;
    packet.flags.transport_type = TransportType::Transport;
    packet.transport_id = Some(transport_id);
    packet.hops = 1;
    packet.context = context;
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    assert_eq!(buf[0], 0x51, "the field frame's flags byte");
    buf[..len].to_vec()
}

/// The board's own `pkt_hash8=` log field: the frame's first eight wire
/// bytes, `[flags][hops][transport_id[0..6]]` for a Type-2 announce
/// (`leviculum-nrf/src/lora.rs`, `[T114_LORA_DELIVER]`;
/// `leviculum-nrf/src/usb.rs`, `[T114_SERIAL_TX]`).
fn pkt_hash8(raw: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&raw[..8]);
    out
}

/// Every packet an output puts on `on_iface`. A `Broadcast` reaches an
/// interface iff it is not excluded.
fn bound_for(output: &TickOutput, on_iface: usize) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface, data, .. } => (iface.0 == on_iface).then(|| data.clone()),
            Action::Broadcast {
                data,
                exclude_iface,
                exclude_ifaces,
            } => {
                let excluded = exclude_iface.map(|i| i.0) == Some(on_iface)
                    || exclude_ifaces.iter().any(|i| i.0 == on_iface);
                (!excluded).then(|| data.clone())
            }
        })
        .collect()
}

/// Drive the board's schedulers for `ms` in 100 ms steps, collecting
/// everything bound for the serial link. 100 ms is well under the announce
/// rebroadcast jitter window, so nothing due inside the span is stepped over.
fn serial_traffic_over(board: &mut Node, ms: u64) -> Vec<Vec<u8>> {
    let mut seen = Vec::new();
    let end = board.transport().clock().now_ms() + ms;
    while board.transport().clock().now_ms() < end {
        let now = board.transport().clock().now_ms();
        board.transport().clock().set(now + 100);
        let out = board.handle_timeout();
        seen.extend(bound_for(&out, SERIAL));
    }
    seen
}

/// How many of `packets` are announces for `dest`.
fn announces_for(packets: &[Vec<u8>], dest: &DestinationHash) -> usize {
    packets
        .iter()
        .filter(|p| match Packet::unpack(p) {
            Ok(pkt) => {
                pkt.flags.packet_type == PacketType::Announce
                    && pkt.destination_hash == *dest.as_bytes()
            }
            Err(_) => false,
        })
        .count()
}

/// How long the board is given to hand the announce up. Two full
/// `PATHFINDER_G` grace periods plus the jitter ceiling, so a scheduled
/// rebroadcast that fires at all has fired by the time this returns.
const UPLINK_BUDGET_MS: u64 = 30_000;

/// Positive control. A plain relayed announce arriving on LoRa must reach the
/// daemon behind the serial link. Without this, every count below is
/// unreadable.
#[test]
fn a_plain_relayed_announce_reaches_the_host() {
    let mut board = make_board();
    let (mut origin, dest) = make_origin("probe");
    let relay_id = [0xb2u8; TRUNCATED_HASHBYTES];
    let frame = relayed(
        &own_announce(&mut origin, &dest),
        relay_id,
        PacketContext::None,
    );

    let out = board.handle_packet(InterfaceId(LORA), &frame);
    let mut serial = bound_for(&out, SERIAL);
    assert!(
        board.hops_to(&dest).is_some(),
        "precondition: the board must have learned the path itself"
    );
    serial.extend(serial_traffic_over(&mut board, UPLINK_BUDGET_MS));

    assert!(
        announces_for(&serial, &dest) > 0,
        "a board that learned a path from an announce must pass the announce \
         on to its host; {} packets reached the serial link",
        serial.len()
    );
}

/// The reviewer's first hypothesis, tested rather than implemented: if the
/// forwarding path deduped on anything that collapses the way `pkt_hash8`
/// does, the SECOND announce relayed by the same board at the same hop count
/// would die. Two announces for different destinations, built so their first
/// eight wire bytes are byte-identical, 333 ms apart — the spacing measured
/// in the field trace.
#[test]
fn two_announces_with_the_same_first_eight_bytes_both_reach_the_host() {
    let mut board = make_board();
    let (mut first_origin, first) = make_origin("first");
    let (mut second_origin, second) = make_origin("second");
    let relay_id = [0xb2u8; TRUNCATED_HASHBYTES];

    let first_frame = relayed(
        &own_announce(&mut first_origin, &first),
        relay_id,
        PacketContext::None,
    );
    let second_frame = relayed(
        &own_announce(&mut second_origin, &second),
        relay_id,
        PacketContext::None,
    );
    assert_ne!(first, second, "two different destinations");
    assert_eq!(
        pkt_hash8(&first_frame),
        pkt_hash8(&second_frame),
        "the collapse the hypothesis is about: same relay, same hop count, \
         same first eight bytes"
    );

    let out = board.handle_packet(InterfaceId(LORA), &first_frame);
    let mut serial = bound_for(&out, SERIAL);
    // 333 ms, the spacing between t=129996 and t=130329 in the field trace.
    let now = board.transport().clock().now_ms();
    board.transport().clock().set(now + 333);
    let out = board.handle_packet(InterfaceId(LORA), &second_frame);
    serial.extend(bound_for(&out, SERIAL));
    serial.extend(serial_traffic_over(&mut board, UPLINK_BUDGET_MS));

    assert!(
        announces_for(&serial, &first) > 0,
        "the first of two announces sharing a pkt_hash8 did not reach the host"
    );
    assert!(
        announces_for(&serial, &second) > 0,
        "the second of two announces sharing a pkt_hash8 did not reach the \
         host: the forwarding path collapses on the first eight bytes"
    );
}

/// The one announce shape `handle_announce` keeps out of the announce table:
/// `PATH_RESPONSE` context (`transport.rs`, the `!is_path_response` term in
/// the insertion gate, Python Transport.py:1886). On a daemon the local-client
/// forward above delivers it anyway; on a board there is no local client, so
/// the announce-table gate is the whole decision.
///
/// The board still learns the path from it — which is what the field trace
/// shows, and what makes this a board that heard something and did not pass
/// it on.
#[test]
fn a_path_response_announce_is_learned_and_not_passed_on() {
    let mut board = make_board();
    let (mut origin, dest) = make_origin("probe");
    let relay_id = [0xb2u8; TRUNCATED_HASHBYTES];
    let frame = relayed(
        &own_announce(&mut origin, &dest),
        relay_id,
        PacketContext::PathResponse,
    );

    let out = board.handle_packet(InterfaceId(LORA), &frame);
    let mut serial = bound_for(&out, SERIAL);
    assert!(
        board.hops_to(&dest).is_some(),
        "the board learns the path from a PATH_RESPONSE announce"
    );
    serial.extend(serial_traffic_over(&mut board, UPLINK_BUDGET_MS));

    assert_eq!(
        announces_for(&serial, &dest),
        0,
        "measurement pin: a PATH_RESPONSE announce is learned and never \
         forwarded to the host. Change this number only with the mechanism \
         that changed it."
    );
}

/// The BLE side of the same question, because the Leitstern puts the same
/// board on a phone link: whatever reaches the serial host must reach a
/// linked phone too.
#[test]
fn a_plain_relayed_announce_also_reaches_the_ble_link() {
    let mut board = make_board();
    let (mut origin, dest) = make_origin("probe");
    let relay_id = [0xb2u8; TRUNCATED_HASHBYTES];
    let frame = relayed(
        &own_announce(&mut origin, &dest),
        relay_id,
        PacketContext::None,
    );

    let out = board.handle_packet(InterfaceId(LORA), &frame);
    let mut ble = bound_for(&out, BLE);
    let end = board.transport().clock().now_ms() + UPLINK_BUDGET_MS;
    while board.transport().clock().now_ms() < end {
        let now = board.transport().clock().now_ms();
        board.transport().clock().set(now + 100);
        let out = board.handle_timeout();
        ble.extend(bound_for(&out, BLE));
    }

    assert!(
        announces_for(&ble, &dest) > 0,
        "a relayed announce must reach a linked phone as well as the host"
    );
}

// ---------------------------------------------------------------------------
// The window, end to end: host asks, board re-originates, the answer comes
// back as a PATH_RESPONSE — early or late.
// ---------------------------------------------------------------------------

/// A network path request (dest_hash + requester_transport_id + tag), the
/// 48-byte layout the daemon behind the serial link sends.
fn build_path_request(
    path_req_hash: &[u8; TRUNCATED_HASHBYTES],
    dest: &DestinationHash,
    requester_id: &[u8; TRUNCATED_HASHBYTES],
    tag: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(48);
    data.extend_from_slice(dest.as_bytes());
    data.extend_from_slice(requester_id);
    data.extend_from_slice(tag);

    let packet = Packet {
        flags: crate::packet::PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Plain,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *path_req_hash,
        context: PacketContext::None,
        data: crate::packet::PacketData::Owned(data),
    };
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// One full round: the host asks the board for `dest` over serial, the board
/// re-originates on LoRa, and `answer_delay_ms` later the relay's
/// `PATH_RESPONSE` announce arrives on LoRa.
///
/// Returns `(board learned the path, announces that reached the host)`.
fn discovery_round(answer_delay_ms: u64) -> (bool, usize) {
    let mut board = make_board();
    let (mut origin, dest) = make_origin("probe");
    let relay_id = [0xb2u8; TRUNCATED_HASHBYTES];
    let answer = relayed(
        &own_announce(&mut origin, &dest),
        relay_id,
        PacketContext::PathResponse,
    );

    let path_req_hash = *board.transport().path_request_hash();
    let request = build_path_request(
        &path_req_hash,
        &dest,
        &[0x77u8; TRUNCATED_HASHBYTES],
        &[0x33u8; TRUNCATED_HASHBYTES],
    );
    let out = board.handle_packet(InterfaceId(SERIAL), &request);
    assert!(
        !bound_for(&out, LORA).is_empty(),
        "precondition: the board must re-originate the host's path request \
         onto its radio (Gateway mode, #117 / mvr_board_radio_pathresolve)"
    );

    // Nothing but time passes until the relay's answer arrives.
    let _ = serial_traffic_over(&mut board, answer_delay_ms);

    let out = board.handle_packet(InterfaceId(LORA), &answer);
    let mut serial = bound_for(&out, SERIAL);
    serial.extend(serial_traffic_over(&mut board, UPLINK_BUDGET_MS));
    (
        board.hops_to(&dest).is_some(),
        announces_for(&serial, &dest),
    )
}

/// Inside the window the board answers its host: the discovery entry
/// (`Storage::set_discovery_path_request`, keyed by the 16-byte destination
/// hash, `DISCOVERY_TIMEOUT_MS` = 30 s, consumed on first use at
/// `transport.rs` `send_discovery_path_response`) is what carries the answer
/// back onto the interface the question came in on.
#[test]
fn a_path_response_inside_the_discovery_window_reaches_the_host() {
    let (learned, delivered) = discovery_round(5_000);
    assert!(learned, "the board learns the path either way");
    assert!(
        delivered > 0,
        "inside the 30 s discovery window the board must answer its host"
    );
}

/// Past the window the same frame is learned and swallowed. This is the
/// field failure: a board that heard it, learned from it, and did not pass
/// it on — with no drop counter moving, because nothing dropped it. The
/// announce-table gate excluded it for being a PATH_RESPONSE
/// (`transport.rs`, the `!is_path_response` term), the local-client forward
/// does not exist on a board, and the discovery entry that was the third and
/// last route had expired.
///
/// At SF8 with the 2 % announce cap one relayed announce buys tens of
/// seconds of holdoff on the answering board, so an answer arriving past
/// 30 s is the normal case on a LoRa hop, not the pathological one.
#[test]
fn a_path_response_past_the_discovery_window_is_learned_and_swallowed() {
    let (learned, delivered) = discovery_round(35_000);
    assert!(
        learned,
        "the board still learns the path from the late answer"
    );
    assert_eq!(
        delivered, 0,
        "measurement pin: past DISCOVERY_TIMEOUT_MS the board keeps what it \
         learned to itself. Change this number only with the mechanism that \
         changed it."
    );
}

/// The question the 60 s scenario budget actually turns on: once the board
/// has swallowed the late answer, can the host still get the path by asking
/// again?
///
/// The board cached the raw announce on the way through
/// (`Transport::handle_announce`, `set_announce_cache`, which runs for a
/// PATH_RESPONSE like any other accepted announce), so
/// `handle_path_request`'s cached-answer arm is the recovery route. This
/// pins whether it is one.
#[test]
fn a_second_host_request_after_a_swallowed_answer_is_served_from_cache() {
    let mut board = make_board();
    let (mut origin, dest) = make_origin("probe");
    let relay_id = [0xb2u8; TRUNCATED_HASHBYTES];
    let answer = relayed(
        &own_announce(&mut origin, &dest),
        relay_id,
        PacketContext::PathResponse,
    );

    let path_req_hash = *board.transport().path_request_hash();
    let first = build_path_request(
        &path_req_hash,
        &dest,
        &[0x77u8; TRUNCATED_HASHBYTES],
        &[0x33u8; TRUNCATED_HASHBYTES],
    );
    let out = board.handle_packet(InterfaceId(SERIAL), &first);
    assert!(!bound_for(&out, LORA).is_empty(), "re-originated");

    // The answer arrives past the discovery window and is swallowed.
    let _ = serial_traffic_over(&mut board, 35_000);
    let out = board.handle_packet(InterfaceId(LORA), &answer);
    let mut serial = bound_for(&out, SERIAL);
    serial.extend(serial_traffic_over(&mut board, 2_000));
    assert_eq!(
        announces_for(&serial, &dest),
        0,
        "precondition: the late answer was swallowed"
    );

    // The host asks a second time, with a fresh tag as a retrying client does.
    let second = build_path_request(
        &path_req_hash,
        &dest,
        &[0x77u8; TRUNCATED_HASHBYTES],
        &[0x34u8; TRUNCATED_HASHBYTES],
    );
    let out = board.handle_packet(InterfaceId(SERIAL), &second);
    let mut serial = bound_for(&out, SERIAL);
    serial.extend(serial_traffic_over(&mut board, UPLINK_BUDGET_MS));

    assert!(
        announces_for(&serial, &dest) > 0,
        "a host that asks again must be served from the board's announce \
         cache; if this is 0 the swallowed answer is unrecoverable and the \
         host stays blind for as long as the destination stays quiet"
    );
}
