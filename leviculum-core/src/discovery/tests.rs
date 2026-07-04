//! Wire-format tests for interface discovery.
//!
//! Golden vectors are captured byte-for-byte from Python `RNS.Discovery` +
//! `LXMF.LXStamper` over the vendored tree (`vendor/Reticulum`, `vendor/LXMF`)
//! via `tests/discovery_golden_gen.py`. They pin:
//!   * the deterministic `msgpack(info)` encoding (our encode == Python packb),
//!   * that our validator accepts a Python-generated stamp at the reported
//!     value, and
//!   * that our decode reproduces Python's surfaced `info` (name sanitisation,
//!     `discovery_hash`, type-specific fields).
//!
//! The stamp itself is a random brute-forced value, so the full `app_data` is
//! not reproducible byte-for-byte; the `packed` prefix is, and the Python stamp
//! is cross-validated here (the accepted "cross-validate bytes" path from the
//! task spec; no live end-to-end Python rnsd is stood up).

use super::*;

/// Decode a hex string into bytes (test helper).
fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn arr16(s: &str) -> [u8; TRUNCATED_HASHBYTES] {
    hx(s).try_into().unwrap()
}

fn arr32(s: &str) -> [u8; STAMP_SIZE] {
    hx(s).try_into().unwrap()
}

const TID_HEX: &str = "00112233445566778899aabbccddeeff";
const NETID_HEX: &str = "ffeeddccbbaa99887766554433221100";

// ---- Vector A: RNodeInterface, name set, nil geo, transport enabled ----
const A_PACKED: &str = "8b00ae524e6f6465496e7465726661636501c3ccfec41000112233445566778899aabbccddeeffccffaa5465737420524e6f646503c004c005c009ce33bca1000ace0001e8480b080c05";
const A_STAMP: &str = "b429796794e50f63f5c02a4e4a458434399a6d356f43e35a26d654bcfd1e583e";
const A_APP: &str = "008b00ae524e6f6465496e7465726661636501c3ccfec41000112233445566778899aabbccddeeffccffaa5465737420524e6f646503c004c005c009ce33bca1000ace0001e8480b080c05b429796794e50f63f5c02a4e4a458434399a6d356f43e35a26d654bcfd1e583e";
const A_VALUE: u32 = 14;
const A_DISCOVERY_HASH: &str = "c08fb00aa74f17a4955f8c23287a0528e4b9e248dc87911863818a7c5aa06f14";

// ---- Vector B: TCPServerInterface, float geo, transport disabled ----
const B_PACKED: &str = "8900b2544350536572766572496e7465726661636501c2ccfec41000112233445566778899aabbccddeeffccffad4261636b626f6e65204e6f646503cb404bd68a71de69ad04cb402922f837b4a23405cb402500000000000002ab6578616d706c652e636f6d06cd1092";
const B_APP: &str = "008900b2544350536572766572496e7465726661636501c2ccfec41000112233445566778899aabbccddeeffccffad4261636b626f6e65204e6f646503cb404bd68a71de69ad04cb402922f837b4a23405cb402500000000000002ab6578616d706c652e636f6d06cd1092c67a26e4457e9c4974689e60409d87b3435f58537456b32399d4f4d6ba0c7d23";
const B_VALUE: u32 = 15;
const B_DISCOVERY_HASH: &str = "7faa22118423b53c28b3bf26b5a3785348cf98aedd5bf108a422b04eaef065c6";

// ---- Vector C: BackboneInterface, nil name (=> default), IFAC fields ----
const C_PACKED: &str = "8b00b14261636b626f6e65496e7465726661636501c3ccfec41000112233445566778899aabbccddeeffccffc003c004c005c002a831302e302e302e3106cd136507a56d796e657408a97365637265746b6579";
const C_APP: &str = "008b00b14261636b626f6e65496e7465726661636501c3ccfec41000112233445566778899aabbccddeeffccffc003c004c005c002a831302e302e302e3106cd136507a56d796e657408a97365637265746b6579615ac31c834c1d72c185b61d68206481539a013b8d5e7e5f5afb8c868bc01d3b";
const C_VALUE: u32 = 15;
const C_DISCOVERY_HASH: &str = "73d02a6d22453cc30e7607338a0bd377f03263e73f872603074819ffa8cf4aaa";

