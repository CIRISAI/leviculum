//! mvr for Codeberg #235: the remote-management allow-list frame is
//! writable over USB and NEVER over the air.
//!
//! Named failure mode this guards: a board that parses the #238 control
//! envelope wherever bytes arrive, so an
//! [`envelope::TYPE_MGMT_ALLOW`](crate::envelope::TYPE_MGMT_ALLOW) frame
//! sent over LoRa or BLE by anybody within radio range rewrites the list
//! of identities allowed to read the board's status. #235's requirement is
//! that only the attached host can write it, and the firmware meets it by
//! *where the parser sits*: `leviculum_nrf::usb::retic_serial_task` is the
//! only caller of
//! [`classify_control_frame`](crate::envelope::classify_control_frame) in
//! the firmware (`leviculum-nrf/src/usb.rs:650`), and it reads from the
//! transport CDC alone. Bytes from the LoRa and BLE tasks are handed to
//! the node core as Reticulum packets instead.
//!
//! "Only the CDC parses it" is an argument about firmware structure. What
//! this test adds is the other half, which is a property of the node and
//! therefore testable here: the exact same bytes, handed to a NodeCore on
//! a radio interface, are dropped — no destination is created, no request
//! handler is registered, no event is emitted and nothing goes back on the
//! wire. That is what makes a frame that leaks onto a radio path harmless
//! rather than merely unlikely.
//!
//! Topology: one node, two interfaces (a LoRa-shaped one and a BLE-shaped
//! one), sans-I/O, no timers, < 5 ms. Single named failure mode.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::envelope;
use crate::mgmt_allow_store::{remote_mgmt_decision, RemoteMgmtDecision, StoredMgmtAllow};
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::InterfaceId;

type BoardNode = NodeCore<OsRng, MockClock, NoStorage>;

/// The two radio interfaces a T114 registers beside its serial one
/// (`leviculum-nrf/src/bin/t114.rs`: 0 serial_usb, 1 lora_sx1262, 2 ble).
/// Registered in the same order here so the ids match the firmware's.
fn board_with_radios() -> (BoardNode, InterfaceId, InterfaceId) {
    let mut node: BoardNode = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        NoStorage,
    );
    for (idx, name) in ["serial_usb", "lora_sx1262", "ble"].into_iter().enumerate() {
        let registered = node
            .transport
            .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
        assert_eq!(registered, idx, "interface ids must match the firmware's");
        node.set_interface_name(registered, String::from(name));
    }
    (node, InterfaceId(1), InterfaceId(2))
}

fn allowed_identity() -> [u8; TRUNCATED_HASHBYTES] {
    // A plausible rig lnsd management identity hash: not all one byte, so
    // a partial copy would be visible.
    let mut hash = [0u8; TRUNCATED_HASHBYTES];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = 0x5A ^ (i as u8);
    }
    hash
}

#[test]
fn the_allow_list_frame_arriving_on_a_radio_interface_is_dropped() {
    let hashes = [allowed_identity()];
    // The very bytes lnflash puts on the CDC.
    let frame = envelope::encode_mgmt_allow(&hashes);
    // Positive control on the frame itself: on the CDC read path this
    // classifies as the executable action. If this assertion ever stopped
    // holding, the drop assertions below would pass for the wrong reason.
    assert_eq!(
        envelope::classify_control_frame(
            &frame,
            &[envelope::TYPE_MGMT_ALLOW, envelope::TYPE_MGMT_ALLOW_QUERY]
        ),
        envelope::ControlAction::MgmtAllow(StoredMgmtAllow::from_hashes(&hashes).unwrap()),
        "the frame must be executable on the USB path, or this test proves nothing"
    );

    for iface in [1usize, 2] {
        let (mut node, lora, ble) = board_with_radios();
        let on = if iface == 1 { lora } else { ble };
        let destinations_before = node.destinations.len();

        let output = node.handle_packet(on, &frame);

        assert!(
            output.actions.is_empty(),
            "iface {iface}: the board answered an envelope frame over the air: {:?}",
            output.actions.len()
        );
        assert!(
            output.events.is_empty(),
            "iface {iface}: an envelope frame over the air produced events"
        );
        assert_eq!(
            node.destinations.len(),
            destinations_before,
            "iface {iface}: an envelope frame over the air changed the destination table"
        );
        assert!(
            node.remote_mgmt_dest_hash().is_none(),
            "iface {iface}: an envelope frame over the air registered remote management"
        );
    }
}

#[test]
fn positive_control_a_real_packet_on_the_same_interface_is_not_silent() {
    // The silence asserted above only means something if this node would
    // have SAID something about a packet it could read. So: a genuine
    // announce from a peer, on the same interface, in the same call.
    // Without this, a `handle_packet` that had stopped reporting anything
    // at all would make the drop test pass for the wrong reason.
    let mut peer: BoardNode = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        NoStorage,
    );
    let peer_iface = peer
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new("peer", 0)));
    peer.set_interface_name(peer_iface, String::from("peer"));
    let identity = crate::identity::Identity::generate(&mut OsRng);
    let mut dest = crate::destination::Destination::new(
        Some(identity),
        crate::destination::Direction::In,
        crate::destination::DestinationType::Single,
        "mvrapp",
        &["mgmt"],
    )
    .unwrap();
    dest.set_proof_strategy(crate::destination::ProofStrategy::All);
    let dest_hash = *dest.hash();
    peer.register_destination(dest);
    let announced = peer.announce_destination(&dest_hash, None).unwrap();
    let announce_bytes = announced
        .actions
        .iter()
        .map(|a| match a {
            crate::transport::Action::Broadcast { data, .. }
            | crate::transport::Action::SendPacket { data, .. } => data.clone(),
        })
        .next()
        .expect("the peer put an announce on the wire");

    let (mut node, lora, _ble) = board_with_radios();
    let output = node.handle_packet(lora, &announce_bytes);
    assert!(
        !output.events.is_empty(),
        "the LoRa interface reports nothing at all, so the drop test proves nothing"
    );
}

#[test]
fn a_radio_peer_cannot_reach_the_decision_either() {
    // The complement of the drop: even if a frame's bytes did reach the
    // board's allow-list record, the decision that turns a record into a
    // registered destination is only ever taken at boot from flash
    // (`leviculum_nrf::mgmt::load_at_boot`), never from a packet. Stated
    // here as the rule the firmware calls: nothing at all is registered
    // without a stored, non-empty list.
    assert_eq!(remote_mgmt_decision(None), RemoteMgmtDecision::Disabled);
    assert_eq!(
        remote_mgmt_decision(Some(&StoredMgmtAllow::EMPTY)),
        RemoteMgmtDecision::Disabled
    );
    let list = StoredMgmtAllow::from_hashes(&[allowed_identity()]).unwrap();
    assert_eq!(
        remote_mgmt_decision(Some(&list)),
        RemoteMgmtDecision::Enabled(Vec::from([allowed_identity()]))
    );
}
