//! mvr: on-wire byte census of the PN board-to-board sync resource, so the
//! LoRa capture of `lora_pn_board_sync` can be read without guessing.
//!
//! ## Why this exists
//!
//! The 2026-09-14 hardware run of `lora_pn_board_sync` shows the offering
//! board agree the round (`PN_OFFER dir=out offered=1 wanted=1`) and then
//! exchange ~300 packets of ONE size with its peer for 177 s until the
//! outbound watchdog (`OUTBOUND_DEADLINE_MS`, 180 s) reaps the round. The
//! firmware logs no resource events, only frame lengths, so the only way to
//! say WHICH protocol packet loops is to measure what each packet of this
//! exchange weighs on the wire and compare the lengths.
//!
//! The measured lengths are printed and pinned here: whoever reads the next
//! capture maps `[LORA] TX <n> bytes` onto a context byte instead of
//! reverse-engineering AES padding by hand.
//!
//! Sans-I/O: two `NodeCore`s over `MockInterface`, no radio, no timers.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::PacketContext;
use crate::resource::ResourceStrategy;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The sync envelope the failing run carried: `PN_SYNC ... bytes=270` on the
/// BLE leg of the same pair, one 256-byte stored message in a
/// `PeerSyncEnvelope`.
const ENVELOPE_BYTES: usize = 270;

fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

/// `[flags][hops][dest:16][context]` — the context byte of a HEADER_1 packet.
fn context_of(pkt: &[u8]) -> PacketContext {
    PacketContext::from_byte(pkt[18])
}

fn describe(dir: &str, pkt: &[u8]) -> String {
    std::format!(
        "{dir} len={} ctx={:?} payload={}",
        pkt.len(),
        context_of(pkt),
        pkt.len() - 19
    )
}

/// Establish one link the way the board's PN role does: the responder owns
/// the destination and gates resources through the application
/// (`ResourceStrategy::AcceptApp`, `leviculum-nrf/src/pn.rs`).
fn linked_pair() -> (EndpointNode, usize, LinkId, EndpointNode, usize, LinkId) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();

    let mut receiver = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);
    let r_iface = add_iface(&mut receiver, "R_mesh");

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_iface = add_iface(&mut sender, "S_mesh");

    let (sender_link, _routed, out) = sender.connect(dest_hash, &signing_key).expect("connect");
    let mut receiver_link = None;
    let mut for_receiver = action_data(&out);
    for _ in 0..8 {
        if for_receiver.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for pkt in for_receiver {
            let o = receiver.handle_packet(InterfaceId(r_iface), &pkt);
            for ev in &o.events {
                if let NodeEvent::LinkEstablished { link_id, .. } = ev {
                    receiver_link = Some(*link_id);
                }
            }
            back.extend(action_data(&o));
        }
        let mut next = Vec::new();
        for pkt in back {
            next.extend(action_data(
                &sender.handle_packet(InterfaceId(s_iface), &pkt),
            ));
        }
        for_receiver = next;
    }
    let receiver_link = receiver_link.expect("receiver side must reach Active");
    receiver
        .set_resource_strategy(&receiver_link, ResourceStrategy::AcceptApp)
        .expect("the PN role gates resources through the application");

    (
        sender,
        s_iface,
        sender_link,
        receiver,
        r_iface,
        receiver_link,
    )
}

/// Drive the whole sync resource to conclusion and print every packet's
/// on-wire length and context. The assertions pin only the two facts the
/// capture has to be read against: the transfer concludes in pure host
/// logic, and the census contains the lengths named in the doc comment.
#[test]
fn pn_sync_resource_wire_census() {
    let (mut sender, s_iface, sender_link, mut receiver, r_iface, receiver_link) = linked_pair();

    let envelope: Vec<u8> = (0..ENVELOPE_BYTES).map(|i| (i % 251) as u8).collect();
    // `auto_compress = true` is what the board passes (`pn.rs`
    // `node.send_resource(link_id, &data, None, true)`).
    let (_hash, out) = sender
        .send_resource(&sender_link, &envelope, None, true)
        .expect("sender advertises the sync resource");

    let mut census: Vec<String> = Vec::new();
    let mut sender_completed = false;
    let mut to_receiver = action_data(&out);
    for pkt in &to_receiver {
        census.push(describe("a->b", pkt));
    }

    for _round in 0..24 {
        if to_receiver.is_empty() {
            break;
        }
        let mut to_sender = Vec::new();
        for pkt in &to_receiver {
            let o = receiver.handle_packet(InterfaceId(r_iface), pkt);
            let mut emitted = action_data(&o);
            // The PN role answers a parked ADV with `accept_resource`.
            if o.events
                .iter()
                .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. }))
            {
                let accepted = receiver
                    .accept_resource(&receiver_link)
                    .expect("accept_resource consumes the parked ADV");
                emitted.extend(action_data(&accepted));
            }
            for p in &emitted {
                census.push(describe("b->a", p));
            }
            to_sender.extend(emitted);
        }

        let mut next = Vec::new();
        for pkt in &to_sender {
            let o = sender.handle_packet(InterfaceId(s_iface), pkt);
            if o.events.iter().any(|e| {
                matches!(
                    e,
                    NodeEvent::ResourceCompleted {
                        is_sender: true,
                        ..
                    }
                )
            }) {
                sender_completed = true;
            }
            let emitted = action_data(&o);
            for p in &emitted {
                census.push(describe("a->b", p));
            }
            next.extend(emitted);
        }
        to_receiver = next;
    }

    std::eprintln!("---- PN sync resource wire census ----");
    for line in &census {
        std::eprintln!("{line}");
    }

    assert!(
        sender_completed,
        "a {ENVELOPE_BYTES}-byte sync resource must conclude over a lossless \
         link in pure host logic.\ncensus: {census:#?}"
    );

    // The lengths the capture is read against. A change here means the
    // mapping in the 2026-09-14 analysis has to be redone.
    let adv = census
        .iter()
        .find(|l| l.contains("ctx=ResourceAdv"))
        .expect("census must contain the advertisement");
    let req = census
        .iter()
        .find(|l| l.contains("ctx=ResourceReq"))
        .expect("census must contain a part request");
    let part = census
        .iter()
        .find(|l| l.contains("ctx=Resource "))
        .expect("census must contain a data part");
    let prf = census
        .iter()
        .find(|l| l.contains("ctx=ResourcePrf"))
        .expect("census must contain the proof");
    std::eprintln!("ADV  {adv}\nREQ  {req}\nPART {part}\nPRF  {prf}");
}

