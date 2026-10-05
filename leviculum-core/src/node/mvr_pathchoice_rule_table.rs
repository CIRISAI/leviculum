//! The #425 evidence table for Codeberg #230: three path-choice rules
//! replayed over the pathchoice rooms, sans-I/O, per loss level and seed.
//!
//! ## What is replayed
//!
//! The periculum `emulated/pathchoice_loss*` room:
//!
//! ```text
//!          bravo            alpha-bravo clean, bravo-charlie clean
//!         /     \           alpha-charlie audible but losing a fraction
//!    alpha - - - charlie    L of frames each way
//! ```
//!
//! Per announce epoch, alpha hears charlie's DIRECT copy with probability
//! `1-L` (one seeded draw) and bravo's relayed copy always, one frame
//! later. Between epochs alpha sends probes to charlie over whatever next
//! hop its table holds; a probe is one frame out and one proof back, each
//! crossing drawn against the same loss when the route is the direct link
//! and clean when it is the two-hop route. Each (loss, seed) schedule is
//! generated ONCE and consumed by every rule, so the comparison is on the
//! rule alone — the A/B volume gate is satisfied by construction, and the
//! evidence rule reads only announces the hop rule also processed: its
//! extra announce traffic is zero by construction.
//!
//! ## The rules
//!
//! * **(i) `hops`** — the shipped rule, measured through the REAL
//!   acceptance gate (`NodeCore::handle_packet`, `path_choice = Hops`).
//! * **(ii) `hops_and_loss`** — the candidate, measured through the SAME
//!   real gate (`path_choice = HopsAndLoss`).
//! * **(iii) Python reference** — 1.3.5 as vendored
//!   (reference/Reticulum/RNS/Transport.py:1772: a same-emission copy
//!   never replaces; a newer emission always does, Transport.py:1797-1815)
//!   and 1.5.2 at default config (its gravity arm rejects at
//!   `DEFAULT_GRAVITY = 0`; prose line numbers in
//!   docs/src/protocol-notes/announce-dedup-and-path-replacement.md §5).
//!   In this room that collapses to "the epoch's first-heard copy wins",
//!   computed as a pure fold over the schedule. The fold doubles as the
//!   model cross-check for arm (i): our only same-emission deviation,
//!   `FewerHopsWins`, is unreachable when the shorter copy always arrives
//!   first, so (i) must equal (iii) on every metric here — asserted, which
//!   validates the replay against the real gate.
//!
//! ## The verdict (#425 step 2, pinned)
//!
//! The rule may change only if (ii) delivers more at ≥ 50 % loss without
//! losing at 0 % and without flapping more than (i). Each clause is an
//! assertion below; the table itself prints under `--nocapture`.
//!
//! Sans-I/O: one real node per arm, deterministic seeds, no sleeps.

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

const BRAVO: [u8; TRUNCATED_HASHBYTES] = [0x42; TRUNCATED_HASHBYTES];

/// 40 fresh emissions per run — the corpus soak's own sample size.
const EPOCHS: usize = 40;
/// Probes between consecutive epochs.
const PROBES_PER_EPOCH: usize = 5;
/// Loss levels of the emulated corpus (`pathchoice_loss{00,30,50,70,90}`),
/// in permille to keep the schedule integer-deterministic.
const LOSS_PERMILLE: [u32; 5] = [0, 300, 500, 700, 900];
const SEEDS: [u64; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

/// SplitMix64: a seeded, dependency-free stream for the loss draws.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// One Bernoulli draw: does a frame SURVIVE a link of loss `permille`?
    fn survives(&mut self, loss_permille: u32) -> bool {
        (self.next() % 1000) as u32 >= loss_permille
    }
}

/// One epoch of the schedule: the announce race and the probe crossings.
struct Epoch {
    direct_heard: bool,
    /// Per probe: (frame out survives the direct link, proof back survives
    /// the direct link). Only consulted when the standing route is direct.
    probes: Vec<(bool, bool)>,
}