fn desc_a() -> InterfaceDescriptor {
    InterfaceDescriptor {
        interface_type: "RNodeInterface".into(),
        name: Some("Test RNode".into()),
        frequency: Some(868_000_000),
        bandwidth: Some(125_000),
        spreadingfactor: Some(8),
        codingrate: Some(5),
        ..Default::default()
    }
}

fn desc_b() -> InterfaceDescriptor {
    InterfaceDescriptor {
        interface_type: "TCPServerInterface".into(),
        name: Some("Backbone Node".into()),
        latitude: Some(55.6761),
        longitude: Some(12.5683),
        height: Some(10.5),
        reachable_on: Some("example.com".into()),
        port: Some(4242),
        ..Default::default()
    }
}

fn desc_c() -> InterfaceDescriptor {
    InterfaceDescriptor {
        interface_type: "BackboneInterface".into(),
        name: None,
        reachable_on: Some("10.0.0.1".into()),
        port: Some(4965),
        ifac_netname: Some("mynet".into()),
        ifac_netkey: Some("secretkey".into()),
        ..Default::default()
    }
}

// ==================== encode: our packb == Python packb ====================

#[test]
fn encode_info_matches_python_vector_a() {
    let packed = encode_info(&desc_a(), &arr16(TID_HEX), true).unwrap();
    assert_eq!(packed, hx(A_PACKED));
}

#[test]
fn encode_info_matches_python_vector_b() {
    let packed = encode_info(&desc_b(), &arr16(TID_HEX), false).unwrap();
    assert_eq!(packed, hx(B_PACKED));
}

#[test]
fn encode_info_matches_python_vector_c() {
    let packed = encode_info(&desc_c(), &arr16(TID_HEX), true).unwrap();
    assert_eq!(packed, hx(C_PACKED));
}

#[test]
fn encode_info_missing_required_field_returns_none() {
    // RNode without radio params must abort (Python returns None).
    let mut desc = desc_a();
    desc.frequency = None;
    assert!(encode_info(&desc, &arr16(TID_HEX), true).is_none());

    // TCPServer without reachable_on / port must abort.
    let mut desc = desc_b();
    desc.reachable_on = None;
    assert!(encode_info(&desc, &arr16(TID_HEX), false).is_none());
}

// ==================== decode: accept Python stamp + fields ====================

#[test]
fn parse_python_announce_vector_a() {
    let net = arr16(NETID_HEX);
    let d = parse_announce_app_data(&hx(A_APP), &net, DEFAULT_STAMP_VALUE).expect("valid");
    assert_eq!(d.interface_type, "RNodeInterface");
    assert!(d.transport);
    assert_eq!(d.name, "Test RNode");
    assert_eq!(d.transport_id, arr16(TID_HEX));
    assert_eq!(d.network_id, net);
    assert_eq!(d.value, A_VALUE);
    assert_eq!(d.stamp, arr32(A_STAMP));
    assert_eq!(d.latitude, None);
    assert_eq!(d.longitude, None);
    assert_eq!(d.height, None);
    assert_eq!(d.frequency, Some(868_000_000));
    assert_eq!(d.bandwidth, Some(125_000));
    assert_eq!(d.spreadingfactor, Some(8));
    assert_eq!(d.codingrate, Some(5));
    assert_eq!(d.discovery_hash, arr32(A_DISCOVERY_HASH));
}

