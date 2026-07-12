//! Deterministic resource receive-window measurement harness (Codeberg #85).
//!
//! The receive-window state machine is sans-I/O: `IncomingResource::
//! receive_part(&mut self, part, now_ms, ...)` derives eifr and every window
//! decision purely from `now_ms` deltas. Advancing the nodes' `MockClock` by
//! the airtime of each delivered packet therefore reproduces the REAL
//! eifr/round feedback of a paced link without any wall-clock waiting.
//!
//! `PacedDelivery` models a half-duplex link: for each packet the shared sim
//! clock advances by `pkt_len_bytes * 1000 / rate_bps` ms, plus a fixed
//! `turnaround_ms` whenever the transfer direction flips (data <-> REQ).
//! Optional deterministic loss drops every k-th RESOURCE-context DATA
//! transmission (the airtime still elapses: the frame was on the air, it just
//! arrives corrupt), forcing receiver timeouts and sender retransmissions.
//!
//! The tests below DOCUMENT the behavior of `WindowPolicy::Current` (they are
//! behavior anchors, not aspirations); `window_bench_sweep` is an `#[ignore]`d
//! bench that sweeps rate x loss x size per policy and appends one line per
//! cell to the file named by `WINBENCH_LOG` (libtest hides stdout, so results
//! go to a file, following the LEVICULUM_DELIVERY_LOG precedent).

extern crate std;

use std::collections::{BTreeSet, VecDeque};
use std::format;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{RESOURCE_WINDOW_INITIAL, RESOURCE_WINDOW_MAX_SLOW};
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{Packet, PacketContext};
use crate::resource::{
    ResourceStrategy, WindowPolicy, RESOURCE_WINDOW_FLEXIBILITY, RESOURCE_WINDOW_MAX_VERY_SLOW,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// Half-duplex turnaround used by all tests and the sweep: the fixed cost of
/// flipping the link direction (RX/TX switch plus preamble), a LoRa-ish 50 ms.
const TURNAROUND_MS: u64 = 50;

/// Hard step cap: no legitimate cell needs this many pump steps; hitting it
/// means the transfer stalled (a harness or protocol bug), so fail loudly.
const MAX_STEPS: usize = 500_000;

// ----------------------------------------------------------------------------
// Sans-I/O helpers (same pattern as mvr_response_resource).
// ----------------------------------------------------------------------------

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

fn packet_context(raw: &[u8]) -> Option<PacketContext> {
    Packet::unpack(raw).ok().map(|p| p.context)
}

/// The node holding the resource data: accepts links, pushes the resource.
fn make_sender() -> (EndpointNode, crate::DestinationHash, [u8; 32]) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["window"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    (node, dest_hash, signing_key)
}

/// The receiving node; its `WindowPolicy` is the variable under measurement.
fn make_receiver(policy: WindowPolicy) -> EndpointNode {
    let clock = MockClock::new(TEST_TIME_MS);
    NodeCoreBuilder::new()
        .resource_window_policy(policy)
        .build(OsRng, clock, NoStorage)
}

/// Deterministic, poorly-compressible payload (compression is off anyway so
/// part counts stay a pure function of `size`).
fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

// ----------------------------------------------------------------------------
// PacedDelivery harness
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    /// Sender -> receiver (ADV, DATA parts, HMU).
    ToReceiver,
    /// Receiver -> sender (link handshake, REQ, proof).
    ToSender,
}

/// Result of one paced transfer. `window_trajectory` holds one
/// `(round, window, window_max)` entry per observed completed round; the final
/// round assembles the resource in the same call that completes it (the
/// `IncomingResource` is gone afterwards), so `rounds` is the last observed
/// round plus one and the trajectory ends one round early.
#[derive(Debug)]
struct TransferResult {
    rounds: usize,
    sim_completion_ms: u64,
    parts_transmitted: usize,
    unique_parts: usize,
    retransmits: usize,
    window_trajectory: Vec<(usize, usize, usize)>,
    final_window: usize,
    final_window_max: usize,
    /// Receiver-side timeouts that re-sent a REQ.
    receiver_timeouts: usize,
    /// Receiver window immediately (before, after) each such timeout.
    timeout_window_pairs: Vec<(usize, usize)>,
}

