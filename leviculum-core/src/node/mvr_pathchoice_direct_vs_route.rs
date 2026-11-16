//! mvr for Codeberg #230 — a direct next hop that loses half its announces
//! versus a two-hop route that loses none.
//!
//! ## The failure mode, minimally
//!
//! charlie is in marginal direct radio range of alpha (every second
//! announce copy decodes) and in clean range of the relay bravo. Under the
//! shipped rule (`path_choice = hops`, [`PathChoice::Hops`]) alpha's path
//! flips to the direct link on every epoch whose direct copy happened to
//! decode — hop count is the whole rule, so half of alpha's traffic is
//! then sent onto a link that loses half of everything. Under
//! [`PathChoice::HopsAndLoss`] the announce re-hear evidence prices the
//! direct link at its bidirectional ETX (`1/0.5² = 4` > 2 route hops) and
//! alpha settles on the route through bravo.
//!
//! Both arms are driven through the REAL acceptance gate with the
//! identical copy sequence; the `Hops` arm doubles as the control pinning
//! today's behaviour — it is the red this fix turns green, asserted in the
//! same run.
//!
//! The norelay control pins the other edge (#230's safety clause): when
//! the lossy direct link is the ONLY route, the penalty must change
//! nothing — there is no same-emission competitor, so the cost rule is
//! never consulted and the path stays installed under both knob settings.
//!
//! Since order 1380 the evidence rule is the DEFAULT on radio interfaces:
//! the media-aware driver declares it per interface and the config's
//! `path_choice` overrides either way. The tests below the original three pin that
//! default against a TCP control on the same copy sequence (through
//! `run_room_by_medium`), and a mixed
//! radio-plus-TCP node pins that the penalty follows the medium of the
//! direct candidate.
//!
//! Sans-I/O: 1 node, 1-2 mock interfaces, deterministic (loss is the
//! alternating epochs, not a draw), sub-second.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{InterfaceId, PathChoice};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;

/// bravo's transport id: the relay carrying the clean two-hop route.
const BRAVO: [u8; TRUNCATED_HASHBYTES] = [0x42; TRUNCATED_HASHBYTES];

/// Epochs fed before the verdict is read. The evidence window needs
/// `DIRECT_EVIDENCE_MIN_KNOWN` (4) known emissions to price anything, so
/// eight alternating epochs put the direct link at a measured re-hear
/// ratio near 0.5 with the warmup well behind.
const EPOCHS: u64 = 8;

fn make_node(choice: PathChoice) -> Box<TransportNode> {
    NodeCoreBuilder::new()
        .enable_transport(true)
        .path_choice(choice)
        .build_boxed(
            OsRng,
            MockClock::new(TEST_TIME_MS),
            MemoryStorage::with_defaults(),
        )
}

fn add_iface(node: &mut TransportNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// charlie, announcing one fresh emission per epoch.
struct Charlie {
    dest_hash: crate::DestinationHash,
    dest: Destination,
}

impl Charlie {
    fn new() -> Self {
        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "mvrapp",
            &["pathchoice"],
        )
        .unwrap();
        let dest_hash = *dest.hash();
        Self { dest_hash, dest }
    }

    /// One fresh emission at `time_ms`: the raw direct copy (wire hops 0,
    /// processed at 1) and the same bytes after bravo forwarded them (wire
    /// hops 1, processed at 2). Only the unsigned header differs; the
    /// random blob and its emission timebase are shared, which is what
    /// makes the pair a same-emission race.
    fn emit(&mut self, time_ms: u64) -> (Vec<u8>, Vec<u8>) {
        let ann = self
            .dest
            .announce(None, &mut OsRng, time_ms, time_ms / 1000)
            .unwrap();
        let mut buf = [0u8; MTU];
        let len = ann.pack(&mut buf).unwrap();
        let direct = buf[..len].to_vec();

        let mut p = Packet::unpack(&direct).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = 1;
        p.transport_id = Some(BRAVO);
        let mut rbuf = [0u8; MTU];
        let rlen = p.pack(&mut rbuf).unwrap();
        (direct, rbuf[..rlen].to_vec())
    }
}

/// Drive one node through the alternating-loss room: epoch `e` decodes the
/// direct copy iff `e` is even; the relayed copy always arrives, one frame
/// later. Returns the node for end-state assertions.
fn run_room(choice: PathChoice, charlie: &mut Charlie) -> (Box<TransportNode>, usize) {
    let mut node = make_node(choice);
    let iface = add_iface(&mut node, "lora_sx1262");
    for epoch in 0..=EPOCHS {
        node.transport.clock().advance(10_000);
        let now = node.transport.clock().now_ms();
        let (direct, relayed) = charlie.emit(now);
        if epoch % 2 == 0 {
            let _ = node.handle_packet(InterfaceId(iface), &direct);
        }
        let _ = node.handle_packet(InterfaceId(iface), &relayed);
    }
    (node, iface)
}

