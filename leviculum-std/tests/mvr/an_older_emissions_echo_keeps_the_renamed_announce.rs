//! mvr for order 1617 (Refs #232): a local client's renamed announce never
//! leaves the shared instance when Python's echo of the client's PREVIOUS
//! announce lands before the scheduler fires it.
//!
//! ## The field record
//!
//! `loquax --test python_interop lnmsg_send_reaches_a_python_lxmf_receiver`
//! red at `python_interop.rs:228` (1611's log, the instance's event dump):
//!
//! ```text
//! PKT_RX  dst=3b13f21a.. hops=0 iface=Local[..-0]/0 len=208 ph=01215a2e.. type=Announce t=717
//! ANN_TX  dst=3b13f21a.. hops=0 iface=all occasion=local t=719
//! IFACE_DOWN iface=Local[..-0]/0 idx=2 paths=1 teardown=transient t=1137
//! PKT_RX  dst=3b13f21a.. hops=0 iface=Local[..-0]/0 len=225 ph=1e518a8a.. type=Announce t=1152
//! PATH_ADD dst=3b13f21a.. hops=0 reason=new_destination t=1152
//! PKT_RX  dst=3b13f21a.. hops=2 iface=tcp_client_0 len=224 ph=01215a2e.. type=Announce t=1152
//! (no ANN_TX for 3b13f21a after this; Python re-sends the OLD one at t=7153)
//! ```
//!
//! ## The mechanism
//!
//! A local client's announce enters the announce table with
//! `retries = PATHFINDER_RETRIES` (1) and `retransmit_at_ms = now`, the
//! reference's "announced immediately, but only one time". The rate block of
//! `handle_announce` treats a copy of the same DESTINATION heard at
//! `hops == entry.hops + 2` while `retries > 0` and a retransmit is pending
//! as "our rebroadcast was forwarded by another node" and clears the
//! retransmit. Python's echo of the OLD announce (A1, relayed once by Python,
//! so hops 2 here) matches all three by destination alone and cancels the
//! RENAMED announce (A2) that never left. The client teardown removed the
//! path, so A1's random blob is no longer in the path's replay set and the
//! echo reaches that block.
//!
//! **Acceptance**: A2 leaves on the network interface although A1's echo
//! arrived before the scheduler ran. Red before the fix (cancellation keyed
//! on the destination), green once it keys on the packet the entry holds.
//!
//! One `Transport`, a hand-stepped clock: deterministic, no I/O.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::{BUILD_UNIX_SECS, MTU, RANDOM_HASHBYTES, TRUNCATED_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::transport::{Action, Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};
use rand_core::OsRng;

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

/// The network side: Python's rnsd over TCP.
const NETWORK_IFACE: usize = 0;
/// The first `loquax send`'s IPC client.
const FIRST_CLIENT: usize = 1;
/// The second (renamed) `loquax send`'s IPC client, a fresh index.
const SECOND_CLIENT: usize = 2;

fn make_random_hash(prefix: [u8; 5], emission_secs: u64) -> [u8; RANDOM_HASHBYTES] {
    let mut hash = [0u8; RANDOM_HASHBYTES];
    hash[..5].copy_from_slice(&prefix);
    hash[5..].copy_from_slice(&emission_secs.to_be_bytes()[3..]);
    hash
}

/// A valid signed announce for `dest` with `app_data`, at wire hop count
/// `hops`, relayed by `transport_id` when one is given (header type 2, as a
/// Python transport node rebroadcasts it).
fn make_announce_raw(
    dest: &Destination,
    hops: u8,
    random_hash: &[u8; RANDOM_HASHBYTES],
    app_data: &[u8],
    transport_id: Option<[u8; TRUNCATED_HASHBYTES]>,
) -> Vec<u8> {
    let identity = dest.identity().expect("destination has an identity");
    let public_key = identity.public_key_bytes();

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

    let (header_type, transport_type) = match transport_id {
        Some(_) => (HeaderType::Type2, TransportType::Transport),
        None => (HeaderType::Type1, TransportType::Broadcast),
    };
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type,
            context_flag: false,
            transport_type,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Announce,
        },
        hops,
        transport_id,
        destination_hash: *dest.hash().as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };

    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack announce");
    buf[..len].to_vec()
}

