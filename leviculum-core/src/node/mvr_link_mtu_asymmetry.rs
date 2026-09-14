//! mvr: both ends of a link end up on ONE negotiated MTU even when the path
//! clamps below the base MTU, so the resource protocol segments the same way
//! on both sides and concludes. Codeberg #390.
//!
//! These tests were written to pin the defect and are kept, inverted, as its
//! regression guard: every assertion below is the negation of what this file
//! asserted before the fix. The mechanism is written out in full because the
//! assertions are only meaningful against it.
//!
//! ## Field failure
//!
//! `lora_pn_board_sync`, hardware, 2026-09-14 (boards on 539259c2). The two
//! boards agree the sync round — `PN_OFFER dir=out offered=1 wanted=1` on
//! lnode_a, `dir=in` on lnode_b — the resource advertisement and its single
//! 336-byte part cross the air intact, and then the pair exchanges ~300
//! packets of exactly 115 bytes for 177 s, 1:1, until lnode_a's
//! `OUTBOUND_DEADLINE_MS` (180 s) reaps the round with
//! `PN_SYNC ... result=timeout`. lnode_b never logs an accept, never sends
//! the 83-byte proof, and the reception of the part itself produces ZERO
//! actions on lnode_b. Over BLE the same leg completes.
//!
//! ## Mechanism
//!
//! The firmware gave its LoRa interface `hw_mtu = 255`
//! (`leviculum-nrf/src/bin/t114.rs`, `rak4631.rs`), while serial and BLE got
//! 564. On a link whose next hop was that LoRa interface:
//!
//! * the responder clamps down to the interface: `path_mtu.min(hw_mtu)`
//!   (`link/mod.rs`, `Link::new_incoming`) → 255, and echoes 255 in its
//!   proof signalling (`link_management.rs`, `proof_mtu =
//!   link.negotiated_mtu()`);
//! * the initiator refused to go below the base MTU: `if confirmed_mtu >=
//!   MTU { confirmed_mtu } else { MTU }` (`link/mod.rs`,
//!   `process_proof`) → 500.
//!
//! The reference adopts the confirmed value verbatim — `self.mtu =
//! confirmed_mtu or RNS.Reticulum.MTU` (`reference/Reticulum/RNS/Link.py`,
//! `validate_proof`) — so in Python both ends land on the same number and
//! this cannot happen. `process_proof` now does the same, which is the fix
//! these tests guard.
//!
//! Both ends derive the resource SDU from their own MTU
//! (`resource_sdu(link.negotiated_mtu())`), so the sender segmented at 464
//! while the receiver expected segmentation at 219:
//!
//! * sender: `ceil(336/464) = 1` part, advertisement carries ONE hashmap
//!   entry;
//! * receiver: `num_parts = ceil(336/219) = 2`
//!   (`incoming.rs`, `from_advertisement` — the part count comes from the
//!   receiver's own `sdu`, never from the advertisement's `n`).
//!
//! The receiver therefore had a 2-slot hashmap with one entry filled, and
//! `build_request` walked into the empty slot and flagged
//! `HASHMAP_IS_EXHAUSTED`. The sender answered every exhausted request with
//! an HMU built from its own one-entry hashmap, which taught the receiver
//! nothing, so the receiver asked again. Both packets weigh 115 bytes on the
//! wire (pinned in `mvr_pn_sync_resource_census`), which is exactly the
//! length that looped in the capture.
//!
//! ## What the fix is
//!
//! Two commits, both needed:
//!
//! 1. the boards declare `hw_mtu = 508` for LoRa, the value the reference,
//!    `rnode::HW_MTU` and `build_lora_frames` already agree on — this takes
//!    the field case out of the clamping regime entirely;
//! 2. `process_proof` adopts the confirmed MTU verbatim — this is the root,
//!    and it is what makes the 255 case below pass. Without it, the next
//!    carrier that clamps below 500 reopens the same hole.
//!
//! The 255 case is therefore not hypothetical housekeeping: it is the only
//! assertion here that fails if commit 2 is reverted.
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

