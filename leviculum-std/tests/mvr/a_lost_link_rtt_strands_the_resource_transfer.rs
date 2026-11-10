//! mvr (1624): a lost LRRTT strands the responder link and every transfer on it.
//!
//! **The named failure mode**, from the 2026-11-10 hardware corpus
//! (`bench_dual_pair_mixed_transfer` step 5, `lora_4node_contention_rust`
//! step 4): the initiator's RTT packet (`LORA_TX len=83`) is lost on the air.
//! The initiator holds its link Active (it went Active on the proof), the
//! responder keeps its link in the handshake. The initiator then advertises a
//! resource five times over 60 s; the responder decrypts nothing of it,
//! because `handle_resource_adv` returns on a non-Active link without a word.
//! After the responder's establishment timeout `LINK_DIED
//! detail=handshake_timeout` closes its side and the initiator's transfer dies
//! with `Timeout`. Nothing in the exchange can recover: the initiator never
//! resends its RTT.
//!
//! The responder derived the link keys when it sent its proof, so any packet
//! of the initiator that decrypts under them proves the initiator holds those
//! keys too; the RTT packet is one such packet, not the only one.
//!
//! Host model: two `NodeCore`s on one virtual clock, one interface each, a
//! wire that drops exactly the first RTT packet the initiator sends and
//! carries everything else. The initiator starts a resource transfer the
//! moment its link is up. Virtual time runs to 120 s in 100 ms steps, so the
//! responder's establishment timeout and the sender's advertisement budget
//! both play out, deterministically, in well under a second of wall time.
//! Every node event and every wire packet goes into one timeline, printed on
//! every run.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::packet::{peek_wire_class, PacketContext};
use leviculum_core::transport::{DropReason, TickOutput};
use leviculum_core::{
    Action, Clock, Destination, DestinationType, Direction, Identity, InterfaceId, MemoryStorage,
    NodeCore, NodeCoreBuilder, NodeEvent, ProofStrategy, ResourceStrategy,
};
use rand_core::OsRng;

/// A clock both nodes read and the test alone moves.
#[derive(Clone)]
struct VirtualClock(Arc<AtomicU64>);

impl Clock for VirtualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn wall_unix_secs(&self) -> Option<u64> {
        Some(1_700_000_000 + self.now_ms() / 1000)
    }
}

type TestNode = NodeCore<OsRng, VirtualClock, MemoryStorage>;

const START_MS: u64 = 1_000_000;
/// Long enough for the responder's establishment timeout and the sender's
/// whole advertisement budget (the field run died at 71 s).
const RUN_MS: u64 = 120_000;
const STEP_MS: u64 = 100;
/// One full resource, several parts, small enough to stay one segment.
const PAYLOAD_LEN: usize = 4_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Initiator,
    Responder,
}

impl Side {
    fn tag(self) -> &'static str {
        match self {
            Side::Initiator => "I",
            Side::Responder => "R",
        }
    }
    fn other(self) -> Side {
        match self {
            Side::Initiator => Side::Responder,
            Side::Responder => Side::Initiator,
        }
    }
}

struct Sim {
    clock: Arc<AtomicU64>,
    initiator: TestNode,
    responder: TestNode,
    /// Packets on the wire, in send order: (receiving side, bytes).
    wire: VecDeque<(Side, Vec<u8>)>,
    rtt_dropped: bool,
    timeline: Vec<String>,
    events: Vec<(Side, NodeEvent)>,
}

impl Sim {
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst) - START_MS
    }

    fn node(&mut self, side: Side) -> &mut TestNode {
        match side {
            Side::Initiator => &mut self.initiator,
            Side::Responder => &mut self.responder,
        }
    }

    /// Put a node's output on the wire and into the timeline. The first RTT
    /// packet the initiator sends is the one the air lost.
    fn take(&mut self, from: Side, out: TickOutput) {
        let t = self.now();
        for action in out.actions {
            let data = match action {
                Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
            };
            let ctx = peek_wire_class(&data).map(|w| PacketContext::from_byte(w.context));
            let lost =
                from == Side::Initiator && !self.rtt_dropped && ctx == Some(PacketContext::Lrrtt);
            self.timeline.push(format!(
                "WIRE from={} ctx={:?} len={} lost={} t={t}",
                from.tag(),
                ctx,
                data.len(),
                lost
            ));
            if lost {
                self.rtt_dropped = true;
                continue;
            }
            self.wire.push_back((from.other(), data));
        }
        for event in out.events {
            self.timeline
                .push(format!("EVENT side={} {:?} t={t}", from.tag(), event));
            self.events.push((from, event));
        }
    }

    /// Deliver everything on the wire, including what the deliveries send. A
    /// drop the node layer decides surfaces on the node's next pass, which is
    /// the next [`Sim::tick`].
    fn drain(&mut self) {
        while let Some((to, data)) = self.wire.pop_front() {
            let out = self.node(to).handle_packet(InterfaceId(0), &data);
            self.take(to, out);
        }
    }

    fn tick(&mut self) {
        self.clock.fetch_add(STEP_MS, Ordering::SeqCst);
        for side in [Side::Initiator, Side::Responder] {
            let out = self.node(side).handle_timeout();
            self.take(side, out);
        }
        self.drain();
    }

    fn any(&self, side: Side, pred: impl Fn(&NodeEvent) -> bool) -> bool {
        self.events.iter().any(|(s, e)| *s == side && pred(e))
    }
}

