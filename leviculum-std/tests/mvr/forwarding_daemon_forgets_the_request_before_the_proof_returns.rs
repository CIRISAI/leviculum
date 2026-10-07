//! #482 mvr: the forwarding daemon forgets the link request before the
//! proof can return.
//!
//! 480's run (emulated/duty_held_link_request_lnode_10pct, 2026-10-06
//! 22:37): alpha's lnsd forwarded a local client's link request toward a
//! destination 2 hops away over a 1757 bit/s serial interface. The proof
//! came back 29.2 s later and was dropped with "LRPROOF for <4a0870ce…>
//! on serial_0: no link_table entry found" — the link table entry that
//! routes the proof back to the client had expired, and nothing logged
//! the removal.
//!
//! The two clocks on that entry, derived:
//!
//! - Ours (`transport.rs` link-entry creation in `handle_link_request`):
//!   `proof_timeout_ms = now + (packet.hops + path_hops + 2) *
//!   DEFAULT_PER_HOP_TIMEOUT * 1000`. A local client's request enters at
//!   `packet.hops == 0` (`incoming_hop_count` undoes the IPC hop,
//!   Transport.py:1482) with `path_hops == 2`, so the entry lives
//!   `(0 + 2 + 2) * 6 s = 24 000 ms`, bitrate-independent.
//! - Python's (Transport.py:1587-1588): `proof_timeout = now +
//!   extra_link_proof_timeout(receiving_interface) +
//!   ESTABLISHMENT_TIMEOUT_PER_HOP * max(1, remaining_hops)`. The
//!   receiving interface for a local client's request is the
//!   LocalInterface at 1 Gbit/s (LocalInterface.py:121), so the extra
//!   term is `8 * 500 / 1e9 = 4 µs` and the entry lives `6 s * 2 ≈
//!   12 000 ms` — half of ours.
//!
//! Both clocks are shorter than the 29.2 s that proof needed, so the
//! loss is a property of the protocol on a duty-held path (#433), not a
//! leviculum lifetime bug: our forwarding entry already outlives
//! Python's by a factor of two on this path. The reverse table is no
//! second chance either: Python's reverse-table proof transport sits in
//! the non-LRPROOF branch (Transport.py:2244-2256), and since #485 ours
//! excludes LRPROOF the same way. Before #485 the exclusion was an
//! accident of keys: the reverse entry sits under the request's
//! truncated packet hash, which the MTU-signalling bytes make distinct
//! from the link id the proof addresses, but a request without them has
//! the two coincide, and the reverse table handed the late proof back to
//! the client. This mvr pins the 24 000 ms number, the expiry boundary,
//! that the late proof is not forwarded with or without signalling
//! bytes, and the LINK_ENTRY_EXPIRED line the sweep now emits (the line
//! 480's trace was missing).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::{ED25519_SIGNATURE_SIZE, TRUNCATED_HASHBYTES, X25519_KEY_SIZE};
use leviculum_core::link::Link;
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::storage_types::PathEntry;
use leviculum_core::transport::{Transport, TransportConfig};
use leviculum_core::{Clock, DestinationType, Identity, MemoryStorage, Storage};
use leviculum_std::test_support::event_log::init_event_log;

use rand_core::OsRng;

/// Scripted clock: the 25 s the proof is delayed by pass here, not on
/// the wall (mvr budget is < 5 s).
#[derive(Clone)]
struct ScriptedClock(Arc<AtomicU64>);
impl Clock for ScriptedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

const T0: u64 = 100_000;
/// The lifetime under test, as derived above from the creation site.
const OUR_LIFETIME_MS: u64 = 24_000;
/// Python's lifetime for the identical entry (Transport.py:1587-1588,
/// LocalInterface receiving side), for the record: shorter than ours.
#[allow(dead_code)]
const PYTHON_LIFETIME_MS: u64 = 12_000;
/// The scripted delay before the proof returns.
const PROOF_DELAY_MS: u64 = 25_000;

/// 480's shape: the request carried the MTU-signalling bytes.
#[test]
fn a_25_s_proof_finds_the_24_s_entry_gone_and_the_expiry_logged() {
    late_proof_is_dropped(true);
}

/// The request without signalling bytes: the reverse key and the link id
/// coincide, and the late LRPROOF is still not forwarded (#485).
#[test]
fn a_late_proof_is_not_forwarded_through_the_reverse_table() {
    late_proof_is_dropped(false);
}