/// What the firmware declared for the SX1262 interface at the time of the
/// field failure (`leviculum-nrf/src/bin/t114.rs`, `rak4631.rs`). The boards
/// now declare 508, but this value stays the parameter of the tests below:
/// it is the clamp that used to fork the two ends, so it is the clamp that
/// has to keep them together.
const LORA_HW_MTU: u32 = 255;

/// The sync envelope of the failing run: one 256-byte stored message in a
/// `PeerSyncEnvelope`, `PN_SYNC ... bytes=270` on the BLE leg of the pair.
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

fn context_of(pkt: &[u8]) -> PacketContext {
    PacketContext::from_byte(pkt[18])
}

struct Pair {
    sender: EndpointNode,
    s_iface: usize,
    sender_link: LinkId,
    receiver: EndpointNode,
    r_iface: usize,
    receiver_link: LinkId,
}

/// One direct link between two nodes whose shared medium declares
/// `hw_mtu`, established the way the board's PN role establishes it: the
/// peer is learned from its announce (so the initiator resolves a next-hop
/// interface at all) and resources are gated by the application.
fn linked_pair_over(hw_mtu: u32) -> Pair {
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
    let r_iface = add_iface(&mut receiver, "R_lora");
    receiver.set_interface_hw_mtu(r_iface, hw_mtu);

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_iface = add_iface(&mut sender, "S_lora");
    sender.set_interface_hw_mtu(s_iface, hw_mtu);

    // The announce is what gives the initiator a path, and therefore a
    // next-hop interface to read the HW MTU off.
    let announce = receiver
        .announce_destination(&dest_hash, None)
        .expect("announce");
    for pkt in action_data(&announce) {
        let _ = sender.handle_packet(InterfaceId(s_iface), &pkt);
    }

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

    Pair {
        sender,
        s_iface,
        sender_link,
        receiver,
        r_iface,
        receiver_link,
    }
}

/// The two ends of one link agree about its MTU even when the path clamps
/// below the base MTU: the responder takes the clamp, and the initiator
/// adopts the confirmed value verbatim instead of flooring it at 500.
///
/// This is the inverse of the assertion that pinned #390, and the 255 case is
/// what proves the root fix and not merely the board's new `hw_mtu`: revert
/// `process_proof` and this test goes red while everything else stays green.
#[test]
fn negotiated_mtu_agrees_under_a_clamping_path() {
    let pair = linked_pair_over(LORA_HW_MTU);

    let initiator_mtu = pair
        .sender
        .link(&pair.sender_link)
        .expect("initiator link")
        .negotiated_mtu();
    let responder_mtu = pair
        .receiver
        .link(&pair.receiver_link)
        .expect("responder link")
        .negotiated_mtu();

    std::eprintln!("initiator_mtu={initiator_mtu} responder_mtu={responder_mtu}");
    assert_eq!(
        responder_mtu, LORA_HW_MTU,
        "the responder clamps to the receiving interface's HW MTU"
    );
    assert_eq!(
        initiator_mtu, responder_mtu,
        "the initiator must adopt the confirmed MTU verbatim (reference: \
         `self.mtu = confirmed_mtu or RNS.Reticulum.MTU`), not floor it at \
         the base MTU"
    );

    let initiator_sdu = crate::resource::resource_sdu(initiator_mtu);
    let responder_sdu = crate::resource::resource_sdu(responder_mtu);
    std::eprintln!("initiator_sdu={initiator_sdu} responder_sdu={responder_sdu}");
    assert_eq!(
        initiator_sdu, responder_sdu,
        "one MTU means one SDU, which is what the resource protocol needs"
    );
}

