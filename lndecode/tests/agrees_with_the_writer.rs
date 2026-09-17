//! The oracle's calibration: what `leviculum-core` writes, `lndecode` reads.
//!
//! The decoder's value comes from sharing no code with the writer, which also
//! means nothing makes the two agree automatically. This file is where that
//! agreement is asserted — on packets the writer actually produced, not on
//! hand-built bytes (those live in the crate's unit tests, where they belong,
//! because a hand-built vector can only ever confirm the decoder against
//! itself).
//!
//! `leviculum-core` is a dev-dependency for exactly this file. The library
//! must never gain it: see the note in `lndecode/Cargo.toml`.

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationType, Direction};
use rand_core::OsRng;

/// A fixed instant so the emission assertions below say the same thing next
/// year as they do today.
const EMISSION: u64 = 1_800_000_000;

fn pack(packet: &leviculum_core::packet::Packet) -> Vec<u8> {
    let mut buf = vec![0u8; packet.packed_size()];
    let n = packet.pack(&mut buf).expect("pack");
    buf.truncate(n);
    buf
}

#[test]
fn a_real_announce_decodes_field_for_field_and_verifies() {
    let identity = Identity::generate(&mut OsRng);
    let identity_hash = *identity.hash();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lndecode",
        &["oracle"],
    )
    .expect("destination");
    let expected_dest_hash = *dest.hash().as_bytes();

    let packet = dest
        .announce(Some(b"app-data-here"), &mut OsRng, 12_000, EMISSION)
        .expect("announce");
    let raw = pack(&packet);

    let decoded = lndecode::decode(&raw, EMISSION).expect("decode");
    let v = &decoded.value;

    assert_eq!(v["flags"]["packet_type"], "announce");
    assert_eq!(v["flags"]["destination_type"], "single");
    assert_eq!(v["hops"], 0);
    assert_eq!(
        v["destination_hash"].as_str().unwrap(),
        hex(&expected_dest_hash)
    );

    let a = &v["announce"];
    // Every one of these was recomputed from the wire bytes alone.
    assert_eq!(a["identity_hash"].as_str().unwrap(), hex(&identity_hash));
    assert_eq!(a["destination_hash_matches"], true);
    assert_eq!(a["signature_valid"], true);
    assert_eq!(a["random_hash"]["emission_secs"], EMISSION);
    assert_eq!(a["app_data"]["utf8"], "app-data-here");
    assert_eq!(
        v["warnings"].as_array().unwrap(),
        &Vec::<serde_json::Value>::new(),
        "a well-formed announce must raise no finding"
    );
}

#[test]
fn one_flipped_signature_byte_is_the_only_difference_the_decoder_reports() {
    // The negative control for the test above: without it, `signature_valid`
    // could be a constant `true` and every assertion would still pass.
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lndecode",
        &["oracle"],
    )
    .expect("destination");
    let packet = dest
        .announce(None, &mut OsRng, 12_000, EMISSION)
        .expect("announce");
    let mut raw = pack(&packet);

    // Signature sits at payload offset 84 in a non-ratcheted announce; the
    // payload starts after the 19-byte header type 1.
    let sig_byte = 19 + 84;
    raw[sig_byte] ^= 0x01;

    let decoded = lndecode::decode(&raw, EMISSION).expect("decode");
    let a = &decoded.value["announce"];
    assert_eq!(a["signature_valid"], false);
    assert_eq!(
        a["destination_hash_matches"], true,
        "the destination hash is untouched by a signature flip"
    );
    let warnings = decoded.value["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("signature does not verify")),
        "expected the signature finding, got {warnings:?}"
    );
}

#[test]
fn a_ratcheted_announce_decodes_the_ratchet_the_writer_put_there() {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lndecode",
        &["ratcheted"],
    )
    .expect("destination");
    dest.enable_ratchets(&mut OsRng, 12_000)
        .expect("enable ratchets");
    let packet = dest
        .announce(Some(b"r"), &mut OsRng, 12_000, EMISSION)
        .expect("announce");
    assert!(
        packet.flags.context_flag,
        "this test is only meaningful on a ratcheted announce"
    );
    let raw = pack(&packet);

    let decoded = lndecode::decode(&raw, EMISSION).expect("decode");
    let a = &decoded.value["announce"];
    assert_eq!(
        a["ratchet"].as_str().map(str::len),
        Some(64),
        "a 32-byte ratchet, hex-encoded"
    );
    assert_eq!(a["signature_valid"], true);
    assert_eq!(a["destination_hash_matches"], true);
    assert_eq!(a["app_data"]["utf8"], "r");
}

#[test]
fn the_decoders_packet_hash_is_the_one_the_stack_dedups_on() {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lndecode",
        &["hash"],
    )
    .expect("destination");
    let packet = dest
        .announce(None, &mut OsRng, 12_000, EMISSION)
        .expect("announce");
    let raw = pack(&packet);

    let expected = leviculum_core::packet::packet_hash(&raw);
    let decoded = lndecode::decode(&raw, EMISSION).expect("decode");
    assert_eq!(
        decoded.value["packet_hash"].as_str().unwrap(),
        hex(&expected)
    );
}

#[test]
fn the_well_known_path_request_hash_is_the_one_the_transport_uses() {
    // Recomputed in the decoder from the name alone. If the two ever differ,
    // one of them is addressing a destination nobody listens on.
    let name_hash = Destination::compute_name_hash("rnstransport", &["path", "request"]);
    let expected = leviculum_core::crypto::truncated_hash(&name_hash);
    assert_eq!(lndecode::path_request_destination_hash(), expected);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