/// Two-`NodeCore` in-process link with paced, optionally lossy delivery.
struct PacedDelivery {
    receiver: EndpointNode,
    sender: EndpointNode,
    rx_iface: usize,
    tx_iface: usize,
    link_id: LinkId,
    rate_bps: u64,
    turnaround_ms: u64,
    drop_every: Option<usize>,
    to_receiver: VecDeque<Vec<u8>>,
    to_sender: VecDeque<Vec<u8>>,
    last_dir: Option<Dir>,
    data_parts_seen: usize,
    parts_transmitted: usize,
    unique_part_packets: BTreeSet<Vec<u8>>,
    completion_ms: Option<u64>,
    completed_data: Option<Vec<u8>>,
    failed: bool,
    trajectory: Vec<(usize, usize, usize)>,
    last_rounds: usize,
    receiver_timeouts: usize,
    timeout_window_pairs: Vec<(usize, usize)>,
}

impl PacedDelivery {
    fn new(
        policy: WindowPolicy,
        rate_bps: u64,
        turnaround_ms: u64,
        drop_every: Option<usize>,
    ) -> (Self, crate::DestinationHash, [u8; 32]) {
        assert!(rate_bps > 0, "rate_bps must be positive");
        if let Some(k) = drop_every {
            assert!(k > 1, "drop_every=1 would drop every part forever");
        }
        let (mut sender, dest_hash, signing_key) = make_sender();
        let mut receiver = make_receiver(policy);
        let tx_iface = add_iface(&mut sender, "S_mesh");
        let rx_iface = add_iface(&mut receiver, "R_mesh");
        (
            Self {
                receiver,
                sender,
                rx_iface,
                tx_iface,
                link_id: LinkId::new([0u8; 16]),
                rate_bps,
                turnaround_ms,
                drop_every,
                to_receiver: VecDeque::new(),
                to_sender: VecDeque::new(),
                last_dir: None,
                data_parts_seen: 0,
                parts_transmitted: 0,
                unique_part_packets: BTreeSet::new(),
                completion_ms: None,
                completed_data: None,
                failed: false,
                trajectory: Vec::new(),
                last_rounds: 0,
                receiver_timeouts: 0,
                timeout_window_pairs: Vec::new(),
            },
            dest_hash,
            signing_key,
        )
    }

    fn now(&self) -> u64 {
        self.receiver.transport.clock().now_ms()
    }

    /// Advance the sim clock: both nodes' clocks move in lockstep, so they
    /// behave as one shared clock.
    fn advance(&self, ms: u64) {
        self.receiver.transport.clock().advance(ms);
        self.sender.transport.clock().advance(ms);
    }

    fn receiver_window(&self) -> Option<(usize, usize, usize)> {
        self.receiver
            .links
            .get(&self.link_id)
            .and_then(|l| l.incoming_resource())
            .map(|r| {
                (
                    r.window_state().rounds_completed(),
                    r.window_state().window(),
                    r.window_state().window_max(),
                )
            })
    }

    /// Record a trajectory point whenever the receiver completed a new round.
    fn observe_receiver_window(&mut self) {
        if let Some((rounds, window, window_max)) = self.receiver_window() {
            if rounds > self.last_rounds {
                self.last_rounds = rounds;
                self.trajectory.push((rounds, window, window_max));
            }
        }
    }

    fn note_receiver_events(&mut self, events: Vec<NodeEvent>) {
        for ev in events {
            match ev {
                NodeEvent::ResourceCompleted {
                    is_sender: false,
                    data,
                    ..
                } => {
                    self.completion_ms = Some(self.now());
                    self.completed_data = Some(data);
                }
                NodeEvent::ResourceFailed { .. } => self.failed = true,
                _ => {}
            }
        }
    }

    /// Deliver one packet with airtime pacing, turnaround on direction flips,
    /// and deterministic loss of every k-th RESOURCE-context DATA part.
    fn deliver(&mut self, dir: Dir, pkt: Vec<u8>) {
        if self.last_dir != Some(dir) {
            self.advance(self.turnaround_ms);
            self.last_dir = Some(dir);
        }
        self.advance(pkt.len() as u64 * 1000 / self.rate_bps);

        let mut dropped = false;
        if dir == Dir::ToReceiver && packet_context(&pkt) == Some(PacketContext::Resource) {
            self.parts_transmitted += 1;
            self.unique_part_packets.insert(pkt.clone());
            self.data_parts_seen += 1;
            if let Some(k) = self.drop_every {
                dropped = self.data_parts_seen.is_multiple_of(k);
            }
        }
        if dropped {
            // The frame was on the air (airtime elapsed above) but arrives
            // corrupt: the receiver never sees it.
            return;
        }

        match dir {
            Dir::ToReceiver => {
                let out = self
                    .receiver
                    .handle_packet(InterfaceId(self.rx_iface), &pkt);
                self.to_sender.extend(action_data(&out));
                self.note_receiver_events(out.events);
                self.observe_receiver_window();
            }
            Dir::ToSender => {
                let out = self.sender.handle_packet(InterfaceId(self.tx_iface), &pkt);
                self.to_receiver.extend(action_data(&out));
                for ev in out.events {
                    if matches!(ev, NodeEvent::ResourceFailed { .. }) {
                        self.failed = true;
                    }
                }
            }
        }
    }