/// The control that says the asymmetry above comes from the clamp and not
/// from the harness: an interface at or above the base MTU has nothing to
/// clamp DOWN to, and both ends stay on one MTU.
///
/// The two values are the ones that matter to the field case. 508 is what
/// the reference declares for the interface that carries LoRa
/// (`reference/Reticulum/RNS/Interfaces/RNodeInterface.py`, `HW_MTU = 508`)
/// and is also what our own splitter really carries — `build_lora_frames`
/// emits two frames of at most `MAX_SINGLE_PAYLOAD = 254`. 564 is what the
/// firmware declares for serial and BLE, the carriers on which this sync
/// leg completes on hardware.
///
/// Measured, not assumed: the clamp in `Link::new_incoming` is downward-only
/// (`path_mtu.min(hw_mtu)` after `path_mtu = max(proposed, MTU)`), so
/// neither value upgrades the link past the base 500 in this harness. The
/// point of the control is the SYMMETRY, which is the property the resource
/// protocol needs.
#[test]
fn negotiated_mtu_agrees_when_the_path_does_not_clamp() {
    for hw_mtu in [508u32, 564] {
        let pair = linked_pair_over(hw_mtu);

        let initiator_mtu = pair
            .sender
            .link(&pair.sender_link)
            .expect("initiator link")
            .negotiated_mtu();
        let responder_mtu = pair
            .receiver
            .link(&pair.receiver_link)
            .expect("responder link")
            .negotiated_mtu();

        std::eprintln!(
            "hw_mtu={hw_mtu} initiator_mtu={initiator_mtu} responder_mtu={responder_mtu}"
        );
        assert_eq!(
            initiator_mtu, responder_mtu,
            "a non-clamping path ({hw_mtu}) must leave both ends on one MTU"
        );
        assert!(
            initiator_mtu >= crate::constants::MTU as u32,
            "both ends must be at or above the base MTU"
        );
    }
}

