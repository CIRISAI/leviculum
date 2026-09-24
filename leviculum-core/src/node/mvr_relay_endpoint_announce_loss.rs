//! #326 mvr: `lora_3node_relay` went red in vollauf3 because alpha never
//! learned a path to `gamma.probe`. What makes one lost announce
//! unrecoverable in an A-B-C relay, and what recovers it?
//!
//! ## The observation this answers
//!
//! `rig-run/vollauf3.log` (2026-08-22, lines 1186-1376): step 4
//! `wait_for_path alpha -> gamma.probe` failed after three attempts. The
//! three daemon log tails in that block carry the whole fingerprint:
//!
//! - alpha: `PATH_TABLE size=0` on every 10 s heartbeat from 08:49:47 to
//!   08:51:37, and `PKT_DROP_SUMMARY total=0` — nothing was dropped
//!   because nothing arrived;
//! - beta (the relay, the only node that hears both ends): `size=1`, not
//!   the 2 a converged run has;
//! - gamma: `size=1`, learned through beta, plus four
//!   `DIRECT_INGRESS_DROP` lines — alpha's path requests, killed by the
//!   scenario's own range emulation.
//!
//! Those three numbers have one cause between them. gamma's announce
//! never reached beta: beta then holds alpha alone (1), gamma holds
//! alpha relayed by beta (1), and alpha holds nothing at all (0) —
//! beta's own announce arrives with wire hops 0 and gamma's, which would
//! arrive relayed at hops 1, was never sent on. Beta had heard alpha and
//! had relayed for gamma, so neither the channel nor the relay was down;
//! exactly one frame was missing.
//!
//! On the 2026-08-22 tree that frame was the destination's entire chance.
//! A management announce went out through `send_on_all_interfaces` once
//! and was not scheduled again until `MGMT_ANNOUNCE_INTERVAL_MS` (2 h)
//! came round, so a single lost carrier window cost the destination the
//! whole interval — the mechanism measured on the `ble_lora_transport`
//! cell nine days later and fixed in 2d9234da (2026-09-01) by giving a
//! node's own announce the second emission the reference gives a shared
//! instance client's (Transport.py:2361-2372, fired again at :765-782).
//!
//! ## What is pinned
//!
//! 1. The fix, stated as the failure it prevents: lose gamma's FIRST
//!    probe announce on the air and alpha must still converge on a 2-hop
//!    path to `gamma.probe`. Red on the tree that ran vollauf3 — there was
//!    no second emission to carry it.
//! 2. Positive control on the loss filter, and the vollauf3 fingerprint
//!    reproduced: lose EVERY gamma probe announce and the path tables come
//!    out 0 / 1 / 1, alpha / beta / gamma, the exact triple the run logged.
//!    Without this, 1 could go green because the drop never bit.
//! 3. Why 2 is terminal rather than merely slow, i.e. why the origin's
//!    retry is the only recovery this topology has: beta, a transport node
//!    in `Full` mode with no local clients, answers a path request for a
//!    destination it has no path to with silence, and alpha's request
//!    reaches gamma only as a wire-hops-0 frame, which the range emulation
//!    drops. Nothing in the topology can solicit the missing announce.
//!
//! The range emulation is the scenario's own: `deaf_to_direct` renders
//! `test_drop_direct_ingress`, which drops a received frame iff its wire
//! hops byte is 0 (`leviculum-std/src/interfaces/mod.rs:453-476`). The
//! predicate here is that byte test, literally.
//!
//! Sans-I/O: no LoRa, no Docker, no rig, deterministic seeded RNGs.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::memory_storage::MemoryStorage;
use crate::node::mvr_probe_announce_phase::SeededRng;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::{Action, InterfaceId};

type SeededNode = NodeCore<SeededRng, MockClock, MemoryStorage>;

const ALPHA: usize = 0;
const BETA: usize = 1;
const GAMMA: usize = 2;
const NAMES: [&str; 3] = ["alpha", "beta", "gamma"];

/// How far the shared channel is driven, and in what steps. The window has
/// to hold the first management announce (15 s + up to 5 s of jitter), its
/// retry one `PATHFINDER_G_MS` later, and beta's rebroadcast ladder on top.
const RUN_MS: u64 = 60_000;
const STEP_MS: u64 = 250;

/// A node of the `rnode_trio` profile as `hardware/lora_3node_relay.toml`
/// declares it: probe responder on everywhere, transport only on beta, one
/// shared-channel interface each.
fn make_node(seed: u64, transport: bool) -> SeededNode {
    let mut node = NodeCoreBuilder::new()
        .enable_transport(transport)
        .respond_to_probes(true)
        .build(
            SeededRng(seed),
            MockClock::new(TEST_TIME_MS),
            MemoryStorage::with_defaults(),
        );
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new("rnode_0", 1)));
    node.set_interface_name(idx, String::from("rnode_0"));
    node
}

