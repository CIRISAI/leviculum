//! A structured event that names an interface must stay one parseable line
//! for ANY interface name a user can configure.
//!
//! An interface name is free text from the config file (`[[TCP Uplink]]`) or
//! from discovery (`autoconnect/Dark Doodad 23`), so it can carry spaces.
//! The event-log contract is one event per line, whitespace-tokenised
//! `key=value` (docs/src/structured-event-logs.md): a raw name with a space
//! splits into bare tokens and the line no longer parses back to the key set
//! it was emitted with. The relay's `LINK_ENTRY_SET` carries two such names
//! (`recv`, `next_hop`) and is the highest-volume site in the field log
//! (612639 emissions, 252669 EVENT_FIELD_VIOLATION lines on the miauhaus
//! node, 2026-09-18): the relay path's observability event was the one that
//! could not be parsed.
//!
//! The oracle here is the production detector itself — the EventLogLayer's
//! `EVENT_FIELD_VIOLATION` line — driven against the REAL emit site in
//! `Transport::handle_link_request`, not a re-emitted copy.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use leviculum_core::constants::{RANDOM_HASHBYTES, TRUNCATED_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::transport::{Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};
use leviculum_std::event_log::Scalar;
use leviculum_std::test_support::event_log::init_event_log;

use rand_core::OsRng;

/// The interface the path is learned on — the `next_hop` side of the entry.
const NEXT_HOP_IFACE: &str = "TCP Uplink 1";
/// The interface the link request arrives on — the `recv` side.
const RECV_IFACE: &str = "autoconnect/Dark Doodad 23";

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A valid signed direct announce on the wire, so the relay learns a path.
fn make_announce_raw() -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "obsapp",
        &["ifacename"],
    )
    .unwrap();
    let id = dest.identity().unwrap();
    let random_hash = [0x42u8; RANDOM_HASHBYTES];

    let mut payload = Vec::new();
    payload.extend_from_slice(&id.public_key_bytes());
    payload.extend_from_slice(dest.name_hash());
    payload.extend_from_slice(&random_hash);

    let app_data = b"obs";
    let mut signed = Vec::new();
    signed.extend_from_slice(dest.hash().as_bytes());
    signed.extend_from_slice(&id.public_key_bytes());
    signed.extend_from_slice(dest.name_hash());
    signed.extend_from_slice(&random_hash);
    signed.extend_from_slice(app_data);
    let signature = id.sign(&signed).unwrap();
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
        hops: 0,
        transport_id: None,
        destination_hash: dest.hash().into_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; 500];
    let len = packet.pack(&mut buf).unwrap();
    (buf[..len].to_vec(), dest.hash().into_bytes())
}

/// A link request addressed at us as the designated next hop, so the relay
/// forwards it and writes the link-table entry.
fn make_link_request_raw(
    dst: [u8; TRUNCATED_HASHBYTES],
    transport_id: [u8; TRUNCATED_HASHBYTES],
) -> Vec<u8> {
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type2,
            context_flag: false,
            transport_type: TransportType::Transport,
            dest_type: DestinationType::Single,
            packet_type: PacketType::LinkRequest,
        },
        hops: 1,
        transport_id: Some(transport_id),
        destination_hash: dst,
        context: PacketContext::None,
        data: PacketData::Owned(vec![0x11u8; 64]),
    };
    let mut buf = [0u8; 500];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

fn lines_for<'a>(dump: &'a [String], event: &str) -> Vec<&'a String> {
    dump.iter()
        .filter(|l| l.starts_with(&format!("{event} ")))
        .collect()
}

/// Every token after the event name must be a `key=value` pair: a value that
/// leaked whitespace shows up here as a token without `=`.
fn keys_of(line: &str) -> Vec<&str> {
    line.split_whitespace()
        .skip(1)
        .filter_map(|tok| tok.split_once('=').map(|(k, _)| k))
        .collect()
}