/// The other half of what `hw_mtu = 508` means on the boards, and the reason
/// the number is 508 and not "some bigger round number".
///
/// The `NoStorage` harness above can only exercise the responder's clamp: the
/// initiator proposes `next_hop_interface_hw_mtu()`, which reads
/// `storage.get_path()`, and `NoStorage::get_path` always returns `None`
/// (`traits.rs`). A board has a real path table, so its proposal IS the
/// interface's HW MTU. This pair runs on `MemoryStorage` so the proposal is
/// real, and measures what a LoRa link between two LNodes now negotiates.
///
/// The answer is 508, not 500: `path_mtu = max(508, 500) = 508`, then
/// `min(508)` (`Link::new_incoming`), and since #390 the initiator adopts
/// that verbatim. 508 is exactly what the carrier can take and not one byte
/// more — `build_lora_frames` emits at most two frames of
/// `1 + MAX_SINGLE_PAYLOAD` = 255 bytes, the SX1262 payload ceiling. So this
/// measures a full-MDU packet through the splitter rather than assuming it
/// fits.
#[test]
fn a_board_lora_link_negotiates_508_and_still_fits_the_splitter() {
    use crate::memory_storage::MemoryStorage;

    type StoredNode = NodeCore<OsRng, MockClock, MemoryStorage>;

    fn add_stored_iface(node: &mut StoredNode, name: &'static str, hw_mtu: u32) -> usize {
        let idx = node
            .transport
            .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
        node.set_interface_name(idx, String::from(name));
        node.set_interface_hw_mtu(idx, hw_mtu);
        idx
    }

    const BOARD_LORA_HW_MTU: u32 = 508;

    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();

    let mut receiver: StoredNode = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
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
    let r_iface = add_stored_iface(&mut receiver, "R_lora", BOARD_LORA_HW_MTU);

    let mut sender: StoredNode = NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let s_iface = add_stored_iface(&mut sender, "S_lora", BOARD_LORA_HW_MTU);

    // The announce installs the path, which is what makes the initiator's
    // proposal the interface's HW MTU instead of the base MTU.
    let announce = receiver
        .announce_destination(&dest_hash, None)
        .expect("announce");
    for pkt in action_data(&announce) {
        let _ = sender.handle_packet(InterfaceId(s_iface), &pkt);
    }
    assert_eq!(
        sender
            .transport
            .next_hop_interface_hw_mtu(dest_hash.as_bytes()),
        Some(BOARD_LORA_HW_MTU),
        "the initiator must resolve the LoRa interface's HW MTU, otherwise \
         this test degenerates into the NoStorage case above"
    );

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
    let receiver_link = receiver_link.expect("responder side must reach Active");

    let link = sender.link(&sender_link).expect("initiator link");
    let initiator_mtu = link.negotiated_mtu();
    let responder_mtu = receiver
        .link(&receiver_link)
        .expect("responder link")
        .negotiated_mtu();
    let mdu = link.mdu();
    std::eprintln!(
        "board-lora initiator_mtu={initiator_mtu} responder_mtu={responder_mtu} mdu={mdu}"
    );
    assert_eq!(
        initiator_mtu, responder_mtu,
        "both ends of a board LoRa link must share one MTU"
    );
    assert_eq!(
        initiator_mtu, BOARD_LORA_HW_MTU,
        "with a real path table both ends negotiate the interface's HW MTU"
    );

    let mut rng = OsRng;
    let payload = std::vec![0xA5u8; mdu];
    let pkt = link
        .build_data_packet_with_context(&payload, PacketContext::None, &mut rng)
        .expect("a full-MDU packet must build");
    let frames = crate::rnode::build_lora_frames(&pkt, 0);
    let widest = frames.iter().map(|f| f.len()).max().unwrap_or(0);
    std::eprintln!(
        "board-lora packet_len={} frames={} widest_frame={widest}",
        pkt.len(),
        frames.len()
    );

    assert!(
        pkt.len() <= BOARD_LORA_HW_MTU as usize,
        "a full-MDU packet must not exceed the negotiated MTU; got {}",
        pkt.len()
    );
    assert!(
        frames.len() <= 2,
        "the splitter emits at most two frames; got {}",
        frames.len()
    );
    assert!(
        widest <= 1 + crate::rnode::MAX_SINGLE_PAYLOAD,
        "no frame may exceed the SX1262 payload ceiling of {} bytes; widest \
         was {widest}. If this is RED, 508 is too large for this carrier and \
         the boards' hw_mtu is wrong again.",
        1 + crate::rnode::MAX_SINGLE_PAYLOAD
    );

    // The resource path is the one #390 burned, and it does NOT go through
    // `mdu()`: a part is sized by `resource_sdu(508) = 472`, which is wider
    // than the 431-byte link MDU above. So drive a real multi-part transfer
    // and put every packet it emits through the splitter.
    receiver
        .set_resource_strategy(&receiver_link, ResourceStrategy::AcceptApp)
        .expect("the PN role gates resources through the application");
    let envelope: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (_hash, out) = sender
        .send_resource(&sender_link, &envelope, None, true)
        .expect("sender advertises the resource");

    let mut sender_completed = false;
    let mut widest_on_air = 0usize;
    let mut widest_packet = 0usize;
    let mut most_frames = 0usize;
    let check = |pkt: &[u8], widest_on_air: &mut usize, most: &mut usize, wp: &mut usize| {
        let frames = crate::rnode::build_lora_frames(pkt, 0);
        *most = (*most).max(frames.len());
        *wp = (*wp).max(pkt.len());
        for f in &frames {
            *widest_on_air = (*widest_on_air).max(f.len());
        }
    };

    let mut to_receiver = action_data(&out);
    for pkt in &to_receiver {
        check(
            pkt,
            &mut widest_on_air,
            &mut most_frames,
            &mut widest_packet,
        );
    }
    for _round in 0..64 {
        if to_receiver.is_empty() {
            break;
        }
        let mut to_sender = Vec::new();
        for pkt in &to_receiver {
            let o = receiver.handle_packet(InterfaceId(r_iface), pkt);
            let mut emitted = action_data(&o);
            if o.events
                .iter()
                .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. }))
            {
                emitted.extend(action_data(
                    &receiver
                        .accept_resource(&receiver_link)
                        .expect("accept_resource consumes the parked ADV"),
                ));
            }
            for p in &emitted {
                check(p, &mut widest_on_air, &mut most_frames, &mut widest_packet);
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
                check(p, &mut widest_on_air, &mut most_frames, &mut widest_packet);
            }
            next.extend(emitted);
        }
        to_receiver = next;
    }

    std::eprintln!(
        "board-lora resource sdu={} widest_packet={widest_packet} \
         most_frames={most_frames} widest_frame={widest_on_air} \
         completed={sender_completed}",
        crate::resource::resource_sdu(initiator_mtu)
    );
    assert!(
        sender_completed,
        "a 4 KB resource must conclude over a 508-MTU board LoRa link"
    );
    assert!(
        widest_packet <= BOARD_LORA_HW_MTU as usize,
        "no packet may exceed the negotiated MTU; widest was {widest_packet}"
    );
    assert!(
        most_frames <= 2,
        "the splitter must never need more than two frames; needed {most_frames}"
    );
    assert!(
        widest_on_air <= 1 + crate::rnode::MAX_SINGLE_PAYLOAD,
        "a resource part must still fit the SX1262 payload ceiling of {} \
         bytes; widest frame was {widest_on_air}",
        1 + crate::rnode::MAX_SINGLE_PAYLOAD
    );
}

