//! mvr: a path request a board receives on a RADIO interface is dropped
//! instead of discovered (hardware cell `ble_lora_transport`, 2026-09-17).
//!
//! ## Field failure
//!
//! `ble_lora_transport` step 5 (`file_transfer`), RED in three consecutive
//! full hardware runs. The topology is
//!
//! ```text
//!     host (lnsd, hci0) --BLE-- pocket --LoRa-- t114
//!                               ble+lora        lora only
//! ```
//!
//! The step starts an `lncp -l` listener on the t114 daemon, which creates a
//! brand-new destination, and then gives the host 60 s (`rnpath -w 60`) to
//! resolve it. Two things can resolve it: the listener's announce propagating
//! across the LoRa hop, or the host's path request reaching the far end.
//!
//! The announce is slow by design. Every relayed announce goes through the RNS
//! announce cap (2 % of link capacity, `Transport::drain_announce_queues`); at
//! the cell's PHY (SF7/BW125/CR5, ~5.5 kbit/s) one 183-byte announce buys
//! ~13.4 s of holdoff, so a board emits at most ~4 relayed announces per
//! minute. With the neighbour's announces and its own retries already in the
//! queue, the new destination's announce sat there for 78 s (2026-09-16
//! 20:19:44 -> 20:21:02, cell GREEN, 48 s into the window) and 91 s
//! (2026-09-17 00:59:45 -> 01:01:16, cell RED, 61 s into the window). Same
//! mechanism, same firmware, either side of the 60 s line — which is why the
//! cell flipped colour without a line of code changing between the two runs.
//! The LoRa hop itself was clean: 25 frames transmitted by the t114 board, 25
//! received by the pocket board, 100 % delivery.
//!
//! The path request is what is supposed to make the queue depth irrelevant,
//! and it went nowhere. The pocket board received it over BLE — its capture
//! holds the one and only 51-byte BLE reception of the run,
//! `[INFO] BLE: RX 51B conn=1 frags=1 t=130949`, at the millisecond the host
//! logged `forwarding to network interfaces` — and emitted no action for it:
//! no `BLE RX -> n actions`, no serial TX, no LoRa TX, nothing.
//!
//! ## Mechanism
//!
//! `Transport::handle_path_request` case 3 re-originates discovery for an
//! unknown destination only when the RECEIVING interface's mode has
//! `discovers_paths()` — `AccessPoint | Gateway | Roaming`, mirroring Python
//! `Interface.DISCOVER_PATHS_FOR`. The firmware binaries set only interface 0
//! (serial) to `Gateway` (#117, so the board discovers on the attached host's
//! behalf); interfaces 1 (LoRa) and 2 (BLE) keep the `Full` default, and
//! `Full.discovers_paths()` is false. A board is therefore deaf to "where is
//! X" on exactly the two interfaces that make it a mesh relay, leaving the
//! capped, minutes-deep announce queue above as the only thing that can
//! resolve anything.
//!
//! `Gateway` differs from `Full` in nothing else: both answer `true` from
//! `announce_allowed_on_interface` (`transport.rs`, the
//! `Full | Gateway | PointToPoint` arm), so this is a discovery change and not
//! an announce-propagation change. Re-origination excludes the receiving
//! interface, so a LoRa-received request adds no LoRa airtime.
//!
//! ## What this test asserts
//!
//! One board, three interfaces indexed as the firmware indexes them, no radio
//! and no clock advance. `Full` on the ingress interface reproduces the field
//! silence; `Gateway` on it re-originates onto the other two. The third test
//! states the mode wiring the firmware binaries must keep.
//!
//! Sans-I/O: no LoRa, no Docker, no Python, sub-second wall clock.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::DestinationType;
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::InterfaceMode;
use crate::transport::{Action, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The board's interface indices, in the order the firmware binaries register
/// them (`leviculum-nrf/src/bin/{t114,rak4631,solarnode}.rs`).
const SERIAL: usize = 0;
const LORA: usize = 1;
const BLE: usize = 2;

const IFACES: [(usize, &str); 3] = [(SERIAL, "serial_usb"), (LORA, "lora_sx1262"), (BLE, "ble")];

/// A board wired the way the firmware wires one, with both radio interfaces in
/// `radio_mode`. Serial is always `Gateway` (#117).
fn make_board(radio_mode: InterfaceMode) -> Node {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    );
    for (idx, name) in IFACES {
        let registered = node
            .transport
            .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
        assert_eq!(registered, idx, "interface {name} must register as {idx}");
        node.set_interface_name(idx, String::from(name));
    }
    node.set_interface_mode(SERIAL, InterfaceMode::Gateway);
    node.set_interface_mode(LORA, radio_mode);
    node.set_interface_mode(BLE, radio_mode);
    node
}

/// A network path request (dest_hash + requester_transport_id + tag), the
/// 48-byte layout a peer that is itself a transport instance sends.
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
        destination_hash: *path_req_hash,
        context: PacketContext::None,
        data: PacketData::Owned(data),
    };
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