    /// Pop and deliver the next queued packet. Returns false when idle.
    fn deliver_next(&mut self) -> bool {
        if let Some(pkt) = self.to_receiver.pop_front() {
            self.deliver(Dir::ToReceiver, pkt);
            true
        } else if let Some(pkt) = self.to_sender.pop_front() {
            self.deliver(Dir::ToSender, pkt);
            true
        } else {
            false
        }
    }

    /// Both queues are empty but the transfer is unfinished: jump the sim
    /// clock to the earliest node deadline and fire the timeout handlers.
    /// Receiver timeouts that re-send a REQ are recorded together with the
    /// window before/after, so tests can pin the on_timeout behavior.
    fn timeout_step(&mut self) {
        let deadline = [self.receiver.next_deadline(), self.sender.next_deadline()]
            .into_iter()
            .flatten()
            .min()
            .expect("transfer incomplete but no node has a deadline: stalled");
        let now = self.now();
        self.advance(deadline.saturating_sub(now).max(1));

        let window_before = self.receiver_window();
        let out = self.receiver.handle_timeout();
        let pkts = action_data(&out);
        if pkts
            .iter()
            .any(|p| packet_context(p) == Some(PacketContext::ResourceReq))
        {
            self.receiver_timeouts += 1;
            if let (Some(b), Some(a)) = (window_before, self.receiver_window()) {
                self.timeout_window_pairs.push((b.1, a.1));
            }
        }
        self.to_sender.extend(pkts);
        self.note_receiver_events(out.events);

        let out = self.sender.handle_timeout();
        self.to_receiver.extend(action_data(&out));
        for ev in out.events {
            if matches!(ev, NodeEvent::ResourceFailed { .. }) {
                self.failed = true;
            }
        }
    }

    /// Drive a clean receiver <-> sender link to Active, paced like everything
    /// else so the measured link RTT reflects the configured rate/turnaround
    /// (the receiver's part-timeout is RTT-capped; an instant handshake would
    /// give it an unrealistic 1 ms cap).
    fn establish(&mut self, dest_hash: crate::DestinationHash, signing_key: &[u8; 32]) {
        let (link_id, _routed, out) = self.receiver.connect(dest_hash, signing_key);
        self.link_id = link_id;
        self.to_sender.extend(action_data(&out));

        let mut steps = 0usize;
        while self.deliver_next() {
            steps += 1;
            assert!(steps < MAX_STEPS, "link establishment did not quiesce");
        }
        assert_eq!(self.receiver.active_link_count(), 1, "receiver link active");
        assert_eq!(self.sender.active_link_count(), 1, "sender link active");
    }

    /// Pump packets (and, when idle, timeouts) until the receiver completed
    /// the resource and all queues drained.
    fn run_to_completion(&mut self) {
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(
                steps < MAX_STEPS,
                "transfer stalled after {} rounds at sim t={} ms",
                self.last_rounds,
                self.now()
            );
            assert!(!self.failed, "resource transfer failed in the harness");
            if !self.deliver_next() {
                if self.completion_ms.is_some() {
                    break;
                }
                self.timeout_step();
            }
        }
    }
}