/// Announces carrying `blob` that leave towards the network interface.
fn to_network_carrying(actions: &[Action], blob: &[u8; RANDOM_HASHBYTES]) -> usize {
    actions
        .iter()
        .filter(|action| match action {
            Action::Broadcast {
                data,
                exclude_iface,
                ..
            } => {
                exclude_iface.map(|i| i.0) != Some(NETWORK_IFACE)
                    && data.windows(blob.len()).any(|w| w == blob)
            }
            Action::SendPacket { iface, data, .. } => {
                iface.0 == NETWORK_IFACE && data.windows(blob.len()).any(|w| w == blob)
            }
        })
        .count()
}

struct RenamedClientRig {
    clock: SteppedClock,
    transport: Transport<SteppedClock, MemoryStorage>,
    dest: Destination,
    blob_1: [u8; RANDOM_HASHBYTES],
    blob_2: [u8; RANDOM_HASHBYTES],
    renamed: Vec<u8>,
    old_echo: Vec<u8>,
}

/// The first client announces A1, the instance sends it, the client goes
/// away (its path with it, as `handle_interface_teardown` does through
/// `remove_paths_for_interface`), and the second client is up with the
/// renamed A2 ready. `old_echo` is Python's rebroadcast of A1.
fn renamed_client_rig() -> RenamedClientRig {
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
    transport.set_interface_name(NETWORK_IFACE, "tcp_client_0".to_string());
    transport.set_interface_name(FIRST_CLIENT, "Local[rns/mvr]/0".to_string());
    transport.set_local_client(FIRST_CLIENT, true);

    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .expect("create destination");

    let emission_1 = BUILD_UNIX_SECS + 120 * 86_400;
    let blob_1 = make_random_hash([0x11; 5], emission_1);
    let blob_2 = make_random_hash([0x22; 5], emission_1 + 1);
    let first = make_announce_raw(&dest, 0, &blob_1, b"lew", None);
    let renamed = make_announce_raw(&dest, 0, &blob_2, b"lew@schneckenschreck", None);
    // Python heard A1 at hops 0, stored 1, and rebroadcasts it with hops 1
    // under its own transport id; we count it as 2 on receipt.
    let old_echo = make_announce_raw(&dest, 1, &blob_1, b"lew", Some([0x5a; 16]));

    transport
        .process_incoming(FIRST_CLIENT, &first)
        .expect("process A1");
    clock.advance(10);
    transport.poll();
    let sent = transport.drain_actions();
    assert_eq!(
        to_network_carrying(&sent, &blob_1),
        1,
        "control: the first client's announce reaches the network"
    );

    // The first client's IPC disconnect (t=1137 in the record).
    clock.advance(400);
    transport.set_local_client(FIRST_CLIENT, false);
    transport.remove_path(dest.hash().as_bytes());

    transport.set_interface_name(SECOND_CLIENT, "Local[rns/mvr]/0".to_string());
    transport.set_local_client(SECOND_CLIENT, true);

    RenamedClientRig {
        clock,
        transport,
        dest,
        blob_1,
        blob_2,
        renamed,
        old_echo,
    }
}

/// THE pin: the renamed announce leaves although the echo of the old one
/// arrived in the same millisecond, before the scheduler ran.
#[test]
fn an_older_emissions_echo_keeps_the_renamed_announce() {
    let mut rig = renamed_client_rig();
    rig.transport
        .process_incoming(SECOND_CLIENT, &rig.renamed)
        .expect("process A2");
    rig.transport
        .process_incoming(NETWORK_IFACE, &rig.old_echo)
        .expect("process A1's echo");
    rig.clock.advance(10);
    rig.transport.poll();
    let out = rig.transport.drain_actions();
    assert_eq!(
        to_network_carrying(&out, &rig.blob_2),
        1,
        "the renamed announce of {:02x?} must reach the network: an echo of \
         an OLDER emission says nothing about whether this one was passed on",
        &rig.dest.hash().as_bytes()[..4]
    );
    assert_eq!(
        to_network_carrying(&out, &rig.blob_1),
        0,
        "the old emission's echo is not relayed back"
    );
}

