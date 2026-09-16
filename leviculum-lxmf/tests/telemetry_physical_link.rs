//! The physical-link sensor as a board emits it (Codeberg #236).
//!
//! The golden vectors in `telemetry_vectors.rs` already prove this sensor
//! against the real `sense.py` — but they prove it for Sideband's own
//! readings (a float snr, a float charge). What a board emits is integers,
//! and the question this file answers is what `Telemeter.from_packed`
//! reads out of *that*.
//!
//! So the expectation here is a **hand-built map**, written byte by byte
//! from the format rather than produced by our encoder: a one-entry msgpack
//! map from `SID_PHYSICAL_LINK` to the three-element `[rssi, snr, q]` list
//! that `PhysicalLink.unpack` indexes (Sideband `2000d81`, and the shape
//! `parse_physical_link` implements). Both directions are then asserted
//! against those bytes, which is what makes the test independent of the
//! encoder it is checking: a codec that packed the list in the wrong order,
//! under the wrong SID, or with a non-minimal integer width would produce
//! different bytes and fail here even though its own round trip closed.

use leviculum_lxmf::msgpack::{self, Number};
use leviculum_lxmf::telemetry::{
    Battery, PhysicalLink, Telemetry, SID_BATTERY, SID_PHYSICAL_LINK, SID_TEMPERATURE, SID_TIME,
};

/// `{SID_PHYSICAL_LINK: [-92, 7, 65]}`, spelled out.
///
/// * `0x81` — fixmap, one entry.
/// * `SID_PHYSICAL_LINK` — 0x05, a positive fixint, which is its own byte.
/// * `0x93` — fixarray, three elements, the arity `PhysicalLink.unpack`
///   indexes `[0]`, `[1]` and `[2]` of.
/// * `0xD0 0xA4` — int8 -92, the dBm. Below the -32 floor of negative
///   fixint, so this is the narrowest encoding msgpack has for it.
/// * `0x07` — positive fixint, the dB.
/// * `0x41` — positive fixint 65, the quality percent.
fn hand_built_link() -> Vec<u8> {
    assert_eq!(SID_PHYSICAL_LINK, 0x05);
    vec![0x81, 0x05, 0x93, 0xD0, 0xA4, 0x07, 0x41]
}

fn board_link() -> PhysicalLink {
    PhysicalLink {
        rssi: Some(Number::Int(-92)),
        snr: Some(Number::Int(7)),
        q: Some(Number::Int(65)),
    }
}

#[test]
fn the_hand_built_map_decodes_to_the_boards_three_figures() {
    let decoded = Telemetry::decode(&hand_built_link()).unwrap();
    assert_eq!(decoded.physical_link, Some(board_link()));
    assert_eq!(
        decoded,
        Telemetry {
            physical_link: Some(board_link()),
            ..Telemetry::default()
        }
    );
}

#[test]
fn the_board_emits_exactly_the_hand_built_map() {
    let telemetry = Telemetry {
        physical_link: Some(board_link()),
        ..Telemetry::default()
    };
    assert_eq!(telemetry.encode(), hand_built_link());
}

/// A board that has heard nothing recently carries no sensor at all, not
/// a nil and not a zero: the SID byte must not appear in the blob. This is
/// the encoding half of the firmware's freshness rule
/// (`leviculum_telemetry_policy::link::reading`), and the reason it is
/// worth asserting on the bytes is that both other encodings decode to
/// something a viewer will render — nil as "sensor present, no reading",
/// and a zeroed list as a -0 dBm link.
/// The sensor IDs a blob carries, in wire order, walked as
/// `Telemeter.from_packed` walks it — key, value, key, value. A byte scan
/// would not do: `0x05` is also a plausible *value* byte, and a test that
/// cannot tell a key from a payload proves nothing about either.
fn sids(packed: &[u8]) -> Vec<i64> {
    let mut p = 0;
    let entries = msgpack::map_len(packed, &mut p).unwrap();
    let mut out = Vec::new();
    for _ in 0..entries {
        let key = msgpack::raw(packed, &mut p).unwrap();
        let mut kp = 0;
        out.push(msgpack::read_int(key, &mut kp).unwrap());
        msgpack::raw(packed, &mut p).unwrap();
    }
    out
}

#[test]
fn a_board_that_heard_nothing_emits_no_physical_link_key() {
    let silent = Telemetry {
        time: Some(1_700_000_000),
        temperature: Some(Number::Float(19.25)),
        battery: Some(Battery {
            charge_percent: Number::Int(87),
            charging: None,
            temperature: None,
        }),
        physical_link: None,
        ..Telemetry::default()
    };
    let packed = silent.encode();
    assert_eq!(sids(&packed), vec![SID_TIME, SID_BATTERY, SID_TEMPERATURE]);
    assert!(!sids(&packed).contains(&SID_PHYSICAL_LINK));
    // And the other sensors are untouched by its absence.
    assert_eq!(Telemetry::decode(&packed).unwrap(), silent);
}

/// The full shape a board with a target actually sends — time, position
/// omitted, temperature, battery, link — in one blob, with the link
/// present. Sensors go out in ascending SID order, so the physical link
/// sits between the battery and the temperature; a viewer indexes by key
/// and does not care, but the order is what makes the vectors
/// byte-comparable at all.
#[test]
fn a_boards_report_carries_the_link_between_battery_and_temperature() {
    let report = Telemetry {
        time: Some(1_700_000_000),
        battery: Some(Battery {
            charge_percent: Number::Int(87),
            charging: None,
            temperature: None,
        }),
        physical_link: Some(board_link()),
        temperature: Some(Number::Float(19.25)),
        ..Telemetry::default()
    };
    let packed = report.encode();
    assert_eq!(
        sids(&packed),
        vec![SID_TIME, SID_BATTERY, SID_PHYSICAL_LINK, SID_TEMPERATURE]
    );
    assert_eq!(Telemetry::decode(&packed).unwrap(), report);
}

/// `q` is the one slot a board may leave unset — the spreading factor has
/// no defined quality scale — and it must go on the wire as nil inside the
/// three-element list, never as a shortened list: `PhysicalLink.unpack`
/// indexes `[2]` unconditionally.
#[test]
fn an_undefined_quality_is_a_nil_in_a_three_element_list() {
    let telemetry = Telemetry {
        physical_link: Some(PhysicalLink {
            rssi: Some(Number::Int(-92)),
            snr: Some(Number::Int(7)),
            q: None,
        }),
        ..Telemetry::default()
    };
    assert_eq!(
        telemetry.encode(),
        vec![0x81, 0x05, 0x93, 0xD0, 0xA4, 0x07, 0xC0]
    );
    assert_eq!(Telemetry::decode(&telemetry.encode()).unwrap(), telemetry);
}

/// Read tolerance, the inverse direction: Sideband packs a sensor it has
/// no data for as its SID mapped to nil, and that is "sensor present, no
/// reading" — the same absent field, not a malformed message.
#[test]
fn a_nil_under_the_sid_is_an_absent_reading() {
    let packed = vec![0x81, 0x05, 0xC0];
    assert_eq!(Telemetry::decode(&packed).unwrap().physical_link, None);
}
