//! mvr for leviculum#440 — the announce rate window withholds a fresh
//! emission that every reference relay forwards.
//!
//! A transport node hears the same destination's announce twice within one
//! second on one interface: first the emission it already relays, then a
//! NEW emission (new random blob, next emission second) at the same hop
//! count. The pair is not a storm — the reference protocol itself produces
//! it whenever a destination's scheduled retransmit and its fresh
//! path-response answer travel the same leg (periculum 488, nightly
//! 2026-10-07 03:54, `hop_asymmetry_leviculum_initiator` fork logs).
//!
//! What each side relays:
//!
//! - Python-RNS 1.3.5 and 1.5.2: **both** emissions. The newer emission
//!   passes the acceptance rule (`announce_emitted > path_timebase` at
//!   equal-or-fewer hops, 1.3.5 `Transport.py:1765/:1772`, 1.5.2
//!   `:2236/:2238`) and overwrites the announce-table entry for
//!   retransmission (1.3.5 `:1898`, 1.5.2 `:2363`). The only
//!   per-destination rate gate either tree has is the default-off
//!   `announce_rate_target` (1.3.5 `:1838`, 1.5.2 `:2303`).
//! - leviculum before the fix: **one**. The 2 s `ANNOUNCE_RATE_LIMIT_MS`
//!   window (no reference counterpart) accepted the newer emission into
//!   the path table but suppressed its rebroadcast
//!   (`ANNOUNCE_LEARNED_NOT_RELAYED closed="rate_limited"`), so a Python
//!   neighbour behind our relay never heard the fresh emission.
//!
//! **Acceptance**: red on master HEAD (second emission withheld), green
//! once a newer emission at equal-or-fewer hops is exempt from the window.
//!
//! One `Transport`, a stub interface, a hand-stepped clock: deterministic,
//! no I/O, well under a second.

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
    let app_data = b"mvr440";

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

/// How many drained `Broadcast` actions carry this random blob. The blob is
/// 10 bytes of the announce payload, carried verbatim in every relay of its
/// emission, so it distinguishes the two emissions without parsing frames.
fn broadcasts_carrying(actions: &[Action], blob: &[u8; RANDOM_HASHBYTES]) -> usize {
    actions
        .iter()
        .filter(|action| match action {
            Action::Broadcast { data, .. } => data.windows(blob.len()).any(|w| w == blob),
            _ => false,
        })
        .count()
}

#[test]
fn two_emissions_in_one_second_both_leave_the_relay() {
    let clock = SteppedClock::new(1_000_000);
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut transport = Transport::new(
        config,
        clock.clone(),
        MemoryStorage::with_defaults(),
        Identity::generate(&mut OsRng),
    );
    // Transport is sans-I/O: interfaces are indices with registered
    // metadata, and rebroadcasts surface as `Action::Broadcast`.
    let iface = 0;
    transport.set_interface_name(iface, "stub".to_string());
    transport.set_interface_online(iface, true);

    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvr440",
        &["two", "emissions"],
    )
    .expect("create destination");

    // A credible emission second, expressed relative to the build floor so
    // the value never ages out (the Codeberg #247 rationale).
    let emission_1 = BUILD_UNIX_SECS + 120 * 86_400;
    let emission_2 = emission_1 + 1;
    // 0x11/0x22 prefixes: distinct blobs, no framing-escape bytes.
    let blob_1 = make_random_hash([0x11; 5], emission_1);
    let blob_2 = make_random_hash([0x22; 5], emission_2);
    // Same wire hop count for both; the relay stores hops+1.
    let announce_1 = make_announce_raw(&dest, 1, &blob_1);
    let announce_2 = make_announce_raw(&dest, 1, &blob_2);

    // The step the scheduler needs to fire a queued rebroadcast: past the
    // whole jitter window, and the two steps together (~1.2 s with no
    // airtime-bearing interface registered) keep the second emission well
    // inside the 2 s rate window — the condition under test.
    let fire_step = transport.announce_jitter_max_ms() + 100;
    assert!(
        2 * fire_step < 2_000,
        "two fire steps of {fire_step} ms must stay inside the 2 s rate \
         window, or this mvr no longer reproduces the condition"
    );

    // Emission 1 arrives and is relayed once the jitter elapses.
    transport
        .process_incoming(iface, &announce_1)
        .expect("process first emission");
    clock.advance(fire_step);
    transport.poll();
    let first = transport.drain_actions();
    assert_eq!(
        broadcasts_carrying(&first, &blob_1),
        1,
        "control: the first emission must be relayed exactly once"
    );

    // Emission 2 — new blob, next emission second, same hop count — arrives
    // inside the rate window.
    transport
        .process_incoming(iface, &announce_2)
        .expect("process second emission");
    clock.advance(fire_step);
    transport.poll();
    let second = transport.drain_actions();

    // The reference relays this emission (1.3.5 Transport.py:1772→:1898,
    // 1.5.2 :2238→:2363); a relay that withholds it diverges semantically.
    assert_eq!(
        broadcasts_carrying(&second, &blob_2),
        1,
        "a newer emission at equal hops must be relayed despite the rate \
         window: ours must relay 2 of 2 emissions as the reference does"
    );
}
