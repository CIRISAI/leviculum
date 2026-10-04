//! mvr: a transport node that overhears another pair's link claims it as
//! its own in the event log (#407).
//!
//! `lora_4node_contention_lnsd`, 2026-10-04T19-04-10Z (404's side
//! observation): gamma's client opened link `0fcd0878…` to delta's client.
//! Alpha and beta share the emulated medium, hold no endpoint of that link
//! and no link-table entry for it, and still logged `LINK_PROOF_RX
//! link=0fcd…` for the overheard LRPROOF and `PKT_LOCAL dst=0fcd…
//! matched=true` for each of the ten overheard link DATA packets. Their own
//! counters said the opposite: `lrproof_no_link=1` and
//! `link_data_no_link=10`, one per misleading line.
//!
//! Nothing moved on the wire or in a table: the proof and every DATA packet
//! died at the node layer under their named drop reasons. What was wrong is
//! the event: `transport.rs` handed every link-addressed DATA packet no
//! local destination claimed to the node layer with a hard-coded
//! `matched=true`, and `handle_link_proof` emitted the initiator's
//! `LINK_PROOF_RX` before asking whether it holds the link at all.
//!
//! Host model (sans-I/O, deterministic, < 5 s): one transport node, one
//! interface, a crafted LRPROOF and a crafted link DATA packet for an id the
//! node never saw. The payloads are opaque to every layer that takes part,
//! because no layer gets far enough to verify them.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::DestinationType;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_log_capture::with_captured_logs;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{InterfaceId, TickOutput};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The link id from the run, so a grep of this file finds the episode.
const FOREIGN_LINK: [u8; 16] = [
    0x0f, 0xcd, 0x08, 0x78, 0x9c, 0xd9, 0xf7, 0x4b, 0x68, 0x01, 0xf1, 0x2c, 0x71, 0xb2, 0xc7, 0x4f,
];

fn link_packet(packet_type: PacketType, context: PacketContext, payload_len: usize) -> Vec<u8> {
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Link,
            packet_type,
        },
        hops: 0,
        transport_id: None,
        destination_hash: FOREIGN_LINK,
        context,
        data: PacketData::Owned(std::vec![0x5a; payload_len]),
    };
    let mut buf = [0u8; crate::constants::MTU];
    let len = packet.pack(&mut buf).expect("pack link packet");
    buf[..len].to_vec()
}

fn assert_inert(node: &TransportNode, out: &TickOutput, what: &str) {
    assert!(
        out.actions.is_empty(),
        "{what}: a node holding no part of the link must not send anything"
    );
    assert!(
        !out.events.iter().any(|e| matches!(
            e,
            NodeEvent::PacketReceived { .. } | NodeEvent::LinkEstablished { .. }
        )),
        "{what}: nothing may be delivered or established for a foreign link"
    );
    assert_eq!(node.link_count(), 0, "{what}: no link may be installed");
    assert_eq!(
        node.transport().storage().link_entry_count(),
        0,
        "{what}: no link-table entry may be installed"
    );
    assert!(
        !node.transport().has_destination(&FOREIGN_LINK),
        "{what}: the foreign link id must not become a local destination"
    );
}

fn lines_naming<'a>(logs: &'a str, marker: &str) -> Vec<&'a str> {
    let id = crate::hex_fmt::HexShort(&FOREIGN_LINK);
    let id = std::format!("{id}");
    logs.lines()
        .filter(|l| l.contains(marker) && l.contains(id.as_str()))
        .collect()
}

#[test]
fn overheard_foreign_link_is_not_claimed() {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: TransportNode = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock,
        MemoryStorage::with_defaults(),
    );
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new("shared", 0)));
    node.set_interface_name(idx, String::from("shared"));

    let ((), logs) = with_captured_logs(|| {
        // The LRPROOF: 99 bytes on the wire in the run (signature, X25519
        // key, signalling bytes).
        let out = node.handle_packet(
            InterfaceId(idx),
            &link_packet(PacketType::Proof, PacketContext::Lrproof, 99),
        );
        assert_inert(&node, &out, "foreign LRPROOF");

        let out = node.handle_packet(
            InterfaceId(idx),
            &link_packet(PacketType::Data, PacketContext::None, 48),
        );
        assert_inert(&node, &out, "foreign link DATA");
    });

    // The counters were right in the run and must stay right.
    assert_eq!(node.transport().stats().drops_lrproof_no_link(), 1);
    assert_eq!(node.transport().stats().drops_link_data_no_link(), 1);

    let proof_rx = lines_naming(&logs, "LINK_PROOF_RX");
    assert!(
        proof_rx.is_empty(),
        "LINK_PROOF_RX is the initiator's establishment event; a node that \
         holds no such link must not emit it:\n{}",
        proof_rx.join("\n")
    );
    let local = lines_naming(&logs, "PKT_LOCAL");
    assert_eq!(
        local.len(),
        1,
        "the DATA packet's hand-off to the node layer stays observable:\n{logs}"
    );
    assert!(
        local[0].contains("matched=false"),
        "no local destination holds the link, so the hand-off must say \
         matched=false:\n{}",
        local[0]
    );
}