/// Run one paced resource transfer and measure the receive-window behavior.
fn run_transfer(
    policy: WindowPolicy,
    size_bytes: usize,
    rate_bps: u64,
    turnaround_ms: u64,
    drop_every: Option<usize>,
) -> TransferResult {
    let (mut h, dest_hash, signing_key) =
        PacedDelivery::new(policy, rate_bps, turnaround_ms, drop_every);
    h.establish(dest_hash, &signing_key);

    h.receiver
        .set_resource_strategy(&h.link_id, ResourceStrategy::AcceptAll)
        .expect("receiver link must exist");

    let data = payload(size_bytes);
    let t0 = h.now();
    let (_resource_hash, out) = h
        .sender
        .send_resource(&h.link_id, &data, None, false)
        .expect("send_resource must advertise");
    h.to_receiver.extend(action_data(&out));

    h.run_to_completion();

    assert_eq!(
        h.completed_data.as_deref(),
        Some(data.as_slice()),
        "assembled data must match the sent payload"
    );

    let completion_ms = h.completion_ms.expect("run_to_completion sets this");
    let (final_window, final_window_max) = h
        .trajectory
        .last()
        .map(|&(_, w, m)| (w, m))
        .unwrap_or((RESOURCE_WINDOW_INITIAL, RESOURCE_WINDOW_MAX_SLOW));
    TransferResult {
        // The final round assembles inside the completing receive_part call
        // and the IncomingResource is cleared, so it is observed rounds + 1.
        rounds: h.last_rounds + 1,
        sim_completion_ms: completion_ms - t0,
        parts_transmitted: h.parts_transmitted,
        unique_parts: h.unique_part_packets.len(),
        retransmits: h.parts_transmitted - h.unique_part_packets.len(),
        window_trajectory: h.trajectory,
        final_window,
        final_window_max,
        receiver_timeouts: h.receiver_timeouts,
        timeout_window_pairs: h.timeout_window_pairs,
    }
}

// ----------------------------------------------------------------------------
// Current-policy behavior anchors. These DOCUMENT today's behavior; their
// meaning changes only once a better policy exists to compare against.
// ----------------------------------------------------------------------------

/// THE #85 reproducer: at ~342 B/s the first-part rate lands below
/// VERY_SLOW_RATE_THRESHOLD (turnaround + REQ airtime dominate the
/// measurement), so window_max is clamped to RESOURCE_WINDOW_MAX_VERY_SLOW
/// and the window can never leave 4. This is the pinned regression a future
/// policy must move.
#[test]
fn window_caps_at_4_at_342bps_current() {
    let r = run_transfer(WindowPolicy::Current, 20480, 342, TURNAROUND_MS, None);
    assert!(
        r.final_window <= 4,
        "window must stay capped at 4, got {} (trajectory: {:?})",
        r.final_window,
        r.window_trajectory
    );
    assert_eq!(
        r.final_window_max, RESOURCE_WINDOW_MAX_VERY_SLOW,
        "window_max must be clamped to VERY_SLOW (trajectory: {:?})",
        r.window_trajectory
    );
    // The clamp hits on the very first completed round and never releases.
    assert!(
        r.window_trajectory
            .iter()
            .all(|&(_, w, m)| w <= 4 && m == RESOURCE_WINDOW_MAX_VERY_SLOW),
        "no round may escape the VERY_SLOW clamp: {:?}",
        r.window_trajectory
    );
}

/// Divergence #1: at a mid rate (3000 B/s) the rate tier keeps window_max at
/// RESOURCE_WINDOW_MAX_SLOW, but growth needs window + FLEXIBILITY completed
/// rounds per +1 step, so a whole 50 KiB transfer ends before the window
/// reaches its ceiling.
///
/// The harness also exposes a second Current quirk this test tolerates and
/// documents: `handle_hashmap_update` builds the post-HMU REQ WITHOUT
/// refreshing `req_sent_ms`, so the next round's first-part rate is measured
/// from the stale previous REQ timestamp. That collapses the measured rate
/// below VERY_SLOW for exactly that round, clamps the window (6 -> 4 here),
/// and restarts the ramp from 4.
#[test]
fn current_reaches_only_slow_growth() {
    let r = run_transfer(WindowPolicy::Current, 51200, 3000, TURNAROUND_MS, None);
    // Every round sits in the SLOW tier except the HMU-boundary artifact
    // rounds described above, which clamp to VERY_SLOW for one round.
    for &(round, w, m) in &r.window_trajectory {
        if m == RESOURCE_WINDOW_MAX_VERY_SLOW {
            assert!(
                w <= RESOURCE_WINDOW_MAX_VERY_SLOW,
                "a VERY_SLOW clamp must also clamp the window (round {round})"
            );
        } else {
            assert_eq!(
                m, RESOURCE_WINDOW_MAX_SLOW,
                "mid rate must never leave the SLOW tier upward: {:?}",
                r.window_trajectory
            );
        }
    }
    // Growth cadence: every +1 step needs at least window + FLEXIBILITY
    // completed rounds since the previous growth.
    let mut window = RESOURCE_WINDOW_INITIAL;
    let mut round_at_growth = 0usize;
    let mut grew = false;
    for &(round, w, _) in &r.window_trajectory {
        if w > window {
            assert_eq!(w, window + 1, "window only ever grows by 1");
            assert!(
                round - round_at_growth >= window + RESOURCE_WINDOW_FLEXIBILITY,
                "window {} -> {} after only {} rounds (trajectory: {:?})",
                window,
                w,
                round - round_at_growth,
                r.window_trajectory
            );
            round_at_growth = round;
            grew = true;
        }
        window = w;
    }
    assert!(
        grew,
        "the window must have grown at all (trajectory: {:?})",
        r.window_trajectory
    );
    assert!(
        r.final_window < RESOURCE_WINDOW_MAX_SLOW,
        "a 50 KiB transfer must end before the slow ramp reaches window_max, got {}",
        r.final_window
    );
}