/// Control: without the echo the renamed announce leaves (the rig itself
/// does not lose it).
#[test]
fn control_without_the_echo_the_renamed_announce_leaves() {
    let mut rig = renamed_client_rig();
    rig.transport
        .process_incoming(SECOND_CLIENT, &rig.renamed)
        .expect("process A2");
    rig.clock.advance(10);
    rig.transport.poll();
    let out = rig.transport.drain_actions();
    assert_eq!(to_network_carrying(&out, &rig.blob_2), 1);
}

/// A relay on the network side: neighbour announces for `dest` arrive on
/// `NETWORK_IFACE` and leave on `OTHER_IFACE`.
const OTHER_IFACE: usize = 3;

struct Relay {
    clock: SteppedClock,
    transport: Transport<SteppedClock, MemoryStorage>,
    dest: Destination,
    blob_0: [u8; RANDOM_HASHBYTES],
    blob_1: [u8; RANDOM_HASHBYTES],
}

/// Announces carrying `blob` that leave on `OTHER_IFACE`.
fn relayed_carrying(actions: &[Action], blob: &[u8; RANDOM_HASHBYTES]) -> usize {
    actions
        .iter()
        .filter(|action| match action {
            Action::Broadcast {
                data,
                exclude_iface,
                ..
            } => {
                exclude_iface.map(|i| i.0) != Some(OTHER_IFACE)
                    && data.windows(blob.len()).any(|w| w == blob)
            }
            Action::SendPacket { iface, data, .. } => {
                iface.0 == OTHER_IFACE && data.windows(blob.len()).any(|w| w == blob)
            }
        })
        .count()
}

/// Emission N1 arrives from a neighbour and the relay fires it once; its
/// second fire is pending, PATHFINDER_G out.
fn relay_after_first_fire() -> Relay {
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
    transport.set_interface_name(NETWORK_IFACE, "tcp_client_0".to_string());
    transport.set_interface_name(OTHER_IFACE, "tcp_server_0".to_string());
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .expect("create destination");
    let emission_1 = BUILD_UNIX_SECS + 120 * 86_400;
    let blob_0 = make_random_hash([0x33; 5], emission_1 - 1);
    let blob_1 = make_random_hash([0x44; 5], emission_1);
    let n1 = make_announce_raw(&dest, 0, &blob_1, b"n1", None);
    transport
        .process_incoming(NETWORK_IFACE, &n1)
        .expect("process N1");
    clock.advance(transport.announce_jitter_max_ms() + 100);
    transport.poll();
    let first = transport.drain_actions();
    assert_eq!(
        relayed_carrying(&first, &blob_1),
        1,
        "control: the relay fires N1 once"
    );
    Relay {
        clock,
        transport,
        dest,
        blob_0,
        blob_1,
    }
}

/// Fires of N1 in the next PATHFINDER_G plus a jitter window.
fn second_fires(relay: &mut Relay) -> usize {
    let step =
        leviculum_core::constants::PATHFINDER_G_MS + relay.transport.announce_jitter_max_ms() + 100;
    relay.clock.advance(step);
    relay.transport.poll();
    let out = relay.transport.drain_actions();
    relayed_carrying(&out, &relay.blob_1)
}

/// Control: a relayed announce fires a second time when nobody is heard
/// passing it on (the arm under test has something to cancel).
#[test]
fn control_a_relayed_announce_fires_twice() {
    let mut relay = relay_after_first_fire();
    assert_eq!(second_fires(&mut relay), 1);
}

