//! #344 mvr: a report to a destination hosted behind a shared-instance client
//! is dropped by the receiving transport node as "overheard".
//!
//! ## The field failure
//!
//! Rig, 2026-08-23. An LNode sends five telemetry reports over LoRa to
//! `c42738413f...`. Every one of them reaches the receiving `lnsd`, and every
//! one is discarded by its own transport:
//!
//! ```text
//! Dropped packet for <c42738413f44a8570bd4120555883466> on rnode_0, transport ID mismatch
//! ```
//!
//! The target is a destination `lnsd` itself delivers to — its own path table
//! holds it at `hops=0 iface=Local[rns/demo]/0`, i.e. behind the Python
//! `lxmd` attached over the shared instance. The drop site is the HEADER_2
//! filter (`transport.rs`, Python Transport.py:1342-1345), so the report
//! carried a `transport_id` that was not the receiver's. Which side wrote the
//! unexpected value — the board addressing the wrong hop, or the receiver
//! answering to a different id than it stamps — was the open question.
//!
//! ## What this models
//!
//! The whole measured topology, radio-free:
//!
//! ```text
//!   client --(local/IPC)--> lnsd --(LoRa)--> board
//!   target registered on the client, so lnsd holds it at hops == 0
//! ```
//!
//! and then the report back down the same chain. Both nodes are real
//! `NodeCore`s; the LoRa medium is an in-process frame queue. Every step the
//! evening exercised is here: the client's announce, lnsd's forward of it onto
//! the radio, the board learning the path, the board encrypting and sending a
//! report, and lnsd's routing decision on receipt.
//!
//! The second variant adds the third node the evening had on the air: another
//! LNode relaying announces on the same medium. It is the only source in the
//! scenario for a `transport_id` that is neither the board's nor lnsd's, so a
//! path learned through it is the one way the board can address a report at
//! somebody who is not the receiver. The relay then leaves the bus, exactly as
//! it did in the measurement, and the board sends again.
//!
//! Two outcomes, both diagnostic:
//!   - RED: the drop reproduces in pure path/announce/routing logic, and the
//!     mechanism is here rather than on the air.
//!   - GREEN: this topology delivers host-side, which puts the remaining
//!     question outside the modelled logic — on the wire, on the identity the
//!     daemon runs with, or on state the model does not carry.
//!
//! Medium under test is modelled, not radiated: `MockInterface` on a
//! `MockClock`, so there is no second medium to switch off.
//!
//! Sans-I/O: no LoRa, no Docker, no Python, sub-second wall clock.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// lnsd's interface to the Python client over the shared instance.
const LNSD_LOCAL: usize = 0;
/// lnsd's RNode interface, the LoRa medium the board is on.
const LNSD_RADIO: usize = 1;
/// The board's only interface.
const BOARD_RADIO: usize = 0;
/// The second LNode's only interface.
const RELAY_RADIO: usize = 0;

/// Simulated time per round of the in-process medium.
const STEP_MS: u64 = 1_000;
/// Rounds a path learning or a path request is given to settle.
const ROUNDS: usize = 12;