fn is_path_request_for(data: &[u8], dest: &DestinationHash) -> bool {
    match Packet::unpack(data) {
        Ok(p) => {
            p.flags.packet_type == PacketType::Data
                && p.data.as_slice().len() >= TRUNCATED_HASHBYTES
                && &p.data.as_slice()[..TRUNCATED_HASHBYTES] == dest.as_bytes()
        }
        Err(_) => false,
    }
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
                ..
            } => {
                let excluded = exclude_iface.map(|i| i.0) == Some(on_iface)
                    || exclude_ifaces.iter().any(|i| i.0 == on_iface);
                (!excluded).then(|| data.clone())
            }
        })
        .collect()
}

/// Which interfaces the discovery for an unknown destination went out on after
/// a path request arrived on `ingress`, in `IFACES` order.
fn reoriginated_onto(radio_mode: InterfaceMode, ingress: usize) -> [bool; 3] {
    let mut board = make_board(radio_mode);

    // A destination hash no node in the model hosts or has a path to — the
    // freshly created `lncp -l` listener of the field failure.
    let dest = DestinationHash::new(*Identity::generate(&mut OsRng).hash());
    assert_eq!(
        board.hops_to(&dest),
        None,
        "precondition: the board must not know this destination"
    );

    let path_req_hash = *board.transport().path_request_hash();
    let request = build_path_request(
        &path_req_hash,
        &dest,
        &[0x77u8; TRUNCATED_HASHBYTES],
        &[0x33u8; TRUNCATED_HASHBYTES],
    );

    let out = board.handle_packet(InterfaceId(ingress), &request);
    [SERIAL, LORA, BLE].map(|iface| {
        bound_for(&out, iface)
            .iter()
            .any(|pkt| is_path_request_for(pkt, &dest))
    })
}

/// The field failure, minimal: with the radio interfaces left at the `Full`
/// default, a path request the board takes off its BLE link is dropped.
///
/// The positive control for the fix below — it is the state the boards were
/// flashed in when `ble_lora_transport` went red, and it says the silence is
/// the interface mode and nothing else.
#[test]
fn full_mode_board_drops_a_ble_received_path_request() {
    let onto = reoriginated_onto(InterfaceMode::Full, BLE);

    assert_eq!(
        onto,
        [false, false, false],
        "a Full-mode ingress interface is expected to drop the request \
         entirely (onto serial/lora/ble = {onto:?}); if this now fires, the \
         discovery gate moved and the fix below no longer describes why"
    );
}

/// The fix: a path request the board takes off its BLE link must be
/// re-originated onto the mesh.
///
/// RED here means a BLE-attached host (the `ble_lora_transport` host, a
/// Columba phone) can never ask its board to resolve anything the board has
/// not already learned passively, and is left waiting on the announce-capped
/// propagation that took 78-91 s across one LoRa hop in the field.
#[test]
fn gateway_mode_board_reoriginates_a_ble_received_path_request() {
    let [onto_serial, onto_lora, onto_ble] = reoriginated_onto(InterfaceMode::Gateway, BLE);

    assert!(
        onto_lora,
        "the board dropped a BLE-received path request for an unknown \
         destination instead of re-originating it onto its LoRa link"
    );
    assert!(
        onto_serial,
        "the discovery must also reach the attached daemon over serial"
    );
    assert!(
        !onto_ble,
        "the discovery must not go back out on the interface it arrived on"
    );
}

/// The mesh-side mirror: a path request arriving over LoRa, for a destination
/// the board does not know, must reach the attached daemon and the BLE peer.
///
/// Costs no LoRa airtime — re-origination excludes the receiving interface —
/// so the board answers for the nodes behind it without adding to the band it
/// heard the question on.
#[test]
fn gateway_mode_board_reoriginates_a_lora_received_path_request() {
    let [onto_serial, onto_lora, onto_ble] = reoriginated_onto(InterfaceMode::Gateway, LORA);

    assert!(
        onto_serial && onto_ble,
        "the board dropped a LoRa-received path request for an unknown \
         destination (onto_serial={onto_serial}, onto_ble={onto_ble}) instead \
         of asking the nodes behind it"
    );
    assert!(
        !onto_lora,
        "the discovery must not go back out on the interface it arrived on"
    );
}

/// The mode wiring itself, stated as the contract the three firmware binaries
/// must keep: every interface a board relays on discovers paths.
///
/// Fails loudly if a `set_interface_mode` line is dropped back to the `Full`
/// default — the shape of the original defect, where only interface 0 was
/// ever set.
#[test]
fn every_board_interface_discovers_paths() {
    let board = make_board(InterfaceMode::Gateway);
    for (idx, name) in IFACES {
        assert!(
            board.transport().interface_mode(idx).discovers_paths(),
            "interface {idx} ({name}) does not discover paths: a path request \
             for an unknown destination arriving on it is dropped"
        );
    }
}
