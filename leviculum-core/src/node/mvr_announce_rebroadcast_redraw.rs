//! The announce-rebroadcast wait is drawn per EVENT, not frozen per
//! (identity, destination).
//!
//! ## The defect this file pins
//!
//! Two transport nodes A and B that both rebroadcast a third party's
//! announce are released by the SAME received frame, so their key-ups are
//! separated by `delta = wait_A - wait_B`. Until 2026-09-22 that wait was
//! `deterministic_jitter_ms(dest_hash, ceiling)` — identity hash XOR
//! destination hash, modulo the ceiling — so `delta` was a CONSTANT of the
//! (A, B, destination) triple. A triple whose delta lands inside the
//! carrier-sense blind window stays there for every announce of that
//! destination, for as long as both identities exist.
//!
//! The mean loss is NOT what changes: averaged over all triples a frozen
//! delta and a fresh draw destroy the same fraction of announces. What the
//! freeze changed is the distribution — the loss stops being spread thinly
//! over triples and becomes a standing property of a few of them.
//!
//! ## What actually makes it a Priority 1 defect, and it is measured
//!
//! The retry does not heal it. A third-party announce is stored at
//! `retries = 0`; when it fires, the reschedule reads `retries` BEFORE
//! incrementing it, so its backoff factor is `1 << 0 == 1` and it asks the
//! SAME ceiling for a second number. On the old code that second number was
//! the same number, to the millisecond: 261 schedule/reschedule pairs out of
//! archived emulated-medium runs, 0 redrawn beyond a 5 ms tolerance, largest
//! |redraw| 0.89 ms, in periculum's
//! `emulated/announce_rebroadcast_lock_lnsd.toml`, whose header carries the
//! figures and the arithmetic behind them. So the
//! retransmission landed at the identical relative offset and collided for
//! exactly the same reason, as often as it was tried.
//!
//! That is why [`the_reschedule_does_not_repeat_the_schedule`] is the
//! headline test here and not a nicety: a fix that made the wait fresh per
//! ANNOUNCE but not per DRAW would leave the locked retry — the half that
//! matters — exactly as it was.
//!
//! ## The reference
//!
//! `retransmit_timeout = now + (RNS.rand() * Transport.PATHFINDER_RW)`
//! (`reference/Reticulum RNS/Transport.py:1873`): a fresh draw at every
//! scheduling event. We keep a hash instead of an RNG — the wait stays a
//! pure function of (identity, destination, announce, draw), so a test can
//! replay it, which [`the_same_node_and_announce_replay_the_same_waits`]
//! holds us to. Bounds are unchanged and were already the reference's;
//! `mvr_announce_rebroadcast_window` pins those.
//!
//! Sans-I/O: 2 nodes at most, seeded identities, deterministic, sub-second.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::{CryptoRng, RngCore};

use crate::constants::{MTU, PATHFINDER_G_MS, PATHFINDER_RW_MS, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, Storage};
use crate::transport::InterfaceId;

/// Deterministic xorshift64* RNG, so a node seeded with the same number is
/// the same node with the same identity on every run and the waits below are
/// fixed values rather than a sample. Not cryptographically strong; the
/// `CryptoRng` marker is only there to satisfy `NodeCore`'s bound. Same
/// shape as `mvr_probe_announce_phase`.
struct SeededRng(u64);

impl RngCore for SeededRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for SeededRng {}

type SeededNode = NodeCore<SeededRng, MockClock, MemoryStorage>;

/// One transport node: the announce arrives on `in`, the rebroadcast has
/// somewhere to go on `out`. Both interfaces are fast, so the jitter ceiling
/// is the reference's `PATHFINDER_RW`.
fn make_relay(seed: u64) -> SeededNode {
    let mut node = NodeCoreBuilder::new().enable_transport(true).build(
        SeededRng(seed),
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    );
    for name in ["in", "out"] {
        let idx = node
            .transport
            .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
        node.set_interface_name(idx, String::from(name));
    }
    node
}

/// A third party's destination and `count` DIFFERENT announce packets for
/// it — same destination hash, different packets, exactly like a node that
/// announces itself again an interval later.
fn make_announces(seed: u64, count: usize) -> ([u8; TRUNCATED_HASHBYTES], Vec<Vec<u8>>) {
    let mut rng = SeededRng(seed);
    let identity = Identity::generate(&mut rng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["annredraw"],
    )
    .unwrap();
    let dest_hash = *dest.hash().as_bytes();
    let mut raws = Vec::new();
    for _ in 0..count {
        let ann = dest
            .announce(None, &mut rng, TEST_TIME_MS, TEST_TIME_MS / 1000)
            .unwrap();
        let mut buf = [0u8; MTU];
        let len = ann.pack(&mut buf).unwrap();
        raws.push(buf[..len].to_vec());
    }
    (dest_hash, raws)
}

/// Feed `raw` to `relay` and return the wait it scheduled, in ms after
/// receipt. Panics if nothing was queued — a silent "no entry" would make
/// every assertion below vacuous.
fn schedule_wait_ms(relay: &mut SeededNode, dest: &[u8; TRUNCATED_HASHBYTES], raw: &[u8]) -> u64 {
    let t0 = relay.transport().clock().now_ms();
    let _ = relay.handle_packet(InterfaceId(0), raw);
    let entry = relay
        .transport()
        .storage()
        .get_announce(dest)
        .expect("a transport node queues a third party's announce for rebroadcast")
        .clone();
    assert_eq!(
        entry.retries, 0,
        "a third-party announce is stored at retries=0; this pairing depends on it"
    );
    entry
        .retransmit_at_ms
        .expect("a queued rebroadcast has a due time")
        - t0
}