fn make_trio() -> [SeededNode; 3] {
    [
        make_node(0x0a1f, false),
        make_node(0x0b2e, true),
        make_node(0x0c3d, false),
    ]
}

/// `test_drop_direct_ingress`, byte for byte: a frame heard directly from
/// its originator carries wire hops 0 and is dropped on ingress, relayed
/// copies pass. alpha and gamma run it, beta does not — that is what makes
/// the co-located trio an A-B-C chain.
fn deaf_to_direct(node: usize) -> bool {
    node == ALPHA || node == GAMMA
}

fn drops_on_ingress(node: usize, frame: &[u8]) -> bool {
    deaf_to_direct(node) && frame.len() >= 2 && frame[1] == 0
}

fn is_announce_for(frame: &[u8], dest: &[u8; TRUNCATED_HASHBYTES]) -> bool {
    Packet::unpack(frame)
        .map(|p| p.flags.packet_type == PacketType::Announce && &p.destination_hash == dest)
        .unwrap_or(false)
}

fn actions_to_wire(
    out: &crate::transport::TickOutput,
    src: usize,
    pending: &mut Vec<(usize, Vec<u8>)>,
) {
    for a in &out.actions {
        match a {
            Action::SendPacket { data, .. } => pending.push((src, data.clone())),
            Action::Broadcast {
                data,
                exclude_iface,
                exclude_ifaces,
                ..
            } => {
                let id = InterfaceId(0);
                if Some(id) == *exclude_iface || exclude_ifaces.contains(&id) {
                    continue;
                }
                pending.push((src, data.clone()));
            }
        }
    }
}

/// The shared channel. One round: fire every node's timers, then hand every
/// queued frame to the two other nodes, letting what they emit in reaction
/// join the same round. `lose` is consulted once per transmission, at the
/// source, and a frame it rejects reaches nobody — the on-air loss the run
/// log attributes gamma's missing announce to.
fn drive(nodes: &mut [SeededNode; 3], lose: &mut dyn FnMut(usize, &[u8]) -> bool) {
    let end = TEST_TIME_MS + RUN_MS;
    while nodes[ALPHA].transport().clock().now_ms() < end {
        let mut pending: Vec<(usize, Vec<u8>)> = Vec::new();
        for (src, node) in nodes.iter_mut().enumerate() {
            let out = node.handle_timeout();
            actions_to_wire(&out, src, &mut pending);
        }

        // Bounded: a reaction chain that will not settle is a bug in its own
        // right, and an unbounded loop here would hide it as a hang.
        let mut guard = 0usize;
        while let Some((src, frame)) = pending.pop() {
            guard += 1;
            assert!(guard < 10_000, "the shared channel never went quiet");
            if lose(src, &frame) {
                continue;
            }
            for (dst, node) in nodes.iter_mut().enumerate() {
                if dst == src || drops_on_ingress(dst, &frame) {
                    continue;
                }
                let out = node.handle_packet(InterfaceId(0), &frame);
                actions_to_wire(&out, dst, &mut pending);
            }
        }

        for node in nodes.iter() {
            let now = node.transport().clock().now_ms();
            node.transport().clock().set(now + STEP_MS);
        }
    }
}

fn probe_hash(node: &SeededNode) -> [u8; TRUNCATED_HASHBYTES] {
    node.probe_dest_hash()
        .expect("respond_to_probes registers a probe destination")
        .into_bytes()
}

fn path_hops(node: &SeededNode, dest: &[u8; TRUNCATED_HASHBYTES]) -> Option<u8> {
    node.transport().storage().get_path(dest).map(|p| p.hops)
}

fn path_counts(nodes: &[SeededNode; 3]) -> [usize; 3] {
    [
        nodes[ALPHA].transport().storage().path_count(),
        nodes[BETA].transport().storage().path_count(),
        nodes[GAMMA].transport().storage().path_count(),
    ]
}