/// The rule under test against its control, on the identical copy
/// sequence. The last epoch is even, so its direct copy DID decode: the
/// hop rule ends the run on the marginal direct link, the evidence rule
/// ends it on bravo.
#[test]
fn evidence_rule_routes_around_a_half_deaf_direct_link() {
    // Control: today's rule. This is the red the knob turns green.
    let mut charlie = Charlie::new();
    let (hops_node, _) = run_room(PathChoice::Hops, &mut charlie);
    let hops_path = hops_node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path installed");
    assert_eq!(
        (hops_path.hops, hops_path.next_hop),
        (1, None),
        "control (path_choice = hops): the epoch's decoded direct copy wins \
         on hop count, however lossy the link has shown itself — Codeberg #230"
    );

    // Same room, same sequence shape, the knob on.
    let mut charlie = Charlie::new();
    let ((loss_node, _), logs) = crate::test_log_capture::with_captured_logs(|| {
        run_room(PathChoice::HopsAndLoss, &mut charlie)
    });
    let loss_path = loss_node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path installed");
    assert_eq!(
        (loss_path.hops, loss_path.next_hop),
        (2, Some(BRAVO)),
        "path_choice = hops_and_loss: a direct link at re-hear ratio ~0.5 \
         prices at ETX ~4 > 2 route hops, so alpha holds the route through \
         bravo even on an epoch whose direct copy decoded"
    );
    // The swap is attributable: the relayed same-emission copy has MORE
    // wire hops, so it can only have entered through the cost arm.
    assert!(
        logs.contains("reason=\"same_emission_lower_cost_worse_hops\""),
        "the route hold must be attributable in PATH_ADD reasons; logs={logs:?}"
    );
}

/// Warmup control: before `DIRECT_EVIDENCE_MIN_KNOWN` emissions the
/// evidence prices nothing, and the two knob settings must decide the
/// first epoch identically (direct copy decoded → direct path).
#[test]
fn warmup_decides_like_the_hop_rule() {
    for choice in [PathChoice::Hops, PathChoice::HopsAndLoss] {
        let mut charlie = Charlie::new();
        let mut node = make_node(choice);
        let iface = add_iface(&mut node, "lora_sx1262");
        node.transport.clock().advance(10_000);
        let now = node.transport.clock().now_ms();
        let (direct, relayed) = charlie.emit(now);
        let _ = node.handle_packet(InterfaceId(iface), &direct);
        let _ = node.handle_packet(InterfaceId(iface), &relayed);
        let path = node
            .transport
            .get_path_clone(charlie.dest_hash.as_bytes())
            .expect("path installed");
        assert_eq!(
            (path.hops, path.next_hop),
            (1, None),
            "one emission is no evidence: {choice:?} must keep the direct \
             copy's path until the window has DIRECT_EVIDENCE_MIN_KNOWN \
             emissions"
        );
    }
}

/// The safety clause: the lossy direct link is the ONLY route (the
/// `pathchoice_norelay_loss50` room). No competitor, no cost comparison —
/// the penalty must never unroute the only path.
#[test]
fn norelay_keeps_the_only_path_under_both_rules() {
    for choice in [PathChoice::Hops, PathChoice::HopsAndLoss] {
        let mut charlie = Charlie::new();
        let mut node = make_node(choice);
        let iface = add_iface(&mut node, "lora_sx1262");
        for epoch in 0..=EPOCHS {
            node.transport.clock().advance(10_000);
            let now = node.transport.clock().now_ms();
            let (direct, _relayed) = charlie.emit(now);
            if epoch % 2 == 0 {
                let _ = node.handle_packet(InterfaceId(iface), &direct);
            }
            let path = node
                .transport
                .get_path_clone(charlie.dest_hash.as_bytes())
                .expect("the only path stays installed from the first decode on");
            assert_eq!(
                (path.hops, path.next_hop),
                (1, None),
                "{choice:?}: a bad score on the only route must change nothing"
            );
        }
    }
}

/// Order 1380: no override in the config, so each interface runs the rule
/// its medium declared. `declared` is what the media-aware driver hands the
/// core at registration (`Some(HopsAndLoss)` for a radio interface, nothing
/// for TCP and the other wired media).
fn run_room_by_medium(
    declared: Option<PathChoice>,
    charlie: &mut Charlie,
) -> (Box<TransportNode>, usize) {
    let mut node: Box<TransportNode> = NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    let iface = add_iface(&mut node, "medium_under_test");
    if let Some(choice) = declared {
        node.set_interface_path_choice(iface, choice);
    }
    for epoch in 0..=EPOCHS {
        node.transport.clock().advance(10_000);
        let now = node.transport.clock().now_ms();
        let (direct, relayed) = charlie.emit(now);
        if epoch % 2 == 0 {
            let _ = node.handle_packet(InterfaceId(iface), &direct);
        }
        let _ = node.handle_packet(InterfaceId(iface), &relayed);
    }
    (node, iface)
}

