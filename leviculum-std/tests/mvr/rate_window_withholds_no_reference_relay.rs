//! mvr for leviculum#441 — the announce rate window withholds relays the
//! reference makes, in the four acceptance cases 2e937b65 left inside it.
//!
//! A transport node relays one emission of a destination's announce, then
//! hears a second announce for it inside the 2 s `ANNOUNCE_RATE_LIMIT_MS`
//! window that its acceptance rule takes into the path table. Each test
//! drives one acceptance branch, named by the `reason` it logs on
//! PATH_ADD:
//!
//! - `newer_emission_worse_hops`: a newer emission at more hops than the
//!   installed path (1.3.5 `Transport.py:1809-1813`).
//! - `expired_path`: an unheard blob at more hops for a path that has
//!   expired (`:1793-1802`).
//! - `unresponsive_same_emission`: the installed emission again, same blob,
//!   at more hops, for a path marked unresponsive (`:1821-1824`; the blob
//!   check is absent from this arm).
//! - `new_destination`: the destination learned again after its path was
//!   culled while its announce-table entry still stood (`:1829-1831`).
//!
//! What each side relays:
//!
//! - Python-RNS 1.3.5: **every one**. Each branch sets `should_add`, and an
//!   added announce is inserted into the announce table for retransmission
//!   (`:1886-1898`) unless `announce_rate_target` blocks it (`:1838`), which
//!   is unset by default. There is no other per-destination gate.
//! - leviculum before the fix: **none of the four**. The window accepted
//!   each announce into the path table but closed the announce table to it
//!   (`ANNOUNCE_LEARNED_NOT_RELAYED closed="rate_limited"`); only
//!   `newer_emission` at equal or fewer hops was exempt (2e937b65,
//!   leviculum#440).
//!
//! **Acceptance**: each test red on 2e937b65 (0 relays of the second
//! announce), green once every reference acceptance branch is exempt from
//! the window (1 relay, the reference's count).
//!
//! One `Transport`, a stub interface, a hand-stepped clock: deterministic,
//! no I/O, well under a second each.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::{BUILD_UNIX_SECS, MTU, RANDOM_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::transport::{Action, Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};
use rand_core::OsRng;

/// A clock the test advances by hand, shared with the transport.
#[derive(Clone)]
struct SteppedClock(Arc<AtomicU64>);

impl SteppedClock {
    fn new(start_ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start_ms)))
    }
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::Relaxed);
    }
}

impl Clock for SteppedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Random hash: 5 random-prefix bytes plus the 5-byte big-endian emission
/// second, the layout Python writes at Destination.py:282.
fn make_random_hash(prefix: [u8; 5], emission_secs: u64) -> [u8; RANDOM_HASHBYTES] {
    let mut hash = [0u8; RANDOM_HASHBYTES];
    hash[..5].copy_from_slice(&prefix);
    hash[5..].copy_from_slice(&emission_secs.to_be_bytes()[3..]);
    hash
}

/// A valid signed announce for `dest` at wire hop count `hops`, carrying
/// exactly `random_hash`.
fn make_announce_raw(
    dest: &Destination,
    hops: u8,
    random_hash: &[u8; RANDOM_HASHBYTES],
) -> Vec<u8> {
    let identity = dest.identity().expect("destination has an identity");
    let public_key = identity.public_key_bytes();
    let app_data = b"mvr441";

    let mut signed_data = Vec::new();
    signed_data.extend_from_slice(dest.hash().as_bytes());
    signed_data.extend_from_slice(&public_key);
    signed_data.extend_from_slice(dest.name_hash());
    signed_data.extend_from_slice(random_hash);
    signed_data.extend_from_slice(app_data);
    let signature = identity.sign(&signed_data).expect("sign announce");

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key);
    payload.extend_from_slice(dest.name_hash());
    payload.extend_from_slice(random_hash);
    payload.extend_from_slice(&signature);
    payload.extend_from_slice(app_data);

    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Announce,
        },
        hops,
        transport_id: None,
        destination_hash: *dest.hash().as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };

    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack announce");
    buf[..len].to_vec()
}

/// How many drained `Broadcast` actions relay this random blob at this hop
/// count. The blob is carried verbatim in every relay of its emission and
/// the hop count is the frame's second byte, so together they tell the two
/// announces of a case apart even where both carry the same blob
/// (`unresponsive_same_emission`) or a retry of the first one falls into
/// the second step.
fn relays_of(actions: &[Action], blob: &[u8; RANDOM_HASHBYTES], hops: u8) -> usize {
    actions
        .iter()
        .filter(|action| match action {
            Action::Broadcast { data, .. } => {
                data.get(1) == Some(&hops) && data.windows(blob.len()).any(|w| w == blob)
            }
            _ => false,
        })
        .count()
}

/// A credible emission second, relative to the build floor so the value
/// never ages out (the Codeberg #247 rationale).
fn emission() -> u64 {
    BUILD_UNIX_SECS + 120 * 86_400
}

/// Wire hop count of the first announce; the relay stores and relays it
/// one higher.
const FIRST_WIRE_HOPS: u8 = 1;