/// The packet half of the fix, independent of the timing half: N1's second
/// fire is pending in the future, and a copy of an OLDER emission N0 two
/// hops on arrives. It is not N1 and says nothing about N1 having been
/// forwarded, so N1 still fires its second time.
#[test]
fn an_older_emissions_copy_keeps_a_relayed_retransmit() {
    let mut relay = relay_after_first_fire();
    let n0_on = make_announce_raw(&relay.dest, 2, &relay.blob_0, b"n0", Some([0x5a; 16]));
    relay
        .transport
        .process_incoming(NETWORK_IFACE, &n0_on)
        .expect("process N0 two hops on");
    assert_eq!(
        second_fires(&mut relay),
        1,
        "a copy of another emission must not cancel N1's pending retransmit"
    );
}

/// Control for the arm the fix must keep: a copy of N1 ITSELF two hops on,
/// heard while its retransmit is pending, still cancels it ("another node
/// forwarded our rebroadcast").
///
/// An identical copy normally dies at the replay gate before it reaches the
/// rate block (the reference checks for echoes before its replay test,
/// Transport.py:1724 vs :1772; ours runs the gate first), and an unresponsive
/// path would re-accept the copy and re-insert the entry. The control
/// therefore swaps N1's blob in the path's replay set for a sibling of the
/// same emission second by hand, the one state in which the copy reaches the
/// arm and nothing else moves.
#[test]
fn control_a_copy_of_the_same_announce_still_cancels() {
    use leviculum_core::Storage;
    let mut relay = relay_after_first_fire();
    let dest_hash = *relay.dest.hash().as_bytes();
    let mut path = relay
        .transport
        .storage()
        .get_path(&dest_hash)
        .cloned()
        .expect("N1 installed a path");
    // A sibling blob of the same emission second keeps the path's timebase
    // (so the copy is not "newer" and does not re-take the path) while the
    // copy's own blob is no longer in the replay set.
    let mut sibling = relay.blob_1;
    sibling[0] ^= 0xff;
    for blob in path.random_blobs.iter_mut() {
        if blob == &relay.blob_1 {
            *blob = sibling;
        }
    }
    relay.transport.storage_mut().set_path(dest_hash, path);
    let n1_on = make_announce_raw(&relay.dest, 2, &relay.blob_1, b"n1", Some([0x5a; 16]));
    relay
        .transport
        .process_incoming(NETWORK_IFACE, &n1_on)
        .expect("process N1 two hops on");
    assert_eq!(
        second_fires(&mut relay),
        0,
        "N1 heard two hops on was forwarded already: its retransmit is cancelled"
    );
}

/// The timing half of the fix, independent of the packet half: a copy of
/// the renamed announce ITSELF two hops on, heard while its retransmit is
/// due now (a local client's entry is due the moment it lands), does not
/// cancel it. The reference wants `now < retransmit_timeout`
/// (Transport.py:1736); a retransmit due now is the one being sent. The
/// replay set is prepared as in the same-announce control above.
#[test]
fn a_copy_against_a_retransmit_due_now_does_not_cancel_it() {
    use leviculum_core::Storage;
    let mut rig = renamed_client_rig();
    rig.transport
        .process_incoming(SECOND_CLIENT, &rig.renamed)
        .expect("process A2");
    let dest_hash = *rig.dest.hash().as_bytes();
    let mut path = rig
        .transport
        .storage()
        .get_path(&dest_hash)
        .cloned()
        .expect("A2 installed a path");
    let mut sibling = rig.blob_2;
    sibling[0] ^= 0xff;
    for blob in path.random_blobs.iter_mut() {
        if blob == &rig.blob_2 {
            *blob = sibling;
        }
    }
    rig.transport.storage_mut().set_path(dest_hash, path);
    let same_on = make_announce_raw(
        &rig.dest,
        1,
        &rig.blob_2,
        b"lew@schneckenschreck",
        Some([0x5a; 16]),
    );
    rig.transport
        .process_incoming(NETWORK_IFACE, &same_on)
        .expect("process A2 two hops on");
    rig.clock.advance(10);
    rig.transport.poll();
    let out = rig.transport.drain_actions();
    assert_eq!(
        to_network_carrying(&out, &rig.blob_2),
        1,
        "a retransmit due now is not cancelled by a copy heard in its own millisecond"
    );
}
