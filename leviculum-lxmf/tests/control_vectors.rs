//! The remote-management control codec against byte-exact fixtures from
//! the Python reference (Codeberg #384 part 4).
//!
//! `VEC-PN-CONTROL` in `docs/src/appendix/lxmf/vectors/vectors.json` is
//! `umsgpack.packb` over a dict mirroring `compile_stats`'s insertion
//! order and value types (`reference/LXMF/LXMF/LXMRouter.py:769-836`),
//! plus the trigger acknowledgement and the `LXMPeer` error codes. Our
//! encoder must reproduce those bytes exactly, and our decoder must read
//! them back — both directions face a genuine `lxmd` in the conformance
//! corpus, and the byte pin is what makes a red there a semantics bug
//! rather than an encoding one.

mod common;

use leviculum_lxmf::{
    encode_control_ack, encode_control_error, ControlNodeStats, ControlPeerStats, ControlResponse,
    PeerError, HOPS_UNKNOWN, STATS_GET_PATH, SYNC_REQUEST_PATH, UNPEER_REQUEST_PATH,
};

fn fixture_hex(field: &str) -> Vec<u8> {
    hex::decode(common::fixture("VEC-PN-CONTROL", field)).expect("fixture hex")
}

/// The state `gen_control_vectors` packed, as our structs.
fn reference_stats() -> ControlNodeStats {
    ControlNodeStats {
        identity_hash: [0x11; 16],
        destination_hash: [0x22; 16],
        uptime_secs: 42.5,
        delivery_limit_kb: Some(1000),
        propagation_limit_kb: Some(256),
        sync_limit_kb: Some(10240),
        target_stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        max_peering_cost: 26,
        autopeer_maxdepth: Some(4),
        from_static_only: false,
        messagestore_count: 2,
        messagestore_bytes: 4096,
        messagestore_limit_bytes: Some(500_000_000),
        client_propagation_messages_received: 5,
        client_propagation_messages_served: 3,
        unpeered_propagation_incoming: 1,
        unpeered_propagation_rx_bytes: 2048,
        static_peers: 1,
        discovered_peers: 1,
        total_peers: 2,
        max_peers: Some(20),
        peers: vec![
            ControlPeerStats {
                peer_id: [0x33; 16],
                is_static: true,
                state: 0,
                alive: true,
                name: Some("pn-b".into()),
                last_heard: 1000,
                next_sync_attempt: 0,
                last_sync_attempt: 900,
                sync_backoff: 0,
                peering_timebase: 800,
                ler: 1200,
                str_rate: 40_000,
                transfer_limit_kb: Some(256),
                sync_limit_kb: Some(10240),
                target_stamp_cost: Some(16),
                stamp_cost_flexibility: Some(3),
                peering_cost: Some(18),
                peering_key_value: Some(19),
                network_distance: 1,
                rx_bytes: 100,
                tx_bytes: 200,
                acceptance_rate: 1.0,
                offered: 4,
                outgoing: 4,
                incoming: 2,
                unhandled: 0,
            },
            ControlPeerStats {
                peer_id: [0x44; 16],
                is_static: false,
                state: 0,
                alive: false,
                name: None,
                last_heard: 0,
                next_sync_attempt: 0,
                last_sync_attempt: 0,
                sync_backoff: 0,
                peering_timebase: 0,
                ler: 0,
                str_rate: 0,
                transfer_limit_kb: None,
                sync_limit_kb: None,
                target_stamp_cost: None,
                stamp_cost_flexibility: None,
                peering_cost: None,
                peering_key_value: None,
                network_distance: HOPS_UNKNOWN,
                rx_bytes: 0,
                tx_bytes: 0,
                acceptance_rate: 0.0,
                offered: 0,
                outgoing: 0,
                incoming: 0,
                unhandled: 0,
            },
        ],
    }
}

#[test]
fn request_paths_match_the_reference() {
    assert_eq!(
        STATS_GET_PATH,
        common::fixture("VEC-PN-CONTROL", "stats_get_path")
    );
    assert_eq!(
        SYNC_REQUEST_PATH,
        common::fixture("VEC-PN-CONTROL", "sync_request_path")
    );
    assert_eq!(
        UNPEER_REQUEST_PATH,
        common::fixture("VEC-PN-CONTROL", "unpeer_request_path")
    );
    assert_eq!(
        HOPS_UNKNOWN.to_string(),
        common::fixture("VEC-PN-CONTROL", "hops_unknown")
    );
}

#[test]
fn our_stats_encoding_is_the_reference_packing() {
    assert_eq!(
        reference_stats().encode(),
        fixture_hex("stats_response_hex")
    );
}

#[test]
fn the_reference_stats_packing_decodes_to_the_same_state() {
    let decoded =
        ControlNodeStats::decode(&fixture_hex("stats_response_hex")).expect("decode fixture");
    assert_eq!(decoded, reference_stats());
}

#[test]
fn ack_and_error_responses_match_the_reference_bytes() {
    assert_eq!(encode_control_ack(), fixture_hex("ack_response_hex"));
    assert_eq!(
        encode_control_error(PeerError::NoIdentity),
        fixture_hex("error_no_identity_hex")
    );
    assert_eq!(
        encode_control_error(PeerError::NoAccess),
        fixture_hex("error_no_access_hex")
    );
    assert_eq!(
        encode_control_error(PeerError::InvalidData),
        fixture_hex("error_invalid_data_hex")
    );
    assert_eq!(
        encode_control_error(PeerError::NotFound),
        fixture_hex("error_not_found_hex")
    );
    assert_eq!(
        ControlResponse::decode(&fixture_hex("error_not_found_hex")),
        Ok(ControlResponse::Error(PeerError::NotFound))
    );
}