fn value_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .skip(1)
        .filter_map(|tok| tok.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[test]
fn link_entry_set_stays_parseable_with_space_bearing_interface_names() {
    let evlog = init_event_log();

    let clock = TestClock(Arc::new(AtomicU64::new(100_000)));
    let identity = Identity::generate(&mut OsRng);
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut transport = Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
    transport.set_interface_name(0, NEXT_HOP_IFACE.into());
    transport.set_interface_name(1, RECV_IFACE.into());

    // Learn the path on interface 0, then relay a link request that arrives
    // on interface 1: LINK_ENTRY_SET names both interfaces.
    let (announce, dst) = make_announce_raw();
    transport.process_incoming(0, &announce).unwrap();
    transport.drain_events();

    let transport_id = *transport.identity().hash();
    let request = make_link_request_raw(dst, transport_id);
    let _ = transport.process_incoming(1, &request);

    let dump = evlog.dump();

    let entries = lines_for(&dump, "LINK_ENTRY_SET");
    assert_eq!(
        entries.len(),
        1,
        "one forwarded link request writes exactly one link-table entry; dump:\n{dump:#?}"
    );
    let line = entries[0];

    // The line parses back to the key set it was emitted with: one token per
    // field, none of them bare.
    let keys = keys_of(line);
    for k in [
        "dst",
        "remaining_hops",
        "packet_hops",
        "recv",
        "next_hop",
        "t",
    ] {
        assert!(keys.contains(&k), "LINK_ENTRY_SET missing {k}: {line}");
    }
    let token_count = line.split_whitespace().skip(1).count();
    assert_eq!(
        token_count,
        keys.len(),
        "every token after the event name must be key=value, so a name with a \
         space cannot split the line: {line}"
    );

    // Both interface names are single tokens that still identify the carrier.
    let recv = value_of(line, "recv").expect("recv present");
    let next_hop = value_of(line, "next_hop").expect("next_hop present");
    assert_eq!(recv, "autoconnect/Dark_Doodad_23", "recv: {line}");
    assert_eq!(next_hop, "TCP_Uplink_1", "next_hop: {line}");

    // The detector is the oracle: the emission site must not hand the sink a
    // value it has to rescue.
    let violations: Vec<&String> = dump
        .iter()
        .filter(|l| l.starts_with("EVENT_FIELD_VIOLATION"))
        .collect();
    assert!(
        violations.is_empty(),
        "an interface name is legitimate input, not a field violation: {violations:#?}"
    );
}

/// The same defect one layer out: an interface's own name reaching a
/// structured field from an interface implementation (BLE's `iface`,
/// RNode's `vport_iface`). `vport_iface` is the field the sink cannot
/// guess about — it is not in the sink's name-field list, so before the
/// wrapper it produced exactly the violation LINK_ENTRY_SET produced.
#[test]
fn a_site_that_knows_its_value_is_a_name_renders_it_as_one_token() {
    let evlog = init_event_log();

    tracing::debug!(
        event = "OBS_SCALAR_PROBE",
        iface = %Scalar("BLE Dongle 2"),
        vport_iface = %Scalar("RNode Multi 0/sub 1"),
    );

    let dump = evlog.dump();
    let probe = lines_for(&dump, "OBS_SCALAR_PROBE");
    assert_eq!(probe.len(), 1, "dump:\n{dump:#?}");
    let line = probe[0];

    assert_eq!(value_of(line, "iface"), Some("BLE_Dongle_2"), "{line}");
    assert_eq!(
        value_of(line, "vport_iface"),
        Some("RNode_Multi_0/sub_1"),
        "{line}"
    );
    let keys = keys_of(line);
    assert_eq!(
        line.split_whitespace().skip(1).count(),
        keys.len(),
        "no bare tokens: {line}"
    );

    let violations: Vec<&String> = dump
        .iter()
        .filter(|l| l.starts_with("EVENT_FIELD_VIOLATION") && l.contains("event=OBS_SCALAR_PROBE"))
        .collect();
    assert!(
        violations.is_empty(),
        "a name rendered at the emission site needs no rescue in the sink: {violations:#?}"
    );
}