/// Direction 1, the bug: the first emission of gamma's probe announce is
/// lost on the air, as the vollauf3 evidence says it was. alpha must still
/// end the settle window holding a 2-hop path to `gamma.probe` — the
/// `expect_hops = 2` the scenario asserts, carried by the second emission
/// `schedule_own_announce_retry` now schedules.
///
/// Red on the pre-2d9234da tree that ran vollauf3: there was one emission
/// and nothing after it until the 2 h management interval.
#[test]
fn a_lost_first_announce_still_reaches_the_far_endpoint() {
    let mut nodes = make_trio();
    let gamma_probe = probe_hash(&nodes[GAMMA]);
    let alpha_probe = probe_hash(&nodes[ALPHA]);

    let mut lost = 0usize;
    drive(&mut nodes, &mut |src, frame| {
        if src == GAMMA && lost == 0 && is_announce_for(frame, &gamma_probe) {
            lost = 1;
            return true;
        }
        false
    });

    assert_eq!(
        lost, 1,
        "the first gamma probe announce must have been dropped, or this proves nothing"
    );

    assert_eq!(
        path_hops(&nodes[ALPHA], &gamma_probe),
        Some(2),
        "alpha must learn gamma.probe at 2 hops after losing the first \
         announce; the retry is the only thing that can carry it"
    );
    // The leg that survived in vollauf3, as a non-vacuity check on the
    // harness: gamma held alpha at 1 entry there, so this direction has to
    // be green here too or the chain itself is miswired.
    assert_eq!(
        path_hops(&nodes[GAMMA], &alpha_probe),
        Some(2),
        "gamma must learn alpha.probe through beta"
    );
}

/// Direction 2, the positive control on the loss filter and the vollauf3
/// fingerprint reproduced: with EVERY gamma probe announce lost, the three
/// path tables must come out 0 / 1 / 1 — alpha nothing, beta alpha alone,
/// gamma alpha through beta. That triple is what the run logged, and it is
/// what direction 1 would look like if the retry did not exist.
#[test]
fn losing_every_announce_reproduces_the_vollauf3_path_tables() {
    let mut nodes = make_trio();
    let gamma_probe = probe_hash(&nodes[GAMMA]);
    let alpha_probe = probe_hash(&nodes[ALPHA]);

    let mut lost = 0usize;
    drive(&mut nodes, &mut |src, frame| {
        if src == GAMMA && is_announce_for(frame, &gamma_probe) {
            lost += 1;
            return true;
        }
        false
    });

    assert!(
        lost >= 2,
        "the origin's announce ladder is two emissions, first and retry; \
         {lost} seen, so the tree under test is the one vollauf3 ran"
    );
    assert_eq!(
        path_counts(&nodes),
        [0, 1, 1],
        "the vollauf3 fingerprint: {} sizes were 0/1/1 in the run log",
        NAMES.join("/")
    );
    assert_eq!(
        path_hops(&nodes[ALPHA], &gamma_probe),
        None,
        "alpha holds no path to gamma.probe: the wait_for_path that went red"
    );
    assert_eq!(
        path_hops(&nodes[GAMMA], &alpha_probe),
        Some(2),
        "beta still relays in the other direction, as it did in the run"
    );
}

/// Direction 3, why direction 2 is terminal: nothing in this topology can
/// solicit the missing announce back. alpha's path request reaches beta,
/// which holds no path to gamma.probe and — `Full` mode, no local clients —
/// re-originates nothing and answers nothing; and it reaches gamma only as
/// a wire-hops-0 frame, which gamma's range emulation drops. Both halves
/// were visible in the run: beta logged the request and fell silent, gamma
/// logged `DIRECT_INGRESS_DROP` for each of the three attempts.
///
/// Green before and after 2d9234da — this is the standing shape the origin
/// retry has to compensate for, not a regression.
#[test]
fn the_relay_cannot_solicit_the_announce_it_never_heard() {
    let mut nodes = make_trio();
    let gamma_probe = probe_hash(&nodes[GAMMA]);

    let mut lost = 0usize;
    drive(&mut nodes, &mut |src, frame| {
        if src == GAMMA && is_announce_for(frame, &gamma_probe) {
            lost += 1;
            return true;
        }
        false
    });
    assert!(
        lost >= 1,
        "the control state requires the announce to be lost"
    );
    assert_eq!(path_hops(&nodes[ALPHA], &gamma_probe), None);

    let request = {
        let out = nodes[ALPHA].request_path(&gamma_probe.into());
        let mut wire = Vec::new();
        actions_to_wire(&out, ALPHA, &mut wire);
        assert_eq!(wire.len(), 1, "alpha puts exactly one path request on air");
        wire.remove(0).1
    };
    assert_eq!(
        request[1], 0,
        "an originated request goes out at wire hops 0"
    );

    assert!(
        drops_on_ingress(GAMMA, &request),
        "gamma's range emulation drops the request: the destination that \
         could answer for itself never hears it"
    );

    let mut beta_wire = Vec::new();
    let out = nodes[BETA].handle_packet(InterfaceId(0), &request);
    actions_to_wire(&out, BETA, &mut beta_wire);
    assert!(
        beta_wire.is_empty(),
        "a Full-mode relay with no local clients answers an unknown-path \
         request with silence, so the request cannot recover the announce: \
         {} frame(s) emitted",
        beta_wire.len()
    );
}