/// The default on a radio interface (Codeberg #230, order 1380): with no
/// `path_choice` in the config, an interface that declared
/// `HopsAndLoss` routes around the half-deaf direct link exactly as the
/// explicit knob does — and an interface that declared nothing (the TCP
/// control) keeps the hop rule on the identical copy sequence.
#[test]
fn radio_interface_defaults_to_the_evidence_rule_and_tcp_keeps_hops() {
    let mut charlie = Charlie::new();
    let (radio, iface) = run_room_by_medium(Some(PathChoice::HopsAndLoss), &mut charlie);
    assert_eq!(radio.interface_path_choice(iface), PathChoice::HopsAndLoss);
    let path = radio
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path installed");
    assert_eq!(
        (path.hops, path.next_hop),
        (2, Some(BRAVO)),
        "a radio interface with no override must price its marginal direct \
         link (re-hear ~0.5, ETX ~4) above the clean two-hop route"
    );

    let mut charlie = Charlie::new();
    let (wired, iface) = run_room_by_medium(None, &mut charlie);
    assert_eq!(wired.interface_path_choice(iface), PathChoice::Hops);
    let path = wired
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path installed");
    assert_eq!(
        (path.hops, path.next_hop),
        (1, None),
        "an interface that declared nothing (TCP, local) keeps hop count: the \
         decoded direct copy wins as it always did"
    );
}

/// The override wins either way: `path_choice = hops` holds a radio
/// interface on hop count, `path_choice = hops_and_loss` prices an
/// interface that declared nothing.
#[test]
fn config_override_wins_over_the_medium_default() {
    let mut charlie = Charlie::new();
    let mut node = make_node(PathChoice::Hops);
    let iface = add_iface(&mut node, "lora_sx1262");
    node.set_interface_path_choice(iface, PathChoice::HopsAndLoss);
    assert_eq!(node.interface_path_choice(iface), PathChoice::Hops);
    for epoch in 0..=EPOCHS {
        node.transport.clock().advance(10_000);
        let now = node.transport.clock().now_ms();
        let (direct, relayed) = charlie.emit(now);
        if epoch % 2 == 0 {
            let _ = node.handle_packet(InterfaceId(iface), &direct);
        }
        let _ = node.handle_packet(InterfaceId(iface), &relayed);
    }
    let path = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path installed");
    assert_eq!(
        (path.hops, path.next_hop),
        (1, None),
        "path_choice = hops overrides the radio default"
    );

    let node = make_node(PathChoice::HopsAndLoss);
    assert_eq!(node.interface_path_choice(7), PathChoice::HopsAndLoss);
}

/// Teardown takes the declaration with it: the index no longer prices
/// loss, and with no declaring interface left and no override the evidence
/// table is emptied, so a re-registered wired interface under the same
/// index starts on the hop rule with nothing inherited.
#[test]
fn teardown_forgets_the_declared_rule_and_its_evidence() {
    let mut charlie = Charlie::new();
    let (mut node, iface) = run_room_by_medium(Some(PathChoice::HopsAndLoss), &mut charlie);
    assert!(!node.transport.direct_link_evidence_is_empty());
    node.transport.remove_interface_path_choice(iface);
    assert_eq!(node.interface_path_choice(iface), PathChoice::Hops);
    assert!(node.transport.direct_link_evidence_is_empty());
}

/// A daemon with a radio and a TCP backbone (the shape `lnsd` runs on a
/// gateway): the penalty follows the medium of the DIRECT candidate. A
/// marginal direct link on the radio loses to the clean route that
/// arrives over TCP; a direct neighbour on TCP keeps its plain hop cost
/// even when the race involves the radio interface.
#[test]
fn the_penalty_follows_the_medium_of_the_direct_candidate() {
    let run = |direct_on_radio: bool| {
        let mut charlie = Charlie::new();
        let mut node: Box<TransportNode> =
            NodeCoreBuilder::new().enable_transport(true).build_boxed(
                OsRng,
                MockClock::new(TEST_TIME_MS),
                MemoryStorage::with_defaults(),
            );
        let radio = add_iface(&mut node, "lora_sx1262");
        let tcp = add_iface(&mut node, "tcp_backbone");
        node.set_interface_path_choice(radio, PathChoice::HopsAndLoss);
        let (direct_iface, relayed_iface) = if direct_on_radio {
            (radio, tcp)
        } else {
            (tcp, radio)
        };
        for epoch in 0..=EPOCHS {
            node.transport.clock().advance(10_000);
            let now = node.transport.clock().now_ms();
            let (direct, relayed) = charlie.emit(now);
            if epoch % 2 == 0 {
                let _ = node.handle_packet(InterfaceId(direct_iface), &direct);
            }
            let _ = node.handle_packet(InterfaceId(relayed_iface), &relayed);
        }
        let path = node
            .transport
            .get_path_clone(charlie.dest_hash.as_bytes())
            .expect("path installed");
        (path.hops, path.next_hop, path.interface_index)
    };
    let (h, n, iface) = run(true);
    assert_eq!(
        (h, n),
        (2, Some(BRAVO)),
        "a half-deaf direct radio link loses to the clean route over TCP"
    );
    assert_eq!(iface, 1, "the held route is the TCP one");
    let (h, n, _) = run(false);
    assert_eq!(
        (h, n),
        (1, None),
        "a direct TCP neighbour is not priced by announce re-hear: TCP does \
         not declare loss pricing, so its direct copy keeps its hop cost"
    );
}