/// Wire hop count of every second announce: worse than the installed path,
/// and far enough above it (stored 5 against 2) that neither local
/// rebroadcast detection branch (`hops+1`, `hops+2`) reads it as an echo of
/// our own relay.
const WORSE_WIRE_HOPS: u8 = 4;

struct Rig {
    clock: SteppedClock,
    transport: Transport<SteppedClock, MemoryStorage>,
    dest: Destination,
    first_blob: [u8; RANDOM_HASHBYTES],
    fire_step: u64,
}

const IFACE: usize = 0;

/// A transport node that has accepted and relayed the first emission once.
fn relayed_first_emission(config: TransportConfig) -> Rig {
    let clock = SteppedClock::new(1_000_000);
    let mut transport = Transport::new(
        config,
        clock.clone(),
        MemoryStorage::with_defaults(),
        Identity::generate(&mut OsRng),
    );
    transport.set_interface_name(IFACE, "stub".to_string());
    transport.set_interface_online(IFACE, true);

    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr441",
        &["rate", "window"],
    )
    .expect("create destination");

    // Past the whole jitter window, so a queued rebroadcast fires; two of
    // them keep the second announce well inside the 2 s window.
    let fire_step = transport.announce_jitter_max_ms() + 100;
    assert!(
        2 * fire_step < 2_000,
        "two fire steps of {fire_step} ms must stay inside the 2 s rate \
         window, or this mvr no longer reproduces the condition"
    );

    let first_blob = make_random_hash([0x11; 5], emission());
    transport
        .process_incoming(
            IFACE,
            &make_announce_raw(&dest, FIRST_WIRE_HOPS, &first_blob),
        )
        .expect("process first emission");
    clock.advance(fire_step);
    transport.poll();
    let first = transport.drain_actions();
    assert_eq!(
        relays_of(&first, &first_blob, FIRST_WIRE_HOPS + 1),
        1,
        "control: the first emission must be relayed exactly once"
    );

    Rig {
        clock,
        transport,
        dest,
        first_blob,
        fire_step,
    }
}

fn transport_config() -> TransportConfig {
    TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    }
}

/// Feed the second announce, let its jitter elapse, and count its relays.
fn relays_of_second(rig: &mut Rig, blob: &[u8; RANDOM_HASHBYTES]) -> usize {
    let raw = make_announce_raw(&rig.dest, WORSE_WIRE_HOPS, blob);
    rig.transport
        .process_incoming(IFACE, &raw)
        .expect("process second announce");
    rig.clock.advance(rig.fire_step);
    rig.transport.poll();
    let actions = rig.transport.drain_actions();
    relays_of(&actions, blob, WORSE_WIRE_HOPS + 1)
}

#[test]
fn newer_emission_worse_hops_leaves_the_relay() {
    let mut rig = relayed_first_emission(transport_config());
    let newer = make_random_hash([0x22; 5], emission() + 1);
    assert_eq!(
        relays_of_second(&mut rig, &newer),
        1,
        "a newer emission at more hops is accepted and relayed by the \
         reference (Transport.py:1809-1813 -> :1898); ours withheld it"
    );
}

#[test]
fn expired_path_announce_leaves_the_relay() {
    // A one-second path lifetime lets the path expire inside the 2 s window
    // without a poll in between, so it is still in the table, expired, when
    // the second announce arrives (a poll would cull it, which is the
    // `new_destination` case).
    let mut rig = relayed_first_emission(TransportConfig {
        path_expiry_secs: 1,
        ..transport_config()
    });
    rig.clock.advance(1_000 - rig.fire_step);
    // Same emission second, unheard blob: neither newer nor a replay, so
    // only the expiry admits it.
    let unheard = make_random_hash([0x33; 5], emission());
    assert_eq!(
        relays_of_second(&mut rig, &unheard),
        1,
        "an unheard announce for an expired path is accepted and relayed by \
         the reference (Transport.py:1793-1802 -> :1898); ours withheld it"
    );
}

#[test]
fn unresponsive_same_emission_leaves_the_relay() {
    let mut rig = relayed_first_emission(transport_config());
    let dest_hash = *rig.dest.hash().as_bytes();
    assert!(rig.transport.mark_path_unresponsive(&dest_hash));
    let same = rig.first_blob;
    assert_eq!(
        relays_of_second(&mut rig, &same),
        1,
        "the installed emission at more hops for an unresponsive path is \
         accepted and relayed with the same blob by the reference \
         (Transport.py:1821-1824 -> :1898); ours withheld it"
    );
}

#[test]
fn new_destination_after_cull_leaves_the_relay() {
    let mut rig = relayed_first_emission(transport_config());
    let dest_hash = *rig.dest.hash().as_bytes();
    // The path goes, the announce-table entry that seeds the window stays.
    assert!(rig.transport.remove_path(&dest_hash));
    let unheard = make_random_hash([0x44; 5], emission());
    assert_eq!(
        relays_of_second(&mut rig, &unheard),
        1,
        "a destination learned again after a path cull is accepted and \
         relayed by the reference (Transport.py:1829-1831 -> :1898); ours \
         withheld it"
    );
}