/// The consequence, inverted: the PN sync resource concludes over the very
/// path that used to livelock it. Both ends now derive the same part count
/// from the same SDU, so the receiver's requests are answerable, it
/// assembles, and it proves — instead of an unbounded 115-byte REQ/HMU
/// exchange that ran until the outbound watchdog reaped the round.
#[test]
fn sync_resource_concludes_under_a_clamping_path() {
    let mut pair = linked_pair_over(LORA_HW_MTU);

    let envelope: Vec<u8> = (0..ENVELOPE_BYTES).map(|i| (i % 251) as u8).collect();
    let (_hash, out) = pair
        .sender
        .send_resource(&pair.sender_link, &envelope, None, true)
        .expect("sender advertises the sync resource");

    let mut sender_completed = false;
    let mut receiver_completed = false;
    let mut to_receiver = action_data(&out);
    // Counted by context so the loop can be named, not just observed.
    let mut req_from_receiver = 0usize;
    let mut hmu_from_sender = 0usize;
    let mut parts_from_sender = 0usize;
    let mut proofs_from_receiver = 0usize;
    let mut looping_lengths = 0usize;

    for pkt in &to_receiver {
        if context_of(pkt) == PacketContext::Resource {
            parts_from_sender += 1;
        }
    }

    for _round in 0..64 {
        if to_receiver.is_empty() {
            break;
        }
        let mut to_sender = Vec::new();
        for pkt in &to_receiver {
            let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), pkt);
            if o.events.iter().any(|e| {
                matches!(
                    e,
                    NodeEvent::ResourceCompleted {
                        is_sender: false,
                        ..
                    }
                )
            }) {
                receiver_completed = true;
            }
            let mut emitted = action_data(&o);
            if o.events
                .iter()
                .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. }))
            {
                let accepted = pair
                    .receiver
                    .accept_resource(&pair.receiver_link)
                    .expect("accept_resource consumes the parked ADV");
                emitted.extend(action_data(&accepted));
            }
            for p in &emitted {
                match context_of(p) {
                    PacketContext::ResourceReq => req_from_receiver += 1,
                    PacketContext::ResourcePrf => proofs_from_receiver += 1,
                    _ => {}
                }
                if p.len() == 115 {
                    looping_lengths += 1;
                }
            }
            to_sender.extend(emitted);
        }

        let mut next = Vec::new();
        for pkt in &to_sender {
            let o = pair.sender.handle_packet(InterfaceId(pair.s_iface), pkt);
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
                match context_of(p) {
                    PacketContext::ResourceHmu => hmu_from_sender += 1,
                    PacketContext::Resource => parts_from_sender += 1,
                    _ => {}
                }
                if p.len() == 115 {
                    looping_lengths += 1;
                }
            }
            next.extend(emitted);
        }
        to_receiver = next;
    }

    std::eprintln!(
        "req={req_from_receiver} hmu={hmu_from_sender} parts={parts_from_sender} \
         proofs={proofs_from_receiver} len115={looping_lengths} \
         sender_completed={sender_completed} receiver_completed={receiver_completed}"
    );

    // The part count is now the receiver's too: `resource_sdu(255) = 219`,
    // `ceil(336/219) = 2`. Before the fix the sender sent one part against a
    // receiver expecting two, and that gap was the livelock.
    assert_eq!(
        parts_from_sender, 2,
        "the sender segments at the SAME SDU the receiver derives"
    );
    assert_eq!(
        proofs_from_receiver, 1,
        "the receiver assembles and proves exactly once"
    );
    assert!(
        sender_completed && receiver_completed,
        "both ends must conclude the transfer over a clamping path. \
         sender={sender_completed} receiver={receiver_completed}"
    );
    assert!(
        hmu_from_sender == 0,
        "no hashmap update is needed when both ends agree on the part count; \
         an HMU here means the exhaustion loop is back. hmu={hmu_from_sender}"
    );
    assert!(
        req_from_receiver <= 2,
        "requests are bounded by the part count, not unbounded retries. \
         req={req_from_receiver}"
    );
}