fn late_proof_is_dropped(signalling: bool) {
    let evlog = init_event_log();

    let hands = Arc::new(AtomicU64::new(T0));
    let clock = ScriptedClock(hands.clone());
    let identity = Identity::generate(&mut OsRng);
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut transport = Transport::new(config, clock, MemoryStorage::with_defaults(), identity);

    // One daemon, two interfaces: 0 is the local client's IPC leg, 1 the
    // slow carrier toward the destination. The carrier's bitrate plays no
    // role in OUR formula (it does in Python's, 4 µs over the IPC leg).
    transport.set_interface_name(0, "client0".into());
    transport.set_interface_name(1, "serial_0".into());
    transport.set_local_client(0, true);

    // The destination is 2 hops out behind serial_0.
    let dest = [0xBB; TRUNCATED_HASHBYTES];
    transport.storage_mut().set_path(
        dest,
        PathEntry {
            hops: 2,
            expires_ms: T0 + 1_000_000,
            interface_index: 1,
            random_blobs: Vec::new(),
            next_hop: Some([0xEE; TRUNCATED_HASHBYTES]),
            via_peer: None,
        },
    );

    // The local client's link request: Type1, hops 0 on the wire. The
    // daemon undoes the IPC hop, so the entry freezes packet.hops == 0.
    // The 3 trailing MTU-signalling bytes are the real-world shape (480's
    // request had them: alpha logged "Clamped link request MTU from
    // 262144 to 564"). `Link::calculate_link_id` strips them, so the link
    // id differs from the request's truncated packet hash — the key the
    // REVERSE table entry sits under. Without them the two coincide, the
    // corner #485 closed.
    let mut lr_data = Vec::with_capacity(X25519_KEY_SIZE * 2 + 3);
    lr_data.extend_from_slice(&[0x11; X25519_KEY_SIZE]);
    lr_data.extend_from_slice(&[0x22; X25519_KEY_SIZE]);
    if signalling {
        lr_data.extend_from_slice(&[0x00, 0x02, 0x34]);
    }
    let request = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::LinkRequest,
        },
        hops: 0,
        transport_id: None,
        destination_hash: dest,
        context: PacketContext::None,
        data: PacketData::Owned(lr_data),
    };
    let mut buf = [0u8; 500];
    let len = request.pack(&mut buf).expect("pack link request");
    let raw_request = &buf[..len];
    let link_id = *Link::calculate_link_id(raw_request).as_bytes();

    transport
        .process_incoming(0, raw_request)
        .expect("daemon takes the client's link request");

    // The lifetime, stated as a number: the deadline the entry was
    // created with, against the derivation above.
    let entry = transport
        .storage()
        .get_link_entry(&link_id)
        .expect("forwarding the request must create the link table entry");
    assert_eq!(
        entry.proof_timeout_ms - T0,
        OUR_LIFETIME_MS,
        "the forwarding entry's lifetime moved; re-derive #482/#433 \
         against the new number"
    );

    // At the deadline itself the entry still stands (expiry is strictly
    // `now > proof_timeout_ms`, as Python's `time.time() > PROOF_TMO`).
    hands.store(T0 + OUR_LIFETIME_MS, Ordering::SeqCst);
    transport.poll();
    assert!(
        transport.storage().get_link_entry(&link_id).is_some(),
        "the entry must survive to its own deadline"
    );

    // One sweep past the deadline removes it — and now says so.
    hands.store(T0 + OUR_LIFETIME_MS + 1, Ordering::SeqCst);
    transport.poll();
    assert!(
        transport.storage().get_link_entry(&link_id).is_none(),
        "the entry must be gone one ms past its deadline"
    );

    // The proof returns after the scripted 25 s, as 480's returned after
    // 29.2 s: the entry that would route it to the client is gone.
    hands.store(T0 + PROOF_DELAY_MS, Ordering::SeqCst);
    let mut proof_data = Vec::with_capacity(ED25519_SIGNATURE_SIZE + X25519_KEY_SIZE);
    proof_data.extend_from_slice(&[0xDE; ED25519_SIGNATURE_SIZE]);
    proof_data.extend_from_slice(&[0x33; X25519_KEY_SIZE]);
    let proof = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Link,
            packet_type: PacketType::Proof,
        },
        hops: 1,
        transport_id: None,
        destination_hash: link_id,
        context: PacketContext::Lrproof,
        data: PacketData::Owned(proof_data),
    };
    let len = proof.pack(&mut buf).expect("pack lrproof");
    // With signalling bytes the reverse entry sits under a key distinct
    // from the link id, as in 480's run (lines 184-188: no forward, local
    // delivery found "0 links tracked"). Without them it sits under the
    // link id itself, so the reverse table could reach the proof; the
    // assertion below is what keeps it from doing so (#485).
    assert_eq!(
        transport.storage().get_reverse(&link_id).is_some(),
        !signalling,
        "the reverse entry sits under the link id exactly when the request \
         carries no signalling bytes; if not, this mvr no longer tests the \
         corner it names"
    );
    let forwarded_before = transport.stats().packets_forwarded();
    transport
        .process_incoming(1, &buf[..len])
        .expect("the late proof is processed, not an error");
    assert!(
        transport.storage().get_link_entry(&link_id).is_none(),
        "when the proof arrives the entry is not there: the daemon has \
         forgotten the request (#482)"
    );
    assert_eq!(
        transport.stats().packets_forwarded(),
        forwarded_before,
        "with no link table entry the late LRPROOF goes nowhere, with or \
         without signalling bytes (signalling={signalling}): Python's \
         reverse table never transports an LRPROOF (Transport.py:2244-2256)"
    );

    // The removal 480's trace could not find is now one event line. The
    // capture buffer is shared across this binary's tests, so match on
    // this link's id.
    let link_hex: String = link_id.iter().map(|b| format!("{b:02x}")).collect();
    let dump = evlog.dump();
    let expiry_line = dump
        .iter()
        .find(|l| l.starts_with("LINK_ENTRY_EXPIRED ") && l.contains(&link_hex))
        .unwrap_or_else(|| panic!("no LINK_ENTRY_EXPIRED line for {link_hex}; dump:\n{dump:#?}"));
    for needle in [
        "validated=0",
        &format!("age_ms={}", OUR_LIFETIME_MS + 1),
        &format!("deadline_ms={}", T0 + OUR_LIFETIME_MS),
    ] {
        assert!(
            expiry_line.contains(needle),
            "LINK_ENTRY_EXPIRED missing `{needle}`: {expiry_line}"
        );
    }
}