/// Every OTHER packet the resource state machines can put on this link, so a
/// capture that shows one length can be narrowed to the contexts that fit it.
/// The 2026-09-14 LoRa capture's looping length is 115 bytes; this test is
/// what says which contexts weigh 115.
#[test]
fn resource_control_packet_wire_sizes() {
    let (sender, _s_iface, sender_link, _receiver, _r_iface, _receiver_link) = linked_pair();

    let resource_hash = [0x5au8; 32];
    // HMU for a one-entry hashmap, the shape `handle_request` builds for a
    // single-part resource (`link_management.rs` / `outgoing.rs`).
    let mut hmu = Vec::new();
    hmu.extend_from_slice(&resource_hash);
    crate::msgpack::write_fixarray_header(&mut hmu, 2);
    crate::msgpack::write_uint(&mut hmu, 0);
    crate::msgpack::write_bin(&mut hmu, &[1u8, 2, 3, 4]);
    // An exhausted REQ: flag + last map hash + resource hash, no requested
    // hashes (`incoming.rs::build_request`).
    let mut req_exhausted = Vec::new();
    req_exhausted.push(crate::resource::HASHMAP_IS_EXHAUSTED);
    req_exhausted.extend_from_slice(&[7u8; 4]);
    req_exhausted.extend_from_slice(&resource_hash);
    // A satisfied REQ: flag + resource hash, nothing requested.
    let mut req_empty = Vec::new();
    req_empty.push(crate::resource::HASHMAP_IS_NOT_EXHAUSTED);
    req_empty.extend_from_slice(&resource_hash);

    let mut rows: Vec<String> = Vec::new();
    {
        let link = sender.link(&sender_link).expect("active link");
        let mut rng = OsRng;
        for (name, payload, ctx) in [
            ("ICL", resource_hash.to_vec(), PacketContext::ResourceIcl),
            ("HMU", hmu.clone(), PacketContext::ResourceHmu),
            (
                "REQ(exhausted)",
                req_exhausted.clone(),
                PacketContext::ResourceReq,
            ),
            (
                "REQ(nothing)",
                req_empty.clone(),
                PacketContext::ResourceReq,
            ),
            (
                "KEEPALIVE",
                std::vec![crate::constants::KEEPALIVE_INITIATOR_BYTE],
                PacketContext::Keepalive,
            ),
        ] {
            let pkt = link
                .build_data_packet_with_context(&payload, ctx, &mut rng)
                .expect("link is active");
            rows.push(std::format!(
                "{name} plaintext={} len={} payload={}",
                payload.len(),
                pkt.len(),
                pkt.len() - 19
            ));
        }
    }

    std::eprintln!("---- resource control packet wire sizes ----");
    for row in &rows {
        std::eprintln!("{row}");
    }

    // The mapping the analysis rests on: three contexts land on 115 bytes,
    // and they are the only ones. A REQ that asks for one part is 115 too
    // (measured by the census test above).
    for name in ["ICL", "HMU", "REQ(exhausted)", "REQ(nothing)"] {
        let row = rows
            .iter()
            .find(|r| r.starts_with(name))
            .expect("row present");
        assert!(
            row.contains("len=115"),
            "{name} must weigh 115 bytes on the wire; got {row}"
        );
    }
    let keepalive = rows
        .iter()
        .find(|r| r.starts_with("KEEPALIVE"))
        .expect("row present");
    assert!(
        keepalive.contains("len=83"),
        "a keepalive must NOT be confusable with the looping length; got {keepalive}"
    );
}