fn make_node() -> Node {
    NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

fn add_iface(node: &mut Node, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// The destination the reports are addressed at, owned by the client process
/// behind lnsd's shared instance.
fn register_target(node: &mut Node) -> DestinationHash {
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap();
    let hash = *dest.hash();
    node.register_destination(dest);
    hash
}

/// Every frame an output puts on the wire that reaches `on_iface`.
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

fn is_data_for(frame: &[u8], dest: &DestinationHash) -> bool {
    match Packet::unpack(frame) {
        Ok(p) => p.flags.packet_type == PacketType::Data && &p.destination_hash == dest.as_bytes(),
        Err(_) => false,
    }
}

/// What the modelled evening produced.
struct Trace {
    /// Hop count the board ended up holding for the target.
    board_hops: Option<u8>,
    /// Header type of the report the board actually put on the air.
    report_header: Option<HeaderType>,
    /// The transport id it addressed the report at, if any.
    report_transport_id: Option<[u8; TRUNCATED_HASHBYTES]>,
    /// lnsd's own transport identity, the id its HEADER_2 filter compares to.
    lnsd_identity: [u8; TRUNCATED_HASHBYTES],
    /// `overheard_transport_id` drops lnsd counted while taking the report.
    overheard_drops: u64,
    /// Whether lnsd handed the report on towards the client that hosts the
    /// target.
    delivered_to_client: bool,
}

/// One modelled report, from the client's announce to lnsd's routing decision.
///
/// With `with_relay`, a second LNode sits on the same medium while the path is
/// learned and is off the bus by the time the report is sent.
fn run(with_relay: bool) -> Trace {
    // The client process behind lnsd's shared instance. Transport-disabled and
    // single-interface, like a Python `lxmd` attached over IPC.
    let mut client = NodeCoreBuilder::new().enable_transport(false).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let client_ipc = client
        .transport
        .register_interface(Box::new(MockInterface::new("ipc", 0)));
    client.set_interface_name(client_ipc, String::from("ipc"));
    let target = register_target(&mut client);

    let mut lnsd = make_node();
    assert_eq!(add_iface(&mut lnsd, "local_rns"), LNSD_LOCAL);
    assert_eq!(add_iface(&mut lnsd, "rnode_0"), LNSD_RADIO);
    lnsd.transport.set_local_client(LNSD_LOCAL, true);
    let lnsd_identity = *lnsd.transport.identity().hash();

    let mut board = make_node();
    assert_eq!(add_iface(&mut board, "lora_sx1262"), BOARD_RADIO);

    let mut relay = make_node();
    assert_eq!(add_iface(&mut relay, "lora_sx1262"), RELAY_RADIO);

    // The client announces its delivery destination over the shared instance.
    let out = client
        .announce_destination(&target, Some(b"lxmf"))
        .expect("client announce");
    let client_announce = bound_for(&out, client_ipc);
    assert!(
        !client_announce.is_empty(),
        "the client must put its announce on the IPC link"
    );

    // Frames in flight on the LoRa medium, tagged with who sent them so a node
    // never hears its own transmission.
    const LNSD: u8 = 0;
    const BOARD: u8 = 1;
    const RELAY: u8 = 2;
    let mut air: Vec<(u8, Vec<u8>)> = Vec::new();

    for frame in client_announce {
        let out = lnsd.handle_packet(InterfaceId(LNSD_LOCAL), &frame);
        for f in bound_for(&out, LNSD_RADIO) {
            air.push((LNSD, f));
        }
    }

    // Let the announce find its way across the medium. lnsd's forward of a
    // local client's announce is scheduled, so the clock has to run.
    for _ in 0..ROUNDS {
        let flight = core::mem::take(&mut air);
        for (from, frame) in flight {
            if from != LNSD {
                let out = lnsd.handle_packet(InterfaceId(LNSD_RADIO), &frame);
                for f in bound_for(&out, LNSD_RADIO) {
                    air.push((LNSD, f));
                }
            }
            if from != BOARD {
                let out = board.handle_packet(InterfaceId(BOARD_RADIO), &frame);
                for f in bound_for(&out, BOARD_RADIO) {
                    air.push((BOARD, f));
                }
            }
            if with_relay && from != RELAY {
                let out = relay.handle_packet(InterfaceId(RELAY_RADIO), &frame);
                for f in bound_for(&out, RELAY_RADIO) {
                    air.push((RELAY, f));
                }
            }
        }

        for node in [&mut lnsd, &mut board, &mut relay] {
            let now = node.transport().clock().now_ms();
            node.transport().clock().set(now + STEP_MS);
        }

        let out = lnsd.handle_timeout();
        for f in bound_for(&out, LNSD_RADIO) {
            air.push((LNSD, f));
        }
        let out = board.handle_timeout();
        for f in bound_for(&out, BOARD_RADIO) {
            air.push((BOARD, f));
        }
        if with_relay {
            let out = relay.handle_timeout();
            for f in bound_for(&out, RELAY_RADIO) {
                air.push((RELAY, f));
            }
        }
    }

    let board_hops = board.hops_to(&target);

    // The relay leaves the bus, as both did in the measurement, and the board
    // sends its report on the otherwise silent channel.
    let mut report_header = None;
    let mut report_transport_id = None;
    let mut overheard_drops = 0;
    let mut delivered_to_client = false;

    if board_hops.is_some() {
        let (_, out) = board
            .send_single_packet(&target, b"[TELEMETRY] report reason=movement")
            .expect("the board holds a path, so the report must be routable");
        let on_air = bound_for(&out, BOARD_RADIO);
        for frame in &on_air {
            if !is_data_for(frame, &target) {
                continue;
            }
            let parsed = Packet::unpack(frame).expect("the board's own frame must parse");
            report_header = Some(parsed.flags.header_type);
            report_transport_id = parsed.transport_id;

            let before = lnsd.transport_stats().drops_overheard_transport_id();
            let out = lnsd.handle_packet(InterfaceId(LNSD_RADIO), frame);
            let after = lnsd.transport_stats().drops_overheard_transport_id();
            overheard_drops += after - before;
            delivered_to_client |= bound_for(&out, LNSD_LOCAL)
                .iter()
                .any(|f| is_data_for(f, &target));
        }
    }

    Trace {
        board_hops,
        report_header,
        report_transport_id,
        lnsd_identity,
        overheard_drops,
        delivered_to_client,
    }
}

fn describe(t: &Trace) -> String {
    std::format!(
        "board_hops={:?} report_header={:?} report_transport_id={:?} \
         lnsd_identity={:?} overheard_drops={} delivered={}",
        t.board_hops,
        t.report_header,
        t.report_transport_id.map(|h| h[..4].to_vec()),
        &t.lnsd_identity[..4],
        t.overheard_drops,
        t.delivered_to_client,
    )
}

/// THE question (#344): does a report to a destination behind lnsd's shared
/// instance reach that client, or does lnsd discard its own traffic?
#[test]
fn client_hosted_target_takes_the_report() {
    let t = run(false);

    assert!(
        t.board_hops.is_some(),
        "precondition: the board must learn a path to the client-hosted \
         target before it can report to it ({})",
        describe(&t)
    );
    assert_eq!(
        t.overheard_drops,
        0,
        "lnsd discarded a report for a destination it serves itself as \
         overheard ({})",
        describe(&t)
    );
    assert!(
        t.delivered_to_client,
        "lnsd took the report but never handed it to the client that hosts \
         the target ({})",
        describe(&t)
    );
}

/// The same report with the evening's third node on the air while the path is
/// learned. A relay is the only source in this scenario for a transport id
/// that is neither endpoint's, so if the board can be made to address its
/// report at one, this is where it happens.
#[test]
fn a_relay_on_the_medium_does_not_misaddress_the_report() {
    let t = run(true);

    assert!(
        t.board_hops.is_some(),
        "precondition: the board must learn a path to the client-hosted \
         target ({})",
        describe(&t)
    );
    if let Some(id) = t.report_transport_id {
        assert_eq!(
            id,
            t.lnsd_identity,
            "the board addressed its report at a transport id that is not the \
             receiver's ({})",
            describe(&t)
        );
    }
    assert_eq!(
        t.overheard_drops,
        0,
        "lnsd discarded the report as overheard after a relay had been on the \
         medium ({})",
        describe(&t)
    );
    assert!(
        t.delivered_to_client,
        "lnsd never handed the report to the client that hosts the target ({})",
        describe(&t)
    );
}

/// The same evening with one link missing: the board never hears lnsd
/// directly, only the second LNode repeating it.
///
/// This is the asymmetry a lossy medium produces routinely and the two
/// variants above cannot: as long as lnsd's own frame reaches the board, the
/// board holds a one-hop path and addresses nobody. Here it holds a two-hop
/// path through the relay, and every report it sends names the relay.
struct RelayOnlyTrace {
    board_hops: Option<u8>,
    report_transport_id: Option<[u8; TRUNCATED_HASHBYTES]>,
    relay_identity: [u8; TRUNCATED_HASHBYTES],
    lnsd_identity: [u8; TRUNCATED_HASHBYTES],
    overheard_drops: u64,
    delivered_to_client: bool,
    /// Hop count after the board asked for the path again and lnsd answered.
    board_hops_after_path_request: Option<u8>,
}

/// The nodes as the relay leaves the bus: the board holds the target at two
/// hops through the relay, and from here the board and lnsd hear each other
/// directly.
struct RelayDeparted {
    lnsd: Node,
    board: Node,
    target: DestinationHash,
    relay_identity: [u8; TRUNCATED_HASHBYTES],
    lnsd_identity: [u8; TRUNCATED_HASHBYTES],
}

/// The path learning of [`run_relay_only`]: lnsd's frames reach the board only
/// through the relay.
fn learn_through_the_relay_only() -> RelayDeparted {
    let mut client = NodeCoreBuilder::new().enable_transport(false).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let client_ipc = client
        .transport
        .register_interface(Box::new(MockInterface::new("ipc", 0)));
    client.set_interface_name(client_ipc, String::from("ipc"));
    let target = register_target(&mut client);

    let mut lnsd = make_node();
    add_iface(&mut lnsd, "local_rns");
    add_iface(&mut lnsd, "rnode_0");
    lnsd.transport.set_local_client(LNSD_LOCAL, true);
    let lnsd_identity = *lnsd.transport.identity().hash();

    let mut board = make_node();
    add_iface(&mut board, "lora_sx1262");

    let mut relay = make_node();
    add_iface(&mut relay, "lora_sx1262");
    let relay_identity = *relay.transport.identity().hash();

    let out = client
        .announce_destination(&target, Some(b"lxmf"))
        .expect("client announce");
    let client_announce = bound_for(&out, client_ipc);

    // Everything lnsd says goes to the relay only: the board is out of earshot
    // of lnsd's own frames for the duration of the path learning.
    let mut to_relay: Vec<Vec<u8>> = Vec::new();
    let mut to_board: Vec<Vec<u8>> = Vec::new();
    let mut to_lnsd: Vec<Vec<u8>> = Vec::new();

    for frame in client_announce {
        let out = lnsd.handle_packet(InterfaceId(LNSD_LOCAL), &frame);
        to_relay.extend(bound_for(&out, LNSD_RADIO));
    }

    for _ in 0..ROUNDS {
        for frame in core::mem::take(&mut to_relay) {
            let out = relay.handle_packet(InterfaceId(RELAY_RADIO), &frame);
            let emitted = bound_for(&out, RELAY_RADIO);
            to_board.extend(emitted.iter().cloned());
            to_lnsd.extend(emitted);
        }
        for frame in core::mem::take(&mut to_board) {
            let out = board.handle_packet(InterfaceId(BOARD_RADIO), &frame);
            to_relay.extend(bound_for(&out, BOARD_RADIO));
        }
        for frame in core::mem::take(&mut to_lnsd) {
            let out = lnsd.handle_packet(InterfaceId(LNSD_RADIO), &frame);
            to_relay.extend(bound_for(&out, LNSD_RADIO));
        }

        for node in [&mut lnsd, &mut board, &mut relay] {
            let now = node.transport().clock().now_ms();
            node.transport().clock().set(now + STEP_MS);
        }

        let out = lnsd.handle_timeout();
        to_relay.extend(bound_for(&out, LNSD_RADIO));
        let out = relay.handle_timeout();
        let emitted = bound_for(&out, RELAY_RADIO);
        to_board.extend(emitted.iter().cloned());
        to_lnsd.extend(emitted);
        let out = board.handle_timeout();
        to_relay.extend(bound_for(&out, BOARD_RADIO));
    }

    RelayDeparted {
        lnsd,
        board,
        target,
        relay_identity,
        lnsd_identity,
    }
}

fn run_relay_only() -> RelayOnlyTrace {
    let RelayDeparted {
        mut lnsd,
        mut board,
        target,
        relay_identity,
        lnsd_identity,
    } = learn_through_the_relay_only();
    let board_hops = board.hops_to(&target);

    // The relay leaves the bus. From here the board and lnsd hear each other
    // directly, exactly as in the repeated trial on the silent channel.
    let mut report_transport_id = None;
    let mut overheard_drops = 0;
    let mut delivered_to_client = false;
    let mut board_to_lnsd: Vec<Vec<u8>> = Vec::new();

    if board_hops.is_some() {
        let (_, out) = board
            .send_single_packet(&target, b"[TELEMETRY] report reason=movement")
            .expect("the board holds a path, so the report must be routable");
        board_to_lnsd.extend(bound_for(&out, BOARD_RADIO));
    }

    for frame in core::mem::take(&mut board_to_lnsd) {
        if !is_data_for(&frame, &target) {
            continue;
        }
        let parsed = Packet::unpack(&frame).expect("the board's own frame must parse");
        report_transport_id = parsed.transport_id;

        let before = lnsd.transport_stats().drops_overheard_transport_id();
        let out = lnsd.handle_packet(InterfaceId(LNSD_RADIO), &frame);
        let after = lnsd.transport_stats().drops_overheard_transport_id();
        overheard_drops += after - before;
        delivered_to_client |= bound_for(&out, LNSD_LOCAL)
            .iter()
            .any(|f| is_data_for(f, &target));
    }

    // And then the board asks for the path again, as the field log shows it
    // doing, and lnsd answers from its own table. Does the stale two-hop entry
    // through the departed relay give way?
    let mut air: Vec<(bool, Vec<u8>)> = Vec::new(); // (from_board, frame)
    let out = board.request_path(&target);
    for f in bound_for(&out, BOARD_RADIO) {
        air.push((true, f));
    }
    for _ in 0..ROUNDS {
        for (from_board, frame) in core::mem::take(&mut air) {
            if from_board {
                let out = lnsd.handle_packet(InterfaceId(LNSD_RADIO), &frame);
                for f in bound_for(&out, LNSD_RADIO) {
                    air.push((false, f));
                }
            } else {
                let out = board.handle_packet(InterfaceId(BOARD_RADIO), &frame);
                for f in bound_for(&out, BOARD_RADIO) {
                    air.push((true, f));
                }
            }
        }
        for node in [&mut lnsd, &mut board] {
            let now = node.transport().clock().now_ms();
            node.transport().clock().set(now + STEP_MS);
        }
        let out = lnsd.handle_timeout();
        for f in bound_for(&out, LNSD_RADIO) {
            air.push((false, f));
        }
        let out = board.handle_timeout();
        for f in bound_for(&out, BOARD_RADIO) {
            air.push((true, f));
        }
    }

    RelayOnlyTrace {
        board_hops,
        report_transport_id,
        relay_identity,
        lnsd_identity,
        overheard_drops,
        delivered_to_client,
        board_hops_after_path_request: board.hops_to(&target),
    }
}

fn describe_relay_only(t: &RelayOnlyTrace) -> String {
    std::format!(
        "board_hops={:?} report_transport_id={:?} relay_identity={:?} \
         lnsd_identity={:?} overheard_drops={} delivered={} hops_after_pr={:?}",
        t.board_hops,
        t.report_transport_id.map(|h| h[..4].to_vec()),
        &t.relay_identity[..4],
        &t.lnsd_identity[..4],
        t.overheard_drops,
        t.delivered_to_client,
        t.board_hops_after_path_request,
    )
}

/// The measured line, reproduced: a board that learned its path through a
/// relay addresses every report at that relay, and the receiver — which
/// serves the destination itself — refuses it.
///
/// This is the only route the modelled topology has to the `transport ID
/// mismatch` line, which makes it the first candidate for what the rig saw —
/// not proof that the rig saw it. What it does settle is that the line is
/// reachable with the receiver behaving correctly: refusing a HEADER_2 packet
/// not addressed to us is what the reference does too (Transport.py:1342-1345),
/// so the drop itself is not the defect. Reading the carried id off the
/// `PKT_LOCAL_DROP` line (landed in 6a7e05d8) on one rerun tells the two
/// candidates apart: a relay's identity means this mechanism, anything else
/// means the board and the receiver disagree about an id neither relayed.
#[test]
fn a_path_learned_through_a_relay_addresses_the_report_at_the_relay() {
    let t = run_relay_only();

    assert_eq!(
        t.board_hops,
        Some(2),
        "precondition: hearing only the relay must leave the board a two-hop \
         path ({})",
        describe_relay_only(&t)
    );
    assert_eq!(
        t.report_transport_id,
        Some(t.relay_identity),
        "the report must be addressed at the hop the path names ({})",
        describe_relay_only(&t)
    );
    assert_ne!(
        t.report_transport_id,
        Some(t.lnsd_identity),
        "and that hop is not the receiver, which is why its filter refuses it \
         ({})",
        describe_relay_only(&t)
    );
    assert_eq!(
        t.overheard_drops,
        1,
        "the receiver counts exactly the drop the rig measured ({})",
        describe_relay_only(&t)
    );
    assert!(
        !t.delivered_to_client,
        "and the client that hosts the destination never sees it ({})",
        describe_relay_only(&t)
    );
}

/// The recovery the field log shows being attempted: the board asks for the
/// path again on the now-silent channel and the receiver answers from its own
/// table. Whether the stale two-hop entry gives way decides whether #344 is a
/// transient or a permanent black hole.
#[test]
fn asking_again_heals_the_path_through_the_departed_relay() {
    let t = run_relay_only();

    assert_eq!(
        t.board_hops_after_path_request,
        Some(1),
        "a path request answered by the destination's own host must replace \
         the entry through a relay that is no longer there; while it does not, \
         every further report is addressed at the departed relay and silently \
         discarded ({})",
        describe_relay_only(&t)
    );
}

/// What the board's owed retransmission did after the relay left.
struct RetryTrace {
    /// The first report never reached anyone who proves it.
    first_failed: bool,
    /// Path requests the board put on the air between the loss and the retry.
    path_requests: usize,
    /// The transport id the retransmission was addressed at, `None` for a
    /// HEADER_1 frame (no next hop named).
    retry_transport_id: Option<Option<[u8; TRUNCATED_HASHBYTES]>>,
    relay_identity: [u8; TRUNCATED_HASHBYTES],
    retry_delivered_to_client: bool,
}

/// The board's side of #344 after the relay departs, driven by the
/// firmware's own rule for an owed retransmission
/// ([`leviculum_telemetry_policy::retry_step`], which
/// `leviculum-nrf/src/telemetry.rs` calls on every tick) and its own
/// [`ProofTracker`](leviculum_telemetry_policy::ProofTracker).
fn run_retry_after_the_relay_departs() -> RetryTrace {
    use leviculum_telemetry_policy::{retry_step, FailureVerdict, ProofTracker, RetryStep};

    let RelayDeparted {
        mut lnsd,
        mut board,
        target,
        relay_identity,
        ..
    } = learn_through_the_relay_only();
    assert_eq!(
        board.hops_to(&target),
        Some(2),
        "precondition: the board must hold the two-hop path through the relay"
    );
    let path_request_dest = *board.transport.path_request_hash();

    let mut tracker = ProofTracker::new();
    let mut path_asked = false;
    let mut first_failed = false;
    let mut path_requests = 0;
    let mut retry_transport_id = None;
    let mut retry_hash = None;
    let mut retry_delivered_to_client = false;

    // (from_board, frame) on the board-lnsd channel; the relay is gone.
    let mut air: Vec<(bool, Vec<u8>)> = Vec::new();

    let (first, out) = board
        .send_single_packet(&target, b"[TELEMETRY] report reason=movement")
        .expect("the board holds a path, so the report must be routable");
    tracker.note_report_sent(first, board.transport().clock().now_ms());
    for f in bound_for(&out, BOARD_RADIO) {
        air.push((true, f));
    }

    // Long enough for the receipt to time out and the retry to be paid.
    const RETRY_ROUNDS: usize = 120;
    for _ in 0..RETRY_ROUNDS {
        let mut board_events = Vec::new();
        for (from_board, frame) in core::mem::take(&mut air) {
            if from_board {
                if let Ok(p) = Packet::unpack(&frame) {
                    if p.destination_hash == path_request_dest {
                        path_requests += 1;
                    }
                }
                let is_retry = retry_hash.is_some() && is_data_for(&frame, &target);
                let out = lnsd.handle_packet(InterfaceId(LNSD_RADIO), &frame);
                if is_retry {
                    retry_delivered_to_client |= bound_for(&out, LNSD_LOCAL)
                        .iter()
                        .any(|f| is_data_for(f, &target));
                }
                for f in bound_for(&out, LNSD_RADIO) {
                    air.push((false, f));
                }
            } else {
                let out = board.handle_packet(InterfaceId(BOARD_RADIO), &frame);
                for f in bound_for(&out, BOARD_RADIO) {
                    air.push((true, f));
                }
                board_events.extend(out.events);
            }
        }
        for node in [&mut lnsd, &mut board] {
            let now = node.transport().clock().now_ms();
            node.transport().clock().set(now + STEP_MS);
        }
        let now = board.transport().clock().now_ms();
        let out = lnsd.handle_timeout();
        for f in bound_for(&out, LNSD_RADIO) {
            air.push((false, f));
        }
        let out = board.handle_timeout();
        for f in bound_for(&out, BOARD_RADIO) {
            air.push((true, f));
        }
        board_events.extend(out.events);

        for event in &board_events {
            if let crate::node::NodeEvent::DeliveryFailed { packet_hash, .. } = event {
                if let FailureVerdict::RetryDue { .. } = tracker.note_failure(packet_hash, now) {
                    first_failed = true;
                }
            }
        }

        if tracker.awaiting_retry().is_none() {
            continue;
        }
        let step = retry_step(board.has_path(&target), path_asked);
        let out = match step {
            RetryStep::Wait => continue,
            RetryStep::AskForPath => {
                path_asked = true;
                board.request_path(&target)
            }
            RetryStep::Rediscover => {
                path_asked = true;
                board.remove_path(target.as_bytes());
                board.request_path(&target)
            }
            RetryStep::Send => {
                let (hash, out) = board
                    .send_single_packet(&target, b"[TELEMETRY] report reason=movement")
                    .expect("the rule only sends along a known path");
                tracker.note_retry_sent(hash);
                retry_hash = Some(hash);
                for f in bound_for(&out, BOARD_RADIO) {
                    if is_data_for(&f, &target) {
                        let parsed = Packet::unpack(&f).expect("the board's own frame must parse");
                        retry_transport_id = Some(parsed.transport_id);
                    }
                }
                out
            }
        };
        for f in bound_for(&out, BOARD_RADIO) {
            air.push((true, f));
        }
    }

    RetryTrace {
        first_failed,
        path_requests,
        retry_transport_id,
        relay_identity,
        retry_delivered_to_client,
    }
}

fn describe_retry(t: &RetryTrace) -> String {
    std::format!(
        "first_failed={} path_requests={} retry_transport_id={:?} relay_identity={:?} \
         retry_delivered={}",
        t.first_failed,
        t.path_requests,
        t.retry_transport_id.map(|id| id.map(|h| h[..4].to_vec())),
        &t.relay_identity[..4],
        t.retry_delivered_to_client,
    )
}

/// The last gap of #344: a report goes out along a path through a relay, the
/// relay departs, no proof comes back, and the one retransmission the board
/// owes must not follow the same dead next hop.
///
/// The reference drops the path and asks again in exactly this case: an
/// opportunistic message still unproven while a path is held gets
/// `drop_path` and then `request_path` before its next attempt
/// (`reference/LXMF/LXMF/LXMRouter.py:2743-2751`). The board takes the
/// mechanism within its single retry: drop, ask once, and send the retry
/// when the answer names a live next hop.
#[test]
fn an_unproven_report_asks_for_the_path_again_before_its_retry() {
    let t = run_retry_after_the_relay_departs();

    assert!(
        t.first_failed,
        "precondition: the report addressed at the departed relay must go \
         unproven ({})",
        describe_retry(&t)
    );
    assert_eq!(
        t.path_requests,
        1,
        "the board held a path when the proof failed to come: a path whose \
         next hop did not prove is asked for again, once per owed retry and \
         never in a loop ({})",
        describe_retry(&t)
    );
    let Some(retry_transport_id) = t.retry_transport_id else {
        panic!("the retransmission never went out ({})", describe_retry(&t));
    };
    assert_ne!(
        retry_transport_id,
        Some(t.relay_identity),
        "the retransmission followed the same dead next hop as the report \
         ({})",
        describe_retry(&t)
    );
    assert!(
        t.retry_delivered_to_client,
        "the retransmission must reach the client that hosts the target ({})",
        describe_retry(&t)
    );
}