#[test]
fn parse_python_announce_vector_b() {
    let net = arr16(NETID_HEX);
    let d = parse_announce_app_data(&hx(B_APP), &net, DEFAULT_STAMP_VALUE).expect("valid");
    assert_eq!(d.interface_type, "TCPServerInterface");
    assert!(!d.transport);
    assert_eq!(d.name, "Backbone Node");
    assert_eq!(d.value, B_VALUE);
    assert_eq!(d.latitude, Some(55.6761));
    assert_eq!(d.longitude, Some(12.5683));
    assert_eq!(d.height, Some(10.5));
    assert_eq!(d.reachable_on.as_deref(), Some("example.com"));
    assert_eq!(d.port, Some(4242));
    assert_eq!(d.discovery_hash, arr32(B_DISCOVERY_HASH));
}

#[test]
fn parse_python_announce_vector_c() {
    let net = arr16(NETID_HEX);
    let d = parse_announce_app_data(&hx(C_APP), &net, DEFAULT_STAMP_VALUE).expect("valid");
    assert_eq!(d.interface_type, "BackboneInterface");
    assert!(d.transport);
    // Nil name falls back to the "Discovered {type}" default.
    assert_eq!(d.name, "Discovered BackboneInterface");
    assert_eq!(d.value, C_VALUE);
    assert_eq!(d.reachable_on.as_deref(), Some("10.0.0.1"));
    assert_eq!(d.port, Some(4965));
    assert_eq!(d.ifac_netname.as_deref(), Some("mynet"));
    assert_eq!(d.ifac_netkey.as_deref(), Some("secretkey"));
    assert_eq!(d.discovery_hash, arr32(C_DISCOVERY_HASH));
}

// ==================== rejection paths ====================

#[test]
fn parse_rejects_tampered_stamp() {
    let mut app = hx(A_APP);
    let last = app.len() - 1;
    app[last] ^= 0x01; // flip a stamp bit
    assert!(parse_announce_app_data(&app, &arr16(NETID_HEX), DEFAULT_STAMP_VALUE).is_none());
}

#[test]
fn parse_rejects_insufficient_required_value() {
    // Vector A has value 14; requiring 15 must reject it.
    assert!(parse_announce_app_data(&hx(A_APP), &arr16(NETID_HEX), 15).is_none());
}

#[test]
fn parse_rejects_encrypted_flag() {
    let mut app = hx(A_APP);
    app[0] = 0b0000_0010; // FLAG_ENCRYPTED
    assert!(parse_announce_app_data(&app, &arr16(NETID_HEX), DEFAULT_STAMP_VALUE).is_none());
}

#[test]
fn parse_rejects_too_short() {
    assert!(parse_announce_app_data(&[0u8; 10], &arr16(NETID_HEX), DEFAULT_STAMP_VALUE).is_none());
}

// ==================== full emit -> receive round-trip ====================

#[test]
fn build_then_parse_roundtrip() {
    use rand_core::OsRng;
    let tid = arr16(TID_HEX);
    let net = arr16(NETID_HEX);
    let desc = desc_a();
    let app = build_announce_app_data(&desc, &tid, true, &mut OsRng).expect("built");

    // The packed prefix must be byte-identical to the golden encode.
    assert_eq!(&app[1..1 + hx(A_PACKED).len()], hx(A_PACKED).as_slice());

    let d = parse_announce_app_data(&app, &net, DEFAULT_STAMP_VALUE).expect("valid");
    assert_eq!(d.interface_type, "RNodeInterface");
    assert_eq!(d.name, "Test RNode");
    assert_eq!(d.transport_id, tid);
    assert!(d.value >= DEFAULT_STAMP_VALUE);
    assert_eq!(d.discovery_hash, arr32(A_DISCOVERY_HASH));
}

#[test]
fn aspect_filter_is_python_compatible() {
    assert_eq!(DISCOVERY_ASPECT_FILTER, "rnstransport.discovery.interface");
    assert_eq!(APP_NAME, "rnstransport");
    assert_eq!(DISCOVERY_ASPECTS, ["discovery", "interface"]);
}
