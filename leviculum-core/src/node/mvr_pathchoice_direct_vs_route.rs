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
//! Sans-I/O: 1 node, 1 mock interface, deterministic (loss is the
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
