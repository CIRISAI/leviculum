//! Pins the initiator link-establishment timeout for a relayed (2-hop)
//! path against Python's formula (leviculum#479, periculum run 475/1).
//!
//! Python's initiator timeout (Link.py:283-284):
//!
//! ```text
//! establishment_timeout  = get_first_hop_timeout(dest)           # RPC on clients
//! establishment_timeout += ESTABLISHMENT_TIMEOUT_PER_HOP * max(1, hops)
//! ```
//!
//! with `first_hop_timeout = MTU * 8 / bitrate + DEFAULT_PER_HOP_TIMEOUT`
//! (Transport.py:2700-2703), `bitrate` being the next-hop interface's.
//! Our initiator base (leviculum-core/src/link/mod.rs
//! `establishment_timeout_ms`) is the same sum split differently:
//! `first_hop_timeout_extra_ms + ESTABLISHMENT_TIMEOUT_PER_HOP_MS * (hops+1)`,
//! where the extra term carries only the `MTU * 8 / bitrate` part.
//!
//! A shared instance client cannot see the daemon's interfaces, so like a
//! Python client it asks the daemon's `first_hop_timeout` RPC and takes
//! the answer as its first hop term (leviculum#480,
//! `NodeCore::connect_with_first_hop_timeout`; the asking is the std
//! driver's, pinned end to end in `leviculum-std/src/driver/mod.rs`,
//! `shared_instance_client_takes_the_daemons_first_hop_timeout`).
//!
//! What stays deliberate, and what this file makes a future change to
//! visible:
//!
//! 1. A node with no daemon answer (no shared instance, or the RPC failed)
//!    and no next-hop bitrate on a relayed path assumes
//!    `UNKNOWN_BITRATE_ASSUMPTION_BPS` (300 bps, LoRa SF12-class). That
//!    adds 13 333 ms; Python's client falls back to the flat per-hop
//!    default instead.
//! 2. The per-link establishment jitter (Codeberg #129) adds up to
//!    +25 % on the base. Every test here pins jitter to 0 by driving the
//!    node's RNG with a constant-zero source (`draw_establishment_jitter`
//!    is `next_u32() % (MAX+1)`).
//!
//! In run 475/1, before #480, both stacked on a client whose daemon would
//! have answered: base 13 333 + 18 000 = 31 333 ms, and the three attempts
//! drew 72, 189 and 195 permille, giving the observed 33 589 ms, 37 255 ms
//! and 37 442 ms against roughly 20 s for a Python initiator on the same
//! path.

use leviculum_core::constants::{
    DEFAULT_PER_HOP_TIMEOUT, MTU, TRUNCATED_HASHBYTES, UNKNOWN_BITRATE_ASSUMPTION_BPS,
};
use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::packet::{HeaderType, Packet, PacketType, TransportType};
use leviculum_core::traits::Clock;
use leviculum_core::transport::{Action, InterfaceId};
use leviculum_core::{
    Destination, DestinationHash, DestinationType, Direction, Identity, MemoryStorage,
};
use rand_core::{CryptoRng, OsRng, RngCore};

/// On-air bitrate of the run-475 emulated modem: LoRa SF9, BW 125 kHz,
/// CR 4/5 comes out near this figure; the exact value only has to be a
/// realistic "given bitrate" for the pin.
const SF9_CLASS_BITRATE_BPS: u32 = 1757;

/// The relay's transport id stamped on the re-emitted announce; any
/// 16 bytes that are not the origin work.
const RELAY_TRANSPORT_ID: [u8; TRUNCATED_HASHBYTES] = [0xAA; TRUNCATED_HASHBYTES];

/// Constant-zero RNG: `draw_establishment_jitter` becomes 0, so
/// `establishment_timeout_ms()` returns the bare base formula. Key
/// material drawn from it is degenerate but valid, and the link under
/// test never completes a handshake anyway.
struct ZeroRng;

impl RngCore for ZeroRng {
    fn next_u32(&mut self) -> u32 {
        0
    }
    fn next_u64(&mut self) -> u64 {
        0
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        dest.fill(0);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        dest.fill(0);
        Ok(())
    }
}

impl CryptoRng for ZeroRng {}

struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        1_000_000
    }
}

/// A raw Type-1 announce for a fresh destination on an origin node.
fn origin_announce() -> (DestinationHash, Vec<u8>) {
    let mut origin =
        NodeCoreBuilder::new().build(OsRng, FixedClock, MemoryStorage::with_defaults());
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "test",
        &["timeout", "parity"],
    )
    .expect("destination");
    let hash = *dest.hash();
    origin.register_destination(dest);
    let out = origin
        .announce_destination(&hash, Some(b"pin"))
        .expect("announce");
    let raw = out
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .next()
        .expect("announce must produce an outbound packet");
    assert_eq!(
        Packet::unpack(&raw).expect("unpack").flags.packet_type,
        PacketType::Announce
    );
    (hash, raw)
}

/// Re-shape the announce the way a relay puts it back on the air: header
/// Type 2, transport propagation, the relay's transport id, wire hops 1.
/// The receiver's inbound increment then records the path at hops 2 —
/// the run-475 topology. The announce signature does not cover the
/// header, so the retag leaves it valid.
fn relayed(raw: &[u8]) -> Vec<u8> {
    let mut packet = Packet::unpack(raw).expect("unpack");
    packet.flags.header_type = HeaderType::Type2;
    packet.flags.transport_type = TransportType::Transport;
    packet.transport_id = Some(RELAY_TRANSPORT_ID);
    packet.hops = 1;
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack");
    buf[..len].to_vec()
}

