//! #384 mvr: a pre-encrypted single data packet delivers and proves like
//! an ordinary opportunistic one.
//!
//! The propagation node's active delivery re-sends a stored blob whose
//! ciphertext the *originator* produced (`dest_hash ‖
//! destination.encrypt(packed[16:])`, `reference/LXMF/LXMF/LXMessage.py:426-434`);
//! the board cannot re-encrypt it and must not have to. So
//! `send_raw_single_packet` takes the ciphertext as-is, and this test pins
//! the two properties active delivery stands on: the recipient decrypts
//! and surfaces exactly the plaintext an encrypting send would have
//! delivered, and the proof it sends back confirms the raw sender's
//! receipt — which is what lets the store purge exactly that record.
//!
//! Sans-I/O: two nodes, one shared medium, sub-second wall clock.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{Action, InterfaceId, PathEntry, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

fn make_node() -> (Node, usize) {
    let mut node = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new("mesh", 1)));
    node.set_interface_name(idx, String::from("mesh"));
    (node, idx)
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

#[test]
fn a_raw_ciphertext_packet_delivers_and_its_proof_confirms() {
    let (mut sender, s_iface) = make_node();
    let (mut receiver, r_iface) = make_node();

    // The receiver owns the destination and proves everything it gets —
    // the shape of an LXMF delivery destination.
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["rawsingle"],
    )
    .unwrap();
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);

    // The sender knows the destination the way an announce would have
    // taught it: identity remembered, one-hop path on the shared medium.
    sender.remember_identity(
        dest_hash,
        Identity::from_public_key_bytes(&identity.public_key_bytes()).unwrap(),
    );
    sender.transport.insert_path(
        *dest_hash.as_bytes(),
        PathEntry {
            hops: 1,
            expires_ms: u64::MAX,
            interface_index: s_iface,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        },
    );

    // The "stored blob": ciphertext the ORIGINATOR produced. Here the
    // sender doubles as originator, which is exactly the byte relationship
    // the store preserves.
    let plaintext = b"opportunistic lxmf payload bytes".to_vec();
    let ciphertext = sender
        .encrypt_for_destination(&dest_hash, &plaintext)
        .unwrap();

    let (packet_hash, out) = sender
        .send_raw_single_packet(&dest_hash, &ciphertext)
        .unwrap();
    let frames = action_data(&out);
    assert!(!frames.is_empty(), "the packet must reach the wire");

    // Deliver to the receiver: the plaintext surfaces as an ordinary
    // PacketReceived, indistinguishable from an encrypting send.
    let mut proof_frames: Vec<Vec<u8>> = Vec::new();
    let mut received: Vec<Vec<u8>> = Vec::new();
    for frame in frames {
        let out = receiver.handle_packet(InterfaceId(r_iface), &frame);
        for event in &out.events {
            if let NodeEvent::PacketReceived {
                destination, data, ..
            } = event
            {
                assert_eq!(destination, &dest_hash);
                received.push(data.clone());
            }
        }
        proof_frames.extend(action_data(&out));
    }
    assert_eq!(received, std::vec![plaintext]);

    // The proof comes back and confirms the exact hash the raw send
    // returned — the purge trigger of #384's active delivery.
    let mut confirmed: Vec<[u8; TRUNCATED_HASHBYTES]> = Vec::new();
    for frame in proof_frames {
        let out = sender.handle_packet(InterfaceId(s_iface), &frame);
        for event in &out.events {
            if let NodeEvent::PacketDeliveryConfirmed { packet_hash } = event {
                confirmed.push(*packet_hash);
            }
        }
    }
    assert_eq!(confirmed, std::vec![packet_hash]);
}
