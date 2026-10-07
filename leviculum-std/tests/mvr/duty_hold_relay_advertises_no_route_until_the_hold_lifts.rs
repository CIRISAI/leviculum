//! mvr for leviculum#493 — a relay in duty hold advertises no route, and
//! the hold provably lifts.
//!
//! 479-482 derived that a relay at its airtime cap cannot carry a link
//! setup under any stack's clocks (initiator 20 s, forwarding entry 24 s
//! ours, 12 s Python; two held frames up to 36 s). A relay that keeps
//! rebroadcasting announces while its interface holds therefore advertises
//! routes it cannot carry, and every initiator behind it spends 20 s per
//! attempt learning that. Python's per-interface announce admission is the
//! precedent: an interface over its `announce_cap` relays no announce
//! through `outbound()` at that moment (Transport.py:1252-1294).
//!
//! The scenario, on one `Transport` with a scripted clock and the REAL
//! hold machinery (`AirtimeTracker` rolling-hour ledger + `DutyHolds`,
//! mirrored into the transport exactly as the firmware's main loop and
//! lnsd's `push_interface_state` do):
//!
//! 1. At t0 the LoRa interface's ledger sits flat at its long-term cap:
//!    the lock engages, the hold qualifies, `duty_hold` is mirrored true.
//!    An announce relayed from the uplink leaves on the uplink but is
//!    refused on the held interface with `closed=duty_hold` — and is NOT
//!    queued for later.
//! 2. The clock steps to the first bin that leaves the rolling hour: the
//!    ledger drops below the cap, the lock disengages, `duty_hold` is
//!    mirrored false, and the next announce is relayed on the formerly
//!    held interface again.
//!
//! Deterministic, no I/O, well under a second.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::{BUILD_UNIX_SECS, MTU, RANDOM_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::rnode::{AirtimeTracker, AIRTIME_BINLEN_MS, AIRTIME_LONGTERM_MS};
use leviculum_core::transport::{Action, Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};
use leviculum_queue_budget::DutyHolds;
use leviculum_std::test_support::event_log::init_event_log;
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
    let app_data = b"mvr493";

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

/// How many drained `SendPacket` actions on `iface` carry this random blob.
/// The blob is 10 bytes of the announce payload, carried verbatim in every
/// relay of its emission, so it distinguishes the two announces without
/// parsing frames.
fn relays_on(actions: &[Action], iface: usize, blob: &[u8; RANDOM_HASHBYTES]) -> usize {
    actions
        .iter()
        .filter(|action| match action {
            Action::SendPacket {
                iface: id, data, ..
            } => id.0 == iface && data.windows(blob.len()).any(|w| w == blob),
            _ => false,
        })
        .count()
}

const UPLINK: usize = 0;
const LORA: usize = 1;

/// The handover-explained closure bound a real RNode at SF8/125 kHz is in
/// the region of; any value works here, the scripted closure is an hour.
const MIN_HOLD_MS: u32 = 2_000;

#[test]
fn duty_hold_relay_advertises_no_route_until_the_hold_lifts() {
    let evlog = init_event_log();

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
    transport.set_interface_name(UPLINK, "uplink".to_string());
    transport.set_interface_online(UPLINK, true);
    transport.set_interface_name(LORA, "lora0".to_string());
    transport.set_interface_online(LORA, true);
    // The held carrier registers an announce cap, as every LoRa interface
    // does since #402 — the refusal under test is the admission in front
    // of that cap, not the cap's own pacing.
    transport.register_interface_bitrate(LORA, 1200);

    // The interface's own clock and hold machinery, scripted: a ledger
    // seeded flat at the 1 % long-term cap (twelve bins of 3000 ms) locks
    // at t0, and the gate closure it causes qualifies as a duty hold.
    let mut ledger = AirtimeTracker::new();
    ledger.set_lt_limit_u16(100);
    for i in 0..12u64 {
        ledger.add_airtime(i * AIRTIME_BINLEN_MS, 3_000);
    }
    let t0 = 12 * AIRTIME_BINLEN_MS;
    ledger.update(t0);
    assert!(ledger.is_locked(), "control: flat at the cap locks at t0");
    let mut holds = DutyHolds::new(MIN_HOLD_MS);
    holds.observe(ledger.is_locked(), t0 as u32);
    let qualified = t0 as u32 + MIN_HOLD_MS;
    assert!(holds.holding(qualified), "control: the hold qualifies");
    // What the firmware main loop / lnsd's push_interface_state do with
    // that boolean, done by hand:
    transport.set_interface_duty_hold(LORA, holds.holding(qualified));

    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr493",
        &["duty", "hold"],
    )
    .expect("create destination");

    let emission_1 = BUILD_UNIX_SECS + 120 * 86_400;
    let blob_1 = make_random_hash([0x11; 5], emission_1);
    let fire_step = transport.announce_jitter_max_ms() + 100;

    // Phase 1: held. The announce is learned and relayed — but not on the
    // held interface, and nothing is queued there for later.
    transport
        .process_incoming(UPLINK, &make_announce_raw(&dest, 1, &blob_1))
        .expect("process first announce");
    clock.advance(fire_step);
    transport.poll();
    let held_actions = transport.drain_actions();
    assert_eq!(
        relays_on(&held_actions, UPLINK, &blob_1),
        1,
        "control: the relay itself still runs (uplink gets the rebroadcast)"
    );
    assert_eq!(
        relays_on(&held_actions, LORA, &blob_1),
        0,
        "a relay in duty hold must advertise no route on the held interface"
    );
    let dump = evlog.dump();
    let refusal = dump
        .iter()
        .find(|l| l.starts_with("ANN_TX_SUPPRESSED ") && l.contains("iface=lora0"))
        .unwrap_or_else(|| panic!("no ANN_TX_SUPPRESSED line for lora0; dump:\n{dump:#?}"));
    assert!(
        refusal.contains("closed=duty_hold"),
        "the refusal must name the duty-hold gate: {refusal}"
    );

    // Phase 2: the clock steps to the first bin that leaves the rolling
    // hour, the ledger drops below the cap, the lock disengages, the hold
    // lifts — and the mirror carries the lift to the transport.
    let lift = AIRTIME_LONGTERM_MS - AIRTIME_BINLEN_MS;
    ledger.update(lift);
    assert!(
        !ledger.is_locked(),
        "the first freed bin must lift the lock"
    );
    holds.observe(ledger.is_locked(), lift as u32);
    assert!(!holds.holding(lift as u32), "duty_hold reads false again");
    transport.set_interface_duty_hold(LORA, holds.holding(lift as u32));
    clock.advance(lift - t0);

    let emission_2 = emission_1 + (lift - t0) / 1_000;
    let blob_2 = make_random_hash([0x22; 5], emission_2);
    transport
        .process_incoming(UPLINK, &make_announce_raw(&dest, 1, &blob_2))
        .expect("process second announce");
    clock.advance(fire_step);
    transport.poll();
    let lifted_actions = transport.drain_actions();
    assert_eq!(
        relays_on(&lifted_actions, LORA, &blob_2),
        1,
        "once the window frees budget the interface must relay again"
    );
}