/// A jitter-free initiator node that learned the destination 2 hops out
/// on interface 0, plus the pending link's establishment timeout.
/// `first_hop_timeout_ms` is a daemon's answer handed to the connect, or
/// `None` for a node that has none.
fn two_hop_initiator_timeout_ms(
    bitrate_bps: Option<u32>,
    first_hop_timeout_ms: Option<u64>,
) -> u64 {
    let (dest_hash, raw) = origin_announce();
    let frame = relayed(&raw);

    let mut node =
        NodeCoreBuilder::new().build(ZeroRng, FixedClock, MemoryStorage::with_defaults());
    if let Some(bps) = bitrate_bps {
        node.register_interface_bitrate(0, bps);
    }
    let _ = node.handle_packet(InterfaceId(0), &frame);

    let path = node
        .get_path_clone(dest_hash.as_bytes())
        .expect("announce must install a path");
    assert_eq!(path.hops, 2, "wire hops 1 + inbound increment");
    assert_eq!(
        node.next_hop_interface_bitrate(dest_hash.as_bytes()),
        bitrate_bps,
        "the bitrate the timeout derivation will see"
    );

    let (link_id, _, _) = node
        .connect_with_first_hop_timeout(dest_hash, &[0x33; 32], first_hop_timeout_ms)
        .expect("connect");
    node.link(&link_id)
        .expect("pending link")
        .establishment_timeout_ms()
}

/// With a known next-hop bitrate and jitter 0, the initiator timeout IS
/// Python's figure for the same path (sub-millisecond floor aside):
/// `MTU*8/bitrate + DEFAULT_PER_HOP_TIMEOUT + DEFAULT_PER_HOP_TIMEOUT*hops`.
#[test]
fn two_hop_known_bitrate_matches_python_formula() {
    let timeout_ms = two_hop_initiator_timeout_ms(Some(SF9_CLASS_BITRATE_BPS), None);

    let mtu_latency_ms = (MTU as u64) * 8 * 1000 / (SF9_CLASS_BITRATE_BPS as u64);
    let per_hop_ms = DEFAULT_PER_HOP_TIMEOUT * 1000;
    let hops = 2u64;
    let python_ms = (mtu_latency_ms + per_hop_ms) + per_hop_ms * hops;

    assert_eq!(python_ms, 20_276, "the figure this test exists to pin");
    assert_eq!(
        timeout_ms, python_ms,
        "initiator base must track Python's Link.py:283-284 sum"
    );
}

/// What a daemon whose next hop runs at `bitrate_bps` answers for
/// `first_hop_timeout`, in seconds (Transport.py:2700-2703), and the
/// millisecond figure the client takes from it.
fn daemon_first_hop_answer_ms(bitrate_bps: u32) -> u64 {
    let secs = MTU as f64 * (8.0 / f64::from(bitrate_bps)) + DEFAULT_PER_HOP_TIMEOUT as f64;
    (secs * 1000.0) as u64
}

/// A shared instance client with no next-hop bitrate of its own, whose
/// daemon answers `first_hop_timeout` for an SF9-class next hop, gets
/// Python's initiator timeout for the path: the figure
/// `two_hop_known_bitrate_matches_python_formula` pins for a node that
/// sees that hop itself (leviculum#480).
#[test]
fn two_hop_daemon_answer_sets_the_first_hop_term() {
    let answer_ms = daemon_first_hop_answer_ms(SF9_CLASS_BITRATE_BPS);
    assert_eq!(answer_ms, 8_276, "the daemon's answer, 8.2766 s");

    let timeout_ms = two_hop_initiator_timeout_ms(None, Some(answer_ms));
    assert_eq!(
        timeout_ms, 20_276,
        "the daemon's first hop term plus Python's hop terms"
    );
}

/// A daemon that knows no bitrate for the next hop answers the flat
/// per-hop default, and the client adds nothing for the first hop on top
/// of it: Python's figure for that answer, not the 300 bps guess.
#[test]
fn two_hop_daemon_answer_without_bitrate_adds_no_first_hop_term() {
    let per_hop_ms = DEFAULT_PER_HOP_TIMEOUT * 1000;
    let timeout_ms = two_hop_initiator_timeout_ms(None, Some(per_hop_ms));
    assert_eq!(timeout_ms, per_hop_ms + per_hop_ms * 2);
}

/// Without a daemon answer and without a next-hop bitrate on a relayed
/// path, the node falls back to UNKNOWN_BITRATE_ASSUMPTION_BPS. This is
/// what a client whose RPC failed is left with, and it is still the
/// run-475 base (31 333 ms); a change to the fallback must move this
/// number.
#[test]
fn two_hop_without_daemon_answer_keeps_the_conservative_fallback() {
    let timeout_ms = two_hop_initiator_timeout_ms(None, None);

    let assumed_latency_ms = (MTU as u64) * 8 * 1000 / (UNKNOWN_BITRATE_ASSUMPTION_BPS as u64);
    let per_hop_ms = DEFAULT_PER_HOP_TIMEOUT * 1000;
    let hops = 2u64;
    let fallback_ms = (assumed_latency_ms + per_hop_ms) + per_hop_ms * hops;

    assert_eq!(fallback_ms, 31_333, "the run-475 base");
    assert_eq!(
        timeout_ms, fallback_ms,
        "unknown-bitrate relayed path with no daemon answer must use the 300 bps assumption"
    );
}