/// Let the queued rebroadcast fire and return the wait the RESCHEDULE drew,
/// in ms after the firing and net of the grace the reschedule adds. This is
/// the same arithmetic periculum's `expect_rebroadcast_redraw` does on the
/// daemon logs: `redraw = (T1 - T0) - (wall1 - wall0) - PATHFINDER_G`.
fn reschedule_wait_ms(relay: &mut SeededNode, dest: &[u8; TRUNCATED_HASHBYTES], due: u64) -> u64 {
    relay.transport().clock().set(due);
    let _ = relay.handle_timeout();
    let entry = relay
        .transport()
        .storage()
        .get_announce(dest)
        .expect("the entry survives its first firing: retries=1 is not past PATHFINDER_RETRIES")
        .clone();
    assert_eq!(
        entry.retries, 1,
        "the entry must have FIRED, not been deferred; a deferral leaves retries at 0"
    );
    entry
        .retransmit_at_ms
        .expect("the fired entry is rescheduled")
        - due
        - PATHFINDER_G_MS
}

/// THE pin, and the one that is red on the frozen wait: an announce's
/// rebroadcast and that rebroadcast's own retry are two draws, not one.
///
/// Both ask the same ceiling — the reschedule reads `retries` before
/// incrementing it, so its backoff factor is `1 << 0 == 1` — which is
/// precisely why nothing but a per-EVENT draw can separate them. Folding
/// the announce's packet hash in and stopping there would not: both draws
/// are for the same packet.
#[test]
fn the_reschedule_does_not_repeat_the_schedule() {
    let (dest, raws) = make_announces(0x5EED_0001, 1);
    let mut relay = make_relay(0xA11CE);

    let first = schedule_wait_ms(&mut relay, &dest, &raws[0]);
    let due = relay.transport().clock().now_ms() + first;
    let second = reschedule_wait_ms(&mut relay, &dest, due);

    assert_ne!(
        first, second,
        "schedule and reschedule drew the same wait ({first} ms); the retry lands at the \
         identical relative offset and collides for the same reason it collided the first time"
    );
    for (label, wait) in [("schedule", first), ("reschedule", second)] {
        assert!(
            wait < PATHFINDER_RW_MS,
            "the {label} wait must stay inside the reference window, got {wait} ms"
        );
    }
}

/// Fresh per ANNOUNCE, the other half: one node, one destination, two
/// different announce packets. On the frozen wait both waits were the same
/// number, which is what made an unlucky (A, B, destination) triple a
/// standing property instead of an unlucky event.
///
/// Two relays with the SAME seed rather than one relay twice: identical
/// identities, so the only thing that differs between the two draws is the
/// announce itself. Feeding both announces to one node would instead
/// measure the second-announce path (dedup, rate limiting), which is not
/// the claim.
#[test]
fn two_announces_of_one_destination_draw_different_waits() {
    let (dest, raws) = make_announces(0x5EED_0002, 2);
    assert_ne!(raws[0], raws[1], "two announces must be different packets");

    let mut first_relay = make_relay(0xB0B);
    let mut second_relay = make_relay(0xB0B);
    let first = schedule_wait_ms(&mut first_relay, &dest, &raws[0]);
    let second = schedule_wait_ms(&mut second_relay, &dest, &raws[1]);

    assert_ne!(
        first, second,
        "the same node drew the same wait ({first} ms) for two different announces of one \
         destination: the wait is still a function of (identity, destination)"
    );
}

/// What the identity XOR bought, and what the fix must not spend: two
/// neighbours released by the same frame still draw different waits. The
/// per-event material is common to both of them, so it cannot cancel the
/// identity difference — this test is what says so.
#[test]
fn two_neighbours_still_split_on_the_same_announce() {
    let (dest, raws) = make_announces(0x5EED_0003, 1);

    let mut a = make_relay(0xAAAA);
    let mut b = make_relay(0xBBBB);
    let wait_a = schedule_wait_ms(&mut a, &dest, &raws[0]);
    let wait_b = schedule_wait_ms(&mut b, &dest, &raws[0]);

    assert_ne!(
        wait_a, wait_b,
        "two identities drew the same wait ({wait_a} ms) for one announce: the decorrelation \
         the identity XOR provides is gone and synchronous relays key up together"
    );
}

/// The reason this is a hash and not `RNS.rand()`: the waits are replayable.
/// A node with the same identity, fed the same announce, walking the same
/// schedule, produces the same two numbers — otherwise the assertions above
/// would be sampling rather than pinning, and a rig measurement could not be
/// reproduced off the rig.
#[test]
fn the_same_node_and_announce_replay_the_same_waits() {
    let run = || {
        let (dest, raws) = make_announces(0x5EED_0004, 1);
        let mut relay = make_relay(0xC0FFEE);
        let first = schedule_wait_ms(&mut relay, &dest, &raws[0]);
        let due = relay.transport().clock().now_ms() + first;
        let second = reschedule_wait_ms(&mut relay, &dest, due);
        (first, second)
    };
    assert_eq!(
        run(),
        run(),
        "the wait must be reproducible: same identity, same announce, same draw, same number"
    );
}