/// The node pair and an initiator link that is up on the initiator's side,
/// with its RTT lost on the way to the responder.
fn handshake_with_lost_rtt() -> (Sim, leviculum_core::LinkId) {
    let clock = Arc::new(AtomicU64::new(START_MS));
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder = NodeCoreBuilder::new().build(
        OsRng,
        VirtualClock(clock.clone()),
        MemoryStorage::with_defaults(),
    );
    let mut destination = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrlostrtt",
        &["transfer"],
    )
    .expect("destination");
    destination.set_accepts_links(true);
    destination.set_proof_strategy(ProofStrategy::None);
    destination.set_resource_strategy(ResourceStrategy::AcceptAll);
    let destination_hash = *destination.hash();
    responder.register_destination(destination);

    let initiator = NodeCoreBuilder::new().build(
        OsRng,
        VirtualClock(clock.clone()),
        MemoryStorage::with_defaults(),
    );
    let mut sim = Sim {
        clock,
        initiator,
        responder,
        wire: VecDeque::new(),
        rtt_dropped: false,
        timeline: Vec::new(),
        events: Vec::new(),
    };
    let (link_id, _, out) = sim
        .initiator
        .connect(destination_hash, &signing_key)
        .expect("connect");
    sim.take(Side::Initiator, out);
    sim.drain();
    assert!(sim.rtt_dropped, "the handshake reached the RTT packet");
    assert!(
        sim.any(Side::Initiator, |e| matches!(
            e,
            NodeEvent::LinkEstablished {
                is_initiator: true,
                ..
            }
        )),
        "the initiator's link is up on the proof"
    );
    (sim, link_id)
}

fn print_timeline(sim: &Sim) {
    println!("--- timeline (I = initiator, R = responder) ---");
    for line in &sim.timeline {
        println!("{line}");
    }
}

/// The field shape: the RTT is lost, the initiator transfers a resource on
/// its Active link. The responder must take the transfer: its first correctly
/// encrypted packet from the initiator proves the initiator holds the link
/// keys, and the link is established on it. Red on the unfixed core with the
/// field's two deaths, the responder's handshake timeout and the initiator's
/// resource timeout.
#[test]
fn a_lost_link_rtt_strands_the_resource_transfer() {
    let (mut sim, link_id) = handshake_with_lost_rtt();

    let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| (i * 31 % 251) as u8).collect();
    let (_, out) = sim
        .initiator
        .send_resource(&link_id, &payload, None, false)
        .expect("the initiator's link is Active, so a resource can start");
    sim.take(Side::Initiator, out);
    sim.drain();

    let done = |sim: &Sim| {
        sim.any(Side::Responder, |e| {
            matches!(
                e,
                NodeEvent::ResourceCompleted {
                    is_sender: false,
                    ..
                }
            )
        }) && sim.any(Side::Initiator, |e| {
            matches!(
                e,
                NodeEvent::ResourceCompleted {
                    is_sender: true,
                    ..
                }
            )
        })
    };
    while sim.now() < RUN_MS && !done(&sim) {
        sim.tick();
    }
    print_timeline(&sim);

    let responder_closed = sim.any(Side::Responder, |e| {
        matches!(e, NodeEvent::LinkClosed { .. })
    });
    let initiator_failed = sim.any(Side::Initiator, |e| {
        matches!(e, NodeEvent::ResourceFailed { .. })
    });
    assert!(
        !responder_closed && !initiator_failed,
        "the lost RTT stranded the link: responder link closed={responder_closed} \
         (the field's LINK_DIED detail=handshake_timeout), initiator transfer \
         failed={initiator_failed} (the field's resource timeout); timeline above"
    );
    assert!(
        sim.any(Side::Responder, |e| matches!(
            e,
            NodeEvent::LinkEstablished {
                is_initiator: false,
                ..
            }
        )),
        "the responder's link is established by the initiator's first packet"
    );
    let received = sim.events.iter().find_map(|(s, e)| match (s, e) {
        (Side::Responder, NodeEvent::ResourceCompleted { data, .. }) => Some(data.clone()),
        _ => None,
    });
    assert_eq!(
        received.as_deref(),
        Some(payload.as_slice()),
        "the responder received the whole resource"
    );
    assert!(done(&sim), "both sides completed the transfer");
}

/// Decrypt failure stays a drop: a packet on the handshake link that does
/// not decrypt under the link keys neither establishes the link nor is taken,
/// and the drop is named. The genuine RTT still establishes the link after it.
#[test]
fn an_undecryptable_packet_does_not_establish_a_handshake_link() {
    let (mut sim, link_id) = handshake_with_lost_rtt();

    let (_, out) = sim
        .initiator
        .send_packet_on_link(&link_id, b"forged")
        .expect("initiator link is active");
    let mut forged = match &out.actions[..] {
        [Action::SendPacket { data, .. }] | [Action::Broadcast { data, .. }] => data.clone(),
        other => panic!("one packet expected, got {other:?}"),
    };
    if let Some(last) = forged.last_mut() {
        *last ^= 0x5a;
    }
    sim.wire.push_back((Side::Responder, forged));
    sim.drain();
    sim.tick();
    print_timeline(&sim);

    assert!(
        !sim.any(Side::Responder, |e| matches!(
            e,
            NodeEvent::LinkEstablished { .. } | NodeEvent::LinkDataReceived { .. }
        )),
        "a packet that does not decrypt must not establish or deliver"
    );
    assert!(
        sim.any(Side::Responder, |e| matches!(
            e,
            NodeEvent::PacketDropped {
                reason: DropReason::LinkNotActive,
                ..
            }
        )),
        "the refused packet is a named drop"
    );
    assert!(
        sim.responder
            .link(&link_id)
            .is_some_and(|l| l.state() == leviculum_core::link::LinkState::Handshake),
        "the link stays in its handshake"
    );
}