fn make_schedule(loss_permille: u32, seed: u64) -> Vec<Epoch> {
    let mut rng = SplitMix64(seed ^ (loss_permille as u64) << 32);
    (0..EPOCHS)
        .map(|_| Epoch {
            direct_heard: rng.survives(loss_permille),
            probes: (0..PROBES_PER_EPOCH)
                .map(|_| (rng.survives(loss_permille), rng.survives(loss_permille)))
                .collect(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Metrics {
    sent: u32,
    delivered: u32,
    /// Frame crossings of delivered probes only: 2 per direct round trip,
    /// 4 per two-hop one — the per-packet speed price of the route.
    frames_delivered: u32,
    /// Next-hop changes between consecutive epoch ends.
    flaps: u32,
}

impl Metrics {
    fn add(&mut self, other: Metrics) {
        self.sent += other.sent;
        self.delivered += other.delivered;
        self.frames_delivered += other.frames_delivered;
        self.flaps += other.flaps;
    }
}

/// Score one epoch's probes against the standing route and count a flap.
fn score_epoch(metrics: &mut Metrics, prev_route: &mut Option<bool>, direct: bool, epoch: &Epoch) {
    if let Some(prev) = *prev_route {
        if prev != direct {
            metrics.flaps += 1;
        }
    }
    *prev_route = Some(direct);
    for &(out_ok, back_ok) in &epoch.probes {
        metrics.sent += 1;
        let delivered = if direct { out_ok && back_ok } else { true };
        if delivered {
            metrics.delivered += 1;
            metrics.frames_delivered += if direct { 2 } else { 4 };
        }
    }
}

/// Arms (i) and (ii): the schedule through the REAL acceptance gate.
fn run_real_arm(choice: PathChoice, schedule: &[Epoch]) -> Metrics {
    let mut node: Box<TransportNode> = NodeCoreBuilder::new()
        .enable_transport(true)
        .path_choice(choice)
        .build_boxed(
            OsRng,
            MockClock::new(TEST_TIME_MS),
            MemoryStorage::with_defaults(),
        );
    let iface = node
        .transport
        .register_interface(Box::new(MockInterface::new("lora_sx1262", 0)));
    node.set_interface_name(iface, String::from("lora_sx1262"));

    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["ruletable"],
    )
    .unwrap();
    let dest_hash = *dest.hash();

    let mut metrics = Metrics::default();
    let mut prev_route = None;
    for epoch in schedule {
        node.transport.clock().advance(10_000);
        let now = node.transport.clock().now_ms();
        let ann = dest.announce(None, &mut OsRng, now, now / 1000).unwrap();
        let mut buf = [0u8; MTU];
        let len = ann.pack(&mut buf).unwrap();
        if epoch.direct_heard {
            let _ = node.handle_packet(InterfaceId(iface), &buf[..len]);
        }
        let mut relayed = Packet::unpack(&buf[..len]).unwrap();
        relayed.flags.header_type = HeaderType::Type2;
        relayed.flags.transport_type = TransportType::Transport;
        relayed.hops = 1;
        relayed.transport_id = Some(BRAVO);
        let mut rbuf = [0u8; MTU];
        let rlen = relayed.pack(&mut rbuf).unwrap();
        let _ = node.handle_packet(InterfaceId(iface), &rbuf[..rlen]);

        let path = node
            .transport
            .get_path_clone(dest_hash.as_bytes())
            .expect("a copy arrived every epoch, so a path is always installed");
        score_epoch(
            &mut metrics,
            &mut prev_route,
            path.next_hop.is_none(),
            epoch,
        );
    }
    metrics
}

/// Arm (iii): the Python reference as a pure fold. A newer emission always
/// installs (both hop arms, Transport.py:1797-1815), a same-emission copy
/// never does (Transport.py:1772; 1.5.2's gravity arm rejects at default
/// 0), so the epoch's first-heard copy wins — the direct one whenever it
/// decoded, having left the originator one frame before the relay's.
fn run_reference_arm(schedule: &[Epoch]) -> Metrics {
    let mut metrics = Metrics::default();
    let mut prev_route = None;
    for epoch in schedule {
        score_epoch(&mut metrics, &mut prev_route, epoch.direct_heard, epoch);
    }
    metrics
}

/// The table and the pinned verdict.
#[test]
fn pathchoice_rule_table() {
    std::println!(
        "#425 pathchoice rule table: {} epochs x {} probes, {} seeds per cell",
        EPOCHS,
        PROBES_PER_EPOCH,
        SEEDS.len()
    );
    std::println!("loss | rule          | delivered     | frames/delivered | flaps");
    for loss in LOSS_PERMILLE {
        let mut totals = [Metrics::default(); 3];
        for seed in SEEDS {
            let schedule = make_schedule(loss, seed);
            let hops = run_real_arm(PathChoice::Hops, &schedule);
            let evidence = run_real_arm(PathChoice::HopsAndLoss, &schedule);
            let reference = run_reference_arm(&schedule);

            // Model cross-check, per seed: in this room the shipped rule
            // and the Python reference are indistinguishable (the
            // FewerHopsWins deviation needs the shorter copy to arrive
            // SECOND, which a lost direct copy never does), so the real
            // gate must reproduce the fold exactly. This pins the replay
            // to the real acceptance path.
            assert_eq!(
                hops, reference,
                "arm (i) through the real gate diverged from the reference \
                 fold at loss={loss} seed={seed} — the replay no longer \
                 models the gate"
            );

            totals[0].add(hops);
            totals[1].add(evidence);
            totals[2].add(reference);
        }
        for (name, m) in [
            ("(i) hops", totals[0]),
            ("(ii) hops+loss", totals[1]),
            ("(iii) python", totals[2]),
        ] {
            std::println!(
                "{:>4} | {:<13} | {:>4}/{:<4} {:>3}% | {:>16.2} | {:>5}",
                loss,
                name,
                m.delivered,
                m.sent,
                m.delivered * 100 / m.sent,
                if m.delivered > 0 {
                    f64::from(m.frames_delivered) / f64::from(m.delivered)
                } else {
                    0.0
                },
                m.flaps
            );
        }

        let (hops, evidence) = (totals[0], totals[1]);
        // Verdict clause "without losing at 0 %": identical schedules give
        // identical outcomes — the evidence rule IS the hop rule on a
        // clean link.
        if loss == 0 {
            assert_eq!(
                hops, evidence,
                "at 0 % loss the evidence rule must be indistinguishable \
                 from the hop rule"
            );
        }
        // Verdict clause "delivers more at >= 50 % loss". The measured
        // switch point of the ETX² pricing is ~29 % direct loss
        // (1/(1-L)² > 2 at L > 0.293), so the 30 % row clears the bar too
        // and is pinned with it: 2026-10-05's run read (ii)/(i) delivered
        // 1343/1025 at loss 300, 1500/1016 at 500, 1564/1217 at 700,
        // 1600/1466 at 900, with (ii) flapping less in every row.
        if loss >= 300 {
            assert!(
                evidence.delivered > hops.delivered,
                "at loss={loss} the evidence rule must deliver more: \
                 (ii) {}/{} vs (i) {}/{}",
                evidence.delivered,
                evidence.sent,
                hops.delivered,
                hops.sent
            );
        }
        // Verdict clause "without flapping more", at every loss level.
        assert!(
            evidence.flaps <= hops.flaps,
            "at loss={loss} the evidence rule flapped more: (ii) {} vs (i) {}",
            evidence.flaps,
            hops.flaps
        );
    }
}