/// Divergence #4: a receiver-side part timeout retransmits the REQ but never
/// reduces the window (`on_timeout` is a no-op under Current).
#[test]
fn current_timeout_does_not_shrink() {
    let r = run_transfer(WindowPolicy::Current, 20480, 3000, TURNAROUND_MS, Some(10));
    assert!(
        r.receiver_timeouts >= 1,
        "the 10% loss pattern must force at least one receiver timeout \
         (retransmits: {})",
        r.retransmits
    );
    assert!(
        r.retransmits >= 1,
        "dropped parts must be retransmitted, got {:?}",
        r
    );
    assert!(
        r.timeout_window_pairs
            .iter()
            .all(|&(before, after)| after == before),
        "a timeout must not change the window: {:?}",
        r.timeout_window_pairs
    );
}

/// The deterministic analogue of the primary rig metric: the round count of a
/// 50 KiB transfer at 342 B/s under Current. A stable number a future policy
/// must beat (fewer rounds / lower sim_ms at equal integrity).
#[test]
fn transfer_50kib_at_342bps_current_round_count() {
    let r = run_transfer(WindowPolicy::Current, 51200, 342, TURNAROUND_MS, None);
    assert_eq!(
        r.rounds, 29,
        "pinned round count moved (sim_ms={}, trajectory: {:?})",
        r.sim_completion_ms, r.window_trajectory
    );
    assert_eq!(r.retransmits, 0, "lossless run must not retransmit");
    assert_eq!(
        r.parts_transmitted, r.unique_parts,
        "lossless run transmits every part exactly once"
    );
    assert_eq!(r.final_window, 4, "window stays capped at 4 (Codeberg #85)");
}

// ----------------------------------------------------------------------------
// WINBENCH sweep (a bench, not a pass/fail test).
// ----------------------------------------------------------------------------

/// Sweep every policy x rate x loss x size cell and append one line per cell
/// to the file named by `WINBENCH_LOG` (unset = run silently; the run itself
/// still validates every cell completes with intact data). Run via:
/// `WINBENCH_LOG=/tmp/winbench.csv cargo test -p leviculum-core --lib
/// window_bench_sweep -- --ignored`.
#[test]
#[ignore]
fn window_bench_sweep() {
    // Adding a WindowPolicy variant extends the sweep here, nothing else.
    let policies = [WindowPolicy::Current];
    // Straddles every threshold boundary of both stacks (VERY_SLOW 1000,
    // SLOW 15000, FAST 50000 B/s) so units/threshold divergence is visible.
    let rates: [u64; 8] = [250, 342, 500, 1500, 3000, 6250, 15000, 50000];
    // None, 2%, 5%, 10% deterministic part loss.
    let drops: [Option<usize>; 4] = [None, Some(50), Some(20), Some(10)];
    let sizes: [usize; 3] = [5120, 51200, 204800];

    let log_path = std::env::var("WINBENCH_LOG").ok();
    for policy in policies {
        for rate in rates {
            for drop in drops {
                for size in sizes {
                    let r = run_transfer(policy, size, rate, TURNAROUND_MS, drop);
                    let loss_pct = drop.map(|k| 100 / k).unwrap_or(0);
                    let line = format!(
                        "WINBENCH policy={:?} rate={} loss={} size={} rounds={} sim_ms={} retx={} wfinal={} wmax={}",
                        policy,
                        rate,
                        loss_pct,
                        size,
                        r.rounds,
                        r.sim_completion_ms,
                        r.retransmits,
                        r.final_window,
                        r.final_window_max,
                    );
                    if let Some(path) = &log_path {
                        use std::io::Write;
                        let mut f = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)
                            .expect("WINBENCH_LOG must be writable");
                        writeln!(f, "{line}").expect("WINBENCH_LOG write");
                    }
                }
            }
        }
    }
}