/// The same transfer over a path that does not clamp concludes, which is
/// what the BLE leg of the same pair does on hardware. This is the control:
/// nothing about a 270-byte PN sync envelope is itself unworkable, and 508
/// — the reference's HW MTU for the interface that carries LoRa — is enough
/// to make this leg work.
#[test]
fn sync_resource_concludes_when_both_ends_share_one_mtu() {
    for hw_mtu in [508u32, 564] {
        assert_sync_resource_concludes(hw_mtu);
    }
}

fn assert_sync_resource_concludes(hw_mtu: u32) {
    let mut pair = linked_pair_over(hw_mtu);

    let envelope: Vec<u8> = (0..ENVELOPE_BYTES).map(|i| (i % 251) as u8).collect();
    let (_hash, out) = pair
        .sender
        .send_resource(&pair.sender_link, &envelope, None, true)
        .expect("sender advertises the sync resource");

    let mut sender_completed = false;
    let mut to_receiver = action_data(&out);
    for _round in 0..16 {
        if to_receiver.is_empty() {
            break;
        }
        let mut to_sender = Vec::new();
        for pkt in &to_receiver {
            let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), pkt);
            let mut emitted = action_data(&o);
            if o.events
                .iter()
                .any(|e| matches!(e, NodeEvent::ResourceAdvertised { .. }))
            {
                let accepted = pair
                    .receiver
                    .accept_resource(&pair.receiver_link)
                    .expect("accept_resource consumes the parked ADV");
                emitted.extend(action_data(&accepted));
            }
            to_sender.extend(emitted);
        }
        let mut next = Vec::new();
        for pkt in &to_sender {
            let o = pair.sender.handle_packet(InterfaceId(pair.s_iface), pkt);
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
            next.extend(action_data(&o));
        }
        to_receiver = next;
    }

    assert!(
        sender_completed,
        "a 270-byte sync resource concludes when both ends share one MTU          (hw_mtu={hw_mtu})"
    );
}
